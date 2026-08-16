//! About dialog — application info, author, open-source license.

use egui::{Color32, Context};
use crate::app::QLogApp;

pub fn render_about(ctx: &Context, app: &mut QLogApp) {
    crate::dialogs::centered_window(ctx, "关于 qview", [440.0, 400.0])
        .fixed_size([440.0, 400.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(24.0);

                // ── App name ───────────────────────────────────────
                ui.label(
                    egui::RichText::new("🔍 文本浏览器 · qview")
                        .size(24.0)
                        .strong()
                        .color(Color32::from_rgb(210, 218, 230)),
                );

                ui.add_space(6.0);

                // ── Version ────────────────────────────────────────
                ui.label(
                    egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(15.0)
                        .color(Color32::from_rgb(121, 178, 106)),
                );

                ui.add_space(18.0);
                ui.separator();
                ui.add_space(14.0);

                // ── Description ────────────────────────────────────
                ui.label(
                    egui::RichText::new("基于 Rust + egui 构建的高性能文本浏览器")
                        .size(13.0)
                        .color(Color32::from_gray(195)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("支持 GB 级超大文件的快速浏览与搜索")
                        .size(13.0)
                        .color(Color32::from_gray(195)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("内存映射 · 按需加载 · 极低占用")
                        .size(13.0)
                        .color(Color32::from_gray(180)),
                );

                ui.add_space(20.0);
                ui.separator();
                ui.add_space(14.0);

                // ── Author (centered) ──────────────────────────────
                ui.label(
                    egui::RichText::new("作 者")
                        .size(12.0)
                        .color(Color32::from_gray(150)),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("qinwh")
                        .size(17.0)
                        .strong()
                        .color(Color32::from_rgb(200, 225, 255)),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new("1510365643@qq.com")
                        .size(12.0)
                        .color(Color32::from_gray(165)),
                );

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(14.0);

                // ── Open-source info ───────────────────────────────
                ui.label(
                    egui::RichText::new("开源许可 · GPL-3.0")
                        .size(13.0)
                        .color(Color32::from_rgb(160, 195, 225)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("源代码完全开放，欢迎 Star、Issue、PR")
                        .size(12.0)
                        .color(Color32::from_gray(165)),
                );

                ui.add_space(20.0);

                // ── Close button ───────────────────────────────────
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("确定").color(Color32::WHITE).size(14.0),
                        )
                        .fill(Color32::from_rgb(33, 115, 237))
                        .min_size(egui::vec2(100.0, 30.0)),
                    )
                    .clicked()
                {
                    app.show_about = false;
                }
            });
        });
}
