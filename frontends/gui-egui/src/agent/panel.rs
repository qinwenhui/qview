//! 器灵浮动聊天窗口（架构 §9.3 的 egui 呈现）。
//!
//! 设计：
//! - 工具栏按钮触发；`egui::Window`（非模态，`Order::Foreground` 置顶），
//!   不阻塞主面板操作，可拖动 / 缩放。
//! - 自定义标题栏（渐变底 + 相位徽章 + 停止 / 清空 / 关闭），无 OS 标题条。
//! - 气泡式会话：用户消息右对齐蓝色气泡，器灵消息左对齐深色气泡，
//!   工具活动等宽弱化行，ViewIntent 可点击链接。

use std::path::PathBuf;

use egui::{Color32, Context, RichText, ScrollArea, TextEdit, Ui};

use qview_agent::config::LlmProvider;
use qview_agent::event::Phase;

use qview_application::protocol::view_intent::{FilterSpec, ViewIntent};

use crate::app::QLogApp;
use super::state::ChatMsg;

/// 面板点击动作（渲染后统一应用，避免 borrow 冲突）。
enum PanelAction {
    Jump(u64),
    ApplyFilter(FilterSpec),
    ClearFilter,
    OpenFile(PathBuf),
    NewFile,
    /// 底部状态栏「历史会话」：切换半透明浮层（历史会话列表）并触发加载。
    ShowHistory,
    /// 历史浮层「返回」：关闭浮层。
    ExitHistory,
    /// 点击一条历史会话 → 加载回看并关闭浮层。
    OpenHistory(String),
    /// 底部状态栏「新建会话」：取消进行中的任务、清空转录，回到干净起始态。
    NewSession,
    /// 底部状态栏「工具记录」：切换半透明浮层（当前会话工具调用记录）。
    ShowToolLog,
    /// 工具记录浮层「关闭」。
    ExitToolLog,
}

// ---- 配色（暗夜蓝专业风）----
const BG_HEADER_A: Color32 = Color32::from_rgb(30, 52, 92);
const BUBBLE_USER: Color32 = Color32::from_rgb(46, 82, 148);
const BUBBLE_AGENT: Color32 = Color32::from_rgb(22, 28, 44);
const BUBBLE_AGENT_ERR: Color32 = Color32::from_rgb(58, 28, 34);
const TXT_DIM: Color32 = Color32::from_rgb(120, 130, 148);
const TXT_LIGHT: Color32 = Color32::from_rgb(214, 222, 236);
const ACCENT: Color32 = Color32::from_rgb(86, 130, 220);

/// 器灵窗口尺寸（独立子窗口 & 嵌主窗口共用）：
/// 宽 456，高 = 屏高 × 0.74（clamp 500..880）。比例 ≈ 1:1.4，与旧 380×533 一致，
/// 只是整体更大。
pub const AGENT_WIN_W: f32 = 456.0;
pub const AGENT_WIN_H_RATIO: f32 = 0.74;

/// 各阶段的拟人化实时气泡文案模板（面向用户，符合小Q人设：欢快、灵动、口语化）。
/// 每次 phase 变化时随机取一条；调用工具时仍显示「调用工具 {name}」，不走这里。
fn bubble_templates(phase: Phase) -> &'static [&'static str] {
    match phase {
        Phase::Routing => &["容我想想", "收到，我处理一下", "好嘞，我来看看", "嗯嗯，明白了"],
        Phase::Thinking => &["让我想想", "我在琢磨一下", "稍等，我想想", "嗯…让我捋一捋"],
        Phase::Searching => &["我找找看", "正在翻找相关记录", "让我搜搜", "翻翻日志去"],
        Phase::Inspecting => &["我看看文件内容", "正在翻看", "让我读一下", "翻到那了，看看"],
        Phase::Drafting => &["我在整理结论", "马上好，组织下回答", "梳理一下给你"],
        Phase::AwaitingApproval => &["需要你拍板一下", "这个要你点头才行", "等你审批"],
        _ => &[], // Done / Failed / Cancelled（终态不显示）
    }
}

/// 相位徽章的中文短标签（不暴露内部英文枚举，直接面向用户）。
fn phase_badge_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Routing => "计划中",
        Phase::Thinking => "思考中",
        Phase::Searching => "搜索中",
        Phase::Inspecting => "检视中",
        Phase::Drafting => "整理中",
        Phase::AwaitingApproval => "待审批",
        Phase::Done => "完成",
        Phase::Failed => "失败",
        Phase::Cancelled => "已取消",
    }
}

/// AgentPanel widget（持有 `&mut QLogApp`，点击动作可投影到主视图）。
pub struct AgentPanel<'a> {
    pub app: &'a mut QLogApp,
}

impl<'a> AgentPanel<'a> {
    pub fn new(app: &'a mut QLogApp) -> Self {
        Self { app }
    }

