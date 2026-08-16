//! Help dialog — quick-start guide with common operations.

use egui::{Color32, Context};
use crate::app::QLogApp;

pub fn render_help(ctx: &Context, app: &mut QLogApp) {
    crate::dialogs::centered_window(ctx, "使用说明", [560.0, 450.0])
        .fixed_size([560.0, 450.0])
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new("快速上手指南")
                    .size(18.0)
                    .strong()
                    .color(Color32::from_rgb(210, 218, 230)),
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(10.0);

            let help_items: Vec<(&str, &str)> = vec![
                ("打开文件", "点击工具栏「打开」按钮或使用 Ctrl+O 快捷键"),
                ("搜索内容", "在搜索框输入关键词后按回车或点击「查找」"),
                ("导航匹配", "使用 < > 按钮或 F3/Shift+F3 切换搜索结果"),
                ("跳转行号", "输入行号后按回车或点击「跳转」按钮"),
                ("重新加载", "菜单 -> 文件 -> 重新加载 获取文件最新内容"),
                ("滚动操作", "鼠标滚轮上下滚动，Shift+滚轮水平滚动"),
                ("切换主题", "菜单 -> 视图 -> 主题 中选择喜欢的配色方案"),
                ("调整显示", "菜单 -> 视图 中切换行号/换行等显示选项"),
                ("键盘快捷键", "菜单 -> 帮助 -> 快捷键一览 查看完整快捷键"),
            ];

            for (title, desc) in help_items {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .size(13.0)
                            .strong()
                            .color(Color32::from_rgb(200, 225, 255)),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(desc)
                            .size(13.0)
                            .color(Color32::from_gray(195)),
                    );
                });
                ui.add_space(6.0);
            }

            ui.add_space(16.0);
            ui.separator();
            ui.add_space(10.0);

            let tips: Vec<&str> = vec![
                "[提示] 支持将日志文件直接拖拽到窗口打开",
                "[提示] 首次打开大文件时会自动构建索引，下次打开速度更快",
                "[提示] 索引文件(.qli) 保存在程序配置目录的 index/ 子目录下",
                "[提示] 可在 工具 -> 缓存管理 中查看和清理索引文件",
                "[提示] 设置会自动保存，下次启动时恢复",
            ];

            for tip in tips {
                ui.label(
                    egui::RichText::new(tip)
                        .size(12.0)
                        .color(Color32::from_rgb(220, 196, 114)),
                );
                ui.add_space(3.0);
            }

            ui.add_space(16.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("知道了").color(Color32::WHITE).size(14.0),
                        )
                        .fill(Color32::from_rgb(33, 115, 237))
                        .min_size(egui::vec2(100.0, 30.0)),
                    )
                    .clicked()
                {
                    app.show_help = false;
                }
            });
        });
}
