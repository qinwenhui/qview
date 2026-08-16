//! Menu bar — rendered at the very top of the window.

use egui::{Align, Context, Layout};

use crate::{log_debug};
use crate::app::QLogApp;

pub fn render_menu_bar(ctx: &Context, app: &mut QLogApp) {
    egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            file_menu(ui, app);
            edit_menu(ui, app);
            view_menu(ui, app, ctx);
            search_menu(ui, app);
            tools_menu(ui, app);
            donate_menu(ui, app);
            help_menu(ui, app);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(arc) = app.engine.as_ref() {
                    let engine = arc.lock();
                    if app.is_new_file {
                        // An unsaved new file has no real backing size yet.
                        ui.label(format!(
                            "未命名 · {} 行",
                            engine.effective_line_count()
                        ));
                    } else {
                        ui.label(format!(
                            "{} 行 | {}",
                            engine.effective_line_count(),
                            crate::viewer::human_bytes(engine.mmap.size())
                        ));
                    }
                }
            });
        });
    });
}

// ---------------------------------------------------------------------------
// File
// ---------------------------------------------------------------------------

fn file_menu(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.menu_button("文件", |ui| {
        if ui.button("新建  Ctrl+N").clicked() {
            log_debug!("menu", "菜单 → 新建文件");
            app.request_new_file();
            ui.close_menu();
        }
        if ui.button("打开文件...  Ctrl+O").clicked() {
            log_debug!("menu", "菜单 → 打开文件");
            open_file_dialog(app);
            ui.close_menu();
        }
        if ui.button("重新加载  Ctrl+R").clicked() {
            if app.is_new_file {
                log_debug!("menu", "菜单 → 重新加载: 新文件，跳过");
                app.flash_status("新文件无需重新加载", 2);
            } else if let Some(ref p) = app.path {
                log_debug!("menu", "菜单 → 重新加载: {}", p.display());
                reload(app);
            }
            ui.close_menu();
        }
        // 最近打开来自 store `files` 表（内存缓存，启动已载入）
        if !app.recent_files.lock().is_empty() {
            ui.menu_button("最近打开", |ui| {
                let recents = app.recent_files.lock().clone();
                for path in &recents {
                    if ui.button(path.display().to_string()).clicked() {
                        log_debug!("menu", "菜单 → 最近打开: {}", path.display());
                        let p = path.clone();
                        app.try_open(p);
                        ui.close_menu();
                    }
                }
            });
        }
        ui.separator();
        if ui.button("文件属性...  Ctrl+I").clicked() {
            log_debug!("menu", "菜单 → 文件属性");
            app.show_file_properties = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button("退出  Alt+F4").clicked() {
            log_debug!("menu", "菜单 → 退出程序");
            app.request_exit();
        }
    });
}

fn open_file_dialog(app: &mut QLogApp) {
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("日志文件", &["log", "txt", "out", "err", "csv", "json", "xml", "yaml", "yml"])
        .add_filter("所有文件", &["*"])
        .pick_file()
    {
        app.try_open(path);
    }
}

fn reload(app: &mut QLogApp) {
    if let Some(ref path) = app.path.clone() {
        app.try_open(path.clone());
    }
}

// ---------------------------------------------------------------------------
// Edit — clipboard ops + (in edit mode) undo/redo/save.
// ---------------------------------------------------------------------------

fn edit_menu(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.menu_button("编辑", |ui| {
        let editing = app.edit_mode;

        // Edit-mode toggle.
        let toggle_label = if editing {
            "🖊 退出编辑模式"
        } else {
            "🖊 编辑模式"
        };
        if ui.button(toggle_label).clicked() {
            log_debug!("menu", "菜单 → 编辑开关");
            app.toggle_edit_mode();
            ui.close_menu();
        }
        ui.separator();

        if editing {
            if ui.button("撤销  Ctrl+Z").clicked() {
                app.editor_undo();
                ui.close_menu();
            }
            if ui.button("重做  Ctrl+Y").clicked() {
                app.editor_redo();
                ui.close_menu();
            }
            if ui.button("保存  Ctrl+S").clicked() {
                app.save_file();
                ui.close_menu();
            }
            // A NEW file has no destination yet — "save" IS "save as", so there
            // is no separate 另存为 until the file has been saved once.
            if !app.is_new_file {
                if ui.button("另存为  Ctrl+Shift+S").clicked() {
                    app.request_save_as();
                    ui.close_menu();
                }
            }
            ui.separator();
        }

        // Copy: prefer selection, fall back to current line.
        let copy_label = if app.selection.is_some() {
            "复制选中  Ctrl+C"
        } else {
            "复制当前行  Ctrl+C"
        };
        if ui.button(copy_label).clicked() {
            if let Some(text) = app.copy_selection_text() {
                ui.ctx().copy_text(text.clone());
                app.flash_status(format!("已复制 {} 个字符到剪贴板", text.len()), 3);
            } else if let Some(text) = app.current_line_text() {
                ui.ctx().copy_text(text);
            }
            ui.close_menu();
        }
    });
}

// ---------------------------------------------------------------------------
// View — navigation, theme, and display toggles.
// ---------------------------------------------------------------------------

fn view_menu(ui: &mut egui::Ui, app: &mut QLogApp, ctx: &Context) {
    ui.menu_button("视图", |ui| {
        // ---- navigation ----
        if ui.button("跳转到顶部  Home").clicked() {
            log_debug!("menu", "视图 → 跳转到顶部");
            app.scroll_y = 0.0;
            ui.close_menu();
        }
        if ui.button("跳转到底部  End").clicked() {
            if let Some(arc) = app.engine.as_ref() {
                let total = arc.lock().effective_line_count();
                log_debug!("menu", "视图 → 跳转到底部 ({} 行)", total);
                app.scroll_y = (total as f64 * app.row_h).max(0.0);
            }
            ui.close_menu();
        }
        if ui.button("跳转到行...  Ctrl+L").clicked() {
            log_debug!("menu", "视图 → 跳转到行");
            ui.ctx().memory_mut(|m| m.request_focus(egui::Id::new("toolbar_goto")));
            ui.close_menu();
        }
        ui.separator();

        // ---- theme ----
        ui.menu_button("主题", |ui| {
            let theme_names: Vec<(String, bool)> = app
                .themes
                .iter()
                .map(|t| (t.name.clone(), app.config.gui.theme == t.name))
                .collect();
            for (name, selected) in &theme_names {
                if ui.selectable_label(*selected, name).clicked() {
                    log_debug!("menu", "视图 → 切换主题: {}", name);
                    app.switch_theme(name, ctx);
                    ui.close_menu();
                }
            }
        });

        ui.separator();

        // ---- display toggles ----
        if ui.checkbox(&mut app.show_line_numbers, "显示行号").changed() {
            log_debug!("menu", "视图 → 显示行号: {}", app.show_line_numbers);
        }
        if ui.checkbox(&mut app.word_wrap, "自动换行").changed() {
            log_debug!("menu", "视图 → 自动换行: {}", app.word_wrap);
        }
        if ui.checkbox(&mut app.show_whitespace, "显示空白字符").changed() {
            log_debug!("menu", "视图 → 显示空白字符: {}", app.show_whitespace);
        }
        if ui.checkbox(&mut app.show_indent_guides, "缩进参考线").changed() {
            log_debug!("menu", "视图 → 缩进参考线: {}", app.show_indent_guides);
        }
        if ui.checkbox(&mut app.level_coloring, "日志级别着色").changed() {
            log_debug!("menu", "视图 → 日志着色: {}", app.level_coloring);
        }
    });
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

fn search_menu(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.menu_button("搜索", |ui| {
        if ui.button("查找...  Ctrl+F").clicked() {
            log_debug!("menu", "搜索 → 查找");
            ui.ctx().memory_mut(|m| m.request_focus(egui::Id::new("toolbar_search")));
            ui.close_menu();
        }
        if ui.button("下一个  F3 / Ctrl+G").clicked() {
            log_debug!("menu", "搜索 → 下一个匹配");
            app.jump_hit(1);
            ui.close_menu();
        }
        if ui.button("上一个  Shift+F3 / Ctrl+Shift+G").clicked() {
            log_debug!("menu", "搜索 → 上一个匹配");
            app.jump_hit(-1);
            ui.close_menu();
        }
        ui.separator();
        if ui.checkbox(&mut app.case_sensitive, "大小写敏感").changed() {
            log_debug!("menu", "搜索 → 大小写敏感: {}", app.case_sensitive);
        }
        if ui.checkbox(&mut app.use_regex, "正则表达式").changed() {
            log_debug!("menu", "搜索 → 正则表达式: {}", app.use_regex);
        }
        if ui.checkbox(&mut app.whole_word, "整词匹配").changed() {
            log_debug!("menu", "搜索 → 整词匹配: {}", app.whole_word);
        }
    });
}

// ---------------------------------------------------------------------------
// Tools — utilities + app settings.
// ---------------------------------------------------------------------------

fn tools_menu(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.menu_button("工具", |ui| {
        if ui.button("缓存管理").clicked() {
            log_debug!("menu", "工具 → 缓存管理");
            app.show_index_manager = true;
            ui.close_menu();
        }
        if ui.button("历史会话").clicked() {
            log_debug!("menu", "工具 → 历史会话");
            app.show_history = true;
            app.request_history_reload();
            ui.close_menu();
        }
        ui.separator();
        if ui.button("设置...").clicked() {
            log_debug!("menu", "工具 → 打开设置");
            app.show_settings = true;
            ui.close_menu();
        }
    });
}

// ---------------------------------------------------------------------------
// Donate — standalone top-level menu; clicking opens the dialog directly.
// ---------------------------------------------------------------------------

fn donate_menu(ui: &mut egui::Ui, app: &mut QLogApp) {
    if ui.selectable_label(false, "❤ 捐赠").clicked() {
        log_debug!("menu", "菜单 → 捐赠");
        app.show_donate = true;
    }
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

fn help_menu(ui: &mut egui::Ui, app: &mut QLogApp) {
    ui.menu_button("帮助", |ui| {
        if ui.button("使用说明  F1").clicked() {
            log_debug!("menu", "帮助 → 使用说明");
            app.show_help = true;
            ui.close_menu();
        }
        if ui.button("快捷键一览").clicked() {
            log_debug!("menu", "帮助 → 快捷键一览");
            app.show_shortcuts = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button("关于 qview").clicked() {
            log_debug!("menu", "帮助 → 关于");
            app.show_about = true;
            ui.close_menu();
        }
    });
}
