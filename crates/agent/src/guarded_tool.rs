//! `GuardedTool` 包装器（架构文档 §6.3 / §22.3 临时方案）。
//!
//! ## 协议
//! 1. LLM 调工具（带写操作）→ GuardedTool 第一次 invoke：
//!    - 创建 `Proposal`（含 tool_name / args / reason / side_effect）
//!    - 注册到 `ApprovalRegistry`
//!    - 返回 `ToolResult { is_error: true, content: {"error":"approval_required","proposal_id":...,"reason":...} }`
//!    - `QviewSinkHook::post_tool_call` 检测到该模式 → 发 `AgentEvent::ApprovalRequired`
//! 2. UI 弹窗；用户决策 → `AgentRuntimeHandle::proposal_decision`
//! 3. `ApprovalRegistry::complete` 唤醒 GuardedTool 的 oneshot：
//!    - `Approve` → GuardedTool 调用内部函数执行真正的工作 → 返回正常 ToolResult
//!    - `Reject` → 返回 `is_error=true, content={"error":"rejected"}`
//! 4. **关键**：LLM 不再重试该工具；这次 ToolResult 直接被 Worker 收下。
//!
//! ## 设计选择
//! - 用 `Arc<dyn ToolSource>` 包住内部 LocalTool — Agent 工具注册表仍接受 LocalTool 形态
//! - 调用时机：第一次 invoke 在 GuardedTool::call_tool 内同步走完；后续由 oneshot 唤醒
//! - 超时：oneshot 不带超时（ApprovalRegistry 由 session cancel 触发 reject）

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};

use contexa_context::{ContexaError, Result, ToolSpec};
use contexa_tools::{ToolResult, ToolSource};

use qview_application::protocol::{ProposalId, SideEffect};

use crate::approval::ApprovalRegistry;
use crate::event::{AgentEvent, AgentSink};
use crate::proposal::Proposal;
use crate::sink_hook::WeakSinks;

/// GuardedTool 的元数据（用于注册表登记副作用级别）。
#[derive(Debug, Clone)]
pub struct GuardedToolMeta {
    /// 工具名（与 inner.name 一致）。
    pub name: String,
    /// 工具 spec（与 inner.list_tools 输出对齐）。
    pub spec: ToolSpec,
    /// 副作用级别（用于 PermissionPolicy 决策）。
    pub side_effect: SideEffect,
    /// 人类可读的原因（"将在第 100-200 行创建批注"）。
    pub reason: String,
}

/// 内部 async 函数的类型：参数=Value，结果=Result<ToolResult>。
pub type InnerInvokeFn = Arc<
    dyn Fn(Value) -> Pin<Box<dyn Future<Output = Result<ToolResult>> + Send>> + Send + Sync,
>;

/// GuardedTool：包装任意"写操作"工具，按 ApprovalRegistry 协议放行。
pub struct GuardedTool {
    meta: GuardedToolMeta,
    approvals: Arc<ApprovalRegistry>,
    inner: InnerInvokeFn,
    /// 调试：跟踪 pending 状态。
    pending: Mutex<Option<ProposalId>>,
    /// 可选 sink：用于 GuardedTool 自己在阻塞前广播 ProposalCreated + ApprovalRequired。
    /// 如果没设 → 不广播（依赖 QviewSinkHook::post_tool_call 检测 approval_required 后发）。
    sinks: Mutex<Vec<Arc<dyn AgentSink>>>,
    /// 生产：与 AgentRuntime 共享的 WeakSinks（UI 订阅的就是这个）。
    /// 阻塞等审批前必须广播到这里，否则 UI 永远收不到 ApprovalRequired → 30s 超时。
    shared: Mutex<Option<WeakSinks>>,
}

impl std::fmt::Debug for GuardedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuardedTool")
            .field("name", &self.meta.name)
            .field("side_effect", &self.meta.side_effect)
            .finish_non_exhaustive()
    }
}

