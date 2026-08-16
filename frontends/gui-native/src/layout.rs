//! 排版模型：word wrap 时把一行逻辑行切成若干可视子行。
//!
//! 字符宽度用 `GetCharWidth32W` 实测（CJK 双宽正确）。ASCII 宽度按字体缓存在
//! `CharWidthCache`（日志以 ASCII 为主，每行只对非 ASCII 字符查询 GDI）。
//! 所有列位置都使用 **UTF-16 单元索引**（BMP 下 == String 字符索引）。

use std::ffi::c_void;

use windows_sys::Win32::Graphics::Gdi::{GetCharWidth32W, GetTextExtentPoint32W};

/// 一个可视子行（UTF-16 单元区间）
#[derive(Clone, Copy, Debug)]
pub struct VisualRow {
    pub start: usize,
    pub end: usize,
    pub x_off: i32,
    pub width: i32,
}

/// 一条已排版逻辑行（可视子行列表 + 每字符宽度 + 原文，供绘制/命中测试复用）
#[derive(Debug)]
pub struct LaidLine {
    pub line: u64,
    pub y_top: i32,
    pub rows: Vec<VisualRow>,
    pub line_h: i32,
    pub char_w: Vec<i32>,
    pub units: Vec<u16>,
    pub text: String,
    pub start_byte: u64,
}

/// ASCII 字符宽度缓存（字体变化时重建）
pub struct CharWidthCache {
    pub ascii: [i32; 128],
    font_size_cached: i32,
}

impl Default for CharWidthCache {
    fn default() -> Self {
        Self {
            ascii: [8; 128],
            font_size_cached: -1,
        }
    }
}

impl CharWidthCache {
    /// 为当前字体刷新缓存（字体像素高变化时才重建）
    pub fn ensure(&mut self, hdc: *mut c_void, font_size_px: i32) {
        if self.font_size_cached == font_size_px {
            return;
        }
        self.font_size_cached = font_size_px;
        let chars: Vec<u16> = (0u16..128).collect();
        let mut w = [0i32; 128];
        let ok = unsafe { GetCharWidth32W(hdc, 0, 127, w.as_mut_ptr()) };
        if ok == 0 {
            for c in &mut self.ascii {
                *c = 8;
            }
        } else {
            self.ascii = w;
        }
        let _ = chars;
    }

    pub fn width(&self, c: u16) -> i32 {
        if c < 128 {
            self.ascii[c as usize]
        } else {
            0 // 非 ASCII 走逐字符查询
        }
    }
}

/// 一次性得到一行的全部 UTF-16 字符宽度（ASCII 用缓存，其余逐字符查询）。
pub fn line_char_widths(hdc: *mut c_void, chars: &[u16], ascii: &[i32; 128], out: &mut Vec<i32>) {
    out.clear();
    out.reserve(chars.len());
    for &c in chars {
        if c < 128 {
            out.push(ascii[c as usize]);
        } else {
            let mut cw = [0i32; 1];
            let ok = unsafe { GetCharWidth32W(hdc, c as u32, c as u32, cw.as_mut_ptr()) };
            out.push(if ok != 0 { cw[0] } else { 8 });
        }
    }
}

/// 把一行字符宽度切成可视子行（断在字符边界，max_px 内）。
pub fn wrap_line_into(widths: &[i32], max_px: i32, out: &mut Vec<VisualRow>) {
    out.clear();
    if widths.is_empty() {
        out.push(VisualRow { start: 0, end: 0, x_off: 0, width: 0 });
        return;
    }
    let max = max_px.max(10);
    let mut start = 0usize;
    let mut x: i32 = 0;
    for (i, &w) in widths.iter().enumerate() {
        if x + w > max && i > start {
            out.push(VisualRow { start, end: i, x_off: 0, width: x });
            start = i;
            x = 0;
        }
        x += w;
    }
    out.push(VisualRow { start, end: widths.len(), x_off: 0, width: x });
}

/// 前缀像素宽：`widths[start..k]` 的累加（k∈[start,end]）。
pub fn prefix_width(widths: &[i32], start: usize, k: usize) -> i32 {
    widths[start..k].iter().sum()
}

/// 测量字符串像素宽（绘制用，与每字符宽一致）。
pub fn measure_text(hdc: *mut c_void, text: &[u16]) -> i32 {
    unsafe {
        let mut sz = std::mem::zeroed::<windows_sys::Win32::Foundation::SIZE>();
        let _ = GetTextExtentPoint32W(hdc, text.as_ptr(), text.len() as i32, &mut sz);
        sz.cx
    }
}
