//! Font setup — register the embedded Chinese font with egui.
//!
//! 中文字体在**编译期**嵌入二进制（见 crate::assets::font_bytes），运行时
//! 不读磁盘，任何平台 / 任何打包方式（裸二进制、Windows 安装、macOS .app）
//! 都能拿到同一份字体。
//!
//! 实验模式：当环境变量 `Q_LOG_NO_FONTS=1` 时，跳过字体加载，直接返回 egui
//! 默认字体（Hack + NotoEmoji + Ubuntu-Light + emoji-icon）。此分支专用于
//! 对比验证字体在空载时的真实占用。

use std::sync::Arc;
use std::borrow::Cow;

use egui::{FontData, FontDefinitions, FontFamily};

use crate::mem_diag;

/// 如果设置为 `1`，则直接返回 egui 默认字体集合（仅 Hack + 表情符号），
/// 整个进程不再持有任何中文字体。
fn no_fonts_mode() -> bool {
    std::env::var("Q_LOG_NO_FONTS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        .unwrap_or(false)
}

/// Return populated `FontDefinitions` plus the list of discovered font names.
pub fn discover_fonts() -> (FontDefinitions, Vec<String>) {
    let mut fonts = FontDefinitions::default();
    let mut discovered: Vec<String> = Vec::new();

    if no_fonts_mode() {
        // 实验模式：完全跳过字体加载
        mem_diag::clear_font_registry();
        discovered.push("(no-fonts mode)".to_string());
        eprintln!("[fonts] Q_LOG_NO_FONTS=1 — 跳过全部字体加载，使用 egui 默认");
        return (fonts, discovered);
    }

    // 唯一内置字体：编译期嵌入，运行时不依赖外部文件。
    let name = crate::assets::FONT_NAME;
    let bytes = crate::assets::font_bytes();
    let font_data = match &bytes {
        Cow::Borrowed(slice) => FontData::from_static(slice),
        Cow::Owned(vec) => FontData::from_owned(vec.clone()),
    };
    fonts.font_data.insert(name.to_owned(), Arc::new(font_data));
    mem_diag::register_font(name, bytes.len() as u64);
    crate::log_info!(
        "fonts",
        "loaded {} ({} bytes, source={})",
        name,
        bytes.len(),
        crate::assets::font_source()
    );
    discovered.push(name.to_owned());

    // ---- register in both families ----
    for name in &discovered {
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push(name.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(name.clone());
    }

    (fonts, discovered)
}
