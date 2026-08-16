//! 自绘组件层 —— 一套符合 qview 风格（现代、扁平、圆角、主题化）的 UI 组件。
//!
//! 设计原则：
//! - **纯自绘**：按钮/标签/面板/标题栏直接画进 DC，命中测试由调用方负责，
//!   与主窗口/对话框的全窗口双缓冲一致。
//! - **8px 网格**：组件间距、内边距都取 8 的倍数。
//! - **圆角**：按钮 5px、卡片 8px，用 `CreateRoundRectRgn` + `FillRgn`。
//! - **状态色**：hover 变亮、pressed 变暗，取自主题的 bg_hover/bg_active。
//!
//! 文本输入框（EDIT）是唯一仍用系统子控件的组件，但通过 `TextInput::create`
//! 统一创建（正确样式 + 主题色 + 聚焦边框）。

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    CreateRoundRectRgn, CreateSolidBrush, DeleteObject, FillRgn,
};

use crate::app::str_wide;
use crate::paint;
use crate::theme::{Rgb, ThemeColors};

extern "system" {
    fn InvalidateRect(hwnd: *mut c_void, rect: *const c_void, erase: i32) -> i32;
}

// ────────────────────────────────────────────────────────────────────────
// 圆角矩形
// ────────────────────────────────────────────────────────────────────────

/// 圆角填充矩形（radius 像素）。
pub fn round_rect(hdc: *mut c_void, r: &RECT, radius: i32, color: Rgb) {
    unsafe {
        let rr = radius.max(1);
        let rgn = CreateRoundRectRgn(r.left, r.top, r.right + 1, r.bottom + 1, rr * 2, rr * 2);
        if rgn.is_null() {
            paint::fill_rect(hdc, r, color);
            return;
        }
        let brush = CreateSolidBrush(color.as_u32());
        let _ = FillRgn(hdc, rgn, brush);
        DeleteObject(brush as *mut c_void);
        DeleteObject(rgn as *mut c_void);
    }
}

// ────────────────────────────────────────────────────────────────────────
// 按钮
// ────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BtnKind {
    Primary,
    Secondary,
    Danger,
    Neutral,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BtnState {
    Normal,
    Hover,
    Pressed,
    Disabled,
}

#[derive(Clone)]
pub struct Button {
    pub rect: RECT,
    pub kind: BtnKind,
    pub state: BtnState,
    pub label: String,
}

impl Button {
    pub fn new(rect: RECT, kind: BtnKind, label: &str) -> Self {
        Self { rect, kind, state: BtnState::Normal, label: label.to_string() }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.rect.left && x <= self.rect.right && y >= self.rect.top && y <= self.rect.bottom
    }

    pub fn base_color(&self, theme: &ThemeColors) -> Rgb {
        match self.kind {
            BtnKind::Primary => theme.btn_primary,
            BtnKind::Secondary => theme.bg_tertiary,
            BtnKind::Danger => theme.btn_danger,
            BtnKind::Neutral => theme.btn_neutral,
        }
    }

    pub fn draw(&self, hdc: *mut c_void, theme: &ThemeColors) {
        let base = self.base_color(theme);
        let bg = match self.state {
            BtnState::Normal => base,
            BtnState::Hover => base.blend(theme.bg_hover, 90),
            BtnState::Pressed => base.blend(Rgb { r: 0, g: 0, b: 0 }, 40),
            BtnState::Disabled => theme.bg_tertiary,
        };
        round_rect(hdc, &self.rect, 5, bg);
        // 文字（水平 + 垂直居中）
        let color = match self.state {
            BtnState::Disabled => theme.text_disabled,
            _ => theme.text_primary,
        };
        let label_w = str_wide(&self.label);
        let tw = paint::text_width(hdc, &label_w);
        let th = paint::text_height(hdc, &label_w);
        let cx = self.rect.left + (self.rect.right - self.rect.left - tw) / 2;
        let cy = self.rect.top + (self.rect.bottom - self.rect.top - th) / 2;
        paint::draw_text_c(hdc, cx.max(self.rect.left + 2), cy, color, &label_w);
    }
}