    /// 渲染浮动聊天窗口。
    ///
    /// `detached=true`：面板渲染在**独立原生子窗口**里（eframe 多视口），
    /// 填满整个子窗口，可拖出主窗口之外；`false`：嵌在主窗口里（右上角浮动，
    /// 多视口不支持的兜底）。两种模式都走 `Area` 全手动布局，
    /// 尺寸完全确定，不会被内容撑大。
    pub fn show_window(self, ctx: &Context, detached: bool) {
        let mut actions: Vec<PanelAction> = Vec::new();

        // 打开后第一帧自动聚焦输入框
        if self.app.agent_focus_input {
            self.app.agent_focus_input = false;
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("agent_chat_input")));
        }

        // Enter 状态在窗口渲染前快照（TextEdit 可能消费该事件）
        let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));

        // 尺寸 / 位置：
        // - 独立子窗口：填满整个子窗口（子窗口尺寸由 ViewportBuilder 决定）。
        // - 嵌在主窗口：竖长方形，右上角（工具栏下方），仍可拖动。
        // 注意：不用 `egui::Window`（它随内容自动撑大，`fixed_size` 实测压不住，
        // 曾把窗口撑到 390/696 宽）。改用 `Area` 全手动布局：
        // 先 `allocate_exact_size` 分配精确 (w,h) 矩形，所有子内容用
        // `scope_builder(max_rect=rect)` 约束在盒内 —— 窗口尺寸完全确定。
        let screen = ctx.screen_rect();
        let (w, h, pos) = if detached {
            (screen.width(), screen.height(), screen.min)
        } else {
            let w = AGENT_WIN_W;
            let h = (screen.height() * AGENT_WIN_H_RATIO).clamp(500.0, 880.0);
            (w, h, egui::pos2(screen.right() - w - 16.0, screen.top() + 54.0))
        };
        let win_size = egui::vec2(w, h);

        let mut close_requested = false;
        let mut header_rect = egui::Rect::ZERO;
        let mut content_rect = egui::Rect::ZERO;
        // 嵌主窗口时顶条本帧拖拽增量（Area 闭包内累积，闭包外应用）
        let mut header_drag_delta = egui::Vec2::ZERO;

        // 位置：独立子窗口固定填满 viewport（无视历史拖动位置）；嵌主窗口用默认位置
        // 或顶条拖过的最新位置。
        let area_pos = if detached {
            pos
        } else {
            self.app.agent_area_pos.unwrap_or(pos)
        };
        let ir = egui::Area::new(egui::Id::new("qview_agent_area_v4"))
            .order(egui::Order::Foreground)
            // **关键**：不能 `movable(true)` —— 那会让整个 Area 矩形都可拖（egui 在
            // 整块区域上注册 drag），用户按住聊天背景拖动会把内容/背景整体拖动，
            // 内容和窗口"脱开"。窗口移动只能由顶条触发（header 里：独立子窗口发
            // OS StartDrag；嵌主窗口手动挪 area_pos）。
            .movable(false)
            .fixed_pos(area_pos)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(win_size, egui::Sense::hover());

                // 窗框：圆角深色底 + 描边
                let painter = ui.painter();
                painter.rect_filled(rect, 12, Color32::from_rgb(14, 19, 30));
                painter.rect_stroke(
                    rect,
                    12,
                    egui::Stroke::new(1.0, Color32::from_rgb(46, 66, 106)),
                    egui::StrokeKind::Inside,
                );

                // 所有子内容约束在 rect 内（scope 从 rect.min 开始布局）。
                // set_clip_rect(rect)：兜底——即使某子内容布局超宽，也**画不出背景**，
                // 不会溢出到主窗口上。正常换行情况下不会裁剪掉任何东西。
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.set_max_width(w);
                    ui.set_clip_rect(rect);
                    header_rect = self.header(
                        ui,
                        detached,
                        &mut header_drag_delta,
                        &mut close_requested,
                    );
                    // 顶条下面**不画分隔线**：顶条背景(46px)直接衔接内容区，否则会出现
                    // 一条"比背景还低"的横线，看着像状态栏底部边框多出来一块（用户反馈）。
                    // 消息区必须限制高度：ScrollArea auto_shrink([false,false]) 会
                    // 吃满剩余高度，若不预留输入栏空间，输入栏会被顶出窗口并裁掉。
                    // 这里给消息区分配「剩余高度 - 输入栏预留 - 底部状态栏预留」的固定高。
                    let input_reserve = 64.0; // 输入栏(~50，含底部 14px 留白) + 分隔线(~6) + 余量
                    let bottom_reserve = 50.0; // 底部状态栏(~34，含 10px 底部留白) + 分隔线(~6) + 余量
                    let msg_h = (ui.available_height() - input_reserve - bottom_reserve).max(100.0);
                    let msg_rect = egui::Rect::from_min_size(
                        ui.cursor().min,
                        egui::vec2(ui.available_width(), msg_h),
                    );
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(msg_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                        |ui| {
                            // **渲染顺序**：历史 / 工具浮层先 `show()`（egui::Area，不参与
                            // 父布局 → 不会挤下聊天），内部 ScrollArea 先消费滚轮；
                            // 随后 messages() 渲染，看到 smooth_scroll_delta=0 → 聊天不动。
                            // 浮层**视觉上**仍然盖在聊天上（Area 是独立 layer，
                            // Order::Tooltip 在 chat 之上）。
                            if self.app.agent_show_history {
                                self.history_overlay(ctx, msg_rect, &mut actions);
                            }
                            if self.app.agent_show_tool_log {
                                self.tool_log_overlay(ctx, msg_rect, &mut actions);
                            }
                            // 消息区始终渲染；历史 / 工具记录是**半透明浮层盖在上面**。
                            self.messages(ui, &mut actions);
                        },
                    );
                    ui.separator();
                    self.input_bar(ui, enter_pressed);
                    ui.separator();
                    self.bottom_bar(ui, &mut actions);
                    content_rect = ui.min_rect();
                });
            });

        // 嵌主窗口：顶条拖拽增量 → 应用到 area_pos（下一帧 fixed_pos 生效）。
        // 独立子窗口不在这挪（OS StartDrag 移动的是原生窗口本体）。
        if !detached && header_drag_delta != egui::Vec2::ZERO {
            let p = self.app.agent_area_pos.get_or_insert(area_pos);
            *p += header_drag_delta;
        }

        // 调试：窗口矩形 = Area 响应矩形（应为精确 (w,h)，差应归零）。
        // `内容` = 实际内容最小矩形；若 内容宽 > fixed 宽 → 有内容把布局撑宽。
        self.app.debug_agent_rects(ir.response.rect, header_rect, content_rect, win_size);

        if close_requested {
            self.app.show_agent_window = false;
        }

        // 统一应用面板点击动作（闭包 borrow 已结束）
        for a in actions {
            match a {
                PanelAction::Jump(line) => self.app.agent_jump_to_line(line),
                PanelAction::ApplyFilter(f) => self.app.agent_set_filter(f),
                PanelAction::ClearFilter => self.app.agent_clear_filter(),
                PanelAction::OpenFile(path) => self.app.open_file(path),
                PanelAction::NewFile => self.app.create_new_file(),
                PanelAction::ShowHistory => {
                    // 两个浮层互斥：开历史就关工具记录
                    self.app.agent_show_tool_log = false;
                    self.app.agent_show_history = !self.app.agent_show_history;
                    if self.app.agent_show_history {
                        self.app.request_history_reload();
                    }
                }
                PanelAction::ExitHistory => self.app.agent_show_history = false,
                PanelAction::OpenHistory(sid) => {
                    self.app.agent_show_history = false;
                    self.app.open_history_session(&sid);
                }
                PanelAction::NewSession => self.app.agent_new_session(),
                PanelAction::ShowToolLog => {
                    self.app.agent_show_history = false;
                    self.app.agent_show_tool_log = !self.app.agent_show_tool_log;
                }
                PanelAction::ExitToolLog => self.app.agent_show_tool_log = false,
            }
        }
    }

    // ------------------------------------------------------------------
    // header
    // ------------------------------------------------------------------

    fn header(
        &self,
        ui: &mut Ui,
        detached: bool,
        drag_out: &mut egui::Vec2,
        close_requested: &mut bool,
    ) -> egui::Rect {
        let desired = egui::vec2(ui.available_width(), 46.0);
        let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());

        // 标题底（顶部圆角，底部方角与内容衔接）。
        // 注意：现在用 Area 全手动布局，内容盒 == 窗口矩形（没有 Window 的
        // frame 内缩），顶条直接铺满 rect 即贴满窗口宽度；不要 expand，
        // 否则上探 1px 会盖住窗口顶部圆角/描边。
        let paint = rect;
        ui.painter().rect_filled(
            paint,
            egui::CornerRadius {
                nw: 12,
                ne: 12,
                sw: 0,
                se: 0,
            },
            BG_HEADER_A,
        );

        // 无系统标题栏 → 自定义顶条充当拖动把手（**唯一的窗口移动方式**：
        // Area 已 movable(false)，聊天背景/内容不再可拖）。
        // **关键**：interact 必须在 scope **之前**注册（在按钮**下面**）——
        // 若在 scope 之后，拖拽 interact 盖在按钮上面，会抢走 ✘/清空/停止 的点击
        // （用户实测：✘ 关不掉窗口）。
        // 按钮（点击）不受影响：拖拽从空白顶条发起时由本 interact 接管。
        // - `detached`（独立子窗口）：发 OS StartDrag，整个原生窗口跟手移动。
        // - 嵌主窗口（多视口兜底）：手动累加 drag_delta 到 `agent_area_pos`，
        //   下一帧 `fixed_pos` 应用 —— 只有顶条能挪，内容始终贴住窗口。
        let drag_id = ui.id().with("agent_header_drag");
        let drag_resp = ui.interact(rect, drag_id, egui::Sense::drag());
        if detached {
            // 按下即发（is_pointer_button_down_on），拖动中也发（dragged）——都覆盖。
            if drag_resp.is_pointer_button_down_on() || drag_resp.dragged() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
        } else if drag_resp.dragged() {
            // 嵌主窗口：把本帧拖拽增量带回 show_window，由后者应用 agent_area_pos
            //（header 是 `&self`，不方便直接改 app 字段；闭包外统一应用）。
            *drag_out += drag_resp.drag_delta();
        }

        let builder = egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new("☯").size(16.0).color(ACCENT).strong());
            ui.label(
                RichText::new("器灵 AI")
                    .size(15.0)
                    .strong()
                    .color(Color32::from_rgb(196, 210, 240)),
            );
            self.phase_badge(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // 右边距：✘ 不贴右缘
                ui.add_space(8.0);
                // 关闭（大一点好点，离开边框一点）
                if ui
                    .add(
                        egui::Button::new(RichText::new("x").size(15.0).color(TXT_LIGHT))
                            .frame(false)
                            .min_size(egui::vec2(24.0, 24.0)),
                    )
                    .on_hover_text("关闭（不退出器灵）")
                    .clicked()
                {
                    *close_requested = true;
                }
                // 清空会话
                if ui
                    .add(
                        egui::Button::new(RichText::new("清空").size(11.0).color(TXT_DIM))
                            .frame(false),
                    )
                    .on_hover_text("清空聊天记录")
                    .clicked()
                {
                    self.app.agent_state.transcript.lock().clear();
                    self.app.agent_state.events.lock().clear();
                }
                // 停止（活跃时可用）
                let active = self.app.agent_state.active_session.lock().is_some();
                let stop_btn = egui::Button::new(
                    RichText::new("■ 停止").size(11.0).color(Color32::from_rgb(255, 140, 150)),
                )
                .fill(Color32::from_rgb(120, 40, 48))
                .min_size(egui::vec2(54.0, 22.0));
                if ui.add_enabled(active, stop_btn).clicked() {
                    self.cancel_active();
                }
            });
        });

        paint
    }

    fn phase_badge(&self, ui: &mut Ui) {
        let phase = *self.app.agent_state.current_phase.lock();
        // 不画背景，只留绿色文字（避免一大块药丸很丑）；
        // 其余阶段保留胶囊但收窄边距，贴合文本高度。
        let (bg, fg) = match phase {
            Phase::Routing
            | Phase::Thinking
            | Phase::Searching
            | Phase::Inspecting
            | Phase::Drafting => (None, Color32::from_rgb(224, 214, 160)),
            Phase::AwaitingApproval => (None, Color32::from_rgb(255, 190, 170)),
            Phase::Done => (None, Color32::from_rgb(150, 230, 180)),
            Phase::Failed => (None, Color32::from_rgb(255, 170, 170)),
            Phase::Cancelled => (None, Color32::from_rgb(180, 184, 196)),
        };
        let label = RichText::new(phase_badge_label(phase)).size(10.5).color(fg).strong();
        match bg {
            Some(bg) => {
                egui::Frame::new()
                    .corner_radius(8)
                    .fill(bg)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.label(label);
                    });
            }
            None => {
                ui.label(label);
            }
        }
    }

    fn cancel_active(&self) {
        let h = self.app.agent_state.handle.lock().clone();
        let sid = self.app.agent_state.active_session.lock().clone();
        if let (Some(h), Some(sid)) = (h, sid) {
            self.app.spawn_tokio(async move {
                let _ = h.cancel_within(sid, std::time::Duration::from_secs(1)).await;
            });
        }
    }

    // ------------------------------------------------------------------
    // messages（气泡式会话）
    // ------------------------------------------------------------------

    fn messages(&self, ui: &mut Ui, actions: &mut Vec<PanelAction>) {
        // 过滤器激活时显示"清除"小条
        if self.app.agent_filter.is_some() {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("⛶ 器灵过滤器已应用")
                        .size(11.0)
                        .color(Color32::from_rgb(90, 150, 235)),
                );
                if ui
                    .link(RichText::new("清除").size(11.0).strong())
                    .clicked()
                {
                    actions.push(PanelAction::ClearFilter);
                }
            });
            ui.separator();
        }

        let msgs = self.app.agent_state.transcript.lock();
        let active = self.app.agent_state.active_session.lock().is_some();
        if msgs.is_empty() && !active {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("☯")
                        .size(30.0)
                        .color(ACCENT),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("还没有对话 — 在下方输入一个问题吧")
                        .color(TXT_DIM),
                );
            });
            return;
        }
        // 只开垂直滚动。egui 滚动区在"水平未启用 + auto_shrink=false"时，
        // 宽度 = max(可用宽, 内容宽)——长内容会把滚动区（乃至窗口）撑宽。
        // 解决：只开垂直滚动（内容宽被视口约束）+ 内容最大宽度钉在视口宽，
        // 超长文本一律自动换行：横向滚动条既不出现，窗口也不会被撑大
        // （Area 的 allocate_exact_size 还兜底了一层）。
        ScrollArea::vertical()
            .id_salt("agent_chat_scroll")
            .stick_to_bottom(true)
            .drag_to_scroll(false)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // 左右内缩：头像 / 气泡不贴窗口左右边框（用户反馈"太贴近边框"）。
                let inset = 12.0;
                let inner_rect = egui::Rect::from_min_max(
                    egui::pos2(ui.cursor().min.x + inset, ui.cursor().min.y),
                    egui::pos2(ui.max_rect().max.x - inset, ui.max_rect().max.y),
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(inner_rect),
                    |ui| {
                        ui.set_max_width(inner_rect.width());
                        ui.add_space(6.0);
                        for m in msgs.iter() {
                            self.message_row(ui, m, actions);
                        }
                        // 会话活跃期间的实时气泡（思考 / 搜索 / 调用工具 / 等待审批）。
                        // 工具调用不再逐行入转录，只在这里显示一条"调用工具 xxx…"。
                        self.typing_bubble(ui);
                        ui.add_space(6.0);
                    },
                );
                // 发送消息后强制滚到底（即使之前向上翻过历史也滚）。
                // 滚动量基于 scope 内容 min_rect → 需在 scope 外调用。
                if *self.app.agent_state.scroll_to_bottom.lock() {
                    *self.app.agent_state.scroll_to_bottom.lock() = false;
                    ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                }
            });
    }

    /// 会话活跃且尚未终态时的实时指示气泡。
    ///
    /// 有工具在飞 → 显示「调用工具 {name} …」；否则按当前 phase 从拟人文案模板
    /// 里随机取一条（如「容我想想…」「我找找看…」，见 [`bubble_templates`]）。
    /// 只显示这一条，工具活动不污染转录（用户要求：只要最终回答）。
    fn typing_bubble(&self, ui: &mut Ui) {
        let active = self.app.agent_state.active_session.lock().is_some();
        if !active {
            return;
        }
        let tool = self.app.agent_state.in_flight_tool.lock().clone();
        let phase = *self.app.agent_state.current_phase.lock();
        // 动态效果：末尾"点点"按时间循环 0..3 个（约每 0.33s 变一次），并持续
        // 请求重绘，让用户看出"还在工作"而不是卡了。
        let t = ui.ctx().input(|i| i.time);
        let dots = ".".repeat(((t * 3.0) as usize) % 4);
        let base = if let Some(name) = tool {
            format!("调用工具 {name}")
        } else {
            let templates = bubble_templates(phase);
            if templates.is_empty() {
                return; // Done / Failed / Cancelled（终态不走这里）
            }
            // 同一 phase 内文案保持稳定；phase 变化时随机重选一条（时间做种子）。
            let mut bubble = self.app.agent_state.phase_bubble.lock();
            let idx = match *bubble {
                Some((p, i)) if p == phase => i,
                _ => {
                    let seed = (t * 1000.0) as u64 ^ (phase as u64).wrapping_mul(0x9E37_79B1);
                    let n = seed as usize % templates.len();
                    *bubble = Some((phase, n));
                    n
                }
            };
            templates[idx % templates.len()].to_string()
        };
        let text = format!("{base}{dots}");
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
        ui.add_space(5.0);
        // 与器灵气泡一致：左边 AI 头像 + 指示气泡
        ui.horizontal(|ui| {
            avatar(ui, "☯", Color32::from_rgb(70, 110, 180));
            ui.add_space(6.0);
            let bw = (ui.available_width() - 8.0).min(320.0);
            let frame = egui::Frame::new()
                .corner_radius(12)
                .fill(Color32::from_rgb(24, 30, 46))
                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(46, 66, 106)))
                .inner_margin(egui::Margin::symmetric(12, 8));
            bubble_fixed(ui, bw, false, &text, TXT_DIM, frame);
        });
    }

    fn message_row(&self, ui: &mut Ui, m: &ChatMsg, actions: &mut Vec<PanelAction>) {
        match m {
            ChatMsg::User { text } => {
                ui.add_space(5.0);
                // 用户消息：右对齐。头像最右，气泡在左，气泡内文本左对齐。
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    avatar(ui, "我", Color32::from_rgb(70, 140, 100));
                    ui.add_space(6.0);
                    let bw = (ui.available_width() - 8.0).min(300.0);
                    let frame = egui::Frame::new()
                        .corner_radius(12)
                        .fill(BUBBLE_USER)
                        .inner_margin(egui::Margin::symmetric(12, 8));
                    bubble_fixed(ui, bw, true, text, Color32::WHITE, frame);
                });
            }
            ChatMsg::Agent { text, is_error } => {
                ui.add_space(5.0);
                // 器灵消息：左对齐。头像最左，气泡在右。
                ui.horizontal(|ui| {
                    avatar(ui, "☯", Color32::from_rgb(70, 110, 180));
                    ui.add_space(6.0);
                    let bw = (ui.available_width() - 8.0).min(320.0);
                    let frame = egui::Frame::new()
                        .corner_radius(12)
                        .fill(if *is_error { BUBBLE_AGENT_ERR } else { BUBBLE_AGENT })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if *is_error {
                                Color32::from_rgb(90, 40, 48)
                            } else {
                                Color32::from_rgb(40, 52, 78)
                            },
                        ))
                        .inner_margin(egui::Margin::symmetric(12, 8));
                    let color = if *is_error {
                        Color32::from_rgb(255, 180, 180)
                    } else {
                        TXT_LIGHT
                    };
                    bubble_fixed(ui, bw, false, text, color, frame);
                });
            }
            ChatMsg::Intent(intent) => {
                ui.add_space(2.0);
                self.intent_row(ui, intent, actions);
            }
            ChatMsg::Note { text } => {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(text)
                        .italics()
                        .size(11.0)
                        .color(TXT_DIM),
                );
            }
        }
    }

    /// ViewIntent 行：FocusLine / ApplyFilter 可点击。
    fn intent_row(&self, ui: &mut Ui, intent: &ViewIntent, actions: &mut Vec<PanelAction>) {
        match intent {
            ViewIntent::FocusLine { line } => {
                if ui
                    .link(RichText::new(format!("↧ 跳转到第 {} 行", line + 1)).strong().size(12.0))
                    .clicked()
                {
                    actions.push(PanelAction::Jump(*line));
                }
            }
            ViewIntent::ApplyFilter { filter } => {
                if ui
                    .link(
                        RichText::new(format!("⛶ 应用过滤器: {}", filter_desc(filter)))
                            .strong()
                            .size(12.0),
                    )
                    .clicked()
                {
                    actions.push(PanelAction::ApplyFilter(filter.clone()));
                }
            }
            ViewIntent::HighlightRange { start, end, kind, .. } => {
                ui.label(
                    RichText::new(format!("  ◧ 已高亮 {}–{} ({kind:?})", start + 1, end + 1))
                        .size(11.0)
                        .color(TXT_DIM),
                );
            }
            ViewIntent::OpenPanel { panel } => {
                ui.label(
                    RichText::new(format!("  ▦ 打开面板: {panel:?}"))
                        .size(11.0)
                        .color(TXT_DIM),
                );
            }
            ViewIntent::ShowMessage { level, text } => {
                let short: String = text.chars().take(80).collect();
                ui.label(
                    RichText::new(format!("  💬 [{level:?}] {short}"))
                        .size(11.0)
                        .color(TXT_DIM),
                );
            }
            ViewIntent::OpenDocument { path } => {
                let short: String = path.chars().take(48).collect();
                if ui
                    .link(RichText::new(format!("↧ 打开文件: {short}")).strong().size(12.0))
                    .clicked()
                {
                    actions.push(PanelAction::OpenFile(PathBuf::from(path.clone())));
                }
            }
            ViewIntent::NewDocument { name } => {
                let short: String = name.chars().take(24).collect();
                if ui
                    .link(RichText::new(format!("✚ 新建文件: {short}")).strong().size(12.0))
                    .clicked()
                {
                    actions.push(PanelAction::NewFile);
                }
            }
            ViewIntent::ClearFilter => {
                ui.label(RichText::new("  ⛶ 已清除过滤器").size(11.0).color(TXT_DIM));
            }
            ViewIntent::ToggleWordWrap { enabled } => {
                ui.label(
                    RichText::new(format!("  ⇥ 自动换行：{}", if *enabled { "开" } else { "关" }))
                        .size(11.0)
                        .color(TXT_DIM),
                );
            }
            ViewIntent::SwitchTheme { theme } => {
                ui.label(RichText::new(format!("  🎨 切换主题：{theme}")).size(11.0).color(TXT_DIM));
            }
        }
    }

    // ------------------------------------------------------------------
    // input
    // ------------------------------------------------------------------

    fn input_bar(&self, ui: &mut Ui, enter_pressed: bool) {
        // 上下留白对称：输入框 + 发送按钮在上下两条分隔线之间垂直居中
        //（之前上 4 下 14，视觉上偏上，像下边框多了一块）。
        ui.add_space(9.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let active = self.app.agent_state.active_session.lock().is_some();
            let mut input = self.app.agent_state.input.lock();
            // 输入框宽度 = 可用宽 - 发送按钮 - 边距。去掉上限 clamp：
            // 之前窗口窄时 clamp 到 320 刚好贴右；窗口加宽后上限反而让
            // 发送按钮右侧留出空隙（用户反馈不协调）。下限 120 保留防缩爆。
            let tw = (ui.available_width() - 72.0).max(120.0);
            let resp = ui.add_sized(
                [tw, 32.0],
                TextEdit::singleline(&mut *input)
                    .id(egui::Id::new("agent_chat_input"))
                    .hint_text("问器灵…（Enter 发送）")
                    .margin(egui::vec2(10.0, 7.0)),
            );
            let send = ui.add(
                egui::Button::new(RichText::new("发送").strong().color(Color32::WHITE))
                    .fill(ACCENT)
                    .min_size(egui::vec2(56.0, 32.0)),
            );
            // Enter 提交：单行输入框回车会**主动放弃焦点**（egui 行为，见
            // TextEdit::singleline 文档），因此用 `lost_focus + 本帧 Enter 快照`
            // 判定（与 toolbar 跳转框同一模式），而不是 `has_focus`。
            let submit = send.clicked() || (enter_pressed && resp.lost_focus());
            if submit && !input.trim().is_empty() {
                let text = input.trim().to_string();
                input.clear();
                drop(input); // 释放锁再 send（send 需要 &self）
                if !active {
                    self.send(&text);
                } else {
                    self.app.agent_state.transcript.lock().push(ChatMsg::Note {
                        text: "⏳ 器灵正在处理上一个问题，请稍候…".into(),
                    });
                }
                // 提交后下一帧重新聚焦输入框，方便连续对话
                ui.ctx().memory_mut(|m| m.request_focus(egui::Id::new("agent_chat_input")));
            }
            ui.add_space(8.0);
        });
        // 与顶部对称（9px），输入行在矩形内垂直居中
        ui.add_space(9.0);
    }

    // ------------------------------------------------------------------
    // bottom bar（历史会话 / 新建会话）
    // ------------------------------------------------------------------
    // 布局要点：左右各留边距（内容不贴窗口边框，右侧计数不被边框挡）、
    // 底部留白（整条状态栏上移，不贴窗口底边）。

    fn bottom_bar(&self, ui: &mut Ui, actions: &mut Vec<PanelAction>) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(12.0); // 左边距
            // 按钮间留足间隔（之前 3px 太挤，三个按钮"贴贴"；右侧计数也随之间距拉开）
            ui.spacing_mut().item_spacing = egui::vec2(14.0, 0.0);

            // 历史会话 → 内嵌列表
            let hist = egui::Button::new(
                RichText::new("☰ 历史会话").size(10.5).color(TXT_LIGHT),
            )
            .frame(false);
            if ui
                .add(hist)
                .on_hover_text("查看本地保存的历史会话（只读回看）")
                .clicked()
            {
                actions.push(PanelAction::ShowHistory);
            }

            // 新建会话 → 清空当前对话
            let new = egui::Button::new(
                RichText::new("✚ 新建会话").size(10.5).color(TXT_LIGHT),
            )
            .frame(false);
            if ui
                .add(new)
                .on_hover_text("取消进行中的任务并清空当前对话")
                .clicked()
            {
                actions.push(PanelAction::NewSession);
            }

            // 工具记录 → 半透明浮层展示当前会话的工具调用记录
            let tools = egui::Button::new(
                RichText::new("⌘ 工具记录").size(10.5).color(TXT_LIGHT),
            )
            .frame(false);
            if ui
                .add(tools)
                .on_hover_text("查看当前会话的工具调用记录（已保存到本地数据库）")
                .clicked()
            {
                actions.push(PanelAction::ShowToolLog);
            }

            // 右侧：会话状态提示（活跃/条数）—— RTL 布局开头加右边距，不贴边框
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(14.0); // 右边距
                let active = self.app.agent_state.active_session.lock().is_some();
                let n = self.app.agent_state.transcript.lock().len();
                let txt = if active {
                    format!("● 处理中 · {n} 条消息")
                } else {
                    format!("{n} 条消息")
                };
                ui.label(RichText::new(txt).size(10.0).color(TXT_DIM));
            });
        });
        ui.add_space(10.0); // 底部留白：状态栏整体上移
    }

    // ------------------------------------------------------------------
    // overlay（半透明浮层：历史会话 / 工具记录）
    // 盖在消息区上，不再替换布局 —— 用户要求「别把上面的内容挤上去」，
    // 聊天内容透过半透明底仍隐约可见。
    // ------------------------------------------------------------------

    /// 浮层外壳：半透明圆角底 + 标题栏 + 关闭按钮 + 内容体。
    ///
    /// 使用 [`egui::Area`] 而非内联 `scope_builder`：浮层**不参与父布局**，
    /// 因此不会"挤"下聊天内容；Area 是独立 layer，绘制顺序自然在 chat 之上。
    /// 同时 Area 的内容先于 messages() 渲染（看调用顺序），浮层内的 `ScrollArea`
    /// 先消费滚轮 → 滚动事件落到浮层而不是聊天。
    fn overlay_shell(
        &self,
        ctx: &egui::Context,
        id: egui::Id,
        rect: egui::Rect,
        title: &str,
        close_action: PanelAction,
        actions: &mut Vec<PanelAction>,
        body: impl FnOnce(&mut Ui, &mut Vec<PanelAction>),
    ) {
        // from_rgba_unmultiplied 非 const → 用局部（浮层非热路径）。
        let overlay_bg = Color32::from_rgba_unmultiplied(16, 22, 36, 242);
        const OVERLAY_BORDER: Color32 = Color32::from_rgb(58, 78, 118);

        egui::Area::new(id)
            // 必须跟父面板同层 (`Order::Foreground`)：
            //   - 默认 `Order::Middle` 会被父 Area (`Foreground`) 盖住，
            //     用户在浮层 rect 内点击实际落在父层（聊天区），浮层收不到事件。
            //   - 同时浮层 rect = msg_rect，浮层跟父层抢同一片区域，Foreground
            //     高的层胜出 → 浮层失效。
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(true)
            .show(ctx, |ui| {
                // 强制 Area 实际尺寸 = rect：allocate_ui(desired_size) 让父层
                // 跳过此子项的 layout 贡献 → Area 完全独立、不挤其它内容。
                ui.allocate_ui(rect.size(), |ui| {
                    let r = ui.max_rect();
                    ui.painter().rect_filled(r, 12.0, overlay_bg);
                    ui.painter().rect_stroke(
                        r,
                        12.0,
                        egui::Stroke::new(1.0, OVERLAY_BORDER),
                        egui::StrokeKind::Inside,
                    );
                    ui.set_max_width(r.width());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(title).size(12.0).strong().color(TXT_LIGHT),
                        );
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.add_space(8.0);
                                if ui.link(RichText::new("x 关闭").size(11.0)).clicked() {
                                    actions.push(close_action);
                                }
                            },
                        );
                    });
                    ui.separator();
                    body(ui, actions);
                });
            });
    }

    /// 历史会话浮层（半透明盖在消息区上）。
    fn history_overlay(
        &self,
        ctx: &egui::Context,
        msg_rect: egui::Rect,
        actions: &mut Vec<PanelAction>,
    ) {
        let body_h = (msg_rect.height() - 40.0).max(100.0);
        let overlay_rect = egui::Rect::from_min_max(
            egui::pos2(msg_rect.min.x + 10.0, msg_rect.min.y),
            egui::pos2(msg_rect.max.x - 10.0, msg_rect.max.y),
        );
        self.overlay_shell(
            ctx,
            egui::Id::new("agent_history_overlay"),
            overlay_rect,
            "最近 50 条 AI 会话",
            PanelAction::ExitHistory,
            actions,
            |ui, actions| {
                let loading = self.app.history_sessions.lock().is_none();
                let list = self.app.history_sessions.lock().clone().unwrap_or_default();

                if loading && list.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("加载中…").size(12.0).color(TXT_DIM));
                    });
                } else if list.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("暂无历史会话。和器灵聊过之后再来看看。").size(12.0).color(TXT_DIM));
                    });
                } else {
                    ScrollArea::vertical()
                        .id_salt("agent_history_overlay_scroll")
                        .drag_to_scroll(false)
                        .auto_shrink([false, false])
                        .max_height(body_h)
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            ui.add_space(2.0);
                            for s in list.iter() {
                                if let Some(sid) = self.history_row(ui, s) {
                                    actions.push(PanelAction::OpenHistory(sid));
                                }
                            }
                            ui.add_space(2.0);
                        });
                }
            },
        );
    }

    /// 工具记录浮层：当前会话的工具调用列表（数据来自 `tool_log`，已落库 qview-store）。
    fn tool_log_overlay(
        &self,
        ctx: &egui::Context,
        msg_rect: egui::Rect,
        actions: &mut Vec<PanelAction>,
    ) {
        let body_h = (msg_rect.height() - 40.0).max(100.0);
        let count = self.app.agent_state.tool_log.lock().len();
        let title = format!("工具调用记录 · {count} 条");
        let overlay_rect = egui::Rect::from_min_max(
            egui::pos2(msg_rect.min.x + 10.0, msg_rect.min.y),
            egui::pos2(msg_rect.max.x - 10.0, msg_rect.max.y),
        );
        self.overlay_shell(
            ctx,
            egui::Id::new("agent_tool_log_overlay"),
            overlay_rect,
            &title,
            PanelAction::ExitToolLog,
            actions,
            |ui, _actions| {
                let log = self.app.agent_state.tool_log.lock().clone();
                if log.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("本会话还没有工具调用。").size(12.0).color(TXT_DIM));
                    });
                } else {
                    ScrollArea::vertical()
                        .id_salt("agent_tool_log_scroll")
                        .drag_to_scroll(false)
                        .auto_shrink([false, false])
                        .max_height(body_h)
                        .show(ui, |ui| {
                            ui.set_max_width(ui.available_width());
                            ui.add_space(2.0);
                            for r in log.iter() {
                                self.tool_log_row(ui, r);
                            }
                            ui.add_space(2.0);
                        });
                }
            },
        );
    }

    /// 单条工具调用记录（整行：状态图标 + 工具名 + 耗时 + 入参/结果摘要）。
    fn tool_log_row(&self, ui: &mut Ui, r: &qview_store::ToolCallRecord) {
        let icon = if r.is_error { "❌" } else { "✅" };
        let dur = if r.duration_ms > 0 {
            format!("{}ms", r.duration_ms)
        } else {
            String::new()
        };
        let input = r.input.chars().take(90).collect::<String>();
        let output = if r.output.is_empty() {
            "…".to_string()
        } else {
            r.output.chars().take(130).collect::<String>()
        };
        let text = format!("{icon} {}  {dur}\n  ← 入参: {input}\n  → 结果: {output}", r.tool);
        ui.add(
            egui::Button::new(
                RichText::new(text)
                    .size(11.0)
                    .color(if r.is_error {
                        Color32::from_rgb(232, 150, 158)
                    } else {
                        TXT_LIGHT
                    }),
            )
            .wrap_mode(egui::TextWrapMode::Truncate)
            .frame(true)
            .min_size(egui::vec2(0.0, 40.0)),
        );
        ui.add_space(2.0);
    }

    /// 单条历史会话（两行：目标 + 状态/时间/摘要）。点击返回 session id。
    fn history_row(&self, ui: &mut Ui, s: &qview_store::SessionMeta) -> Option<String> {
        let (title, sub) = self.history_row_text(s);
        let resp = ui.add(
            egui::Button::new(
                RichText::new(format!("{title}\n{sub}"))
                    .size(11.5)
                    .color(TXT_LIGHT),
            )
            .wrap_mode(egui::TextWrapMode::Truncate)
            .frame(true)
            .min_size(egui::vec2(0.0, 36.0)),
        );
        if resp.clicked() {
            Some(s.id.clone())
        } else {
            None
        }
    }

    fn history_row_text(&self, s: &qview_store::SessionMeta) -> (String, String) {
        let goal = if s.goal.trim().is_empty() {
            "(无目标)".into()
        } else {
            s.goal.chars().take(34).collect::<String>()
        };
        let mut sub = format!(
            "{}  {}  {}",
            self.history_status_icon(s.status),
            self.history_rel_time(s.finished_at_ms),
            s.provider
        );
        if !s.summary.trim().is_empty() {
            sub.push_str(&format!(
                "  {}",
                s.summary.chars().take(26).collect::<String>()
            ));
        }
        (goal, sub)
    }

    fn history_status_icon(&self, s: qview_store::StoreStatus) -> &'static str {
        match s {
            qview_store::StoreStatus::Success => "✅",
            qview_store::StoreStatus::Failed => "❌",
            qview_store::StoreStatus::Timeout => "⏱️",
            qview_store::StoreStatus::Cancelled => "🚫",
            qview_store::StoreStatus::Empty => "·",
        }
    }

    fn history_rel_time(&self, ms: u64) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let diff = now.saturating_sub(ms) / 1000;
        if diff < 60 {
            format!("{diff}s 前")
        } else if diff < 3600 {
            format!("{} 分钟前", diff / 60)
        } else if diff < 86400 {
            format!("{} 小时前", diff / 3600)
        } else {
            format!("{} 天前", diff / 86400)
        }
    }

    fn send(&self, text: &str) {
        // 日志也记录用户说的话（方便对照 AI 是怎么理解问题的）
        crate::log_info!("agent", "用户: {text}");
        // 发送后消息列表自动滚到底
        *self.app.agent_state.scroll_to_bottom.lock() = true;
        // 先记录用户消息（即使后续未真正启动 session 也可见）
        self.app.agent_state.transcript.lock().push(ChatMsg::User { text: text.to_string() });

        // Mock 未配置脚本/静态回复 → 只给提示，不空跑演示（避免 "(mock: no static text)"）
        let provider = self.app.config.agent.provider.provider;
        if provider == LlmProvider::Mock {
            let cfg = &self.app.config.agent.provider;
            if cfg.mock_script_path.is_none() && cfg.mock_static.is_none() {
                let mut t = self.app.agent_state.transcript.lock();
                t.push(ChatMsg::Note {
                    text: "ℹ 当前为 Mock（离线）模式，尚未配置演示内容。要接入真实 AI，请到「设置 → AI」选择 Provider 并填入 API Key；或在设置里配置 Mock 脚本。".into(),
                });
                return;
            }
        }

        // 未初始化提示
        let h = match self.app.agent_state.handle.lock().clone() {
            Some(h) => h,
            None => {
                let mut t = self.app.agent_state.transcript.lock();
                t.push(ChatMsg::Note {
                    text: "⚠ Agent 尚未初始化。请到「设置 → AI」检查 Provider 与 API Key 后重试。"
                        .into(),
                });
                return;
            }
        };
        let mut goal = qview_agent::handle::AgentGoal::new(text.to_string())
            .with_spec("qview", text, text);
        // 注入当前文档上下文（总是注入：有文件 → 告诉 AI document_id；
        // 没文件 → 明确禁止调文档工具，避免瞎猜 id 报 unknown_document）
        goal = goal.with_document_context(self.app.agent_doc_hint());
        // 附带当前文件 canonical path（会话历史落库 `file_id`）
        if let Some(p) = self.app.path.as_ref() {
            let canonical = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            goal = goal.with_document_path(canonical.display().to_string());
        }
        let active = self.app.agent_state.active_session.clone();
        let phase = self.app.agent_state.current_phase.clone();
        let conv = self.app.agent_state.conversation_id.clone();
        // 多轮对话：复用当前会话 id（None = 新会话），带上前几轮上下文供 LLM 延续。
        // 一次对话共用一个 session_id → 历史会话只记一条（之前是每条消息一个会话）。
        let conv_id = conv.lock().clone();
        let history = self.app.agent_conversation_history();
        self.app.spawn_tokio(async move {
            match h.start_session_with(goal, conv_id, history).await {
                Ok(sid) => {
                    // 只记会话 id 供下一轮复用；**不要**再设 active_session / phase——
                    // 活动状态完全由事件流维护（app.rs 事件处理器：SessionStarted →
                    // Some + Routing、SessionFinished/Cancelled/Failed → None + 终态）。
                    // 若在 Ok 分支再设 active=Some：Chat 短路（"你好"这类，start_session_with
                    // 内部**同步**广播 Started→Finished，Finished 已把 active 清成 None）会在
                    // 结束后又把 active 复活成 Some → 气泡一直出、发第二条被拦、
                    // stop 调 cancel 却因 runtime 从未注册过该会话而"未找到 session"反复刷。
                    *conv.lock() = Some(sid);
                }
                Err(e) => {
                    crate::log_error!("agent", "start_session 失败: {e}");
                    // SessionStarted 已广播（app.rs 事件处理器已把会话标为活动），
                    // 失败要复位，否则「正在思考…」会永远挂在那里。
                    *active.lock() = None;
                    *phase.lock() = Phase::Failed;
                }
            }
        });
    }
}

