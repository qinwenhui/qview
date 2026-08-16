//! Dialog dispatch — render all modal windows conditionally.

use egui::Context;

use crate::{log_info};
use crate::app::QLogApp;

/// 居中但可拖拽的弹窗：`Window::anchor` 会让窗口不可移动，
/// 改用 `default_pos` 仅设初始居中位置（拖拽后 egui 记住新位置）。
pub(crate) fn centered_window(ctx: &Context, title: impl Into<egui::WidgetText>, size: [f32; 2]) -> egui::Window<'_> {
    let size = egui::vec2(size[0], size[1]);
    let pos = ctx.screen_rect().center() - size * 0.5;
    egui::Window::new(title).default_pos(pos)
}

mod about;
mod annotation;
mod donate;
mod edit;
mod help;
mod history;
mod index_manager;
mod settings;
mod shortcuts;

pub use about::render_about;
pub use annotation::{render_annotation_dialog, render_annotation_list};
pub use donate::render_donate;
pub use edit::render_discard_confirm;
pub use help::render_help;
pub use history::render_history;
pub use index_manager::render_index_manager;
pub use settings::render_settings;
pub use shortcuts::render_shortcuts;

/// Render every dialog whose `show_*` flag is true.
pub fn render_all(ctx: &Context, app: &mut QLogApp) {
    if app.show_about {
        render_about(ctx, app);
    }
    if app.show_donate {
        render_donate(ctx, app);
    }
    if app.show_help {
        render_help(ctx, app);
    }
    if app.show_shortcuts {
        render_shortcuts(ctx, app);
    }
    if app.show_settings {
        render_settings(ctx, app);
    }
    if app.show_file_properties {
        render_file_properties(ctx, app);
    }
    if app.show_index_manager {
        render_index_manager(ctx, app);
    }
    if app.show_encoding_confirm {
        render_encoding_confirm(ctx, app);
    }
    if app.show_annotation_dialog {
        render_annotation_dialog(ctx, app);
    }
    if app.show_annotation_list {
        render_annotation_list(ctx, app);
    }
    if app.show_history {
        render_history(ctx, app);
    }
    if app.pending_discard.is_some() {
        render_discard_confirm(ctx, app);
    }
}

// ---------------------------------------------------------------------------
// File properties (inline — small enough not to merit its own file)
// ---------------------------------------------------------------------------

fn render_file_properties(ctx: &Context, app: &mut QLogApp) {
    centered_window(ctx, "文件属性", [520.0, 380.0])
        .fixed_size([520.0, 380.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("文件属性")
                    .size(18.0)
                    .strong()
                    .color(egui::Color32::from_rgb(191, 201, 214)),
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            if let Some(arc) = app.engine.as_ref() {
                let engine = arc.lock();
                let path = app.path.as_ref();
                let file_name = path
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("-");
                let full_path = path.map(|p| p.display().to_string()).unwrap_or_default();
                let size = engine.mmap.size();
                let lines = engine.effective_line_count();
                let indexed = if engine.index.is_complete() {
                    "已索引"
                } else {
                    "索引中..."
                };

                // Compute the index cache path.
                let index_path = app.config.engine.index_path(
                    path.unwrap_or(&std::path::PathBuf::new()),
                );
                let index_info = if index_path.exists() {
                    if let Ok(meta) = std::fs::metadata(&index_path) {
                        format!(
                            "{} ({})",
                            index_path.display(),
                            crate::viewer::human_bytes(meta.len()),
                        )
                    } else {
                        format!("{}", index_path.display())
                    }
                } else if app.config.engine.index_cache_enabled {
                    format!("{} (尚未创建)", index_path.display())
                } else {
                    "（索引缓存已禁用）".to_string()
                };

                let props: Vec<(&str, String)> = vec![
                    ("文件名", file_name.to_string()),
                    ("路径", full_path),
                    ("大小", crate::viewer::human_bytes(size)),
                    ("行数", format!("{}", lines)),
                    ("编码", "UTF-8".to_string()),
                    ("索引状态", indexed.to_string()),
                    ("索引文件", index_info),
                ];

                for (label, value) in &props {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{}:", label))
                                .size(13.0)
                                .strong()
                                .color(egui::Color32::from_rgb(160, 170, 185)),
                        );
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(value)
                                .size(13.0)
                                .color(egui::Color32::from_gray(200)),
                        );
                    });
                    ui.add_space(4.0);
                }
            } else {
                ui.label(
                    egui::RichText::new("未打开文件")
                        .size(13.0)
                        .color(egui::Color32::from_gray(140)),
                );
            }

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("确定").color(egui::Color32::WHITE).size(14.0),
                        )
                        .fill(egui::Color32::from_rgb(33, 115, 237))
                        .min_size(egui::vec2(100.0, 30.0)),
                    )
                    .clicked()
                {
                    app.show_file_properties = false;
                }
            });
        });
}

// ---------------------------------------------------------------------------
// Encoding switch confirmation
// ---------------------------------------------------------------------------

fn render_encoding_confirm(ctx: &Context, app: &mut QLogApp) {
    let pending = app.pending_encoding.clone();
    let current = app.config.engine.encoding.clone();

    // Find display labels.
    let current_label = crate::app::ENCODINGS
        .iter()
        .find(|(k, _)| *k == current.as_str())
        .map(|(_, v)| *v)
        .unwrap_or(current.as_str());
    let pending_label = crate::app::ENCODINGS
        .iter()
        .find(|(k, _)| *k == pending.as_str())
        .map(|(_, v)| *v)
        .unwrap_or(pending.as_str());

    centered_window(ctx, "切换编码", [400.0, 180.0])
        .fixed_size([400.0, 180.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(18.0);

                ui.label(
                    egui::RichText::new("切换文本编码")
                        .size(17.0)
                        .strong()
                        .color(egui::Color32::from_rgb(210, 218, 230)),
                );

                ui.add_space(12.0);

                ui.label(
                    egui::RichText::new(format!(
                        "将编码从 {} 切换到 {}？\n切换后将重新加载文件，未保存的编辑将丢失。",
                        current_label, pending_label,
                    ))
                    .size(13.0)
                    .color(egui::Color32::from_gray(185)),
                );

                ui.add_space(16.0);

                ui.horizontal(|ui| {
                    ui.add_space(60.0);

                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("切换并重新加载")
                                    .color(egui::Color32::WHITE)
                                    .size(14.0),
                            )
                            .fill(egui::Color32::from_rgb(15, 157, 89))
                            .min_size(egui::vec2(130.0, 30.0)),
                        )
                        .clicked()
                    {
                        log_info!("dialogs", "确认切换编码: {} → {}", current, pending);
                        app.config.engine.encoding = pending.clone();
                        app.save_config();
                        app.show_encoding_confirm = false;
                        app.pending_encoding.clear();
                        if let Some(ref path) = app.path.clone() {
                            app.open_file(path.clone());
                        }
                    }

                    ui.add_space(12.0);

                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("取消")
                                    .color(egui::Color32::WHITE)
                                    .size(14.0),
                            )
                            .fill(egui::Color32::from_rgb(83, 91, 105))
                            .min_size(egui::vec2(80.0, 30.0)),
                        )
                        .clicked()
                    {
                        log_info!("dialogs", "取消编码切换");
                        app.show_encoding_confirm = false;
                        app.pending_encoding.clear();
                    }

                    ui.add_space(60.0);
                });
            });
        });
}
