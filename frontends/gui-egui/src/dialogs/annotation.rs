//! Annotation dialogs: the add/edit input window and the current file's
//! annotation list panel (status-bar 📌 entry point).

use egui::{Align, Color32, Context, Layout};

use crate::log_info;
use crate::app::QLogApp;

/// 把窗口 / 浮层区域内的滚轮事件就地吃掉，防止泄漏到底层主 viewer 触发误滚动。
///
/// egui 0.31 的 `ScrollArea` 只在 `state.offset` 还能继续滚的方向上消费滚轮
/// （`if scrolling_up || scrolling_down` 块里把 `smooth_scroll_delta` 清零）；
/// 当内容已到顶/底时滚轮会"穿透" ScrollArea 流到下一层。本函数把窗口/浮层
/// 范围内的滚轮统一清零，覆盖"已到顶/底"的边缘 case。调用方负责保证 `rect`
/// 与实际窗口/浮层可视区域一致。
fn consume_scroll_within(ctx: &Context, rect: egui::Rect) {
    if ctx.input(|i| {
        i.pointer
            .hover_pos()
            .is_some_and(|p| rect.contains(p))
    }) {
        ctx.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }
}

/// Truncate `s` to `max_chars` characters, appending `…` when cut.
fn clamp_preview(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

/// Add/edit annotation input window.  `annotation_edit_id == None` → new
/// annotation from the current selection; `Some(id)` → edit an existing note.
pub fn render_annotation_dialog(ctx: &Context, app: &mut QLogApp) {
    let editing = app.annotation_edit_id.is_some();
    let title = if editing { "编辑批注" } else { "添加批注" };
    let mut open = app.show_annotation_dialog;
    let mut confirm = false;
    let mut cancel = false;

    let resp = crate::dialogs::centered_window(ctx, title, [420.0, 300.0])
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.set_width(380.0);

            if !editing {
                // Preview the selected content (read-only).
                let preview = app.copy_selection_text().unwrap_or_default();
                if !preview.is_empty() {
                    ui.label(
                        egui::RichText::new("选中内容:")
                            .strong()
                            .color(Color32::from_gray(174))
                            .size(12.0),
                    );
                    egui::ScrollArea::vertical()
                        .max_height(110.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(clamp_preview(&preview, 500))
                                        .monospace()
                                        .color(Color32::from_rgb(170, 185, 200)),
                                )
                                .wrap(),
                            );
                        });
                    ui.add_space(6.0);
                    ui.separator();
                } else {
                    ui.label(
                        egui::RichText::new("请先在日志中选中要批注的内容。")
                            .color(Color32::from_rgb(221, 116, 129))
                            .size(13.0),
                    );
                    ui.add_space(6.0);
                }
            }

            ui.label(
                egui::RichText::new("批注内容:")
                    .strong()
                    .color(Color32::from_gray(174))
                    .size(12.0),
            );
            ui.add(
                egui::TextEdit::multiline(&mut app.annotation_input)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text("输入批注内容…"),
            );

            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                let save = egui::Button::new(
                    egui::RichText::new(if editing { "保存" } else { "添加" })
                        .color(Color32::WHITE)
                        .size(14.0),
                )
                .fill(Color32::from_rgb(33, 115, 237))
                .min_size(egui::vec2(90.0, 28.0));
                if ui.add(save).clicked() {
                    confirm = true;
                }
                ui.add_space(10.0);
                let cancel_btn = egui::Button::new(
                    egui::RichText::new("取消").color(Color32::WHITE).size(14.0),
                )
                .fill(Color32::from_rgb(83, 91, 105))
                .min_size(egui::vec2(80.0, 28.0));
                if ui.add(cancel_btn).clicked() {
                    log_info!("dialogs", "取消批注编辑");
                    cancel = true;
                }
            });
        });

    if confirm {
        log_info!("dialogs", "保存批注 (edit={:?})", app.annotation_edit_id);
        app.save_annotation_dialog();
    }
    if !open || cancel {
        app.show_annotation_dialog = false;
        app.annotation_edit_id = None;
        app.annotation_input.clear();
    }
    // 吃掉落在本窗口内的滚轮（覆盖 ScrollArea 已到顶/底的边缘 case）。
    if let Some(inner) = resp {
        consume_scroll_within(ctx, inner.response.rect);
    }
}

