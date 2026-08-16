//! 写操作的"提案"数据模型（架构文档 §6.3）。
//!
//! GuardedTool 第一次被 LLM 调用时返回 `approval_required`，
//! 携带 `Proposal` 信息 → UI 显示 → 用户决策 → `ApprovalRegistry::complete` → 工具继续执行。

use serde::{Deserialize, Serialize};

use qview_application::protocol::{ProposalId, SideEffect};

/// 单条提案：LLM 想做的一次写操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// 唯一 ID。
    pub id: ProposalId,
    /// 哪个 session 产生的。
    pub session_id: String,
    /// 哪个工具触发的（用于 UI 弹窗标题）。
    pub tool_name: String,
    /// 工具入参（原始 JSON；UI 可格式化展示）。
    pub args: serde_json::Value,
    /// 副作用级别。
    pub side_effect: SideEffect,
    /// 人类可读的原因（"将在第 100-200 行创建批注…"）。
    pub reason: String,
    /// 创建时间戳（毫秒）。
    pub created_at_ms: u64,
}

impl Proposal {
    /// 构造一条新提案。
    pub fn new(
        session_id: impl Into<String>,
        tool_name: impl Into<String>,
        args: serde_json::Value,
        side_effect: SideEffect,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: ProposalId::new(),
            session_id: session_id.into(),
            tool_name: tool_name.into(),
            args,
            side_effect,
            reason: reason.into(),
            created_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }
}

/// 用户的决策（架构 §6.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDecision {
    /// 通过 — 工具继续执行。
    Approve,
    /// 拒绝 — 工具向 LLM 返回 is_error=true 的 ToolResult。
    Reject,
    /// 跳过 — 视同 Reject，但 reason 不向 LLM 暴露（保留给将来"忽略"语义）。
    Skip,
}

impl ProposalDecision {
    /// 是否继续工具的执行。
    pub fn allows_execution(self) -> bool {
        matches!(self, ProposalDecision::Approve)
    }
}
