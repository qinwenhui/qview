//! AgentEvent / AgentSink / Phase / SessionId。
//!
//! 架构文档 §6.1 / §8：UI 只通过这一组类型与 Runtime 通信。

use std::sync::{Arc, Weak};

use serde::{Deserialize, Serialize};

use qview_application::protocol::{ProposalId, ToolCallId, ViewIntent};

use crate::proposal::Proposal;
use contexa_core::WorkerStatus;

/// Agent 会话 ID（直接用 `contexa::Task::task_id`，即 String）。
pub type SessionId = String;

/// Agent 阶段（UI 状态机）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// 意图路由（router 前置分类；架构 §22.x P1）。
    Routing,
    /// LLM 思考中（pre_llm_call → post_llm_call 之间）。
    Thinking,
    /// 正在搜索 / 读上下文。
    Searching,
    /// 检视 / 聚合结果。
    Inspecting,
    /// 起草最终回复 / 准备 worker_finish。
    Drafting,
    /// 等待用户审批（GuardedTool 已发出 ApprovalRequired）。
    AwaitingApproval,
    /// 任务正常结束。
    Done,
    /// 任务失败。
    Failed,
    /// 任务被取消。
    Cancelled,
}

impl Phase {
    /// 是否终态。
    pub fn is_terminal(self) -> bool {
        matches!(self, Phase::Done | Phase::Failed | Phase::Cancelled)
    }
}

/// Agent 消息角色（qview-agent 自有，避免 UI 依赖 contexa_context::Tier）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    Developer,
    User,
    Assistant,
    Tool,
}

impl From<contexa_context::Tier> for Role {
    fn from(t: contexa_context::Tier) -> Self {
        match t {
            contexa_context::Tier::System => Role::System,
            contexa_context::Tier::Developer => Role::Developer,
            contexa_context::Tier::User => Role::User,
            contexa_context::Tier::Assistant => Role::Assistant,
            contexa_context::Tier::Tool => Role::Tool,
        }
    }
}

/// Agent 事件（Runtime 推给 UI）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 会话开始。
    SessionStarted {
        session_id: SessionId,
        goal: String,
        instance_id: String,
    },
    /// 阶段切换。
    PhaseChanged {
        session_id: SessionId,
        phase: Phase,
    },
    /// 工具调用开始。
    ToolCallStarted {
        session_id: SessionId,
        call_id: ToolCallId,
        tool: String,
        input: serde_json::Value,
    },
    /// 工具调用进度（可选；进度事件可丢）。
    ToolCallProgress {
        session_id: SessionId,
        call_id: ToolCallId,
        message: String,
        progress: Option<f32>,
    },
    /// 工具调用结束。
    ///
    /// `tool`：工具名。**必须**由 hook 用 `post_tool_call` 的 `name` 参数填，
    /// 不能依赖 UI 的共享「在飞工具」槽 —— 并行工具调用时槽会被后面的调用覆盖，
    /// 导致完成日志把 A 的结果记到 B 的名字上。
    ToolCallFinished {
        session_id: SessionId,
        call_id: ToolCallId,
        tool: String,
        output_summary: String,
        duration_ms: u64,
        is_error: bool,
    },
    /// 视图意图（由工具结果中的 `view_intents` 字段触发）。
    ViewIntentEmitted {
        session_id: SessionId,
        intent: ViewIntent,
    },
    /// 写操作提案已生成（GuardedTool 返回 `approval_required`）。
    ProposalCreated {
        session_id: SessionId,
        proposal: Proposal,
    },
    /// UI 需要用户决定提案。
    ApprovalRequired {
        session_id: SessionId,
        proposal_id: ProposalId,
        tool: String,
        reason: String,
    },
    /// LLM 消息（assistant 文本）。
    MessageEmitted {
        session_id: SessionId,
        role: Role,
        text: String,
    },
    /// 项目经理的实时进度交代（`report_progress` 工具触发；普通文本不实时显示）。
    ProgressNote {
        session_id: SessionId,
        text: String,
    },
    /// 会话结束（成功 / 取消 / 失败）。
    SessionFinished {
        session_id: SessionId,
        status: WorkerStatus,
        summary: String,
    },
    /// 会话被取消（由 cancel 触发；走 WorkerResult::Timeout → 翻译为 Cancelled）。
    Cancelled {
        session_id: SessionId,
    },
    /// 会话失败。
    Failed {
        session_id: SessionId,
        error: String,
    },
}

/// 订阅者 handle（drop = 取消订阅）。
#[derive(Debug)]
pub struct SubscriptionGuard {
    inner: Weak<dyn AgentSink>,
}

impl SubscriptionGuard {
    /// 用 Weak 包装一个 sink。
    pub fn new(sink: &Arc<dyn AgentSink>) -> Self {
        Self {
            inner: Arc::downgrade(sink),
        }
    }

    /// sink 是否还活着。
    pub fn is_alive(&self) -> bool {
        self.inner.strong_count() > 0
    }

    /// 升级为强引用。
    pub fn upgrade(&self) -> Option<Arc<dyn AgentSink>> {
        self.inner.upgrade()
    }
}

/// Agent 事件接收器（UI 实现）。
///
/// 注意：`on_event` 是 `Send + Sync`，由 Runtime 在后台线程上调用。
/// 实现内部用 channel / parking_lot mutex 缓冲。
pub trait AgentSink: Send + Sync + std::fmt::Debug + 'static {
    /// 推一条事件给订阅者。
    fn on_event(&self, event: AgentEvent);
}

/// AgentSink 错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentSinkError {
    #[error("sink 已关闭")]
    Closed,
    #[error("发送失败：{0}")]
    SendFailed(String),
}
