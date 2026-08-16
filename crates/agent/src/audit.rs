//! AuditHook + AuditSink：审计（架构文档 §11.6）。
//!
//! ## P2 范围
//! - `AuditRecord` 数据结构
//! - `AuditSink` trait + `InMemoryAuditSink` 默认实现
//! - `AuditHook` 实现 `contexa_hooks::Hook::post_tool_call` + `on_task_end`
//!
//! ## P4 落地项
//! - 落盘到 `data/audit/agent-YYYY-MM-DD.ndjson`
//! - 脱敏字段写入

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use contexa_hooks::{Hook, TaskContext};
use contexa_tools::ToolResult;

use std::io::Write as IoWrite;
use std::path::PathBuf;

/// 单条审计记录（架构 §11.6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub timestamp_ms: u64,
    pub session_id: String,
    pub tool: String,
    pub call_id: String,
    /// 入参哈希（不存原值，避免泄漏）。
    pub input_hash: String,
    pub duration_ms: u64,
    pub result_kind: AuditResult,
    pub output_bytes: usize,
    pub redactions_applied: u32,
}

/// 审计结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditResult {
    Ok,
    Error,
    Cancelled,
    ApprovalRequired,
}

/// 审计 sink。
pub trait AuditSink: Send + Sync + std::fmt::Debug + 'static {
    fn record(&self, rec: AuditRecord);
}

/// 内存 sink：把所有记录存到 Vec；测试用。
#[derive(Default)]
pub struct InMemoryAuditSink {
    records: Mutex<Vec<AuditRecord>>,
}

impl std::fmt::Debug for InMemoryAuditSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InMemoryAuditSink")
            .field("count", &self.records.lock().len())
            .finish()
    }
}

impl InMemoryAuditSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn snapshot(&self) -> Vec<AuditRecord> {
        self.records.lock().clone()
    }
}

impl AuditSink for InMemoryAuditSink {
    fn record(&self, rec: AuditRecord) {
        self.records.lock().push(rec);
    }
}

/// 文件 sink：把每条 `AuditRecord` 写成 ndjson 行到
/// `<dir>/agent-YYYY-MM-DD.ndjson`（架构 §11.6）。
///
/// 设计选择：
/// - 一天一个文件（轮转）
/// - 写入用 `parking_lot::Mutex<BufWriter<File>>`：单线程串行写，无锁开销
/// - 文件不存在 → 自动创建父目录 + 头部空
/// - 每写完一行立即 flush（防崩溃丢数据）
pub struct FileAuditSink {
    dir: PathBuf,
    state: Mutex<Option<FileState>>,
}

struct FileState {
    day: String,
    file: std::fs::File,
}

impl std::fmt::Debug for FileAuditSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileAuditSink")
            .field("dir", &self.dir)
            .finish()
    }
}

impl FileAuditSink {
    /// 默认目录：`<exe_dir>/data/audit/`，或 `QVIEW_AUDIT_DIR` 环境变量。
    pub fn new() -> anyhow::Result<Arc<Self>> {
        let dir = std::env::var("QVIEW_AUDIT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("data").join("audit"));
        Self::with_dir(dir)
    }

    /// 自定义目录。
    pub fn with_dir(dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        Ok(Arc::new(Self {
            dir,
            state: Mutex::new(None),
        }))
    }

    fn path_for(&self, day: &str) -> PathBuf {
        self.dir.join(format!("agent-{day}.ndjson"))
    }

    fn open_state(&self, day: &str) -> std::io::Result<FileState> {
        let path = self.path_for(day);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(FileState {
            day: day.to_string(),
            file,
        })
    }
}

fn today_str() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // UTC 日期（粗略；P5 改进用 chrono）
    let days = secs / 86400;
    // 1970-01-01 是周四；这里只用作文件名前缀的稳定字符串
    format!("day-{days}")
}

impl AuditSink for FileAuditSink {
    fn record(&self, rec: AuditRecord) {
        let line = match serde_json::to_string(&rec) {
            Ok(s) => s,
            Err(_) => return,
        };
        let mut guard = self.state.lock();
        let today = today_str();
        let needs_open = match guard.as_ref() {
            Some(s) => s.day != today,
            None => true,
        };
        if needs_open {
            match self.open_state(&today) {
                Ok(s) => *guard = Some(s),
                Err(_) => return,
            }
        }
        let state = match guard.as_mut() {
            Some(s) => s,
            None => return,
        };
        let _ = state.file.write_all(line.as_bytes());
        let _ = state.file.write_all(b"\n");
        let _ = state.file.flush();
    }
}