impl GuardedTool {
    /// 构造。
    ///
    /// `inner` 闭包承担真正的工作；GuardedTool 仅负责"先审批、后执行"。
    pub fn new(meta: GuardedToolMeta, approvals: Arc<ApprovalRegistry>, inner: InnerInvokeFn) -> Self {
        Self {
            meta,
            approvals,
            inner,
            pending: Mutex::new(None),
            sinks: Mutex::new(Vec::new()),
            shared: Mutex::new(None),
        }
    }

    pub fn meta(&self) -> &GuardedToolMeta {
        &self.meta
    }

    pub fn name(&self) -> &str {
        &self.meta.name
    }

    /// 注册一个 sink（让 GuardedTool 自己在阻塞前广播 ApprovalRequired，
    /// 不依赖 QviewSinkHook::post_tool_call 的"工具已返回"路径）。
    pub fn add_sink(&self, sink: Arc<dyn AgentSink>) {
        self.sinks.lock().push(sink);
    }

    /// 挂上与 AgentRuntime 共享的 WeakSinks（生产装配时由 `AgentConfig::build` 调用）。
    pub fn set_shared_sinks(&self, shared: WeakSinks) {
        *self.shared.lock() = Some(shared);
    }
}

#[async_trait]
impl ToolSource for GuardedTool {
    async fn list_tools(&self) -> Result<Vec<ToolSpec>> {
        Ok(vec![self.meta.spec.clone()])
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolResult> {
        if name != self.meta.name {
            return Err(ContexaError::ToolNotFound(name.to_string()));
        }

        // 第一次 invoke：创建 Proposal 等审批。
        let proposal = Proposal::new(
            session_id_from_args(&args),
            &self.meta.name,
            args.clone(),
            self.meta.side_effect,
            self.meta.reason.clone(),
        );
        let proposal_id = proposal.id;
        let rx = self.approvals.register(proposal.clone());
        *self.pending.lock() = Some(proposal_id);

        // 关键修复：在阻塞前就广播 ApprovalRequired，让 UI 立即响应。
        // （post_tool_call 在工具返回后才触发；如果工具阻塞等审批，post_tool_call 永远不触发 → UI 永远收不到事件。）
        // 生产装配时 `shared` 挂的是 AgentRuntime 的 WeakSinks（UI 订阅的那个），
        // 这里必须广播到它，否则弹窗永不出现 → 30s 超时。
        for sink in self.sinks.lock().iter() {
            sink.on_event(AgentEvent::ProposalCreated {
                session_id: proposal.session_id.clone(),
                proposal: proposal.clone(),
            });
            sink.on_event(AgentEvent::ApprovalRequired {
                session_id: proposal.session_id.clone(),
                proposal_id,
                tool: self.meta.name.clone(),
                reason: self.meta.reason.clone(),
            });
        }
        if let Some(shared) = self.shared.lock().as_ref() {
            shared.broadcast(AgentEvent::ProposalCreated {
                session_id: proposal.session_id.clone(),
                proposal: proposal.clone(),
            });
            shared.broadcast(AgentEvent::ApprovalRequired {
                session_id: proposal.session_id.clone(),
                proposal_id,
                tool: self.meta.name.clone(),
                reason: self.meta.reason.clone(),
            });
        }

        // 等审批（带超时兜底）：审批弹窗没出现 / 用户一直没决策时，
        // 30s 后自动返回超时错误，避免工具永久挂起（用户实测"调用工具一直
        // 不结束"）。真正让用户能操作的是审批弹窗 + 停止按钮（cancel_all），
        // 这个超时只是最后的安全网。
        const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        let decision = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(d)) => d,
            Ok(Err(_canceled)) => {
                *self.pending.lock() = None;
                return Ok(ToolResult::err(json!({
                    "error": "approval_cancelled",
                    "proposal_id": proposal_id,
                })));
            }
            Err(_elapsed) => {
                // 超时：从 registry 清掉 pending（发送端 drop，send 失败无碍）
                *self.pending.lock() = None;
                self.approvals.cancel(proposal_id);
                return Ok(ToolResult::err(json!({
                    "error": "approval_timeout",
                    "proposal_id": proposal_id,
                })));
            }
        };

