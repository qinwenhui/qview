//! `RedbStore` — redb 后端实现（纯 Rust，单文件，事务）。
//!
//! 表结构：
//! - `sessions`: session_id (`&str`) → 完整 `StoredSession`（meta + messages，serde_json 字节）
//! - `files`:    canonical path (`&str`) → `FileMeta`（serde_json 字节）
//!
//! 查询：`recent_sessions` / `sessions_for_file` 走全表遍历 + 内存排序
//! （会话量级为百，够用；量大时再加时间索引表）。

use std::path::Path;

use anyhow::Context as _;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::model::{
    FileMeta, SearchEntry, SessionMeta, StoredSession, ToolCallRecord,
};
use crate::Storage;

const SESSIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("sessions");
const FILES: TableDefinition<&str, &[u8]> = TableDefinition::new("files");
const SEARCH_HISTORY: TableDefinition<&str, &[u8]> = TableDefinition::new("search_history");
const TOOL_CALLS: TableDefinition<&str, &[u8]> = TableDefinition::new("tool_calls");

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// redb 后端。`Database` 是 `Send + Sync`，`begin_write` 内部串行，多线程写安全。
#[derive(Debug)]
pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// 打开（不存在则创建）。父目录由 `crate::open_store` 保证存在。
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let db = Database::create(path)
            .with_context(|| format!("open redb store {}", path.display()))?;
        // 确保表存在（首写事务内建表）
        let write_txn = db.begin_write().context("begin write txn")?;
        {
            let mut _sessions = write_txn.open_table(SESSIONS).context("open sessions table")?;
            let mut _files = write_txn.open_table(FILES).context("open files table")?;
            let mut _search = write_txn
                .open_table(SEARCH_HISTORY)
                .context("open search_history table")?;
            let mut _tools = write_txn
                .open_table(TOOL_CALLS)
                .context("open tool_calls table")?;
        }
        write_txn.commit().context("commit init txn")?;
        Ok(Self { db })
    }
}

fn encode<T: serde::Serialize>(v: &T) -> anyhow::Result<Vec<u8>> {
    serde_json::to_vec(v).context("serialize")
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    serde_json::from_slice(bytes).context("deserialize")
}

impl Storage for RedbStore {
    fn save_session(&self, session: &StoredSession) -> anyhow::Result<()> {
        let bytes = encode(session)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SESSIONS)?;
            table.insert(session.meta.id.as_str(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn load_session(&self, id: &str) -> anyhow::Result<Option<StoredSession>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SESSIONS)?;
        match table.get(id)? {
            Some(guard) => {
                let session = decode(guard.value())?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    fn recent_sessions(&self, limit: u64) -> anyhow::Result<Vec<SessionMeta>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SESSIONS)?;
        let mut metas = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let session: StoredSession = decode(value.value())?;
            metas.push(session.meta);
        }
        metas.sort_by(|a, b| b.finished_at_ms.cmp(&a.finished_at_ms));
        if (metas.len() as u64) > limit {
            metas.truncate(limit as usize);
        }
        Ok(metas)
    }

