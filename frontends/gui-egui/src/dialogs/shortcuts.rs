//! Shortcuts reference dialog — tabular listing grouped by category.

use egui::{Align, Color32, Context, Layout};
use crate::app::QLogApp;

pub fn render_shortcuts(ctx: &Context, app: &mut QLogApp) {
    crate::dialogs::centered_window(ctx, "快捷键一览", [520.0, 500.0])
        .fixed_size([520.0, 500.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("快捷键一览")
                    .size(18.0)
                    .strong()
                    .color(Color32::from_rgb(210, 218, 230)),
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            // Use the window's own scroll, not an inner ScrollArea, so
            // wheel events don't leak through to the main view.
            egui::ScrollArea::vertical()
                .id_salt("shortcuts_scroll")
                .max_height(ui.available_height() - 52.0)
                .show(ui, |ui| {
                    shortcut_group(
                        ui,
                        "文件操作",
                        &[
                            ("Ctrl+O", "打开文件"),
                            ("Ctrl+R", "重新加载"),
                            ("Ctrl+I", "文件属性"),
                            ("Alt+F4", "退出程序"),
                        ],
                    );

                    ui.add_space(8.0);
                    shortcut_group(
                        ui,
                        "搜索",
                        &[
                            ("Ctrl+F", "打开搜索栏"),
                            ("Enter", "执行搜索"),
                            ("F3 / Ctrl+G", "下一个匹配"),
                            ("Shift+F3 / Ctrl+Shift+G", "上一个匹配"),
                            ("Esc", "取消搜索 / 关闭对话框"),
                        ],
                    );

                    ui.add_space(8.0);
                    shortcut_group(
                        ui,
                        "导航",
                        &[
                            ("Home", "跳到文件顶部"),
                            ("End", "跳到文件底部"),
                            ("PageUp", "向上翻页"),
                            ("PageDown", "向下翻页"),
                            ("↑ ↓", "上移 / 下移一行"),
                            ("Ctrl+L", "跳转到指定行"),
                        ],
                    );

                    ui.add_space(8.0);
                    shortcut_group(
                        ui,
                        "显示",
                        &[
                            ("Ctrl +", "放大字体"),
                            ("Ctrl -", "缩小字体"),
                            ("Ctrl+0", "重置字体大小"),
                            ("Ctrl+Shift+T", "循环切换主题"),
                        ],
                    );

                    ui.add_space(8.0);
                    shortcut_group(
                        ui,
                        "编辑",
                        &[
                            ("Ctrl+A", "全选"),
                            ("Ctrl+C", "复制选中内容"),
                        ],
                    );

                    ui.add_space(8.0);
                    shortcut_group(
                        ui,
                        "帮助",
                        &[
                            ("F1", "打开使用说明"),
                        ],
                    );
                });

            // Button at bottom-right
            ui.add_space(8.0);
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("知道了")
                                .color(Color32::WHITE)
                                .size(14.0),
                        )
                        .fill(Color32::from_rgb(33, 115, 237))
                        .min_size(egui::vec2(100.0, 30.0)),
                    )
                    .clicked()
                {
                    app.show_shortcuts = false;
                }
            });
        });
}

fn shortcut_group(ui: &mut egui::Ui, title: &str, items: &[(&str, &str)]) {
    ui.label(
        egui::RichText::new(title)
            .size(13.0)
            .strong()
            .color(Color32::from_rgb(175, 210, 245)),
    );
    ui.add_space(2.0);

    for (key, desc) in items {
        ui.horizontal(|ui| {
            ui.add_sized(
                [190.0, 18.0],
                egui::Label::new(
                    egui::RichText::new(*key)
                        .size(12.5)
                        .color(Color32::from_rgb(245, 235, 160))
                        .monospace(),
                ),
            );
            ui.label(
                egui::RichText::new(*desc)
                    .size(12.5)
                    .color(Color32::from_gray(200)),
            );
        });
    }
}