// ────────────────────────────────────────────────────────────────────────
// 标签 / 文本
// ────────────────────────────────────────────────────────────────────────

pub struct Label {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub color: Rgb,
    pub text: String,
}

impl Label {
    pub fn new(x: i32, y: i32, w: i32, color: Rgb, text: &str) -> Self {
        Self { x, y, w, color, text: text.to_string() }
    }
    pub fn draw(&self, hdc: *mut c_void) {
        paint::draw_text_clipped_c(hdc, self.x, self.y, self.w.max(1), self.color, &str_wide(&self.text));
    }
}

/// 次级文本（theme.text_secondary）。
pub fn label_secondary(hdc: *mut c_void, x: i32, y: i32, w: i32, theme: &ThemeColors, text: &str) {
    paint::draw_text_clipped_c(hdc, x, y, w.max(1), theme.text_secondary, &str_wide(text));
}

// ────────────────────────────────────────────────────────────────────────
// 面板 / 卡片
// ────────────────────────────────────────────────────────────────────────

/// 圆角卡片容器（bg_secondary，8px 圆角，可带边框）。
pub fn panel(hdc: *mut c_void, r: &RECT, theme: &ThemeColors) {
    round_rect(hdc, r, 8, theme.bg_secondary);
}

/// 分隔线（水平）。
pub fn hline(hdc: *mut c_void, x: i32, y: i32, w: i32, color: Rgb) {
    paint::fill_rect(hdc, &RECT { left: x, top: y, right: x + w, bottom: y + 1 }, color);
}

// ────────────────────────────────────────────────────────────────────────
// 标题栏（对话框用）
// ────────────────────────────────────────────────────────────────────────

pub const TITLE_H: i32 = 36;

/// 关闭按钮矩形（标题栏右侧 30×30）。
pub fn close_rect(w: i32) -> RECT {
    RECT { left: w - 30, top: 4, right: w - 4, bottom: 30 }
}

/// 绘制对话框标题栏（自绘，含关闭 × 按钮）。
pub fn title_bar(hdc: *mut c_void, w: i32, theme: &ThemeColors, title: &str, close_hover: bool) {
    let bar = RECT { left: 0, top: 0, right: w, bottom: TITLE_H };
    paint::fill_rect(hdc, &bar, theme.bg_secondary);
    // 底部细分隔线
    paint::fill_rect(hdc, &RECT { left: 0, top: TITLE_H - 1, right: w, bottom: TITLE_H }, theme.bg_hover);
    // 标题
    paint::draw_text_c(hdc, 16, 8, theme.text_primary, &str_wide(title));
    // 关闭按钮
    let c = close_rect(w);
    let cbg = if close_hover { theme.bg_hover } else { theme.bg_secondary };
    round_rect(hdc, &c, 5, cbg);
    // 画 ×（两条斜线近似）
    paint::draw_text_c(hdc, c.left + 6, c.top + 2, theme.text_secondary, &str_wide("x"));
}

pub fn hit_close(w: i32, x: i32, y: i32) -> bool {
    let c = close_rect(w);
    x >= c.left && x <= c.right && y >= c.top && y <= c.bottom
}

/// 命中标题栏（非关闭钮区域）→ 可拖动。
pub fn hit_titlebar(y: i32) -> bool {
    y >= 0 && y < TITLE_H
}

// ────────────────────────────────────────────────────────────────────────
// 文本输入框（EDIT 封装）
// ────────────────────────────────────────────────────────────────────────

