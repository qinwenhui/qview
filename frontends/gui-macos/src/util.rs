//! 小工具：字节换算、UTF-8↔UTF-16 区间换算、NS 字符串、剪贴板、打开 URL。

use objc2::rc::Retained;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// 人类可读字节数（1024 进制）。
pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut n = n as f64;
    let mut i = 0;
    while n >= 1024.0 && i < UNITS.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n as u64, UNITS[i])
    } else {
        format!("{:.2} {}", n, UNITS[i])
    }
}

/// 行内匹配区间（UTF-8 字节区间）→ UTF-16 区间，供 CoreText 定位。
///
/// CFString / CTLine 用 UTF-16 码元索引，而 qview-core 的匹配是 UTF-8 字节
/// 偏移，必须换算。
pub fn byte_ranges_to_utf16(text: &str, spans: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // 先把 UTF-8 字节偏移映射到字符索引（char index）。
    // text[..] 按 char 迭代累积字节偏移。
    let char_starts: Vec<usize> = text
        .char_indices()
        .map(|(byte_idx, _)| byte_idx)
        .collect();
    // 含 text.len() 作为末尾哨兵
    let mut out = Vec::with_capacity(spans.len());
    for &(s, e) in spans {
        let s = s.min(text.len());
        let e = e.min(text.len());
        let sc = byte_to_char_index(&char_starts, s, text.len());
        let ec = byte_to_char_index(&char_starts, e, text.len());
        // UTF-16 长度：累计 chars 的 UTF-16 长度
        let u16_start = utf16_len_of_chars(text, 0, sc);
        let u16_end = utf16_len_of_chars(text, 0, ec);
        if u16_end > u16_start {
            out.push((u16_start, u16_end));
        }
    }
    out
}

fn byte_to_char_index(char_starts: &[usize], byte: usize, text_len: usize) -> usize {
    if byte >= text_len {
        return char_starts.len();
    }
    match char_starts.binary_search(&byte) {
        Ok(i) => i,
        Err(0) => 0,
        // 不在 char 边界 → 落在前一个字符内部，属于该字符
        Err(i) => i - 1,
    }
}

fn utf16_len_of_chars(text: &str, start_char: usize, end_char: usize) -> usize {
    text.chars()
        .skip(start_char)
        .take(end_char.saturating_sub(start_char))
        .map(|c| c.len_utf16())
        .sum()
}

/// 把 &str 转成 NSString（NSUTF8StringEncoding 要求有效 UTF-8，这里用 UTF-8）。
pub fn ns_string(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

/// 复制文本到系统剪贴板（Cmd+C）。
pub fn copy_to_clipboard(s: &str) {
    let pb = NSPasteboard::generalPasteboard();
    let _ = pb.clearContents();
    let str = NSString::from_str(s);
    unsafe {
        let _ = pb.setString_forType(&str, &NSPasteboardTypeString);
    }
}