/// 固定宽度气泡容器。
///
/// 背景（重要）：`ui.with_layout(水平)` 下 egui 的 `wrap_mode` 是 `Extend`
/// （水平布局默认不换行），而 `ui.set_max_width` 在垂直布局里又是 no-op
/// （`available_width` 不会被收紧）——两者叠加导致气泡里的长文本**按自然宽度
/// 渲染、把窗口撑爆**（曾实测 2964px）。`Label::wrap()` 只强制换行模式，但
/// 换行宽度仍取 available_width（在水平行里是无限宽），所以也必须配合固定宽。
///
/// 做法：固定 `max_w` 宽（不做"贴合自然宽"——那样短文本如"你好"会窄到几个字
/// 就换行，用户反馈不对）。短文本在 max_w 内一行显示，长文本在 max_w 内换行。
/// `right=true` 时气泡右对齐（用户消息），`false` 左对齐（器灵消息）。
/// 气泡文本 galley（wrap 宽度 = 气泡最大内宽）。
///
/// 独立出来既给 [`bubble_fixed`] 用，也让测试能直接断言「首行不缩进」。
fn bubble_job(ui: &egui::Ui, text: &str, wrap_w: f32, color: Color32) -> egui::text::LayoutJob {
    let font = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Body)
        .cloned()
        .unwrap_or(egui::FontId::proportional(14.0));
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font, color, wrap_w);
    // `simple` 的 halign 默认 LEFT；显式写死，杜绝任何布局继承把文字带歪。
    job.halign = egui::Align::LEFT;
    job
}

