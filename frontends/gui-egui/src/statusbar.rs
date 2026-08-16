//! Status bar — bottom strip modelled after IDE status bars (VS Code /
//! IntelliJ).  Three fixed-width zones prevent text overlap.
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────┐
//! │ putty7.log …         已打开 · 即时加载           GBK │ 758行 │ 70KiB │
//! └──────────────────────────────────────────────────────────────────┘
//! ```
//!
//! A progress bar replaces the centre zone when indexing or searching.

use egui::{Align, Color32, Context, Layout};

use crate::log_debug;
use crate::app::QLogApp;
use crate::viewer;

/// 返回 `s` 的**尾部最多 `n` 字节**，但绝不在 UTF-8 字符中间切断。
///
/// 直接用 `&s[s.len()-n..]` 时，若 `n` 落在多字节字符（如中文路径里的
/// 汉字）中间，会 panic（"byte index is not a char boundary"→ 闪退）。
/// 这里从目标字节位置**向前回退**到最近的字符边界：UTF-8 单字符最长 4 字节，
/// 所以最坏只回退 3 字节，代价可忽略。
fn tail_boundary(s: &str, n: usize) -> &str {
    let target = s.len().saturating_sub(n);
    let start = (0..=target)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0);
    &s[start..]
}

/// Render the bottom status bar.
pub fn render_status_bar(ctx: &Context, app: &mut QLogApp) {
    egui::TopBottomPanel::bottom("status_bar")
        .min_height(23.0)
        .show_separator_line(true)
        .show(ctx, |ui| {
            // ---- progress bar row (when loading) ----
            let loading = app.loading_state();
            if let Some((frac, label)) = loading {
                let is_indexing = app
                    .engine
                    .as_ref()
                    .map(|arc| arc.lock().index_progress.is_some())
                    .unwrap_or(false);
                let color = if is_indexing {
                    Color32::from_rgb(23, 172, 199)
                } else {
                    Color32::from_rgb(121, 178, 106)
                };
                ui.horizontal(|ui| {
                    ui.set_height(16.0);
                    let pb = egui::ProgressBar::new(frac)
                        .text(label)
                        .fill(color);
                    ui.add(pb);
                    if is_indexing {
                        let stop_btn = egui::Button::new("取消")
                            .fill(Color32::from_rgb(221, 116, 129))
                            .small();
                        if ui.add(stop_btn).clicked() {
                            crate::log_debug!("statusbar", "取消索引");
                            if let Some(arc) = app.engine.as_mut() {
                                arc.lock().cancel_index();
                            }
                        }
                    }
                });
            }

            // ---- main status row ----
            ui.horizontal(|ui| {
                ui.set_height(20.0);
                ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

                let w = ui.available_width();
                // Fixed zone widths so nothing overlaps.
                let left_w = (w * 0.38).min(420.0); // file path
                let right_w = 310.0; // annotations | encoding | lines | size
                let center_w = (w - left_w - right_w).max(100.0);

                // ---- Left zone: file path ----
                ui.allocate_ui_with_layout(
                    egui::vec2(left_w, 20.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_height(20.0);
                        if app.is_new_file {
                            ui.label(
                                egui::RichText::new("未命名 · 新文件（未保存）")
                                    .color(Color32::from_rgb(221, 116, 129))
                                    .size(11.5),
                            );
                        } else if let Some(ref p) = app.path {
                            let path_str = p.display().to_string();
                            let display = if path_str.len() > 50 {
                                format!("…{}", tail_boundary(&path_str, 49))
                            } else {
                                path_str
                            };
                            ui.label(
                                egui::RichText::new(display)
                                    .color(Color32::from_gray(174))
                                    .size(11.5),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("未打开文件")
                                    .color(Color32::from_gray(140))
                                    .size(11.5),
                            );
                        }
                    },
                );

                // ---- Centre zone: status / search / progress ----
                ui.allocate_ui_with_layout(
                    egui::vec2(center_w, 20.0),
                    Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| {
                        ui.set_height(20.0);
                        if !app.status_msg.is_empty() {
                            ui.label(
                                egui::RichText::new(&app.status_msg)
                                    .color(Color32::from_rgb(121, 210, 130))
                                    .size(11.5),
                            );
                        } else if !app.search_status.is_empty()
                            && !app.search_query.is_empty()
                        {
                            ui.label(
                                egui::RichText::new(&app.search_status)
                                    .color(Color32::from_gray(177))
                                    .size(11.5),
                            );
                        }
                    },
                );

                // ---- Right zone: encoding | lines | size ----
                ui.allocate_ui_with_layout(
                    egui::vec2(right_w, 20.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
                        ui.set_height(20.0);
                        ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);

                        if let Some(arc) = app.engine.as_ref() {
                            let engine = arc.lock();
                            // size
                            ui.label(
                                egui::RichText::new(viewer::human_bytes(
                                    engine.mmap.size(),
                                ))
                                .color(Color32::from_gray(160))
                                .size(11.5),
                            );
                            dim_sep(ui);

                            // line count
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} 行",
                                    engine.effective_line_count()
                                ))
                                .color(Color32::from_gray(160))
                                .size(11.5),
                            );
                            dim_sep(ui);

                            // 释放 engine guard：下方 encoding popup 需要 &mut app
                            drop(engine);

                            // encoding — clickable label that shows a popup with encoding list
                            let enc_text = egui::RichText::new(
                                app.config.engine.encoding.as_str(),
                            )
                            .color(Color32::from_rgb(160, 195, 225))
                            .size(11.5);
                            let enc_resp = ui.selectable_label(false, enc_text);
                            let popup_id = ui.make_persistent_id("encoding_popup");
                            if enc_resp.clicked() {
                                log_debug!("statusbar", "点击编码标签, 当前={}", app.config.engine.encoding);
                                ui.memory_mut(|m| m.toggle_popup(popup_id));
                            }
                            let _popup_open = ui.memory(|m| m.is_popup_open(popup_id));
                            egui::popup::popup_above_or_below_widget(
                                ui, popup_id, &enc_resp,
                                egui::AboveOrBelow::Above,
                                egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                                |ui: &mut egui::Ui| {
                                    ui.set_min_width(200.0);
                                    render_encoding_popup(ui, app);
                                },
                            );
                            dim_sep(ui);

                            // annotations — clickable label → list panel
                            let ann_count = app.annotations.len();
                            let ann_text = egui::RichText::new(format!("📌 批注({})", ann_count))
                                .color(Color32::from_rgb(224, 172, 56))
                                .size(11.5);
                            if ui.selectable_label(false, ann_text).clicked() {
                                log_debug!("statusbar", "点击批注入口, 当前文件 {} 条", ann_count);
                                app.show_annotation_list = true;
                            }
                        }
                    },
                );
            });
        });
}

/// A subtle vertical separator for the right-zone stats.
fn dim_sep(ui: &mut egui::Ui) {
    ui.add_space(3.0);
    ui.label(
        egui::RichText::new("│")
            .color(Color32::from_gray(80))
            .size(10.0),
    );
    ui.add_space(3.0);
}

/// Render the encoding-switch popup.
fn render_encoding_popup(ui: &mut egui::Ui, app: &mut QLogApp) {
    let current = app.config.engine.encoding.clone();
    for (key, label) in crate::app::ENCODINGS {
        let selected = *key == current.as_str();
        if ui.selectable_label(selected, *label).clicked() {
            if *key != current.as_str() {
                log_debug!("statusbar", "选择编码: {} → {}", current, key);
                app.pending_encoding = key.to_string();
                app.show_encoding_confirm = true;
            }
            ui.memory_mut(|m| m.close_popup());
        }
    }
}
