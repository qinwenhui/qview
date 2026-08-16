//! ApprovalRegistry：oneshot-based proposal 等待器（架构文档 §6.3 / §8.3）。
//!
//! ## 协议
//! 1. GuardedTool 第一次 invoke → `pending_proposal` 注册 + 等待 decision
//! 2. Runtime 发 `AgentEvent::ApprovalRequired`
//! 3. UI 弹窗；用户决策 → `AgentRuntimeHandle::proposal_decision`
//! 4. Registry 唤醒该 proposal 的 oneshot receiver；GuardedTool 继续 / 中止
//!
//! 关键约束：
//! - oneshot 通道；同一 proposal 只能决策一次（重复决策 → 错误）
//! - 取消 / 超时 → 决策视为 Reject

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::oneshot;

use qview_application::protocol::ProposalId;

use crate::proposal::{Proposal, ProposalDecision};

/// Approval 注册表（线程安全）。
pub struct ApprovalRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    pending: Mutex<HashMap<ProposalId, oneshot::Sender<ProposalDecision>>>,
}

impl std::fmt::Debug for ApprovalRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovalRegistry")
            .field("pending_count", &self.inner.pending.lock().len())
            .finish()
    }
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ApprovalRegistry {
    /// 构造空注册表。
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// 注册一个 pending proposal，返回 receiver（GuardedTool 在 invoke 内 await）。
    ///
    /// `peek_pending` 在 on_tool_call hook 中调用，匹配 LLM 当前正在调的工具名。
    pub fn register(&self, proposal: Proposal) -> oneshot::Receiver<ProposalDecision> {
        let (tx, rx) = oneshot::channel();
        self.inner.pending.lock().insert(proposal.id, tx);
        rx
    }

    /// 取出待决策的 proposal（按 tool_name + session_id 匹配）。
    pub fn peek_pending(&self, _session_id: &str, _tool_name: &str) -> Option<Proposal> {
        // 简化：peek 时不带 proposal 信息（实际由 GuardedTool 自己持有）。
        // 这里仅用作 hook 触发 ApprovalRequired 事件时反查。
        // 当前实现：让 QviewSinkHook 在 `on_tool_call` 时拿不到具体 proposal，
        // 由 GuardedTool 自己在 `boxed_invoke` 里把 Proposal 提交到 registry。
        // 因此 peek_pending 仅返回 Some(unit) 占位。
        let _g = self.inner.pending.lock();
        if _g.is_empty() {
            None
        } else {
            // 任意一个 pending 都给 None 让上层走通用路径
            None
        }
    }

    /// 是否存在任何 pending proposal。
    pub fn has_pending(&self) -> bool {
        !self.inner.pending.lock().is_empty()
    }

    /// 完成一次决策。
    ///
    /// - 决策 Approve / Reject → 把结果通过 oneshot 发给 GuardedTool；返回 Ok(())。
    /// - proposal_id 不在 pending 中（已决策 / 已超时）→ 返回 Err。
    /// - receiver 已 drop（GuardedTool 已取消等待）→ 返回 Err。
    pub fn complete(&self, proposal_id: ProposalId, decision: ProposalDecision) -> Result<(), String> {
        let sender = self.inner.pending.lock().remove(&proposal_id);
        match sender {
            Some(tx) => tx
                .send(decision)
                .map_err(|_| "receiver 已 drop（GuardedTool 已取消等待）".to_string()),
            None => Err(format!("未知 proposal_id: {proposal_id}")),
        }
    }

    /// 取消某个 pending（用于取消 session 时清空队列）。
    pub fn cancel(&self, proposal_id: ProposalId) {
        if let Some(tx) = self.inner.pending.lock().remove(&proposal_id) {
            // 决策视为 Reject；让 GuardedTool 走错误分支
            let _ = tx.send(ProposalDecision::Reject);
        }
    }

    /// 取消该 session 的全部 pending（cancel session 时调）。
    pub fn cancel_all(&self) {
        let pending = std::mem::take(&mut *self.inner.pending.lock());
        for (_id, tx) in pending {
            let _ = tx.send(ProposalDecision::Reject);
        }
    }

    /// 当前 pending 数（测试用）。
    pub fn pending_count(&self) -> usize {
        self.inner.pending.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qview_application::protocol::SideEffect;

    #[tokio::test]
    async fn complete_routes_decision() {
        let reg = ApprovalRegistry::new();
        let p = Proposal::new(
            "s1",
            "annotate_create",
            serde_json::json!({}),
            SideEffect::Reversible,
            "x",
        );
        let rx = reg.register(p.clone());

        reg.complete(p.id, ProposalDecision::Approve).unwrap();
        let d = rx.await.unwrap();
        assert_eq!(d, ProposalDecision::Approve);
        assert_eq!(reg.pending_count(), 0);
    }

    #[tokio::test]
    async fn double_complete_returns_err() {
        let reg = ApprovalRegistry::new();
        let p = Proposal::new("s", "x", serde_json::json!({}), SideEffect::Reversible, "x");
        let _rx = reg.register(p.clone());
        reg.complete(p.id, ProposalDecision::Approve).unwrap();
        assert!(reg.complete(p.id, ProposalDecision::Approve).is_err());
    }

    #[tokio::test]
    async fn cancel_all_rejects_all() {
        let reg = ApprovalRegistry::new();
        let p1 = Proposal::new("s", "a", serde_json::json!({}), SideEffect::Reversible, "x");
        let p2 = Proposal::new("s", "b", serde_json::json!({}), SideEffect::Reversible, "y");
        let rx1 = reg.register(p1.clone());
        let rx2 = reg.register(p2.clone());
        reg.cancel_all();
        assert_eq!(rx1.await.unwrap(), ProposalDecision::Reject);
        assert_eq!(rx2.await.unwrap(), ProposalDecision::Reject);
        assert_eq!(reg.pending_count(), 0);
    }
}