/// 审计 Hook（实现 contexa_hooks::Hook）。
pub struct AuditHook {
    sink: Arc<dyn AuditSink>,
}

impl std::fmt::Debug for AuditHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditHook").finish_non_exhaustive()
    }
}

impl AuditHook {
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self { sink }
    }
}

#[async_trait]
impl Hook for AuditHook {
    fn name(&self) -> &str {
        "qview-audit"
    }

    async fn post_tool_call(
        &self,
        ctx: &TaskContext<'_>,
        name: &str,
        result: &ToolResult,
    ) {
        let kind = if result.is_error {
            // is_error 但 content 含 approval_required 标记 → 单独分类
            if result
                .content
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s == "approval_required")
                .unwrap_or(false)
            {
                AuditResult::ApprovalRequired
            } else {
                AuditResult::Error
            }
        } else {
            AuditResult::Ok
        };
        let call_id = ctx
            .last_tool_call
            .map(|(id, _, _)| id.to_string())
            .unwrap_or_default();
        let input_hash = ctx
            .last_tool_call
            .map(|(_, _, args)| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                // 用 serde_json 的 Display 字符串做哈希
                args.to_string().hash(&mut h);
                format!("{:016x}", h.finish())
            })
            .unwrap_or_default();
        let output_bytes = serde_json::to_string(&result.content)
            .map(|s| s.len())
            .unwrap_or(0);
        let rec = AuditRecord {
            timestamp_ms: now_ms(),
            session_id: ctx.task_id.to_string(),
            tool: name.to_string(),
            call_id,
            input_hash,
            duration_ms: 0, // P2 暂不记录；post 与 on 之间无时间
            result_kind: kind,
            output_bytes,
            redactions_applied: 0,
        };
        self.sink.record(rec);
    }

    async fn on_task_end(&self, ctx: &TaskContext<'_>, _result: &serde_json::Value) {
        // 任务结束 — 写一条汇总审计
        let rec = AuditRecord {
            timestamp_ms: now_ms(),
            session_id: ctx.task_id.to_string(),
            tool: "<task_end>".into(),
            call_id: String::new(),
            input_hash: String::new(),
            duration_ms: (ctx.wall_seconds * 1000.0) as u64,
            result_kind: AuditResult::Ok,
            output_bytes: 0,
            redactions_applied: 0,
        };
        self.sink.record(rec);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use contexa_context::Message;

    #[tokio::test]
    async fn post_tool_call_writes_record() {
        let sink = InMemoryAuditSink::new();
        let hook = AuditHook::new(sink.clone());
        let msgs = vec![Message::user("hi")];
        let ctx = TaskContext {
            task_id: "t1",
            business_code: "qview",
            instance_id: "i",
            query: "q",
            messages: &msgs,
            rounds: 1,
            tool_calls_total: 1,
            tokens_prompt: 0,
            tokens_completion: 0,
            wall_seconds: 0.0,
            last_tool_call: Some(("call_1", "search_text", &serde_json::json!({"q":"x"}))),
            last_tool_result: None,
            last_llm_response: None,
            tags: &[],
        };
        let res = ToolResult::ok(serde_json::json!({"total": 1}));
        hook.post_tool_call(&ctx, "search_text", &res).await;
        let recs = sink.snapshot();
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].tool, "search_text");
        assert_eq!(recs[0].call_id, "call_1");
        assert_eq!(recs[0].result_kind, AuditResult::Ok);
    }

    #[test]
    fn file_sink_appends_ndjson() {
        let dir = std::env::temp_dir().join(format!(
            "qview-audit-{}",
            uuid::Uuid::new_v4()
        ));
        let sink = FileAuditSink::with_dir(dir.clone()).unwrap();
        sink.record(AuditRecord {
            timestamp_ms: 1,
            session_id: "s1".into(),
            tool: "search_text".into(),
            call_id: "c1".into(),
            input_hash: "h".into(),
            duration_ms: 10,
            result_kind: AuditResult::Ok,
            output_bytes: 100,
            redactions_applied: 0,
        });
        sink.record(AuditRecord {
            timestamp_ms: 2,
            session_id: "s1".into(),
            tool: "annotate_create".into(),
            call_id: "c2".into(),
            input_hash: "h2".into(),
            duration_ms: 5,
            result_kind: AuditResult::ApprovalRequired,
            output_bytes: 50,
            redactions_applied: 0,
        });

        // 找到今天那个文件
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 1);
        let path = entries.remove(0).path();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        // 第一行可解析
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["tool"], "search_text");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
