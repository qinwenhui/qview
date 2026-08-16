//! Toolbar — open, search, navigation controls. Rendered below the menu bar.

use egui::{Align, Color32, Context, TextEdit};

use crate::log_debug;
use crate::app::QLogApp;

/// Render the toolbar panel.
pub fn render_toolbar(ctx: &Context, app: &mut QLogApp) {
    // Snapshot Enter key state BEFORE any TextEdit consumes it.
    let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter));

    egui::TopBottomPanel::top("toolbar")
        .min_height(36.0)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {

                // ---- New ----
                let new_btn = egui::Button::new(
                    egui::RichText::new("新建")
                        .color(Color32::WHITE)
                        .size(13.0),
                )
                .fill(Color32::from_rgb(15, 157, 89))
                .min_size(egui::vec2(56.0, 24.0));
                if ui.add(new_btn).clicked() {
                    log_debug!("toolbar", "点击 新建");
                    app.request_new_file();
                }

                ui.add_space(4.0);

                // ---- Open ----
                let open_btn = egui::Button::new(
                    egui::RichText::new("打开")
                        .color(Color32::WHITE)
                        .size(13.0),
                )
                .fill(Color32::from_rgb(33, 115, 237))
                .min_size(egui::vec2(56.0, 24.0));
                if ui.add(open_btn).clicked() {
                    log_debug!("toolbar", "点击 打开");
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter(
                            "日志文件",
                            &["log", "txt", "out", "err", "csv", "json", "xml", "yaml", "yml"],
                        )
                        .add_filter("所有文件", &["*"])
                        .pick_file()
                    {
                        app.try_open(path);
                    }
                }

                ui.add_space(4.0);

                // ---- Close current file ----
                if app.engine.is_some() {
                    let close_btn = egui::Button::new(
                        egui::RichText::new("关闭")
                            .color(Color32::WHITE)
                            .size(13.0),
                    )
                    .fill(Color32::from_rgb(218, 67, 74))
                    .min_size(egui::vec2(64.0, 24.0));
                    if ui.add(close_btn).clicked() {
                        log_debug!("toolbar", "点击 关闭文件");
                        app.try_close();
                    }
                    ui.add_space(4.0);
                }

                // Search-option toggles (VSCode-style): case-sensitive /
                // regex / whole-word.  Highlighted blue when active.  Toggling
                // only flips the flag — the next Enter / 查找 applies it, same
                // as the 搜索 menu checkboxes.
                let opt_btn = |ui: &mut egui::Ui, label: &str, active: bool, tip: &str| -> egui::Response {
                    let btn = egui::Button::new(
                        egui::RichText::new(label).size(12.0).color(Color32::WHITE),
                    )
                    .fill(if active {
                        Color32::from_rgb(33, 115, 237)
                    } else {
                        Color32::from_rgb(58, 66, 82)
                    })
                    .stroke(egui::Stroke::NONE)
                    .min_size(egui::vec2(28.0, 24.0));
                    ui.add(btn).on_hover_text(tip)
                };
                if opt_btn(ui, "Aa", app.case_sensitive, "大小写敏感").clicked() {
                    app.case_sensitive = !app.case_sensitive;
                    log_debug!("toolbar", "搜索开关-大小写敏感: {}", app.case_sensitive);
                }
                if opt_btn(ui, ".*", app.use_regex, "正则表达式").clicked() {
                    app.use_regex = !app.use_regex;
                    log_debug!("toolbar", "搜索开关-正则: {}", app.use_regex);
                }
                if opt_btn(ui, "\\b", app.whole_word, "整词匹配").clicked() {
                    app.whole_word = !app.whole_word;
                    log_debug!("toolbar", "搜索开关-整词: {}", app.whole_word);
                }
                ui.add_space(4.0);

                let search_id = egui::Id::new("toolbar_search");
                // Enter handling for the multiline search box, done BEFORE the
                // TextEdit runs (same trick as the global Ctrl+C copy):
                //   Enter            → run the search (consume the event so the
                //                      box doesn't insert a newline)
                //   Shift/Ctrl+Enter → strip the modifiers so egui's own
                //                      multiline handler inserts '\n' AT THE
                //                      CURSOR (egui doesn't implement Shift+Enter).
                if ctx.memory(|m| m.has_focus(search_id)) && enter_pressed {
                    let mods = ctx.input(|i| i.modifiers);
                    if mods.shift || mods.ctrl || mods.command {
                        ctx.input_mut(|i| {
                            for e in i.events.iter_mut() {
                                if let egui::Event::Key {
                                    key: egui::Key::Enter,
                                    pressed: true,
                                    modifiers,
                                    ..
                                } = e
                                {
                                    modifiers.shift = false;
                                    modifiers.ctrl = false;
                                    modifiers.command = false;
                                    modifiers.alt = false;
                                    modifiers.mac_cmd = false;
                                }
                            }
                        });
                    } else {
                        ctx.input_mut(|i| {
                            i.events.retain(|e| {
                                !matches!(e, egui::Event::Key {
                                    key: egui::Key::Enter,
                                    pressed: true,
                                    ..
                                })
                            });
                        });
                        app.run_search();
                    }
                }

                // Search box: one row tall by default, grows to at most 3 rows
                // when the query has newlines, then scrolls INSIDE the fixed
                // box — like a normal textarea.  The toolbar follows the box.
                let row_h = ui.text_style_height(&egui::TextStyle::Body);
                let rows = (app.search_input.matches('\n').count() + 1).clamp(1, 3);
                let box_h = rows as f32 * row_h + 12.0;
                ui.allocate_ui_with_layout(
                    egui::vec2(220.0, box_h),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("toolbar_search_scroll")
                            .max_height(box_h)
                            .show(ui, |ui| {
                                ui.add(
                                    TextEdit::multiline(&mut app.search_input)
                                        .id(search_id)
                                        .desired_rows(1)
                                        .hint_text("输入关键词...  Enter 搜索 / Shift+Enter 换行")
                                        .desired_width(220.0)
                                        .margin(egui::vec2(6.0, 5.0)),
                                );
                            });
                    },
                );

                // ---- Find button ----
                let find_btn = egui::Button::new(
                    egui::RichText::new("查找")
                        .color(Color32::WHITE)
                        .size(13.0),
                )
                .fill(Color32::from_rgb(15, 157, 89))
                .min_size(egui::vec2(48.0, 24.0));
                if ui.add(find_btn).clicked() {
                    log_debug!("toolbar", "点击 查找: \"{}\"", app.search_input);
                    app.run_search();
                }

                // ---- Clear button ----
                let clear_btn = egui::Button::new(
                    egui::RichText::new("清空")
                        .color(Color32::WHITE)
                        .size(13.0),
                )
                .fill(Color32::from_rgb(110, 118, 130))
                .min_size(egui::vec2(56.0, 24.0));
                if ui.add(clear_btn).clicked() {
                    log_debug!("toolbar", "点击 清空搜索");
                    app.clear_search();
                }

                // ---- Stop button ----
                if app.engine.as_ref()
                    .map(|arc| arc.lock().search_progress.is_some())
                    .unwrap_or(false)
                {
                    let stop_btn = egui::Button::new(
                        egui::RichText::new("停止").color(Color32::WHITE).size(13.0),
                    )
                    .fill(Color32::from_rgb(218, 67, 74))
                    .min_size(egui::vec2(48.0, 24.0));
                    if ui.add(stop_btn).clicked() {
                        log_debug!("toolbar", "点击 停止搜索");
                        if let Some(arc) = app.engine.as_mut() {
                            arc.lock().cancel_search();
                        }
                    }
                }

                ui.add_space(4.0);

                // ---- Prev / Next ----
                let prev_btn = egui::Button::new(
                    egui::RichText::new("<")
                        .color(Color32::WHITE)
                        .size(15.0),
                )
                .fill(Color32::from_rgb(70, 80, 96))
                .min_size(egui::vec2(28.0, 24.0));
                if ui.add(prev_btn).on_hover_text("上一个匹配").clicked() {
                    log_debug!("toolbar", "点击 上一个匹配");
                    app.jump_hit(-1);
                }

                let next_btn = egui::Button::new(
                    egui::RichText::new(">")
                        .color(Color32::WHITE)
                        .size(15.0),
                )
                .fill(Color32::from_rgb(70, 80, 96))
                .min_size(egui::vec2(28.0, 24.0));
                if ui.add(next_btn).on_hover_text("下一个匹配").clicked() {
                    log_debug!("toolbar", "点击 下一个匹配");
                    app.jump_hit(1);
                }

                ui.add_space(8.0);

                // ---- Search status ----
                if !app.search_status.is_empty() {
                    ui.label(
                        egui::RichText::new(&app.search_status)
                            .color(Color32::from_rgb(150, 195, 125))
                            .size(13.0),
                    );
                }

                ui.add_space(8.0);

                // ---- Edit mode ----
                if app.engine.is_some() {
                    let edit_active = app.edit_mode;
                    let edit_btn = egui::Button::new(
                        egui::RichText::new("🖊 编辑")
                            .color(Color32::WHITE)
                            .size(12.5),
                    )
                    .fill(if edit_active {
                        Color32::from_rgb(15, 157, 89)
                    } else {
                        Color32::from_rgb(58, 66, 82)
                    })
                    .min_size(egui::vec2(60.0, 24.0));
                    if ui.add(edit_btn).clicked() {
                        log_debug!("toolbar", "点击 编辑开关");
                        app.toggle_edit_mode();
                    }

                    if edit_active {
                        ui.add_space(2.0);
                        if app.is_modified() {
                            ui.label(
                                egui::RichText::new("● 已修改")
                                    .color(Color32::from_rgb(221, 116, 129))
                                    .size(12.0),
                            );
                            ui.add_space(2.0);
                        }
                        let save_btn = egui::Button::new(
                            egui::RichText::new("保存")
                                .color(Color32::WHITE)
                                .size(12.5),
                        )
                        .fill(Color32::from_rgb(15, 157, 89))
                        .min_size(egui::vec2(44.0, 24.0));
                        if ui.add(save_btn).clicked() {
                            log_debug!("toolbar", "点击 保存");
                            app.save_file();
                        }
                        // A NEW file has no destination yet — 保存 already
                        // prompts for one, so no separate 另存为 until saved.
                        if !app.is_new_file {
                            ui.add_space(2.0);
                            let saveas_btn = egui::Button::new(
                                egui::RichText::new("另存为")
                                    .color(Color32::WHITE)
                                    .size(12.5),
                            )
                            .fill(Color32::from_rgb(133, 93, 204))
                            .min_size(egui::vec2(56.0, 24.0));
                            if ui.add(saveas_btn).clicked() {
                                log_debug!("toolbar", "点击 另存为");
                                app.request_save_as();
                            }
                        }
                    }
                }

                // ---- 器灵 AI（浮动聊天窗口开关）----
                let ai_active = app.show_agent_window;
                let ai_btn = egui::Button::new(
                    egui::RichText::new(if ai_active { "☯ 器灵小Q" } else { "☯ 器灵小Q" })
                        .color(Color32::LIGHT_GREEN)
                        .size(13.0)
                        .strong(),
                )
                .fill(if ai_active {
                    Color32::from_rgb(88, 70, 160)
                } else {
                    Color32::from_rgb(58, 66, 82)
                })
                .stroke(if ai_active {
                    egui::Stroke::new(1.0, Color32::from_rgb(140, 120, 255))
                } else {
                    egui::Stroke::NONE
                })
                .min_size(egui::vec2(64.0, 24.0));
                if ui.add(ai_btn).on_hover_text("器灵 AI 助手 — 浮动聊天窗口").clicked() {
                    log_debug!("toolbar", "点击 器灵");
                    app.toggle_agent_window();
                }

                ui.add_space(8.0);

                // Right-aligned: [__input__] [跳转]
                ui.with_layout(
                    egui::Layout::right_to_left(Align::Center),
                    |ui| {
                        let go_btn = egui::Button::new(
                            egui::RichText::new("跳转")
                                .color(Color32::WHITE)
                                .size(13.0),
                        )
                        .fill(Color32::from_rgb(133, 93, 204))
                        .min_size(egui::vec2(48.0, 24.0));
                        if ui.add(go_btn).clicked() {
                            log_debug!("toolbar", "点击 跳转到行: \"{}\"", app.goto_input);
                            app.goto_line();
                        }

                        ui.add_space(4.0);

                        let goto_id = egui::Id::new("toolbar_goto");
                        let g_resp = ui.add_sized(
                            [90.0, 26.0],
                            TextEdit::singleline(&mut app.goto_input)
                                .id(goto_id)
                                .hint_text("行号")
                                .desired_width(90.0)
                                .margin(egui::vec2(5.0, 5.0)),
                        );
                        // TextEdit may consume Enter, so we check the snapshot.
                        if g_resp.lost_focus() && enter_pressed {
                            app.goto_line();
                        }
                    },
                );
            });
        });
}
