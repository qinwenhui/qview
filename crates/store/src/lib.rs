//! `qview-store` — 本地结构化存储层（TODO-shared-engine-and-local-store §2）。
//!
//! ## 定位
//! - 只存结构化元数据 + AI 会话内容，**绝不存日志内容**（日志永远是 mmap + `.qli`）。
//! - 后端抽象：`Storage` trait；当前实现 `RedbStore`（纯 Rust、单文件、事务）。
//!   `rusqlite` 若后续需要复杂 SQL 只是换一个实现。
//! - 崩溃安全：redb 事务；写整会话单事务原子提交。
//! - 启动容错：DB 文件损坏 → `open_store` 返回 Err，调用方回退 `NullStore`，
//!   **绝不让程序起不来**。

pub mod model;
mod redb_store;

use std::sync::Arc;

use anyhow::Result;

pub use model::{
    FileMeta, SearchEntry, SessionMeta, StoredSession, StoreMessage, StoreRole, StoreStatus,
    ToolCallRecord,
};
pub use redb_store::RedbStore;

/// 存储后端抽象（`Send + Sync`，任意线程可调；实现内部保证线程安全）。
pub trait Storage: Send + Sync + std::fmt::Debug + 'static {
    /// 保存（upsert）一个完整会话（meta + messages），单事务原子。
    fn save_session(&self, session: &StoredSession) -> Result<()>;

    /// 按 id 加载完整会话。
    fn load_session(&self, id: &str) -> Result<Option<StoredSession>>;

    /// 最近会话列表（按 `finished_at_ms` 倒序，取前 `limit` 条，仅元信息）。
    fn recent_sessions(&self, limit: u64) -> Result<Vec<SessionMeta>>;

    /// 与某文件关联的会话（按 `file_id` 过滤，倒序）。
    fn sessions_for_file(&self, path: &str) -> Result<Vec<SessionMeta>>;

    /// 记录一次文件打开（upsert）。
    fn record_file(&self, meta: &FileMeta) -> Result<()>;

    /// 按 canonical path 查单个文件的元数据。
    fn load_file(&self, path: &str) -> Result<Option<FileMeta>>;

    /// 最近打开的文件（按 `last_opened_at_ms` 倒序，取前 `limit` 条）。
    fn load_files(&self, limit: u64) -> Result<Vec<FileMeta>>;

    /// 记录一次搜索（upsert：去重 + 刷新时间戳 + 计数）。
    fn record_search(&self, query: &str) -> Result<()>;

    /// 原样写入一条搜索记录（upsert，不刷新时间/计数）——用于旧 config 迁移时
    /// 保留原始顺序与使用次数。
    fn save_search_entry(&self, entry: &SearchEntry) -> Result<()>;

    /// 最近搜索（按 `last_used_at_ms` 倒序，取前 `limit` 条）。
    fn recent_searches(&self, limit: u64) -> Result<Vec<SearchEntry>>;

    /// 批量保存一个会话的全部工具调用记录（整会话覆盖写，单事务）。
    /// 每次工具完成就全量写一次：记录量级小（单会话几十条），redb 事务开销可忽略。
    fn save_tool_calls(&self, session_id: &str, calls: &[ToolCallRecord]) -> Result<()>;

    /// 按会话取工具调用记录（按 `seq` 升序；无则空）。
    fn tool_calls_for_session(&self, session_id: &str) -> Result<Vec<ToolCallRecord>>;

    /// 清空文件元数据表（缓存管理的「清空最近文件」）。
    fn clear_files(&self) -> Result<()>;

    /// 清空搜索历史表（缓存管理的「清空搜索历史」）。
    fn clear_searches(&self) -> Result<()>;
}

/// 空实现：损坏 / 未配置时兜底，所有操作 no-op，保证程序可启动。
#[derive(Debug, Clone, Default)]
pub struct NullStore;

impl Storage for NullStore {
    fn save_session(&self, _s: &StoredSession) -> Result<()> {
        Ok(())
    }
    fn load_session(&self, _id: &str) -> Result<Option<StoredSession>> {
        Ok(None)
    }
    fn recent_sessions(&self, _limit: u64) -> Result<Vec<SessionMeta>> {
        Ok(Vec::new())
    }
    fn sessions_for_file(&self, _path: &str) -> Result<Vec<SessionMeta>> {
        Ok(Vec::new())
    }
    fn record_file(&self, _meta: &FileMeta) -> Result<()> {
        Ok(())
    }
    fn load_file(&self, _path: &str) -> Result<Option<FileMeta>> {
        Ok(None)
    }
    fn load_files(&self, _limit: u64) -> Result<Vec<FileMeta>> {
        Ok(Vec::new())
    }
    fn record_search(&self, _query: &str) -> Result<()> {
        Ok(())
    }
    fn save_search_entry(&self, _entry: &SearchEntry) -> Result<()> {
        Ok(())
    }
    fn recent_searches(&self, _limit: u64) -> Result<Vec<SearchEntry>> {
        Ok(Vec::new())
    }
    fn save_tool_calls(&self, _session_id: &str, _calls: &[ToolCallRecord]) -> Result<()> {
        Ok(())
    }
    fn tool_calls_for_session(&self, _session_id: &str) -> Result<Vec<ToolCallRecord>> {
        Ok(Vec::new())
    }
    fn clear_files(&self) -> Result<()> {
        Ok(())
    }
    fn clear_searches(&self) -> Result<()> {
        Ok(())
    }
}

/// 打开（或创建）DB 并返回 `Arc<dyn Storage>`。
///
/// - 文件不存在 → 创建（含父目录）。
/// - 文件损坏 → 返回 Err（调用方决定回退 `NullStore` 或上报）。
pub fn open_store(path: impl AsRef<std::path::Path>) -> Result<Arc<dyn Storage>> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let store = RedbStore::open(path)?;
    Ok(Arc::new(store))
}

/// 便捷：`open_store` 失败时返回 `NullStore`（启动容错的一行式调用）。
pub fn open_store_or_null(path: impl AsRef<std::path::Path>) -> Arc<dyn Storage> {
    match open_store(path) {
        Ok(s) => s,
        Err(_) => Arc::new(NullStore),
    }
}
