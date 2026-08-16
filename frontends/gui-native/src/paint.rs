//! GDI 绘制工具函数

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetTextExtentPoint32W, HFONT, TEXTMETRICW,
};

use crate::theme::Rgb;

#[link(name = "gdi32")]
extern "system" {
    #[link_name = "SetTextColor"]
    fn gdi_set_text_color(hdc: *mut c_void, color: u32) -> u32;
    #[link_name = "SetBkMode"]
    fn set_bk_mode(hdc: *mut c_void, mode: i32) -> i32;
    #[link_name = "SelectObject"]
    fn select_object(hdc: *mut c_void, h: *mut c_void) -> *mut c_void;
}

pub fn set_text_color(hdc: *mut c_void, color: u32) {
    unsafe { let _ = gdi_set_text_color(hdc, color); }
}

const TRANSPARENT: i32 = 1;

/// 主题感知填充：把 `Rgb` 填到矩形。
pub fn fill_rect(hdc: *mut c_void, rect: &RECT, color: Rgb) {
    fill_rect_rgb(hdc, rect, color.r, color.g, color.b);
}

pub fn fill_rect_rgb(hdc: *mut c_void, rect: &RECT, r: u8, g: u8, b: u8) {
    unsafe {
        let color: u32 = (r as u32) | ((g as u32) << 8) | ((b as u32) << 16);
        let brush = CreateSolidBrush(color);
        FillRect(hdc, rect, brush);
        DeleteObject(brush as *mut c_void);
    }
}

pub fn draw_text(hdc: *mut c_void, x: i32, y: i32, text_w: &[u16]) {
    unsafe {
        set_bk_mode(hdc, TRANSPARENT);
        set_text_color(hdc, 0xE0E0E0);
        let mut r = RECT {
            left: x,
            top: y,
            right: x + 4000,
            bottom: y + 18,
        };
        let _ = DrawTextW(
            hdc,
            text_w.as_ptr(),
            text_w.len() as i32 - 1, // 减掉结尾 \0
            &mut r,
            DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
        );
    }
}

/// 主题感知：带前景色绘制单行文本（左对齐、垂直居中）。
pub fn draw_text_c(hdc: *mut c_void, x: i32, y: i32, color: Rgb, text_w: &[u16]) {
    unsafe {
        set_bk_mode(hdc, TRANSPARENT);
        set_text_color(hdc, color.as_u32());
        let mut r = RECT {
            left: x,
            top: y,
            right: x + 4000,
            bottom: y + 2000,
        };
        let _ = DrawTextW(
            hdc,
            text_w.as_ptr(),
            text_w.len() as i32 - 1,
            &mut r,
            DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
        );
    }
}

const DT_NOPREFIX: u32 = 0x00000008;
const DT_SINGLELINE: u32 = 0x00000020;
const DT_VCENTER: u32 = 0x00000004;

/// 创造字体句柄 (像素高度 = pixel_h)
pub fn create_font(pixel_h: i32, name: &str) -> HFONT {
    unsafe {
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        CreateFontW(
            -pixel_h,
            0, 0, 0, 400,
            0, 0, 0,
            1, // DEFAULT_CHARSET
            0, 0, 0, 0,
            name_w.as_ptr(),
        )
    }
}

/// 在像素字体 (10/12/14...) 下衡量字符串宽
pub fn text_width(hdc: *mut c_void, text: &[u16]) -> i32 {
    unsafe {
        let mut sz = std::mem::zeroed::<SIZE>();
        let _ = GetTextExtentPoint32W(hdc, text.as_ptr(), text.len() as i32, &mut sz);
        sz.cx
    }
}

/// 单行文本高度（像素），用于按钮/标签文本垂直居中。
pub fn text_height(hdc: *mut c_void, text: &[u16]) -> i32 {
    unsafe {
        let mut sz = std::mem::zeroed::<SIZE>();
        let _ = GetTextExtentPoint32W(hdc, text.as_ptr(), text.len() as i32, &mut sz);
        sz.cy
    }
}

pub fn select_font(hdc: *mut c_void, font: HFONT) {
    unsafe {
        let _ = select_object(hdc, font as *mut c_void);
    }
}

/// 安全地选择字体，返回原字体句柄（用于稍后 restore）
pub fn select_font_safe(hdc: *mut c_void, font: HFONT) -> *mut c_void {
    unsafe { select_object(hdc, font as *mut c_void) }
}

pub fn restore_font(hdc: *mut c_void, old: *mut c_void) {
    unsafe {
        let _ = select_object(hdc, old);
    }
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
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

/// 文本水平偏移 (单测)
pub fn text_metrics(hdc: *mut c_void, tm: &mut TEXTMETRICW) {
    unsafe {
        let _ = GetTextMetrics(hdc, tm);
    }
}

/// 绘制文本到指定宽 度内（超出部分被 GDI 剪裁）
pub fn draw_text_clipped(hdc: *mut c_void, x: i32, y: i32, w: i32, _h: i32, text: &[u16]) {
    unsafe {
        set_bk_mode(hdc, TRANSPARENT);
        let mut r = RECT { left: x, top: y, right: x + w.max(1), bottom: y + 1000 };
        let _ = DrawTextW(
            hdc,
            text.as_ptr(),
            text.len() as i32 - 1,
            &mut r,
            DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
        );
    }
}

/// 带色版 `draw_text_clipped`。
pub fn draw_text_clipped_c(hdc: *mut c_void, x: i32, y: i32, w: i32, color: Rgb, text: &[u16]) {
    unsafe {
        set_bk_mode(hdc, TRANSPARENT);
        set_text_color(hdc, color.as_u32());
        let mut r = RECT { left: x, top: y, right: x + w.max(1), bottom: y + 1000 };
        let _ = DrawTextW(
            hdc,
            text.as_ptr(),
            text.len() as i32 - 1,
            &mut r,
            DT_NOPREFIX | DT_SINGLELINE | DT_VCENTER,
        );
    }
}

extern "system" {
    fn GetTextMetrics(hdc: *mut c_void, tm: *mut TEXTMETRICW) -> i32;
}
