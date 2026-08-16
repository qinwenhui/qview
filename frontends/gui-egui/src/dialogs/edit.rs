//! Edit-related dialogs: the "有未保存修改" discard-confirm window.

use egui::{Align, Color32, Context, Layout};

use crate::app::DiscardAction;
use crate::log_info;
use crate::app::QLogApp;

/// Confirm before dropping unsaved edits (closing / opening another / exiting).
pub fn render_discard_confirm(ctx: &Context, app: &mut QLogApp) {
    if app.pending_discard.is_none() {
        return;
    }
    let action_label = match app.pending_discard {
        Some(DiscardAction::Open(_)) => "打开其他文件",
        Some(DiscardAction::New) => "新建文件",
        Some(DiscardAction::Close) => "关闭当前文件",
        Some(DiscardAction::Exit) => "退出程序",
        None => "",
    };

    let mut discard = false;
    let mut cancel = false;
    crate::dialogs::centered_window(ctx, "未保存的修改", [400.0, 220.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(360.0);
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "当前文件有未保存的修改。\n确定要「{action_label}」并丢弃这些修改吗？"
                ))
                .color(Color32::from_gray(190))
                .size(13.5),
            );
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("保存修改请先点「取消」，再在工具栏点「保存」。")
                    .color(Color32::from_gray(140))
                    .size(11.5),
            );
            ui.add_space(14.0);
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                let d_btn = egui::Button::new(
                    egui::RichText::new("放弃修改并继续")
                        .color(Color32::WHITE)
                        .size(13.5),
                )
                .fill(Color32::from_rgb(218, 67, 74))
                .min_size(egui::vec2(130.0, 30.0));
                if ui.add(d_btn).clicked() {
                    discard = true;
                }
                ui.add_space(10.0);
                let c_btn = egui::Button::new(
                    egui::RichText::new("取消")
                        .color(Color32::WHITE)
                        .size(13.5),
                )
                .fill(Color32::from_rgb(83, 91, 105))
                .min_size(egui::vec2(80.0, 30.0));
                if ui.add(c_btn).clicked() {
                    cancel = true;
                }
            });
        });

    if discard {
        log_info!("dialogs", "确认丢弃未保存修改");
        app.confirm_discard();
    }
    if cancel {
        app.pending_discard = None;
    }
}
