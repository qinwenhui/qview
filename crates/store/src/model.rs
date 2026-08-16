//! 结构化数据模型（与后端无关）。
//!
//! 全部类型 `Serialize + Deserialize + Clone`，由 `RedbStore` 序列化落盘。
//! 设计约束（TODO-shared-engine-and-local-store §2.4）：
//! - 只存结构化元数据 + AI 会话内容，**绝不存日志内容**；
//! - 消息随会话整体原子落盘（B1 MVP：会话结束写一次；后续可拆增量表）。

use serde::{Deserialize, Serialize};

/// 会话中一条消息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreMessage {
    pub role: StoreRole,
    pub content: String,
    /// 消息在会话内的序号（兼作展示顺序；0-based）。
    pub seq: u64,
}

/// 消息角色（与 `contexa_context::Tier` / agent 的 `Role` 对齐的平替）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 会话终态（映射自 `contexa_core::WorkerStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreStatus {
    Success,
    Failed,
    Timeout,
    Cancelled,
    Empty,
}

/// 一次 AI 会话的元信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    /// 用户目标（session 启动时的 query）。
    pub goal: String,
    pub status: StoreStatus,
    pub summary: String,
    pub provider: String,
    pub model: String,
    /// 会话关联的文件（canonical path）；无文件会话为 None。
    pub file_id: Option<String>,
    pub tokens_prompt: u32,
    pub tokens_completion: u32,
    pub rounds: u32,
    pub tool_calls: u32,
}

/// 完整会话（元信息 + 全部消息），单事务原子写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSession {
    pub meta: SessionMeta,
    pub messages: Vec<StoreMessage>,
}

/// 文件元数据（B2：打开记录）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMeta {
    /// canonical path。
    pub path: String,
    pub last_opened_at_ms: u64,
    pub open_count: u64,
    pub size_bytes: u64,
    pub encoding: String,
}

/// 一条搜索历史（B2 扩展：GUI 搜索历史从 config.json 迁到 store）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchEntry {
    /// 查询串（表主键，去重）。
    pub query: String,
    /// 最近一次使用的毫秒时间戳（倒序排序用）。
    pub last_used_at_ms: u64,
    /// 累计使用次数（可做"最常搜"）。
    pub use_count: u64,
}

/// 一条工具调用记录（AI 会话内，落盘到 `tool_calls` 表）。
///
/// 器灵调用过的每个工具都记一条：名字、入参摘要、结果摘要、耗时、是否出错。
/// 会话内序号 `seq` 决定展示顺序；落盘按 session_id 整会话覆盖写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// 所属会话 id。
    pub session_id: String,
    /// 会话内序号（0-based，展示顺序）。
    pub seq: u64,
    /// 工具名。
    pub tool: String,
    /// 调用入参（JSON 字符串，可能被截断）。
    pub input: String,
    /// 结果摘要（成功=截断文本；失败=`error: …`）。
    pub output: String,
    /// 执行耗时（毫秒）。
    pub duration_ms: u64,
    /// 是否出错。
    pub is_error: bool,
    /// 记录时刻（毫秒时间戳）。
    pub at_ms: u64,
}