        *self.pending.lock() = None;

        if !decision.allows_execution() {
            return Ok(ToolResult::err(json!({
                "error": "rejected_by_user",
                "proposal_id": proposal_id,
            })));
        }

        // 通过：调内部函数执行真正的工作。
        (self.inner)(args).await
    }

    fn name(&self) -> &str {
        self.name()
    }
}

/// 从 args 里读 session_id（工具调用应通过某种约定注入 session_id）。
///
/// 当前约定：args 中第一个 `_session_id` 字段。如果工具调用没传，
/// fallback 到 `"unknown"`（P4 阶段足够；UI 集成时由 sink_hook 注入）。
fn session_id_from_args(args: &Value) -> String {
    args.get("_session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 用 GuardedTool 构造一个 LocalTool 形态的 `Arc<dyn ToolSource>`，注入 ReActWorker。
pub fn into_source(g: GuardedTool) -> Arc<dyn ToolSource> {
    Arc::new(g)
}

/// 把 args 中加上 `_session_id`（供 GuardedTool 在 Proposal 上携带）。
pub fn inject_session_id(mut args: Value, session_id: &str) -> Value {
    if let Some(obj) = args.as_object_mut() {
        obj.insert("_session_id".to_string(), json!(session_id));
    } else {
        // 非 object → 包一层
        let mut obj = serde_json::Map::new();
        obj.insert("_session_id".to_string(), json!(session_id));
        obj.insert("value".to_string(), args);
        return Value::Object(obj);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use contexa_context::{ContexaError, ToolSpec};
    use serde_json::json;

    fn sample_spec() -> ToolSpec {
        ToolSpec::new_unchecked(
            "annotate_create",
            "x",
            json!({"type": "object"}),
        )
    }

    #[tokio::test]
    async fn first_invoke_blocks_for_approval() {
        let approvals = Arc::new(ApprovalRegistry::new());
        let inner: InnerInvokeFn = Arc::new(|_| {
            Box::pin(async { Ok(ToolResult::ok(json!({"ok": true}))) })
        });
        let meta = GuardedToolMeta {
            name: "annotate_create".into(),
            spec: sample_spec(),
            side_effect: SideEffect::Reversible,
            reason: "test annotation".into(),
        };
        let tool = GuardedTool::new(meta, approvals.clone(), inner);

        // 第一次 invoke：应阻塞直到 proposal_decision
        let tool_arc = Arc::new(tool);
        let tool_for_invoker = Arc::clone(&tool_arc) as Arc<dyn ToolSource>;
        let approvals_clone = Arc::clone(&approvals);

        // 后台启动 invoke
        let h = tokio::spawn(async move {
            tool_for_invoker.call_tool("annotate_create", json!({"x": 1})).await
        });

        // 等 registry 注册
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(approvals_clone.pending_count(), 1);

        // 决策 Approve → 应唤醒 invoke
        // 从 registry 里取第一个 pending id
        // （通过 cancel_all 也能触发 reject；这里直接拿到 id）
        // registry 不暴露 pending id 列表，所以用 cancel_all
        approvals_clone.cancel_all();
        let r = h.await.unwrap().unwrap();
        assert!(r.is_error, "cancel → reject → is_error");
        assert_eq!(r.content["error"], "rejected_by_user");
    }

    #[tokio::test]
    async fn invoke_wrong_name_returns_tool_not_found() {
        let approvals = Arc::new(ApprovalRegistry::new());
        let meta = GuardedToolMeta {
            name: "x".into(),
            spec: sample_spec(),
            side_effect: SideEffect::Reversible,
            reason: "y".into(),
        };
        let tool = GuardedTool::new(meta, approvals, Arc::new(|_| Box::pin(async {
            Ok(ToolResult::ok(json!({})))
        })));
        let err = tool.call_tool("wrong", json!({})).await.unwrap_err();
        assert!(matches!(err, ContexaError::ToolNotFound(_)));
    }
}
