//! WndProc 消息分发

use std::ffi::c_void;
use std::ptr;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_F1, VK_F3,
    VK_G, VK_HOME, VK_I, VK_L, VK_NEXT, VK_O, VK_PRIOR, VK_RETURN, VK_SHIFT, VK_UP, VK_F,
    VK_END, VK_T, VK_W, VK_C, VK_MENU, VK_OEM_PLUS, VK_OEM_MINUS, VK_0,
};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CallWindowProcW, CreatePopupMenu, DefWindowProcW, DestroyMenu,
    DestroyWindow, GetClientRect, GetParent, GetWindowLongPtrW, GetWindowTextW,
    LoadCursorW, PostMessageW, PostQuitMessage, SetTimer, SetWindowLongPtrW,
    TrackPopupMenu, GWLP_USERDATA, GWLP_WNDPROC,
    IDC_ARROW, TPM_RETURNCMD, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_NCDESTROY, WM_PAINT, WM_RBUTTONDOWN, WM_SIZE, WM_TIMER,
};
extern "system" {
    fn ClientToScreen(hwnd: *mut c_void, lp: *mut POINT) -> i32;
}

const WM_DROPFILES: u32 = 0x0233;

// TrackMouseEvent 在 windows-sys 未暴露，手动声明
#[repr(C)]
struct TrackMouseEventStruct {
    cb_size: u32,
    dw_flags: u32,
    hwnd_track: HWND,
    dw_hover_time: u32,
}
const TME_LEAVE: u32 = 0x0002;
const WM_MOUSELEAVE: u32 = 0x02A3;
extern "system" {
    fn TrackMouseEvent(lp: *mut TrackMouseEventStruct) -> i32;
}

extern "system" {
    fn SetCapture(hwnd: *mut c_void) -> *mut c_void;
    fn ReleaseCapture() -> i32;
    fn InvalidateRect(hwnd: *mut c_void, rect: *const c_void, erase: i32) -> i32;
    fn SetFocus(hwnd: *mut c_void) -> *mut c_void;
}

use crate::app::{ctrl, App};

const CLASS_NAME: &str = "QLogNativeMain";

static CLASS_NAME_BUF: OnceLock<Vec<u16>> = OnceLock::new();

pub fn class_name_wide() -> &'static [u16] {
    CLASS_NAME_BUF.get_or_init(|| {
        CLASS_NAME.encode_utf16().chain(std::iter::once(0)).collect()
    })
}

pub unsafe fn set_window_user_data(hwnd: HWND, app: *mut App) {
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
}
pub unsafe fn get_window_user_data(hwnd: HWND) -> *mut App {
    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App
}

pub fn load_default_cursor() -> *mut c_void {
    unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) }
}