/// The current file's annotation list.  Clicking a row jumps to it; each row
/// also offers inline edit (🖊) and delete (🗑).
pub fn render_annotation_list(ctx: &Context, app: &mut QLogApp) {
    let mut open = app.show_annotation_list;
    let mut jump: Option<u64> = None;
    let mut edit: Option<(u64, String)> = None;
    let mut del: Option<u64> = None;

    let resp = egui::Window::new("批注列表")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_size([470.0, 430.0])
        .show(ctx, |ui| {
            ui.set_width(450.0);
            if app.annotations.is_empty() {
                ui.label(
                    egui::RichText::new("当前文件还没有批注。\n在日志中选中内容后右键 → 添加批注。")
                        .color(Color32::from_gray(150))
                        .size(13.0),
                );
                return;
            }

            // Snapshot item data so click actions (which mutate `app`) can be
            // applied AFTER the window instead of during iteration.
            let items: Vec<(u64, u64, u64, bool, String, String, String)> = app
                .annotations
                .iter()
                .map(|a| {
                    (
                        a.id,
                        a.start_line,
                        a.end_line,
                        a.stale,
                        a.selected_text.clone(),
                        a.text.clone(),
                        a.created_at.clone(),
                    )
                })
                .collect();

            egui::ScrollArea::vertical()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (id, start_line, end_line, stale, selected, note, created) in &items {
                        let id = *id;
                        let start_line = *start_line;
                        let end_line = *end_line;
                        let stale = *stale;
                        let selected = selected.clone();
                        let note = note.clone();
                        let created = created.clone();

                        let is_sel = app.annotation_selected_id == Some(id);
                        let header = if start_line == end_line {
                            format!("行 {}", start_line + 1)
                        } else {
                            format!("行 {}–{}", start_line + 1, end_line + 1)
                        };
                        let header = if stale {
                            format!("{header}  ⚠️ 位置已失效")
                        } else {
                            header
                        };

                        ui.horizontal(|ui| {
                            let resp = ui.selectable_label(
                                is_sel,
                                egui::RichText::new(format!("📍 {}  ", header))
                                    .color(if stale {
                                        Color32::from_rgb(200, 120, 60)
                                    } else {
                                        Color32::from_rgb(224, 172, 56)
                                    })
                                    .size(13.0)
                                    .strong(),
                            );
                            if resp.clicked() {
                                jump = Some(id);
                                app.annotation_selected_id = Some(id);
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("🗑").on_hover_text("删除批注").clicked() {
                                    del = Some(id);
                                }
                                if ui.small_button("🖊").on_hover_text("编辑批注").clicked() {
                                    edit = Some((id, note.clone()));
                                }
                            });
                        });

                        // Selected-content preview (monospace, truncated).
                        let sel_disp = clamp_preview(selected.trim(), 160);
                        if !sel_disp.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(sel_disp)
                                        .monospace()
                                        .color(Color32::from_rgb(150, 165, 185))
                                        .size(12.0),
                                )
                                .wrap(),
                            );
                        }
                        // Note body (wrapped).
                        let note_disp = if note.is_empty() {
                            "(无批注内容)".to_string()
                        } else {
                            note.clone()
                        };
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(note_disp)
                                    .color(Color32::from_gray(200))
                                    .size(12.5),
                            )
                            .wrap(),
                        );
                        // Meta row.
                        ui.label(
                            egui::RichText::new(created)
                                .color(Color32::from_gray(130))
                                .size(11.0),
                        );

                        ui.separator();
                    }
                });
        });

    if !open {
        app.show_annotation_list = false;
    }
    if let Some(id) = jump {
        app.jump_to_annotation(id);
    }
    if let Some(id) = del {
        app.remove_annotation(id);
    }
    if let Some((id, cur)) = edit {
        app.annotation_edit_id = Some(id);
        app.annotation_input = cur;
        app.show_annotation_dialog = true;
    }
    // 吃掉落在本窗口内的滚轮（覆盖 ScrollArea 已到顶/底的边缘 case）。
    if let Some(inner) = resp {
        consume_scroll_within(ctx, inner.response.rect);
    }
}
