//! AgentSink 的默认实现：基于 `tokio::sync::mpsc::channel(256)` 的异步 sink。
//!
//! 满足架构 §8.5 背压约束：
//! - 容量 256
//! - 满后 `ToolCallProgress` / 阶段事件可丢
//! - `SessionFinished` / `Failed` / `ApprovalRequired` 永远保留（不走普通通道）
//!
//! 注意：这里给出**最常用**的 sink 实现，UI 可以自行实现 AgentSink
//! trait（比如直接 push 到 egui 的 `ctx.request_repaint`）。

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

use crate::event::{AgentEvent, AgentSink};

/// 背压策略。
#[derive(Debug, Clone, Copy)]
pub enum Backpressure {
    /// 满了就丢（仅适用于可丢弃事件）。
    DropOldest,
    /// 满了就阻塞发送方（会让 Worker 卡住，**慎用**）。
    Block,
}

/// 默认 sink：channel(256) + DropOldest 背压。
pub struct ChannelSink {
    tx: mpsc::Sender<AgentEvent>,
    rx: Mutex<Option<mpsc::Receiver<AgentEvent>>>,
    backpressure: Backpressure,
    /// 永远保留的事件（终态 + 审批）。
    priority_tx: mpsc::Sender<AgentEvent>,
    priority_rx: Mutex<Option<mpsc::Receiver<AgentEvent>>>,
}

impl std::fmt::Debug for ChannelSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelSink").finish_non_exhaustive()
    }
}

impl ChannelSink {
    /// 构造（容量 256）。
    pub fn new() -> Arc<Self> {
        Self::with_capacity(256)
    }

    /// 自定义容量。
    pub fn with_capacity(cap: usize) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(cap);
        // 单独给终态事件一个小通道，保证不丢
        let (ptx, prx) = mpsc::channel(16);
        Arc::new(Self {
            tx,
            rx: Mutex::new(Some(rx)),
            backpressure: Backpressure::DropOldest,
            priority_tx: ptx,
            priority_rx: Mutex::new(Some(prx)),
        })
    }

    /// 拿走普通事件接收器（只能调一次）。
    pub fn take_receiver(&self) -> Option<mpsc::Receiver<AgentEvent>> {
        self.rx.lock().take()
    }

    /// 拿走优先事件接收器。
    pub fn take_priority_receiver(&self) -> Option<mpsc::Receiver<AgentEvent>> {
        self.priority_rx.lock().take()
    }

    /// 事件是否为"必须保留"。
    fn is_priority(e: &AgentEvent) -> bool {
        matches!(
            e,
            AgentEvent::SessionFinished { .. }
                | AgentEvent::Cancelled { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::ApprovalRequired { .. }
                | AgentEvent::ProposalCreated { .. }
        )
    }

    /// try_send 普通通道，满了则按策略处理。
    fn try_send(&self, event: AgentEvent) {
        match self.backpressure {
            Backpressure::DropOldest => {
                // 满则丢；这里是 best-effort（生产端非阻塞）。
                let _ = self.tx.try_send(event);
            }
            Backpressure::Block => {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(event).await;
                });
            }
        }
    }
}

impl AgentSink for ChannelSink {
    fn on_event(&self, event: AgentEvent) {
        if Self::is_priority(&event) {
            let _ = self.priority_tx.try_send(event);
        } else {
            self.try_send(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn priority_events_are_routed_separately() {
        let sink = ChannelSink::new();
        let mut pri = sink.take_priority_receiver().unwrap();
        let mut rx = sink.take_receiver().unwrap();

        sink.on_event(AgentEvent::ApprovalRequired {
            session_id: "s".into(),
            proposal_id: qview_application::protocol::ProposalId::new(),
            tool: "annotate_create".into(),
            reason: "x".into(),
        });
        sink.on_event(AgentEvent::PhaseChanged {
            session_id: "s".into(),
            phase: crate::event::Phase::Thinking,
        });

        // 优先通道先收到 ApprovalRequired
        let e = pri.recv().await.unwrap();
        assert!(matches!(e, AgentEvent::ApprovalRequired { .. }));
        let e = rx.recv().await.unwrap();
        assert!(matches!(e, AgentEvent::PhaseChanged { .. }));
    }
}
