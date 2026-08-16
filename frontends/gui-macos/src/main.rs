//! qview-gui-macos — 原生 AppKit / CoreText 前端
//!
//! 设计：直接调用 AppKit + CoreText（objc2 生态），不依赖 egui/iced/slint。
//! qview-core 提供 mmap / 索引 / 搜索，前端只调用公开 API。

mod app;
mod bridge;
mod config;
mod dialogs;
mod layout;
mod menu;
mod selection;
mod settings_sheet;
mod statusbar;
mod text;
mod theme;
mod toolbar;
mod util;
mod view;
mod window;

fn main() {
    crate::app::run();
}
