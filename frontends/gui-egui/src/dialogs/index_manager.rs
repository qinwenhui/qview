//! Index / cache management dialog.
//! Lists all `.qli` index files with sizes and provides a one-click
//! "clear all except current file" button.

use egui::{Color32, Context};

use crate::log_info;
use crate::app::QLogApp;

pub fn render_index_manager(ctx: &Context, app: &mut QLogApp) {
    crate::dialogs::centered_window(ctx, "缓存管理", [520.0, 420.0])
        .fixed_size([520.0, 420.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("缓存管理")
                    .size(18.0)
                    .strong()
                    .color(Color32::from_rgb(191, 201, 214)),
            );
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("管理索引缓存文件、搜索历史、最近打开记录")
                    .size(12.0)
                    .color(Color32::from_gray(140)),
            );
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(8.0);

            // ---- index directory info ----
            let index_dir = app.config.engine.index_dir.as_ref();
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("索引目录:")
                        .size(13.0)
                        .strong()
                        .color(Color32::from_rgb(160, 170, 185)),
                );
                if let Some(dir) = index_dir {
                    ui.label(
                        egui::RichText::new(dir.display().to_string())
                            .size(12.0)
                            .color(Color32::from_gray(190)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("（未设置）")
                            .size(12.0)
                            .color(Color32::from_gray(130)),
                    );
                }
            });

            ui.add_space(10.0);

            // ---- current file indicator ----
            let keep_path = app
                .path
                .as_ref()
                .map(|p| app.config.engine.index_path(p));

            // ---- index file list ----
            ui.label(
                egui::RichText::new("索引文件列表:")
                    .size(13.0)
                    .strong()
                    .color(Color32::from_rgb(160, 170, 185)),
            );

            let mut total_count = 0usize;
            let mut total_bytes = 0u64;
            let mut current_kept = false;

            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    if let Some(dir) = index_dir {
                        if let Ok(entries) = std::fs::read_dir(dir) {
                            let mut files: Vec<_> = entries
                                .flatten()
                                .filter(|e| {
                                    e.path()
                                        .extension()
                                        .and_then(|ext| ext.to_str())
                                        == Some("qli")
                                })
                                .map(|e| {
                                    let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                                    (e.path(), size)
                                })
                                .collect();
                            files.sort_by_key(|(_, sz)| *sz);

                            if files.is_empty() {
                                ui.label(
                                    egui::RichText::new("（暂无索引文件）")
                                        .size(12.0)
                                        .color(Color32::from_gray(130)),
                                );
                            }

                            for (path, size) in &files {
                                let is_current = keep_path.as_ref().map(|kp| kp == path).unwrap_or(false);
                                if is_current {
                                    current_kept = true;
                                }
                                total_count += 1;
                                total_bytes += size;

                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("-");
                                let marker = if is_current { " [当前文件]" } else { "" };
                                let text = format!(
                                    "{}  {}  {}{}",
                                    name,
                                    crate::viewer::human_bytes(*size),
                                    if is_current { "★ " } else { "" },
                                    marker,
                                );
                                let color = if is_current {
                                    Color32::from_rgb(121, 210, 130)
                                } else {
                                    Color32::from_gray(170)
                                };
                                ui.label(
                                    egui::RichText::new(text)
                                        .size(12.0)
                                        .color(color),
                                );
                            }
                        } else {
                            ui.label(
                                egui::RichText::new("（无法读取索引目录）")
                                    .size(12.0)
                                    .color(Color32::from_gray(130)),
                            );
                        }
                    } else {
                        ui.label(
                            egui::RichText::new("（索引目录未设置，使用旧版模式）")
                                .size(12.0)
                                .color(Color32::from_gray(130)),
                        );
                    }
                });

            ui.add_space(6.0);

            // ---- summary ----
            ui.label(
                egui::RichText::new(format!(
                    "共 {} 个索引文件，占用 {}",
                    total_count,
                    crate::viewer::human_bytes(total_bytes),
                ))
                .size(12.0)
                .color(Color32::from_gray(160)),
            );

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!(
                    "最近文件 {} 条 | 搜索历史 {} 条",
                    app.recent_files.lock().len(),
                    app.search_history.lock().len(),
                ))
                .size(12.0)
                .color(Color32::from_gray(150)),
            );

            if current_kept {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("★ 标记的索引文件在清空时会被保留")
                        .size(11.0)
                        .color(Color32::from_gray(140)),
                );
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            // ---- buttons ----
            ui.horizontal(|ui| {
                // Clear cache button (red, left side)
                let clear_btn = egui::Button::new(
                    egui::RichText::new("清空缓存")
                        .color(Color32::WHITE)
                        .size(14.0),
                )
                .fill(Color32::from_rgb(218, 67, 74))
                .min_size(egui::vec2(120.0, 30.0));
                if ui.add(clear_btn).clicked() {
                    log_info!("index_manager", "点击 清空缓存");
                    let (count, bytes) = app.clear_cache();
                    app.flash_status(
                        format!(
                            "已清空缓存：删除 {} 个索引文件，释放 {}",
                            count,
                            crate::viewer::human_bytes(bytes),
                        ),
                        5,
                    );
                    app.show_index_manager = false;
                }

                // Right-side close button
                ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("关闭")
                                    .color(Color32::WHITE)
                                    .size(14.0),
                            )
                            .fill(Color32::from_rgb(83, 91, 105))
                            .min_size(egui::vec2(100.0, 30.0)),
                        )
                        .clicked()
                    {
                        app.show_index_manager = false;
                    }
                });
            });
        });
}