fn bubble_fixed(
    ui: &mut Ui,
    max_w: f32,
    right: bool,
    text: &str,
    color: Color32,
    frame: egui::Frame,
) -> egui::Rect {
    // 全手动布局（不再用 Frame-in-RTL-scope 那套）：
    //
    // 背景：右对齐（用户气泡）需要气泡**贴右缘**；之前用 `right_to_left` scope 包
    // Frame，内部再用 `ui.horizontal` 放 Label —— 但 egui 的 `horizontal` 会继承父
    // 布局方向（`prefer_right_to_left()`），Frame 内实际是 RTL，文字右对齐
    // （halign=Max）：短的首行比续行更靠右，看起来像"首行缩进"（用户实测）。
    // `with_layout(LTR)` 又能强制 LTR，但 child 吃满可用宽，短文本气泡不再收缩
    // （"你好"撑到整宽）。所以这里：手工构造 galley → 按内容尺寸 allocate 精确
    // 矩形 → 画 Frame 形状 + 画文本。没有布局继承，文字永远 LTR 左对齐。
    let margin = frame.inner_margin;
    let ml = margin.left as f32;
    let mr = margin.right as f32;
    let mt = margin.top as f32;
    let mb = margin.bottom as f32;
    let stroke_w = frame.stroke.width;
    let h_margin = ml + mr;
    let v_margin = mt + mb;
    let wrap_w = (max_w - h_margin - 2.0 * stroke_w).max(20.0);
    // 排版**一次**：尺寸计算与文本渲染共用同一个 galley。绝不能把 LayoutJob 再交给
    // Label 重排——`WidgetText::LayoutJob` 会用子 Ui 的 wrap_mode / available_width
    // **覆盖** job.wrap（子 Ui 是不换行的 left_to_right → 文本单行溢出气泡）。
    let galley = ui.fonts(|f| f.layout_job(bubble_job(ui, text, wrap_w, color)));

    // 气泡外框：短文本贴合内容宽，长文本封顶 max_w（内容已按 wrap_w 换行）。
    let widget_w = (galley.size().x + h_margin + 2.0 * stroke_w).min(max_w);
    let widget_h = galley.size().y + v_margin + 2.0 * stroke_w;

    let origin = if right {
        egui::pos2(ui.cursor().max.x - widget_w, ui.cursor().min.y)
    } else {
        ui.cursor().min
    };
    let widget_rect = egui::Rect::from_min_size(origin, egui::vec2(widget_w, widget_h));
    ui.allocate_rect(widget_rect, egui::Sense::hover());

    // `Frame::paint` 会把传入的 content_rect 再外扩「内边距 + 描边」得到外框，
    // 所以要传**内容矩形**（外框向内缩边距+描边），paint 出来的形状才正好盖住
    // widget_rect。文本画在内容矩形左上角，左对齐。
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(
            widget_rect.min.x + ml + stroke_w,
            widget_rect.min.y + mt + stroke_w,
        ),
        egui::pos2(
            widget_rect.max.x - mr - stroke_w,
            widget_rect.max.y - mb - stroke_w,
        ),
    );
    ui.painter().add(frame.paint(content_rect));
    // 文本用**可选择的 Label** 渲染（painter().galley 是纯绘制，无法拖选 / Ctrl+C 复制）。
    // new_child 精确定位到内容矩形且不扰动父布局；job.halign=LEFT 保持文字左对齐。
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::TOP)),
    );
    // 传**已排好的 galley**（WidgetText::Galley 原样返回，不重排），换行与气泡宽度一致。
    child.add(egui::Label::new(egui::WidgetText::Galley(galley)).selectable(true));

    widget_rect
}