pub mod input {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, SendMessageW, SetWindowLongPtrW, ES_LEFT, ES_MULTILINE, ES_AUTOVSCROLL,
        ES_READONLY, ES_WANTRETURN, GWLP_WNDPROC, WM_KILLFOCUS, WM_NCDESTROY,
        WM_SETFOCUS, WS_CHILD, WS_CLIPSIBLINGS, WS_EX_CLIENTEDGE, WS_VISIBLE,
    };
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// 创建多行文本输入框（统一样式 + 聚焦边框子类化）。
    /// `focus_border=true` 时聚焦/失焦会重绘父窗口以更新边框。
    pub unsafe fn create(parent: HWND, r: RECT, id: u16, multiline: bool, readonly: bool) -> HWND {
        let instance = GetModuleHandleW(std::ptr::null());
        let class: Vec<u16> = "EDIT".encode_utf16().chain(std::iter::once(0)).collect();
        let empty: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
        let mut style = ES_LEFT;
        if multiline {
            style |= ES_MULTILINE | ES_AUTOVSCROLL;
            if !readonly {
                style |= ES_WANTRETURN;
            }
        }
        if readonly {
            style |= ES_READONLY;
        }
        let hwnd = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            class.as_ptr(),
            empty.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | style as u32,
            r.left, r.top, r.right - r.left, r.bottom - r.top,
            parent, id as usize as *mut c_void, instance, std::ptr::null(),
        );
        if !hwnd.is_null() {
            // 子类化以在聚焦/失焦时通知父窗口重绘边框
            let orig = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, edit_proc as *const () as isize);
            EDIT_PROCS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().insert(hwnd as isize, orig);
        }
        hwnd
    }

    pub unsafe fn set_text(hwnd: HWND, text: &str) {
        let t = str_wide(text);
        let _ = SendMessageW(hwnd, 0x000C /* WM_SETTEXT */, 0, t.as_ptr() as isize);
    }
    pub unsafe fn get_text(hwnd: HWND) -> String {
        if hwnd.is_null() {
            return String::new();
        }
        let len = SendMessageW(hwnd, 0x000E /* WM_GETTEXTLENGTH */, 0, 0);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        SendMessageW(hwnd, 0x000D /* WM_GETTEXT */, buf.len() as usize, buf.as_mut_ptr() as isize);
        String::from_utf16_lossy(&buf[..len as usize])
    }

    static EDIT_PROCS: OnceLock<Mutex<HashMap<isize, isize>>> = OnceLock::new();

    unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
        if msg == WM_SETFOCUS || msg == WM_KILLFOCUS {
            // 通知父窗口重绘（聚焦边框）
            let parent = windows_sys::Win32::UI::WindowsAndMessaging::GetParent(hwnd);
            let _ = InvalidateRect(parent, std::ptr::null(), 0);
        }
        if msg == WM_NCDESTROY {
            EDIT_PROCS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().remove(&(hwnd as isize));
        }
        let orig = EDIT_PROCS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap().get(&(hwnd as isize)).copied().unwrap_or(0);
        if orig == 0 {
            return windows_sys::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wp, lp);
        }
        let orig_fn = std::mem::transmute::<isize, unsafe extern "system" fn(HWND, u32, usize, isize) -> isize>(orig);
        windows_sys::Win32::UI::WindowsAndMessaging::CallWindowProcW(Some(orig_fn), hwnd, msg, wp, lp)
    }
}

// ────────────────────────────────────────────────────────────────────────
// 提示（Tooltip）
// ────────────────────────────────────────────────────────────────────────

/// 画一个简单的悬停提示（圆角小卡片，光标附近）。
pub fn tooltip(hdc: *mut c_void, x: i32, y: i32, theme: &ThemeColors, text: &str) {
    let tw = paint::text_width(hdc, &str_wide(text));
    let w = tw + 16;
    let h = 24;
    // 防止超出屏幕右缘
    let x = if x + w > 1200 { x - w - 8 } else { x + 12 };
    let r = RECT { left: x, top: y - h - 8, right: x + w, bottom: y - 8 };
    round_rect(hdc, &r, 6, Rgb { r: 40, g: 42, b: 48 });
    paint::draw_text_c(hdc, x + 8, r.top + 4, theme.text_primary, &str_wide(text));
}
