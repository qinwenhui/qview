//! AgentRuntimeHandle / AgentGoal / ProposalDecision / AgentError。
//!
//! UI 通过这个 handle 调 4 类 API：start_session / cancel / subscribe / proposal_decision。

use std::sync::Arc;
use std::time::Duration;

use thiserror::Error;

use qview_application::protocol::ProposalId;

use crate::event::SessionId;
use crate::proposal::ProposalDecision as PD;

pub use crate::proposal::ProposalDecision;

use contexa_core::{ContexaError, WorkerStatus};

/// 用户输入的目标（架构 §8.3 映射）。
#[derive(Debug, Clone)]
pub struct AgentGoal {
    /// 任务名（TaskSpec.name）。
    pub name: String,
    /// 一句话目标（TaskSpec.goal）。
    pub goal: String,
    /// 成功标准（TaskSpec.success_criteria；可选）。
    pub success_criteria: Option<String>,
    /// 用户原始输入（Task.query）。
    pub query: String,
    /// 当前文档上下文（可选）：如 `文档 id=3, 路径=..., 行数=...`，
    /// 注入 `TaskSpec::context_hints`，让 LLM 知道该用哪个 document_id。
    pub document_context: Option<String>,
    /// 会话关联的文件 canonical path（落库 `SessionMeta.file_id` 用）。
    pub document_path: Option<String>,
}

impl AgentGoal {
    /// 简单目标（最常用）。
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            name: String::new(),
            goal: query.into(),
            success_criteria: None,
            query: String::new(),
            document_context: None,
            document_path: None,
        }
    }

    /// 设置任务名 + 目标 + 用户输入（一次写完）。
    pub fn with_spec(
        mut self,
        name: impl Into<String>,
        goal: impl Into<String>,
        query: impl Into<String>,
    ) -> Self {
        self.name = name.into();
        self.goal = goal.into();
        self.query = query.into();
        self
    }

    /// 附带当前文档上下文（真实 LLM 用它拿到 document_id）。
    pub fn with_document_context(mut self, ctx: impl Into<String>) -> Self {
        self.document_context = Some(ctx.into());
        self
    }

    /// 附带会话关联文件（canonical path，落库 `file_id` 用）。
    pub fn with_document_path(mut self, path: impl Into<String>) -> Self {
        self.document_path = Some(path.into());
        self
    }
}

/// qview 端 Agent 错误。
#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Contexa(#[from] ContexaError),

    #[error("参数错误: {0}")]
    InvalidArgument(String),

    #[error("会话未找到: {0}")]
    UnknownSession(SessionId),

    #[error("会话取消超时")]
    CancelTimeout,

    #[error("内部错误: {0}")]
    Internal(String),
}

pub mod error {
    //! Re-export for ergonomic path.
    pub use super::AgentError;
}

impl AgentError {
    /// 把 `ContexaError` 翻译为 qview 端 AgentError。
    pub fn from_worker(e: &ContexaError) -> Self {
        match e {
            ContexaError::ToolNotFound(s) => {
                AgentError::InvalidArgument(format!("tool not found: {s}"))
            }
            ContexaError::ReservedName(s) => {
                AgentError::InvalidArgument(format!("reserved name: {s}"))
            }
            ContexaError::InvalidConfig(s) => AgentError::InvalidArgument(s.clone()),
            other => AgentError::Contexa(clone_contexa(other)),
        }
    }

    /// WorkerStatus → AgentError 终态。
    pub fn from_status(status: WorkerStatus, note: Option<&str>) -> Option<Self> {
        match status {
            WorkerStatus::Failed => Some(AgentError::Internal(note.unwrap_or("failed").to_string())),
            WorkerStatus::Timeout => {
                // 取消超时走特殊路径
                if note.map(|n| n.contains("cancel")).unwrap_or(false) {
                    Some(AgentError::CancelTimeout)
                } else {
                    Some(AgentError::Internal(format!("timeout: {}", note.unwrap_or("?"))))
                }
            }
            WorkerStatus::Empty => Some(AgentError::Internal("empty result".into())),
            WorkerStatus::Success => None,
        }
    }
}