/// 聊天头像：28px 圆 + 一个字符（AI=☯ / 用户=我）。放在气泡外侧。
fn avatar(ui: &mut Ui, ch: &str, bg: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 14.0, bg);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        ch,
        egui::FontId::proportional(13.0),
        Color32::WHITE,
    );
}

/// 过滤器的人类可读描述。
fn filter_desc(f: &FilterSpec) -> String {
    match f {
        FilterSpec::Literal { pattern, case_sensitive } => format!(
            "字面量 \"{pattern}\"{}",
            if *case_sensitive { " (敏感)" } else { "" }
        ),
        FilterSpec::ErrorLevel { min, max } => format!("错误码 {min}–{max}"),
        FilterSpec::Contains { needle } => format!("包含 \"{needle}\""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    const WIN_W: f32 = 380.0;
    const WIN_H: f32 = 533.0;

    /// 复现 show_window 的结构：Area(movable) 内 allocate_exact_size(380x533) 画背景，
    /// 再 scope_builder(max_rect=rect) 放真实结构的 header / 气泡 / 工具行 / 输入栏。
    /// 断言：内容布局宽度不超出背景宽度。
    /// 用途：排查"内容宽度超出窗口背景"——若失败说明 scope 没真正约束子内容。
    /// 验证正确的固定宽气泡模式：固定宽 rect + top_down 布局 + `Label::wrap()`。
    /// 若宽 ≤ 380，说明该模式可靠换行、不溢出 → 应用到 message_row。
    #[test]
    fn bubble_fixed_width_wraps() {
        let long = "这是一条非常长的 AI 回复，没有任何空格".repeat(20);
        let mut bubble_w = 0.0f32;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // 用户气泡（右对齐）：先留白，再放固定宽气泡盒
                    ui.horizontal(|ui| {
                        let right_space = (ui.available_width() - 290.0).max(0.0);
                        ui.add_space(right_space);
                        let rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(290.0, ui.available_height()),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(12, 8))
                                    .show(ui, |ui| {
                                        ui.add(egui::Label::new(long.clone()).wrap());
                                    });
                                bubble_w = ui.min_rect().width();
                            },
                        );
                    });
                });
            },
        );
        eprintln!("气泡盒实测宽 = {bubble_w}（目标 290，换行后应 ≤ 290）");
        assert!(
            bubble_w <= 300.0,
            "气泡盒 {bubble_w} 超出 290，说明未按固定宽换行"
        );
    }

    /// 用户气泡必须**贴右缘**（此前固定宽 rect + top_down 让它停在窗口左中间）；
    /// 短文本窄气泡靠右、长文本封顶 max_w 换行。
    #[test]
    fn user_bubble_is_flush_right() {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(380.0, 533.0));
        let frame = || {
            egui::Frame::new()
                .fill(Color32::from_rgb(46, 82, 148))
                .inner_margin(egui::Margin::symmetric(12, 8))
        };
        let mut short = egui::Rect::ZERO;
        let mut long = egui::Rect::ZERO;
        let mut container_right = 0.0f32;
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    container_right = ui.max_rect().max.x;
                    short = bubble_fixed(ui, 290.0, true, "你好", Color32::WHITE, frame());
                    long = bubble_fixed(
                        ui,
                        310.0,
                        true,
                        &"这是一条非常长的用户消息，没有任何空格".repeat(8),
                        Color32::WHITE,
                        frame(),
                    );
                });
            },
        );
        eprintln!(
            "短文本气泡 rect={short:?}（宽 {:.0}，右缘 {:.0}）  长文本 rect={long:?}（宽 {:.0}，右缘 {:.0}）  容器右缘 {container_right}",
            short.width(), short.max.x, long.width(), long.max.x
        );
        // 贴右缘（允许 1px 误差）
        assert!(
            (short.max.x - container_right).abs() < 1.5,
            "短文本气泡未贴右缘: 右缘 {} vs 容器右缘 {}",
            short.max.x,
            container_right
        );
        assert!(
            (long.max.x - container_right).abs() < 1.5,
            "长文本气泡未贴右缘: 右缘 {} vs 容器右缘 {}",
            long.max.x,
            container_right
        );
        // 短文本窄气泡（贴合文本），长文本封顶换行
        assert!(short.width() < 200.0, "短文本气泡应窄: {}", short.width());
        assert!(long.width() <= 340.0, "长文本气泡应封顶: {}", long.width());
    }

    /// 回归：气泡文本**首行不缩进**（首行第一个字与续行左对齐）。
    ///
    /// 背景：旧实现 `bubble_fixed(right=true)` 用 `right_to_left` scope 包 Frame，
    /// Frame 内部 `ui.horizontal` 继承 RTL → 文字右对齐（halign=Max），短的首行比续行
    /// 更靠右，看起来像"首行缩进"（用户实测）。`bubble_galley` 直接用
    /// `LayoutJob::simple(…, Align::LEFT)`，不经任何布局继承。
    ///
    /// 断言：wrap 出多行时，所有行**galley 内 x 起点相同**（同一条左对齐线），halign=Min。
    #[test]
    fn bubble_text_first_line_flush_left() {
        let text = "你能看到我这个目录有啥文件嘛？D:\\qinwh\\code\\qview\\target\\release";
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(456.0, 646.0));
        let ctx = egui::Context::default();
        let mut row_starts: Vec<f32> = Vec::new();
        let mut halign = egui::Align::Max;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // 最大气泡宽 300 → 内边距 12×2 → wrap 宽 276（与真实调用一致）
                    let job = bubble_job(ui, text, 276.0, Color32::WHITE);
                    let galley = ui.fonts(|f| f.layout_job(job));
                    halign = galley.job.halign;
                    for row in galley.rows.iter() {
                        row_starts.push(row.rect.min.x);
                    }
                });
            },
        );
        assert!(
            row_starts.len() >= 2,
            "长文本应 wrap 出至少 2 行，实际 {} 行",
            row_starts.len()
        );
        eprintln!("行起点 = {row_starts:?}");
        assert_eq!(halign, egui::Align::Min, "文字必须左对齐（halign=Min）");
        let first = row_starts[0];
        for (i, x) in row_starts.iter().enumerate() {
            assert!(
                (x - first).abs() < 0.5,
                "第 {} 行起点 {x} 与首行 {first} 不一致（首行缩进 bug）",
                i
            );
        }
    }

    #[test]
    fn area_content_stays_within_background() {
        // 注意：不能 `egui::__run_test_ui` —— 它用 `FontDefinitions::empty()`，
        // 文本宽度会测成 0，超长文本不触发换行，测试会假通过。
        // 用 `Context::default()` + 手动 run（默认字体，真实测量）。
        let mut bg_w = 0.0f32;
        let mut content_w = 0.0f32;
        let mut content_h = 0.0f32;
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 860.0));
        let ctx = egui::Context::default();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ctx| {
                egui::Area::new(egui::Id::new("test_agent_area"))
                    .order(egui::Order::Foreground)
                    .movable(true)
                    .default_pos(egui::pos2(100.0, 100.0))
                    .show(ctx, |ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(WIN_W, WIN_H), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 12.0, Color32::DARK_GRAY);

                    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                        ui.set_max_width(WIN_W);
                        let report = |label: &str, ui: &egui::Ui, w: &mut f32| {
                            let mw = ui.min_rect().width();
                            if mw > *w {
                                *w = mw;
                                eprintln!("  [新增后] min_rect 宽 = {mw:.1}  ← {label}");
                            }
                        };
                        // 模拟 header（左标签 + 右侧按钮），结构与真实 header() 相同
                        let (hrect, _) =
                            ui.allocate_exact_size(egui::vec2(WIN_W, 46.0), egui::Sense::hover());
                        let b = egui::UiBuilder::new()
                            .max_rect(hrect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center));
                        ui.scope_builder(b, |ui| {
                            ui.add_space(6.0);
                            ui.label("☯ 器灵小Q");
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                ui.add(egui::Button::new("x").frame(false));
                                ui.add(egui::Button::new("清空").frame(false));
                                ui.add(egui::Button::new("■ 停止"));
                            });
                        });
                        report("header", ui, &mut content_w);
                        ui.separator();
                        // 模拟消息区（与真实 show_window 一致：限制高度，预留输入栏）
                        let input_reserve = 52.0;
                        let msg_h = (ui.available_height() - input_reserve).max(100.0);
                        let msg_rect = egui::Rect::from_min_size(
                            ui.cursor().min,
                            egui::vec2(ui.available_width(), msg_h),
                        );
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .max_rect(msg_rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                            |ui| {
                                // 用户气泡（右对齐封顶 290）
                                let f = egui::Frame::new()
                                    .fill(Color32::from_rgb(46, 82, 148))
                                    .inner_margin(egui::Margin::symmetric(12, 8));
                                bubble_fixed(
                                    ui,
                                    290.0,
                                    true,
                                    "你好，这是一条用户消息",
                                    Color32::WHITE,
                                    f,
                                );
                                report("scroll+用户气泡", ui, &mut content_w);
                                // 器灵气泡（左对齐封顶 310，长文本必须换行）
                                let f = egui::Frame::new()
                                    .fill(Color32::from_rgb(22, 28, 44))
                                    .inner_margin(egui::Margin::symmetric(12, 8));
                                bubble_fixed(
                                    ui,
                                    310.0,
                                    false,
                                    &"这是一条非常长的 AI 回复，没有任何空格".repeat(20),
                                    TXT_LIGHT,
                                    f,
                                );
                                report("scroll+器灵气泡", ui, &mut content_w);
                                // 工具行（monospace + 长 detail，垂直布局下自动换行）
                                ui.label(
                                    egui::RichText::new(format!(
                                        "  ⚙ search_text {}",
                                        "detail".repeat(40)
                                    ))
                                    .monospace()
                                    .size(10.5),
                                );
                                report("scroll+工具行", ui, &mut content_w);
                            },
                        );
                        ui.separator();
                        // 模拟输入栏（input_bar）
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            let mut s = String::new();
                            ui.add_sized(
                                [ui.available_width() - 72.0, 32.0],
                                egui::TextEdit::singleline(&mut s),
                            );
                            ui.add(egui::Button::new("发送"));
                        });
                        report("输入栏", ui, &mut content_w);
                        content_h = ui.min_rect().height();
                    });
                    bg_w = rect.width();
                });
        });
        assert!(
            content_w <= bg_w + 1.0,
            "内容布局宽 {content_w} 超出背景宽 {bg_w}"
        );
        assert!(
            content_h <= WIN_H + 1.0,
            "内容高度 {content_h} 超出窗口高 {WIN_H}（输入栏会被顶出窗口）"
        );
    }
}
