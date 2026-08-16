//! Win32 窗口辅助（原生 OS 窗口的圆角等）。
//!
//! 只用于 Windows。Windows 11 可用 `DwmSetWindowAttribute(DWMWA_WINDOW_CORNER_PREFERENCE)`
//! 圆角，但用户机器是 Windows 10（build 19045），该属性不支持 → 用
//! `SetWindowRgn + CreateRoundRectRgn` 把窗口本体裁成圆角（全版本兼容）。

#![cfg(windows)]

use windows_sys::Win32::Foundation::{BOOL, HWND};
use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindow};

/// 按窗口标题找顶层窗口，返回 HWND（找不到返回 0）。
///
/// 器灵子窗口用 `with_title("器灵 AI")` 创建 —— 即使 `with_decorations(false)`
/// 去掉边框，窗口标题字符串仍挂在 HWND 上，`FindWindowW` 按标题能找到它。
pub fn find_window_by_title(title: &str) -> isize {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { FindWindowW(std::ptr::null(), wide.as_ptr()) as isize }
}

/// 句柄是否仍是有效窗口（关掉重开后 HWND 会变，用这个兜底）。
pub fn is_window(hwnd: isize) -> bool {
    unsafe { IsWindow(hwnd as HWND) != 0 }
}

/// 给窗口设置圆角区域。
///
/// - `w` / `h`：窗口**物理像素**尺寸（= 逻辑尺寸 × pixels_per_point）。
/// - `radius`：圆角半径（物理像素）。
///
/// `SetWindowRgn` 成功后系统接管 region 所有权并自动释放，调用方**不要**
/// `DeleteObject`。窗口尺寸/缩放变化时区域会失效，需用新尺寸重设。
pub fn set_rounded_region(hwnd: isize, w: i32, h: i32, radius: i32) {
    let rgn = unsafe { CreateRoundRectRgn(0, 0, w, h, radius, radius) };
    if rgn.is_null() {
        return;
    }
    unsafe {
        SetWindowRgn(hwnd as HWND, rgn, BOOL::from(true));
    }
}
