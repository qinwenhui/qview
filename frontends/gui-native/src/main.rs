//! qview-gui-native — 极简 Win32/GDI 前端 (轻量，仅 ~12 MiB RSS)
//!
//! 设计：
//! - 直接调用 kernel32 / user32 / gdi32 — 零 GUI framework
//! - qview-core 提供 mmap / 索引 / 搜索

#![cfg(windows)]
#![windows_subsystem = "windows"]
#![allow(dead_code)] // Win32 应用中大量 FFI/辅助 API 按需使用，允许未使用的表面

mod annotations;
mod app;
mod config;
mod diagnostics;
mod dlg;
mod engine_bridge;
mod fontmgr;
mod layout;
mod menu;
mod msg;
mod paint;
mod scroll;
mod selection;
mod settings;
mod shell;
mod statusbar;
mod theme;
mod toolbar;
mod view;
mod widgets;

use std::ffi::c_void;
use std::ptr;

use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
const WM_MOUSEWHEEL: u32 = 0x020A;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, RegisterClassExW, SetTimer, ShowWindow, TranslateMessage,
    CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, KillTimer, MSG, SW_SHOWDEFAULT, WNDCLASSEXW,
};

use crate::app::App;
use crate::msg::dispatch;

const CLASS_NAME: &str = "QLogNativeMain";

unsafe extern "system" fn wndproc(
    hwnd: *mut c_void,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    dispatch(hwnd, msg, wparam, lparam)
}

fn install_window_class() -> bool {
    unsafe {
        let instance = GetModuleHandleW(ptr::null());

        let class_name_wide: Vec<u16> = CLASS_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let wcex = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: ptr::null_mut(),
            hCursor: msg::load_default_cursor(),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name_wide.as_ptr(),
            hIconSm: ptr::null_mut(),
        };

        RegisterClassExW(&wcex) != 0
    }
}

fn main() {
    diagnostics::log_initial();

    if !install_window_class() {
        eprintln!("RegisterClassExW failed");
        return;
    }

    unsafe {
        let instance = GetModuleHandleW(ptr::null());

        let mut app = App::new();
        if let Some(path) = std::env::args().nth(1) {
            app.open_path(std::path::PathBuf::from(path));
        }
        let hwnd = app.create_main_window(instance);
        if hwnd.is_null() {
            eprintln!("CreateWindowExW failed");
            return;
        }

        let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);

        // 拖拽文件打开
        use windows_sys::Win32::UI::Shell::DragAcceptFiles;
        DragAcceptFiles(hwnd, 1);

        diagnostics::log_post_window();
        diagnostics::spawn_periodic_reporter();

        // 500ms 定时器 → poll 索引/搜索进度，但不强制重绘
        SetTimer(hwnd, 1, 500, None);

        // 主消息循环：截住 WM_MOUSEWHEEL 在子控件之前处理
        let mut msg = std::mem::zeroed::<MSG>();
        loop {
            let r = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
            if r == 0 || r == -1 {
                break;
            }

            // 子控件 (EDIT/BUTTON) 会吞掉滚轮；在主循环截获前转发给父窗口
            if msg.message == WM_MOUSEWHEEL {
                dispatch(hwnd, msg.message, msg.wParam, msg.lParam);
                continue;
            }

            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        KillTimer(hwnd, 1);
        diagnostics::log_exit();
    }
}
