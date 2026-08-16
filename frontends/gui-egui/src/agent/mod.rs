//! Agent integration module（架构 §5.2.4 / §9.3）。
//!
//! 模块地图：
//! - `panel`：右侧 AgentPanel（时间线 / 输入框 / 取消）
//! - `sink`：egui 版 AgentSink（事件 → Mutex 缓冲 + request_repaint）
//! - `project`：ViewIntent → 主视图（viewer.rs）状态投影
//! - `approval`：ApprovalRequired 弹窗
//! - `state`：AgentPanelState（UI 共享状态）

pub mod approval;
pub mod panel;
pub mod project;
pub mod sink;
pub mod state;

pub use panel::AgentPanel;
pub use sink::EguiAgentSink;
pub use state::{AgentPanelState, ChatMsg};

/// 当前毫秒时间戳（工具记录落库时间用）。
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