    fn sessions_for_file(&self, path: &str) -> anyhow::Result<Vec<SessionMeta>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SESSIONS)?;
        let mut metas = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let session: StoredSession = decode(value.value())?;
            if session.meta.file_id.as_deref() == Some(path) {
                metas.push(session.meta);
            }
        }
        metas.sort_by(|a, b| b.finished_at_ms.cmp(&a.finished_at_ms));
        Ok(metas)
    }

    fn record_file(&self, meta: &FileMeta) -> anyhow::Result<()> {
        let bytes = encode(meta)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FILES)?;
            table.insert(meta.path.as_str(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn load_file(&self, path: &str) -> anyhow::Result<Option<FileMeta>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FILES)?;
        match table.get(path)? {
            Some(guard) => {
                let meta: FileMeta = decode(guard.value())?;
                Ok(Some(meta))
            }
            None => Ok(None),
        }
    }

    fn load_files(&self, limit: u64) -> anyhow::Result<Vec<FileMeta>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(FILES)?;
        let mut metas = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let meta: FileMeta = decode(value.value())?;
            metas.push(meta);
        }
        metas.sort_by(|a, b| b.last_opened_at_ms.cmp(&a.last_opened_at_ms));
        if (metas.len() as u64) > limit {
            metas.truncate(limit as usize);
        }
        Ok(metas)
    }

    fn record_search(&self, query: &str) -> anyhow::Result<()> {
        // 读旧值（计数）→ 写新值。分两次事务，规避 redb AccessGuard 的借用约束；
        // record_search 非热路径（用户每触发一次搜索才一次），双事务可接受。
        let count = {
            let read_txn = self.db.begin_read()?;
            let table = read_txn.open_table(SEARCH_HISTORY)?;
            match table.get(query)? {
                Some(guard) => {
                    let e: SearchEntry = decode(guard.value())?;
                    e.use_count
                }
                None => 0,
            }
        };
        let entry = SearchEntry {
            query: query.to_string(),
            last_used_at_ms: now_ms(),
            use_count: count + 1,
        };
        let bytes = encode(&entry)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SEARCH_HISTORY)?;
            table.insert(query, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn save_search_entry(&self, entry: &SearchEntry) -> anyhow::Result<()> {
        let bytes = encode(entry)?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SEARCH_HISTORY)?;
            table.insert(entry.query.as_str(), bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn recent_searches(&self, limit: u64) -> anyhow::Result<Vec<SearchEntry>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(SEARCH_HISTORY)?;
        let mut entries = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let e: SearchEntry = decode(value.value())?;
            entries.push(e);
        }
        entries.sort_by(|a, b| b.last_used_at_ms.cmp(&a.last_used_at_ms));
        if (entries.len() as u64) > limit {
            entries.truncate(limit as usize);
        }
        Ok(entries)
    }

    fn save_tool_calls(&self, session_id: &str, calls: &[ToolCallRecord]) -> anyhow::Result<()> {
        // 切片不走 encode()（它要求 Sized T）；serde_json 直接序列化切片。
        let bytes = serde_json::to_vec(calls).context("serialize tool calls")?;
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(TOOL_CALLS)?;
            table.insert(session_id, bytes.as_slice())?;
        }
        write_txn.commit()?;
        Ok(())
    }

    fn tool_calls_for_session(&self, session_id: &str) -> anyhow::Result<Vec<ToolCallRecord>> {
        let read_txn = self.db.begin_read()?;
        let table = read_txn.open_table(TOOL_CALLS)?;
        match table.get(session_id)? {
            Some(guard) => {
                let calls: Vec<ToolCallRecord> = decode(guard.value())?;
                Ok(calls)
            }
            None => Ok(Vec::new()),
        }
    }

    fn clear_files(&self) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(FILES)?;
            // redb 4.1.0 无 `clear()`，用 `pop_first` 逐个弹空。
            while table.pop_first()?.is_some() {}
        }
        write_txn.commit()?;
        Ok(())
    }

    fn clear_searches(&self) -> anyhow::Result<()> {
        let write_txn = self.db.begin_write()?;
        {
            let mut table = write_txn.open_table(SEARCH_HISTORY)?;
            while table.pop_first()?.is_some() {}
        }
        write_txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StoreMessage, StoreRole, StoreStatus};
    use crate::NullStore;

    fn tmp_db() -> (std::path::PathBuf, RedbStore) {
        let p = std::env::temp_dir().join(format!("qview-store-test-{}.db", uuid::Uuid::new_v4()));
        let store = RedbStore::open(&p).unwrap();
        (p, store)
    }

    fn sample_session(id: &str, finished: u64, file: Option<&str>) -> StoredSession {
        StoredSession {
            meta: SessionMeta {
                id: id.to_string(),
                started_at_ms: finished - 1000,
                finished_at_ms: finished,
                goal: format!("goal-{id}"),
                status: StoreStatus::Success,
                summary: format!("sum-{id}"),
                provider: "mock".into(),
                model: "dummy".into(),
                file_id: file.map(str::to_string),
                tokens_prompt: 100,
                tokens_completion: 50,
                rounds: 2,
                tool_calls: 3,
            },
            messages: vec![
                StoreMessage { role: StoreRole::User, content: "hi".into(), seq: 0 },
                StoreMessage { role: StoreRole::Assistant, content: "hello".into(), seq: 1 },
            ],
        }
    }

    #[test]
    fn round_trip_save_load() {
        let (p, store) = tmp_db();
        let s = sample_session("s1", 2000, None);
        store.save_session(&s).unwrap();
        let loaded = store.load_session("s1").unwrap().unwrap();
        assert_eq!(loaded, s);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].content, "hi");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn load_missing_returns_none() {
        let (p, store) = tmp_db();
        assert!(store.load_session("nope").unwrap().is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn recent_sessions_sorted_desc() {
        let (p, store) = tmp_db();
        store.save_session(&sample_session("a", 1000, None)).unwrap();
        store.save_session(&sample_session("b", 5000, None)).unwrap();
        store.save_session(&sample_session("c", 3000, None)).unwrap();
        let recent = store.recent_sessions(10).unwrap();
        let ids: Vec<&str> = recent.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"], "按 finished_at_ms 倒序");
        let limited = store.recent_sessions(2).unwrap();
        assert_eq!(limited.len(), 2);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn sessions_for_file_filters() {
        let (p, store) = tmp_db();
        store.save_session(&sample_session("a", 1000, Some("/x.log"))).unwrap();
        store.save_session(&sample_session("b", 2000, None)).unwrap();
        store.save_session(&sample_session("c", 3000, Some("/x.log"))).unwrap();
        let hit = store.sessions_for_file("/x.log").unwrap();
        assert_eq!(hit.len(), 2);
        assert_eq!(hit[0].id, "c", "倒序");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn file_meta_round_trip() {
        let (p, store) = tmp_db();
        store.record_file(&FileMeta {
            path: "/a.log".into(),
            last_opened_at_ms: 1000,
            open_count: 3,
            size_bytes: 1024,
            encoding: "UTF-8".into(),
        }).unwrap();
        store.record_file(&FileMeta {
            path: "/b.log".into(),
            last_opened_at_ms: 5000,
            open_count: 1,
            size_bytes: 2048,
            encoding: "UTF-8".into(),
        }).unwrap();
        let files = store.load_files(10).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "/b.log", "按 last_opened_at_ms 倒序");
        // upsert 覆盖
        store.record_file(&FileMeta {
            path: "/a.log".into(),
            last_opened_at_ms: 9000,
            open_count: 4,
            size_bytes: 1024,
            encoding: "UTF-8".into(),
        }).unwrap();
        let files = store.load_files(10).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "/a.log");
        assert_eq!(files[0].open_count, 4);
        // load_file 单条查询
        let a = store.load_file("/a.log").unwrap().unwrap();
        assert_eq!(a.open_count, 4);
        assert!(store.load_file("/missing.log").unwrap().is_none());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn upsert_session_overwrites() {
        let (p, store) = tmp_db();
        store.save_session(&sample_session("s1", 1000, None)).unwrap();
        let mut s2 = sample_session("s1", 2000, None);
        s2.meta.summary = "updated".into();
        store.save_session(&s2).unwrap();
        let loaded = store.load_session("s1").unwrap().unwrap();
        assert_eq!(loaded.meta.finished_at_ms, 2000);
        assert_eq!(loaded.meta.summary, "updated");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn search_history_round_trip_and_ordering() {
        let (p, store) = tmp_db();
        store.record_search("error 404").unwrap();
        store.record_search("timeout").unwrap();
        store.record_search("error 404").unwrap(); // upsert：去重 + 计数 +1

        let recent = store.recent_searches(10).unwrap();
        assert_eq!(recent.len(), 2, "同查询去重");
        assert_eq!(recent[0].query, "error 404", "最后使用的最前");
        assert_eq!(recent[0].use_count, 2, "计数累加");
        assert_eq!(recent[1].query, "timeout");
        let limited = store.recent_searches(1).unwrap();
        assert_eq!(limited.len(), 1);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn save_search_entry_preserves_order_for_migration() {
        // 迁移场景：旧 config 顺序 = 最近优先，给递减时间戳，recent_searches 应还原顺序。
        let (p, store) = tmp_db();
        store.save_search_entry(&SearchEntry {
            query: "a".into(),
            last_used_at_ms: 3000,
            use_count: 1,
        }).unwrap();
        store.save_search_entry(&SearchEntry {
            query: "b".into(),
            last_used_at_ms: 2000,
            use_count: 1,
        }).unwrap();
        store.save_search_entry(&SearchEntry {
            query: "c".into(),
            last_used_at_ms: 1000,
            use_count: 1,
        }).unwrap();
        let recent = store.recent_searches(10).unwrap();
        let qs: Vec<&str> = recent.iter().map(|e| e.query.as_str()).collect();
        assert_eq!(qs, vec!["a", "b", "c"], "时间戳倒序还原旧顺序");
        // 覆盖写（upsert）保留原时间戳
        store.save_search_entry(&SearchEntry {
            query: "b".into(),
            last_used_at_ms: 2000,
            use_count: 5,
        }).unwrap();
        let recent = store.recent_searches(10).unwrap();
        assert_eq!(recent.iter().find(|e| e.query == "b").unwrap().use_count, 5);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn clear_files_and_searches_empty_tables() {
        let (p, store) = tmp_db();
        store.record_file(&FileMeta {
            path: "/a.log".into(),
            last_opened_at_ms: 1000,
            open_count: 2,
            size_bytes: 10,
            encoding: "UTF-8".into(),
        }).unwrap();
        store.record_search("q1").unwrap();

        store.clear_files().unwrap();
        assert!(store.load_files(10).unwrap().is_empty());
        assert!(store.load_file("/a.log").unwrap().is_none(), "单条查询也清空");

        store.clear_searches().unwrap();
        assert!(store.recent_searches(10).unwrap().is_empty());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn tool_calls_round_trip_and_overwrite() {
        use crate::model::ToolCallRecord;
        let (p, store) = tmp_db();
        let rec = |seq, tool: &str| ToolCallRecord {
            session_id: "sess-1".into(),
            seq,
            tool: tool.into(),
            input: format!("{{\"q\":{seq}}}"),
            output: format!("out-{tool}"),
            duration_ms: 3 + seq,
            is_error: false,
            at_ms: 1000 + seq,
        };
        // 覆盖写：先写 2 条，再写 3 条（含替换）
        store.save_tool_calls("sess-1", &[rec(0, "search_text"), rec(1, "read_context")]).unwrap();
        store.save_tool_calls("sess-1", &[rec(0, "search_text"), rec(1, "read_context"), rec(2, "annotate_list")]).unwrap();
        let calls = store.tool_calls_for_session("sess-1").unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[2].tool, "annotate_list");
        assert_eq!(calls[2].seq, 2, "按 seq 顺序");
        // 其它会话独立
        assert!(store.tool_calls_for_session("sess-other").unwrap().is_empty());
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn corrupt_db_returns_error_and_null_fallback() {
        let p = std::env::temp_dir().join(format!("qview-store-corrupt-{}.db", uuid::Uuid::new_v4()));
        std::fs::write(&p, b"this is not a redb file at all, definitely corrupt").unwrap();
        // 打开损坏文件应报错（不 panic）
        assert!(RedbStore::open(&p).is_err());
        // 调用方回退 NullStore
        let store: std::sync::Arc<dyn Storage> = std::sync::Arc::new(NullStore);
        assert!(store.save_session(&sample_session("x", 5000, None)).is_ok());
        assert!(store.load_session("x").unwrap().is_none());
        let _ = std::fs::remove_file(p);
    }
}
