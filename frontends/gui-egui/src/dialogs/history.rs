//! 历史会话对话框（B2）：列出最近会话，点击打开回看（填回 transcript）。
//!
//! - 打开 / 刷新 → `app.request_history_reload()` 后台拉取 `recent_sessions`。
//! - 点击某条 → `app.open_history_session(id)` 后台加载并映射回聊天气泡。
//! - 只读回看，不重放会话；数据来自 `qview-store`（redb）。

use egui::{Color32, RichText, ScrollArea, Ui};

use qview_store::StoreStatus;

use crate::app::QLogApp;

/// 渲染历史会话窗口。
pub fn render_history(ctx: &egui::Context, app: &mut QLogApp) {
    let mut is_open = true;
    egui::Window::new("历史会话")
        .open(&mut is_open)
        .default_size([460.0, 500.0])
        .resizable(true)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("最近 50 条 AI 会话").color(Color32::from_gray(150)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("刷新").clicked() {
                        app.request_history_reload();
                    }
                });
            });
            ui.separator();

            let loading = app.history_sessions.lock().is_none();
            let list = app.history_sessions.lock().clone().unwrap_or_default();

            if loading && list.is_empty() {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("加载中…").color(Color32::from_gray(130)));
                });
            } else if list.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("暂无历史会话。和器灵聊过之后再来看看。")
                            .color(Color32::from_gray(130)),
                    );
                });
            } else {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut clicked: Option<String> = None;
                        for (i, s) in list.iter().enumerate() {
                            session_row(ui, i, s, &mut clicked);
                        }
                        if let Some(sid) = clicked {
                            app.open_history_session(&sid);
                        }
                    });
            }
        });
    if !is_open {
        app.show_history = false;
    }
}

/// 单条会话（两行：目标 + 状态/时间/摘要）。
fn session_row(ui: &mut Ui, _idx: usize, s: &qview_store::SessionMeta, clicked: &mut Option<String>) {
    let (title, sub) = row_text(s);
    let resp = ui.add(
        egui::Button::new(
            RichText::new(format!("{title}\n{sub}"))
                .color(Color32::from_gray(190))
                .size(12.5),
        )
        .wrap_mode(egui::TextWrapMode::Truncate)
        .frame(true)
        .min_size(egui::vec2(0.0, 42.0)),
    );
    if resp.clicked() {
        *clicked = Some(s.id.clone());
    }
}

fn row_text(s: &qview_store::SessionMeta) -> (String, String) {
    let goal = if s.goal.trim().is_empty() { "(无目标)".into() } else { s.goal.chars().take(48).collect::<String>() };
    let mut sub = format!("{}  {}  ·  {}",
        status_icon(s.status),
        rel_time(s.finished_at_ms),
        s.provider);
    if !s.summary.trim().is_empty() {
        sub.push_str(&format!("  {}", s.summary.chars().take(40).collect::<String>()));
    }
    (goal, sub)
}

fn status_icon(s: StoreStatus) -> &'static str {
    match s {
        StoreStatus::Success => "✅",
        StoreStatus::Failed => "❌",
        StoreStatus::Timeout => "⏱️",
        StoreStatus::Cancelled => "🚫",
        StoreStatus::Empty => "·",
    }
}

fn rel_time(ms: u64) -> String {
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
