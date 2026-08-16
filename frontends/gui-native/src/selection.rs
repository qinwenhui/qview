//! 文本选择模型：`(start_line, start_col, end_line, end_col)`，
//! 列位置是**行内 UTF-16 单元索引**（CJK 一个字符 = 1 单元）。
//!
//! 命中测试（像素→行列）在 view.rs 完成（需要排版结果）；这里提供
//! 归一化、复制文本、批注字节定位等纯逻辑。

use crate::app::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start_line: u64,
    pub start_col: usize,
    pub end_line: u64,
    pub end_col: usize,
}

impl Selection {
    /// 归一化：返回 (anchor_line, anchor_col, active_line, active_col)，始终正向。
    pub fn normalized(&self) -> (u64, usize, u64, usize) {
        if self.start_line < self.end_line
            || (self.start_line == self.end_line && self.start_col <= self.end_col)
        {
            (self.start_line, self.start_col, self.end_line, self.end_col)
        } else {
            (self.end_line, self.end_col, self.start_line, self.start_col)
        }
    }

    /// 是否退化为一个点（空选择）。
    pub fn is_empty(&self) -> bool {
        let (sl, sc, el, ec) = self.normalized();
        sl == el && sc == ec
    }
}

/// 把选中内容复制为文本（跨行用 `\n` 连接）。
pub fn copy_text(app: &App, sel: &Selection) -> Option<String> {
    let b = app.bridge.as_ref()?;
    let (sl, sc, el, ec) = sel.normalized();
    let mut parts: Vec<String> = Vec::with_capacity((el - sl + 1) as usize);
    for (i, line) in (sl..=el).enumerate() {
        let text = b.read_line(line);
        let units: Vec<u16> = text.encode_utf16().collect();
        let a = if i == 0 { sc.min(units.len()) } else { 0 };
        let be = if line == el { ec.min(units.len()) } else { units.len() };
        if a < be {
            parts.push(String::from_utf16_lossy(&units[a..be]));
        } else {
            parts.push(String::new());
        }
    }
    Some(parts.join("\n"))
}

/// UTF-16 单元位置 → 该字符的 UTF-8 字节偏移（BMP 下 == 字符数前向累加）。
pub fn utf16_to_byte_offset(text: &str, unit: usize) -> usize {
    let mut u = 0usize;
    for (b, ch) in text.char_indices() {
        if u >= unit {
            return b;
        }
        u += ch.len_utf16();
    }
    text.len()
}

/// 选中区 → 批注字节范围（源文件内 start_byte..end_byte）。
pub fn annotation_bytes(app: &App, sel: &Selection) -> Option<(u64, u64)> {
    let b = app.bridge.as_ref()?;
    let (sl, sc, el, ec) = sel.normalized();
    let raw_s = b.read_raw(sl);
    let raw_e = b.read_raw(el);
    let start_byte = raw_s.start_byte + utf16_to_byte_offset(&raw_s.text, sc) as u64;
    let end_byte = raw_e.start_byte + utf16_to_byte_offset(&raw_e.text, ec) as u64;
    Some((start_byte, end_byte))
}
