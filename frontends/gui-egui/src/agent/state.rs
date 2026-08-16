//! AgentPanel 共享状态（嵌入 QLogApp）。

use std::sync::Arc;

use parking_lot::Mutex;

use qview_agent::event::{AgentEvent, AgentSink};
use qview_agent::handle::AgentRuntimeHandle;
use qview_application::protocol::ProposalId;
use qview_application::protocol::view_intent::ViewIntent;

/// AgentPanel 状态。
///
/// 设计选择：用 `Arc<Mutex<Vec<AgentEvent>>>` 缓冲事件，egui 主线程每帧
/// `take_events()` 消费。事件 sink 推入时也 `request_repaint()` 触发立即重绘。
pub struct AgentPanelState {
    /// 事件缓冲（sink 写入；UI 读取）。
    pub events: Arc<Mutex<Vec<AgentEvent>>>,
    /// 当前活跃 session（仅同时跑一个；多 session 是 P5 工作）。
    pub active_session: Arc<Mutex<Option<String>>>,
    /// 当前对话的会话 id（**一次对话一个会话**，跨多轮复用）。
    ///
    /// `Some(id)` = 当前对话已开始，下一条消息继续同一会话（LLM 上下文带上历史）；
    /// `None` = 新会话（首次发送 / 点了「新建会话」）。见 `QLogApp::agent_conversation_history`。
    pub conversation_id: Arc<Mutex<Option<String>>>,
    /// UI 输入框（Arc<Mutex> 让 TextEdit 闭包可以持有）。
    pub input: Arc<Mutex<String>>,
    /// Agent runtime handle（启动 / 取消 / 审批）。
    pub handle: Arc<Mutex<Option<Arc<AgentRuntimeHandle>>>>,
    /// 当前 phase（UI 顶部显示）。
    pub current_phase: Arc<Mutex<qview_agent::event::Phase>>,
    /// 当前正在执行的工具名（ToolCallStarted 置、ToolCallFinished 清；
    /// 用于会话活跃时在消息区底部渲染"正在调用工具…"实时气泡）。
    pub in_flight_tool: Arc<Mutex<Option<String>>>,
    /// 实时气泡的拟人文案模板索引（按 phase 稳定：同一 phase 内文案不变，
    /// phase 变化时随机重选一条，见 panel.rs `typing_bubble`）。
    pub phase_bubble: Arc<Mutex<Option<(qview_agent::event::Phase, usize)>>>,
    /// 当前会话的工具调用记录（ToolCallStarted 入队、Finished 补结果；
    /// 「⌘ 工具记录」浮层展示，并随每次完成落盘到 qview-store）。
    pub tool_log: Arc<Mutex<Vec<qview_store::ToolCallRecord>>>,
    /// 请求消息列表滚动到底（发送消息时置位；messages() 渲染后消费并强制滚底）。
    pub scroll_to_bottom: Arc<Mutex<bool>>,
    /// 待审批 proposal（弹窗触发条件）。
    pub pending_proposal: Arc<Mutex<Option<(ProposalId, String, String)>>>,
    /// 聊天转录（气泡式会话；事件流 → ChatMsg 的映射由 UI 每帧做）。
    pub transcript: Arc<Mutex<Vec<ChatMsg>>>,
    /// 保持 sink 强引用（Weak 订阅语义：sink drop 即取消订阅）。
    pub sink_keepalive: Option<Arc<dyn AgentSink>>,
}

/// 一条聊天消息（气泡式渲染）。
#[derive(Debug, Clone)]
pub enum ChatMsg {
    /// 用户发送的问题（右对齐气泡）。
    User { text: String },
    /// 器灵回复 / 任务摘要（左对齐气泡）。
    Agent { text: String, is_error: bool },
    /// 视图意图（可点击跳转 / 应用）。
    Intent(ViewIntent),
    /// 系统提示（审批 / 阶段说明，弱化显示）。
    Note { text: String },
}

impl Default for AgentPanelState {
    fn default() -> Self {
        Self {
            events: Arc::new(Mutex::new(Vec::new())),
            active_session: Arc::new(Mutex::new(None)),
            conversation_id: Arc::new(Mutex::new(None)),
            input: Arc::new(Mutex::new(String::new())),
            handle: Arc::new(Mutex::new(None)),
            current_phase: Arc::new(Mutex::new(qview_agent::event::Phase::Done)),
            in_flight_tool: Arc::new(Mutex::new(None)),
            phase_bubble: Arc::new(Mutex::new(None)),
            tool_log: Arc::new(Mutex::new(Vec::new())),
            scroll_to_bottom: Arc::new(Mutex::new(false)),
            pending_proposal: Arc::new(Mutex::new(None)),
            transcript: Arc::new(Mutex::new(Vec::new())),
            sink_keepalive: None,
        }
    }
}

impl AgentPanelState {
    /// 取出全部事件并清空缓冲。
    pub fn drain_events(&self) -> Vec<AgentEvent> {
        std::mem::take(&mut *self.events.lock())
    }

    /// 设置 handle（runtime 构造时调一次）。
    pub fn set_handle(&self, h: Arc<AgentRuntimeHandle>) {
        *self.handle.lock() = Some(h);
    }
}