fn clone_contexa(e: &ContexaError) -> ContexaError {
    match e {
        ContexaError::ReservedName(s) => ContexaError::ReservedName(s.clone()),
        ContexaError::InvalidConfig(s) => ContexaError::InvalidConfig(s.clone()),
        ContexaError::ToolNameTooLong { name, len, max } => ContexaError::ToolNameTooLong {
            name: name.clone(),
            len: *len,
            max: *max,
        },
        ContexaError::InvalidToolName(s, n) => ContexaError::InvalidToolName(s.clone(), *n),
        ContexaError::ToolNotFound(s) => ContexaError::ToolNotFound(s.clone()),
        ContexaError::ToolInvocation(s) => ContexaError::ToolInvocation(s.clone()),
        ContexaError::Llm(s) => ContexaError::Llm(s.clone()),
        ContexaError::Mcp(s) => ContexaError::Mcp(s.clone()),
        ContexaError::Memory(s) => ContexaError::Memory(s.clone()),
        ContexaError::Flow(s) => ContexaError::Flow(s.clone()),
        ContexaError::Delegation(s) => ContexaError::Delegation(s.clone()),
        ContexaError::Io(e) => ContexaError::Io(std::io::Error::new(e.kind(), e.to_string())),
        // Json / Other 不能直接 clone（serde_json::Error / Box<dyn Error>）；
        // 这里转为字符串形式保持信息密度。
        ContexaError::Json(e) => ContexaError::Json(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ))),
        ContexaError::UrlParse(s) => ContexaError::UrlParse(s.clone()),
        ContexaError::Http(s) => ContexaError::Http(s.clone()),
        ContexaError::Other(e) => ContexaError::Other(format!("{e}").into()),
    }
}

/// UI 端入口。
///
/// 持有 `Arc<AgentRuntimeInner>`，对外只暴露 4 类 API：
/// - `start_session(goal)` → 启动一次任务，返回 session_id
/// - `cancel(session_id)` → 取消（oneshot 通知后台任务在下一轮 LLM 前退出）
/// - `subscribe(sink)` → 订阅事件
/// - `proposal_decision(proposal_id, decision)` → 决策
pub struct AgentRuntimeHandle {
    pub(crate) inner: Arc<crate::runtime::AgentRuntimeInner>,
}

// 给 tests / bin 用：暴露 inner。
#[doc(hidden)]
impl AgentRuntimeHandle {
    /// 直接访问内部（仅测试 / cli 调试使用）。
    pub fn _inner(&self) -> &Arc<crate::runtime::AgentRuntimeInner> {
        &self.inner
    }
}

impl std::fmt::Debug for AgentRuntimeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRuntimeHandle").finish()
    }
}

impl AgentRuntimeHandle {
    /// 启动一次任务；返回 session_id。
    pub async fn start_session(&self, goal: AgentGoal) -> Result<SessionId, AgentError> {
        self.inner.start_session(goal).await
    }

    /// 启动 / 继续一次任务（多轮对话）。
    ///
    /// - `session_id = Some`：复用既有会话继续（同一次对话共用一个 id，历史会话只记一条）；
    ///   `None`：新建会话。
    /// - `conversation_history`：前几轮 User/Agent 文本块，注入 LLM 上下文（延续记忆）。
    pub async fn start_session_with(
        &self,
        goal: AgentGoal,
        session_id: Option<SessionId>,
        conversation_history: Option<String>,
    ) -> Result<SessionId, AgentError> {
        self.inner
            .start_session_with(goal, session_id, conversation_history)
            .await
    }

    /// 取消正在运行的 session。
    pub async fn cancel(&self, session_id: SessionId) {
        self.inner.cancel(session_id).await;
    }

    /// 带超时取消（架构 §8.5：1s 内生效）。
    pub async fn cancel_within(&self, session_id: SessionId, timeout: Duration) {
        let _ = tokio::time::timeout(timeout, self.cancel(session_id)).await;
    }

    /// 订阅事件；返回取消订阅 guard。
    ///
    /// **重要**：内部用 `Weak` 持有 sink —— 调用方必须保留传入的 `Arc` 强引用
    /// （例如 `let g = handle.subscribe(sink.clone()); let _keep = sink;`），
    /// 否则订阅在函数返回后立即失效（事件收不到）。
    pub fn subscribe(&self, sink: Arc<dyn crate::event::AgentSink>) -> crate::event::SubscriptionGuard {
        self.inner.subscribe(sink)
    }

    /// 提案决策；通过 ApprovalRegistry 唤醒 GuardedTool。
    pub async fn proposal_decision(
        &self,
        proposal_id: ProposalId,
        decision: ProposalDecision,
    ) -> Result<(), AgentError> {
        self.inner.proposal_decision(proposal_id, decision).await
    }

    /// 当前活跃 session 数。
    pub fn active_sessions(&self) -> usize {
        self.inner.active_sessions()
    }
}

/// 旧 API 兼容层：`ProposalDecision` 也可由 `PD` 别名引用。
pub type ProposalDecisionAlias = PD;
