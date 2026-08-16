//! egui 版 AgentSink：把事件推到共享缓冲 + 触发 request_repaint。

use std::sync::Arc;

use egui::Context;
use parking_lot::Mutex;

use qview_agent::event::{AgentEvent, AgentSink};

/// EguiAgentSink：UI 线程每帧调用 `state.drain_events()` 取出。
#[derive(Clone)]
pub struct EguiAgentSink {
    pub events: Arc<Mutex<Vec<AgentEvent>>>,
    pub ctx: Context,
}

impl std::fmt::Debug for EguiAgentSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EguiAgentSink")
            .field("pending", &self.events.lock().len())
            .finish()
    }
}

impl EguiAgentSink {
    pub fn new(events: Arc<Mutex<Vec<AgentEvent>>>, ctx: Context) -> Self {
        Self { events, ctx }
    }
}

impl AgentSink for EguiAgentSink {
    fn on_event(&self, event: AgentEvent) {
        // 终态事件单独标记（让 UI 优先处理）
        let is_terminal = matches!(
            event,
            AgentEvent::SessionFinished { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::Cancelled { .. }
                | AgentEvent::ApprovalRequired { .. }
                | AgentEvent::ProposalCreated { .. }
        );
        self.events.lock().push(event);
        if is_terminal {
            // 终态：立刻请求重绘
            self.ctx.request_repaint();
        } else {
            // 普通事件：throttled 重绘
            self.ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}