pub unsafe fn dispatch(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = std::mem::zeroed::<PAINTSTRUCT>();
            let hdc = BeginPaint(hwnd, &mut ps);
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let mut rect = std::mem::zeroed::<RECT>();
                GetClientRect(hwnd, &mut rect);
                (*app).paint(hdc, &rect);
            }
            EndPaint(hwnd, &mut ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_SIZE => {
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let _ = (*app).relayout();
                let _ = inv_view(hwnd);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let delta = (wparam >> 16) as i16 as i32;
                let keys = (wparam & 0xFFFF) as u16;
                let ctrl = (keys & 0x0008) != 0;  // MK_CONTROL
                let shift = (keys & 0x0004) != 0;  // MK_SHIFT
                let app_ref = &mut *app;
                if ctrl {
                    if delta > 0 { app_ref.metrics.font_size_px = (app_ref.metrics.font_size_px + 1).min(36); }
                    else if delta < 0 && app_ref.metrics.font_size_px > 8 {
                        app_ref.metrics.font_size_px -= 1;
                    }
                    app_ref.metrics.invalidate();
                } else if shift {
                    // Shift+滚轮 = 横向滚动（delta>0=滚轮上 → 向左）
                    let px = (delta / 120) as i32 * 40;
                    app_ref.scroll.h_scroll_by(-px);
                } else {
                    // 滚轮上(delta>0) → 向上滚（y 减小）
                    let lines = (delta / 40) as i32; // WHEEL_DELTA=120 → 3 行
                    app_ref.scroll.scroll_by_lines(-lines);
                }
                let _ = inv_view(hwnd);
            }
            0
        }
        WM_TIMER => {
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let (search_done, index_done, new_line_count, new_size) = {
                    let app_ref = &mut *app;
                    if let Some(ref mut b) = app_ref.bridge {
                        let (sd, _msg) = b.poll_search();
                        let id = b.poll_index();
                        (sd, id, b.line_count, b.size)
                    } else {
                        (false, false, 0, 0)
                    }
                };
                let app_ref = &mut *app;
                if search_done {
                    app_ref.search.searching = false;
                    if let Some(ref b) = app_ref.bridge {
                        let total = b.search_len();
                        if total > 0 {
                            app_ref.search.total = total;
                            app_ref.search.status = format!("{} 条匹配", total);
                            app_ref.anchor_search_to_viewport();
                        } else {
                            app_ref.search.status = "无匹配".into();
                            app_ref.current_hit_byte = None;
                        }
                        app_ref.search_status = app_ref.search.status.clone();
                    }
                }
                if index_done {
                    app_ref.file_lines = new_line_count;
                    app_ref.file_size = new_size;
                    app_ref.status_text = format!(
                        "已打开 · {} 行 · {}",
                        new_line_count,
                        crate::paint::human_bytes(new_size),
                    );
                }
                if search_done || index_done {
                    let _ = inv_view(hwnd);
                }
                // 后台活动时加快轮询，空闲恢复 500ms
                let active = app_ref.search.searching
                    || app_ref.bridge.as_ref().map_or(false, |b| b.indexing_active());
                let _ = if active {
                    SetTimer(hwnd, 1, 100, None)
                } else {
                    SetTimer(hwnd, 1, 500, None)
                };
            }
            0
        }
        WM_KEYDOWN => {
            let app = get_window_user_data(hwnd);
            if app.is_null() {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let ctrl = unsafe { GetAsyncKeyState(VK_CONTROL as i32) < 0 };
            let shift = unsafe { GetAsyncKeyState(VK_SHIFT as i32) < 0 };
            let vk = wparam as u16;
            let app_ref = &mut *app;

            // 全局快捷键
            let consumed = if ctrl {
                match vk {
                    VK_O => { crate::shell::pick_file().map(|p| app_ref.open_path(p)); true }
                    VK_F => { let _ = SetFocus(app_ref.h_edit_search); true }
                    VK_L => { let _ = SetFocus(app_ref.h_edit_goto); true }
                    VK_I => {
                        crate::dlg::show_properties(hwnd, app_ref);
                        true
                    }
                    VK_W => {
                        app_ref.close_file();
                        true
                    }
                    VK_T if shift => {
                        app_ref.cycle_theme();
                        crate::menu::rebuild(app_ref);
                        true
                    }
                    VK_G => {
                        app_ref.jump_hit(if shift { -1 } else { 1 });
                        true
                    }
                    VK_C => {
                        copy_selection(app_ref, hwnd);
                        true
                    }
                    VK_OEM_PLUS => { app_ref.font_inc(); true }
                    VK_OEM_MINUS => { app_ref.font_dec(); true }
                    VK_0 => { app_ref.font_reset(); true }
                    _ => false,
                }
            } else {
                false
            };

            // 单键快捷键
            let consumed2 = match vk {
                VK_ESCAPE => {
                    app_ref.clear_search();
                    true
                }
                VK_HOME => { app_ref.scroll.top(); true }
                VK_END => {
                    app_ref.scroll.bottom(app_ref.total_lines());
                    true
                }
                VK_PRIOR | VK_NEXT | VK_UP | VK_DOWN if !ctrl => {
                    let total = app_ref.total_lines();
                    let page = app_ref.scroll.page_size_lines.max(1);
                    let n = match vk {
                        VK_PRIOR => -(page as i32) + (page / 4) as i32,
                        VK_NEXT => (page as i32) - (page / 4) as i32,
                        VK_UP => -1,
                        VK_DOWN => 1,
                        _ => 0,
                    };
                    if vk == VK_PRIOR || vk == VK_UP {
                        app_ref.scroll.scroll_by_lines(n);
                    } else if vk == VK_NEXT {
                        app_ref.scroll.page_down(total);
                    } else {
                        app_ref.scroll.scroll_by_lines(n);
                    }
                    true
                }
                VK_F3 => {
                    app_ref.jump_hit(if shift { -1 } else { 1 });
                    true
                }
                VK_F1 => {
                    crate::dlg::show_help(hwnd);
                    true
                }
                _ => false,
            };

            if consumed || consumed2 {
                let _ = inv_view(hwnd);
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as u16;
            let code = (wparam >> 16) as u16;
            let app_ptr = get_window_user_data(hwnd);
            if app_ptr.is_null() {
                return 0;
            }

            // 1. 来自菜单 / 加速键 (HIWORD == 0)
            if code == 0 {
                if crate::menu::matches(id) {
                    crate::menu::dispatch(id, &mut *app_ptr, hwnd);
                    let _ = inv_view(hwnd);
                    return 0;
                }
            }

            // 2. 来自按钮 / 编辑框通知
            let app_ref = &mut *app_ptr;
            match id {
                ctrl::BTN_OPEN => {
                    if let Some(p) = crate::shell::pick_file() {
                        app_ref.open_path(p);
                    }
                }
                ctrl::BTN_CLOSE => {
                    app_ref.close_file();
                    let _ = inv_view(hwnd);
                }
                ctrl::BTN_RELOAD => {
                    if let Some(p) = app_ref.path.clone() {
                        app_ref.open_path(p);
                    }
                }
                ctrl::BTN_SEARCH => {
                    app_ref.submit_search();
                }
                ctrl::BTN_PREV => {
                    app_ref.jump_hit(-1);
                }
                ctrl::BTN_NEXT => {
                    app_ref.jump_hit(1);
                }
                ctrl::BTN_GOTO => {
                    app_ref.submit_goto();
                }
                ctrl::BTN_FONT_BIGGER => {
                    app_ref.font_inc();
                }
                ctrl::BTN_FONT_SMALLER => {
                    app_ref.font_dec();
                }
                _ => {}
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i32;
            let y = (lparam >> 16) as i32;
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let app_ref = &mut *app;
                // 工具栏按钮：按下（释放时执行）
                if y < crate::app::TOOLBAR_H {
                    let mut r = std::mem::zeroed::<RECT>();
                    GetClientRect(hwnd, &mut r);
                    if let Some(act) = crate::toolbar::hit(x, y, r.right, app_ref.search.searching) {
                        app_ref.toolbar_pressed = Some(act);
                        SetCapture(hwnd);
                        crate::toolbar::invalidate(app_ref);
                        return 0;
                    }
                }
                if pt_in_rect(x, y, &app_ref.vsb_thumb) {
                    app_ref.scroll.thumb_dragging = true;
                    app_ref.scroll.drag_start_mouse = y;
                    app_ref.scroll.drag_start_scroll_y = app_ref.scroll.y;
                    app_ref.scroll.drag_track_len = app_ref.vsb_track.bottom - app_ref.vsb_track.top;
                    app_ref.scroll.drag_thumb_len = app_ref.vsb_thumb.bottom - app_ref.vsb_thumb.top;
                    // 有效行数 = 逻辑行 × wrap 因子（scroll.y 是有效行单位）
                    let total_rows = (app_ref.total_lines() as f64 * app_ref.scroll.wrap_factor.max(1.0)) as u64;
                    app_ref.scroll.drag_total_lines = total_rows.max(1);
                    unsafe { SetCapture(hwnd); }
                    return 0;
                }
                if pt_in_rect(x, y, &app_ref.hsb_thumb) {
                    app_ref.scroll.thumb_dragging = true;
                    app_ref.scroll.drag_start_mouse = x;
                    app_ref.scroll.drag_start_h_scroll = app_ref.scroll.h_scroll;
                    app_ref.scroll.drag_track_len = app_ref.hsb_track.right - app_ref.hsb_track.left;
                    app_ref.scroll.drag_thumb_len = app_ref.hsb_thumb.right - app_ref.hsb_thumb.left;
                    app_ref.scroll.drag_max_scroll_px = app_ref.scroll.max_h_scroll_px;
                    unsafe { SetCapture(hwnd); }
                    return 0;
                }
                // 状态栏：编码 / 批注 / 进度取消
                let mut cr = std::mem::zeroed::<RECT>();
                GetClientRect(hwnd, &mut cr);
                if y >= cr.bottom - crate::app::STATUSBAR_H {
                    if pt_in_rect(x, y, &app_ref.progress_cancel_rect) {
                        if let Some(ref b) = app_ref.bridge {
                            b.cancel_index();
                        }
                        let _ = inv_view(hwnd);
                        return 0;
                    }
                    if pt_in_rect(x, y, &app_ref.status_rects.enc) {
                        crate::dlg::show_encoding_menu(hwnd, app_ref, x, y);
                        let _ = inv_view(hwnd);
                        return 0;
                    }
                    if pt_in_rect(x, y, &app_ref.status_rects.ann) {
                        crate::dlg::show_annotation_list(hwnd, app_ref);
                        return 0;
                    }
                }
                // 视图文本区：开始文本选择
                if y >= crate::app::TOOLBAR_H + 2 && x < app_ref.view.text_right {
                    let (line, col) = crate::view::pixel_to_line_col(x, y, app_ref);
                    app_ref.selection = Some(crate::selection::Selection {
                        start_line: line, start_col: col, end_line: line, end_col: col,
                    });
                    app_ref.selecting = true;
                    unsafe { SetCapture(hwnd); }
                    let _ = inv_view(hwnd);
                    return 0;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEMOVE => {
            let x = (lparam & 0xFFFF) as i32;
            let y = (lparam >> 16) as i32;
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let app_ref = &mut *app;
                // 工具栏悬停反馈
                if y < crate::app::TOOLBAR_H {
                    let mut r = std::mem::zeroed::<RECT>();
                    GetClientRect(hwnd, &mut r);
                    let hover = crate::toolbar::hit(x, y, r.right, app_ref.search.searching);
                    if hover != app_ref.toolbar_hover {
                        app_ref.toolbar_hover = hover;
                        crate::toolbar::invalidate(app_ref);
                        // 进入窗口后请求 WM_MOUSELEAVE，离开时清悬停
                        let mut tme = TrackMouseEventStruct {
                            cb_size: std::mem::size_of::<TrackMouseEventStruct>() as u32,
                            dw_flags: TME_LEAVE,
                            hwnd_track: hwnd,
                            dw_hover_time: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    return 0;
                } else if app_ref.toolbar_hover.is_some() {
                    app_ref.toolbar_hover = None;
                    crate::toolbar::invalidate(app_ref);
                }
                // 文本选择拖拽 + 越界自动滚动
                if app_ref.selecting {
                    const AUTO_SCROLL_RATE: f64 = 0.12;
                    let view_top = crate::app::TOOLBAR_H + 2;
                    let view_bottom = app_ref.view.view_top + app_ref.view.view_h;
                    let mut mx = x;
                    let mut my = y;
                    if my < view_top {
                        let d = (view_top - my) as f64;
                        app_ref.scroll.scroll_by_lines(-(d * AUTO_SCROLL_RATE) as i32);
                        my = view_top;
                    } else if my > view_bottom {
                        let d = (my - view_bottom) as f64;
                        app_ref.scroll.scroll_by_lines((d * AUTO_SCROLL_RATE) as i32);
                        my = view_bottom;
                    }
                    if !app_ref.config.gui.word_wrap && app_ref.scroll.max_h_scroll_px > 0.0 {
                        if mx < app_ref.view.content_x {
                            let d = (app_ref.view.content_x - mx) as f64;
                            app_ref.scroll.h_scroll = (app_ref.scroll.h_scroll + (d * AUTO_SCROLL_RATE) as i64).max(0);
                            mx = app_ref.view.content_x;
                        } else if mx > app_ref.view.text_right {
                            let d = (mx - app_ref.view.text_right) as f64;
                            app_ref.scroll.h_scroll = app_ref.scroll.h_scroll + (d * AUTO_SCROLL_RATE) as i64;
                            mx = app_ref.view.text_right;
                        }
                    }
                    let (line, col) = crate::view::pixel_to_line_col(mx, my, app_ref);
                    match app_ref.selection {
                        Some(mut sel) => {
                            sel.end_line = line;
                            sel.end_col = col;
                            app_ref.selection = Some(sel);
                        }
                        None => {
                            app_ref.selection = Some(crate::selection::Selection {
                                start_line: line, start_col: col, end_line: line, end_col: col,
                            });
                        }
                    }
                    let _ = inv_view(hwnd);
                    return 0;
                }
                if app_ref.scroll.thumb_dragging {
                    let track_len = app_ref.scroll.drag_track_len.max(1) as f64;
                    let thumb_len = app_ref.scroll.drag_thumb_len.max(1) as f64;
                    let scrollable = track_len - thumb_len;
                    if app_ref.scroll.drag_total_lines > 0 {
                        // 纵向拖动：使用 Y 坐标
                        let d = (y - app_ref.scroll.drag_start_mouse) as f64;
                        let total_h = app_ref.scroll.drag_total_lines as f64 * app_ref.row_h as f64;
                        let view_h = app_ref.scroll.page_size_lines as f64 * app_ref.row_h as f64;
                        let max_scroll = (total_h - view_h).max(0.0);
                        if scrollable > 0.0 {
                            let frac = d / scrollable;
                            let dy = (frac * max_scroll / app_ref.row_h as f64) as i64;
                            app_ref.scroll.y = (app_ref.scroll.drag_start_scroll_y + dy).max(0);
                        }
                    } else if app_ref.scroll.drag_max_scroll_px > 0.0 {
                        // 横向拖动：使用 X 坐标
                        let d = (x - app_ref.scroll.drag_start_mouse) as f64;
                        if scrollable > 0.0 {
                            let frac = d / scrollable;
                            let dx = (frac * app_ref.scroll.drag_max_scroll_px) as i64;
                            app_ref.scroll.h_scroll = (app_ref.scroll.drag_start_h_scroll + dx).max(0);
                        }
                    }
                    let _ = inv_view(hwnd);
                    return 0;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONUP => {
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let app_ref = &mut *app;
                if let Some(act) = app_ref.toolbar_pressed.take() {
                    unsafe { ReleaseCapture(); }
                    crate::toolbar::dispatch(act, app_ref);
                    crate::toolbar::invalidate(app_ref);
                    return 0;
                }
                if app_ref.selecting {
                    app_ref.selecting = false;
                    unsafe { ReleaseCapture(); }
                    let _ = inv_view(hwnd);
                    return 0;
                }
                app_ref.scroll.thumb_dragging = false;
                unsafe { ReleaseCapture(); }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_RBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i32;
            let y = (lparam >> 16) as i32;
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let app_ref = &mut *app;
                if y >= crate::app::TOOLBAR_H + 2 && app_ref.selection.is_some() {
                    let mut pt = POINT { x, y };
                    let _ = ClientToScreen(hwnd, &mut pt);
                    let hmenu = CreatePopupMenu();
                    let c1 = crate::app::str_wide("复制选中内容");
                    let c2 = crate::app::str_wide("添加批注");
                    AppendMenuW(hmenu, 0x00000000 /* MF_STRING */, 1, c1.as_ptr());
                    AppendMenuW(hmenu, 0x00000000 /* MF_STRING */, 2, c2.as_ptr());
                    let cmd = TrackPopupMenu(hmenu, TPM_RETURNCMD | 0x00000002 /* TPM_RIGHTBUTTON */, pt.x, pt.y, 0, hwnd, std::ptr::null());
                    DestroyMenu(hmenu);
                    match cmd {
                        1 => copy_selection(app_ref, hwnd),
                        2 => {
                            crate::dlg::show_annotation_edit(hwnd, app_ref, None);
                        }
                        _ => {}
                    }
                    return 0;
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSELEAVE => {
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                let app_ref = &mut *app;
                if app_ref.toolbar_hover.is_some() {
                    app_ref.toolbar_hover = None;
                    crate::toolbar::invalidate(app_ref);
                }
            }
            0
        }
        WM_DROPFILES => {
            use windows_sys::Win32::UI::Shell::{DragFinish, DragQueryFileW};
            let hdrop = wparam as *mut c_void;
            let mut buf = [0u16; 1024];
            let n = DragQueryFileW(hdrop, 0, buf.as_mut_ptr(), buf.len() as u32);
            if n > 0 {
                let path = String::from_utf16_lossy(&buf[..n as usize]);
                let app = get_window_user_data(hwnd);
                if !app.is_null() {
                    (*app).open_path(std::path::PathBuf::from(path));
                }
            }
            DragFinish(hdrop);
            0
        }
        WM_CLOSE => { DestroyWindow(hwnd); 0 }
        WM_DESTROY => {
            let app = get_window_user_data(hwnd);
            if !app.is_null() {
                crate::view::destroy_backbuf(&mut *app);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub fn focus_search(app: &mut App) {
    unsafe { let _ = SetFocus(app.h_edit_search); }
}
pub fn focus_goto(app: &mut App) {
    unsafe { let _ = SetFocus(app.h_edit_goto); }
}

fn pt_in_rect(x: i32, y: i32, r: &RECT) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

/// 把文本复制进系统剪贴板（CF_UNICODETEXT）。
fn copy_to_clipboard(hwnd: HWND, text: &str) {
    unsafe {
        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        if OpenClipboard(hwnd) == 0 {
            return;
        }
        let _ = EmptyClipboard();
        let bytes = units.len() * 2;
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if !h.is_null() {
            let p = GlobalLock(h);
            if !p.is_null() {
                std::ptr::copy_nonoverlapping(units.as_ptr(), p as *mut u16, units.len());
                GlobalUnlock(h);
                let _ = SetClipboardData(13 /* CF_UNICODETEXT */, h);
            }
        }
        CloseClipboard();
    }
}

/// 复制当前选中内容到剪贴板。
fn copy_selection(app: &App, hwnd: HWND) {
    if let Some(sel) = app.selection {
        if let Some(t) = crate::selection::copy_text(app, &sel) {
            copy_to_clipboard(hwnd, &t);
        }
    }
}

// ── EDIT 子类化：搜索框 Enter=搜索 / Shift+Enter=换行；跳行框 Enter=跳行 ──

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Search,
    Goto,
}

type OrigProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> isize;

static SUBCLASSED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<isize, (isize, EditKind)>>> =
    std::sync::OnceLock::new();

fn subclass_map() -> &'static std::sync::Mutex<std::collections::HashMap<isize, (isize, EditKind)>> {
    SUBCLASSED.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 给编辑框装子类 proc，拦截 Enter。
pub fn subclass_edit(hwnd: HWND, kind: EditKind) {
    unsafe {
        if hwnd.is_null() {
            return;
        }
        let orig = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        subclass_map().lock().unwrap().insert(hwnd as isize, (orig, kind));
        SetWindowLongPtrW(hwnd, GWLP_WNDPROC, edit_wndproc as *const () as isize);
    }
}

unsafe extern "system" fn edit_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> isize {
    let (orig, kind) = match subclass_map().lock().unwrap().get(&(hwnd as isize)) {
        Some(&v) => v,
        None => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };
    if msg == WM_KEYDOWN {
        let vk = wparam as u16;
        if vk == VK_RETURN {
            match kind {
                EditKind::Search => {
                    let shift = GetAsyncKeyState(VK_SHIFT as i32) < 0;
                    if !shift {
                        // Enter → 搜索（不插入换行）
                        let parent = GetParent(hwnd);
                        let _ = PostMessageW(parent, WM_COMMAND, crate::app::ctrl::BTN_SEARCH as usize, hwnd as isize);
                        return 0;
                    }
                    // Shift+Enter → 走默认（插入换行）
                }
                EditKind::Goto => {
                    let parent = GetParent(hwnd);
                    let _ = PostMessageW(parent, WM_COMMAND, crate::app::ctrl::BTN_GOTO as usize, hwnd as isize);
                    return 0;
                }
            }
        } else if vk == VK_DOWN && kind == EditKind::Search {
            // Alt+↓ 在搜索历史中循环
            let alt = GetAsyncKeyState(VK_MENU as i32) < 0;
            if alt {
                let parent = GetParent(hwnd);
                let app = get_window_user_data(parent);
                if !app.is_null() {
                    let app_ref = &*app;
                    if !app_ref.config.search_history.is_empty() {
                        let mut buf = [0u16; 512];
                        let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
                        let cur = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                        let hist = &app_ref.config.search_history;
                        let mut idx = hist.iter().position(|h| *h == cur).map(|i| i + 1).unwrap_or(0);
                        if idx >= hist.len() {
                            idx = 0;
                        }
                        if let Some(text) = hist.get(idx) {
                            let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                            let _ = windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(hwnd, w.as_ptr());
                        }
                    }
                }
                return 0;
            }
        }
    }
    if msg == WM_NCDESTROY {
        subclass_map().lock().unwrap().remove(&(hwnd as isize));
    }
    let orig_fn = std::mem::transmute::<isize, OrigProc>(orig);
    CallWindowProcW(Some(orig_fn), hwnd, msg, wparam, lparam)
}

// ── 工具栏自绘按钮命中 ──

/// 命中工具栏按钮并执行动作；命中「编辑框区域」返回 false（让给子控件）。
fn hit_toolbar(app: &mut App, hwnd: HWND, x: i32, y: i32) -> bool {
    unsafe {
        let mut r = std::mem::zeroed::<RECT>();
        GetClientRect(hwnd, &mut r);
        let w = r.right;
        if y < 0 || y >= crate::app::TOOLBAR_H {
            return false;
        }
        if let Some(act) = crate::toolbar::hit(x, y, w, app.search.searching) {
            crate::toolbar::dispatch(act, app);
            return true;
        }
        false
    }
}

/// 只重绘视图区+状态栏，跳过工具栏（避免子控件闪烁）
unsafe fn inv_view(hwnd: HWND) {
    let mut r = std::mem::zeroed::<RECT>();
    GetClientRect(hwnd, &mut r);
    let top = crate::app::TOOLBAR_H + 2;
    let inv = RECT { left: 0, top, right: r.right, bottom: r.bottom };
    InvalidateRect(hwnd, &inv as *const RECT as *const c_void, 0);
}
