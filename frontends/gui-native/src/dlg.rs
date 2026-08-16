//! 对话框：基于 `widgets` 组件层的现代模态弹窗。
//!
//! 特性：
//! - 无边框（WS_POPUP）自绘窗口：圆角标题栏 + 关闭 x 按钮，标题栏可拖动
//!   （WM_NCHITTEST 返回 HTCAPTION）。
//! - 按钮全部用 `widgets::Button` 自绘（圆角、hover/pressed 状态）。
//! - 文本输入用 `widgets::input`（正确样式 + 主题色 + 聚焦边框）。
//! - 模态循环内联运行，`DlgCtx` 由调用方持有（栈上），WM_DESTROY 不释放
//!   （修复此前 Box::from_raw(栈指针) 崩溃）。

use std::ffi::c_void;
use std::ptr;

use qview_core::annotation::Annotation;
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, DeleteObject, EndPaint, GetDC, ReleaseDC, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetClientRect, GetMessageW, GetWindowLongPtrW, IsWindow, LoadCursorW,
    RegisterClassExW, SendMessageW, SetWindowLongPtrW, ShowWindow,
    TrackPopupMenu, TranslateMessage, CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA,
    HTCAPTION, HTCLIENT, IDC_ARROW, MSG, SW_SHOW, TPM_RETURNCMD, WM_CLOSE, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WS_CHILD, WS_CLIPSIBLINGS, WS_POPUP,
    WS_VISIBLE, MF_CHECKED, MF_STRING,
};
extern "system" {
    fn ScreenToClient(hwnd: HWND, lp: *mut POINT) -> i32;
}

extern "system" {
    fn EnableWindow(hwnd: HWND, enabled: i32) -> i32;
    fn SetForegroundWindow(hwnd: HWND) -> i32;
    fn SetBkColor(hdc: *mut c_void, color: u32) -> u32;
    fn InvalidateRect(hwnd: HWND, rect: *const c_void, erase: i32) -> i32;
    fn CreateSolidBrush(color: u32) -> *mut c_void;
    fn SetFocus(hwnd: HWND) -> HWND;
}

use crate::app::App;
use crate::paint;
use crate::theme::ThemeColors;
use crate::widgets::{self, BtnKind, BtnState, Button};

const CLASS: &str = "QLogDlg";

// EDIT / COMBOBOX 样式常量
const ES_LEFT: u32 = 0x0000;
const ES_MULTILINE: u32 = 0x0004;
const ES_AUTOVSCROLL: u32 = 0x0040;
const ES_AUTOHSCROLL: u32 = 0x0080;
const ES_READONLY: u32 = 0x0800;
const ES_WANTRETURN: u32 = 0x1000;
const ES_NUMBER: u32 = 0x2000;
const WM_MOUSEWHEEL: u32 = 0x020A;
const CBS_DROPDOWNLIST: u32 = 0x0002;
const CBS_HASSTRINGS: u32 = 0x0200;
const WS_VSCROLL: u32 = 0x00200000;
const BS_AUTOCHECKBOX: u32 = 0x0002;
const BS_AUTORADIOBUTTON: u32 = 0x0009;

// 设置对话框控件 ID
const ID_SET_FONT_COMBO: u16 = 2101;
const ID_SET_FONT_SIZE: u16 = 2102;
const ID_SET_ROW_H: u16 = 2103;
const ID_SET_CB_LINENUM: u16 = 2201;
const ID_SET_CB_WRAP: u16 = 2202;
const ID_SET_CB_WS: u16 = 2203;
const ID_SET_CB_INDENT: u16 = 2204;
const ID_SET_CB_COLOR: u16 = 2205;
const ID_SET_CB_CASE: u16 = 2301;
const ID_SET_CB_REGEX: u16 = 2302;
const ID_SET_CB_WORD: u16 = 2303;
const ID_SET_ENC_COMBO: u16 = 2401;
const ID_SET_SMALL_COMBO: u16 = 2402;
const ID_SET_CACHE_COMBO: u16 = 2403;
const ID_SET_CB_INDEXCACHE: u16 = 2404;
const ID_SET_SCANW_COMBO: u16 = 2405;

// 自绘按钮索引（buttons Vec）
const BTN_OK: usize = 0;
const BTN_CANCEL: usize = 1;

/// 对话框上下文（调用方栈上持有；userdata 只存指针供 CTLCOLOR / NCHITTEST 读）
struct DlgCtx {
    theme: ThemeColors,
    title: String,
    w: i32,
    h: i32,
    tab: i32,
    tab_groups: [Vec<HWND>; 4],
    /// 自绘按钮（圆角，hover/pressed 由 modal_loop 维护）
    buttons: Vec<Button>,
    close_hover: bool,
    ok_clicked: bool,
    /// 批注列表状态
    anno: Vec<Annotation>,
    anno_sel: i32,
    /// 文本面板状态（帮助/快捷键/关于/属性 等纯文本对话框）
    text: Option<TextViewState>,
}

/// 文本面板行的种类。
#[derive(Clone, Copy, PartialEq, Eq)]
enum TKind {
    Header, // 小节标题（## 前缀）
    Kv,     // 键值行（含 ':' 或 '：'）
    Body,   // 普通正文
}

/// 文本面板的一行（已按面板宽度换行）。
struct TextLine {
    text: String,
    kind: TKind,
}

/// 自绘文本面板状态：排版行 + 滚动 + 字体 + 滚动条命中区。
struct TextViewState {
    lines: Vec<TextLine>,
    scroll: i32,
    panel: RECT,
    max_scroll: i32,
    font: windows_sys::Win32::Graphics::Gdi::HFONT,
    track: RECT,
    thumb: RECT,
}

// ────────────────────────────────────────────────────────────────────────
// 控件创建辅助（EDIT / COMBOBOX / CHECKBOX 仍为子控件）
// ────────────────────────────────────────────────────────────────────────

unsafe fn mk(parent: HWND, class: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
    let instance = GetModuleHandleW(ptr::null());
    let class_w: Vec<u16> = class.encode_utf16().chain(std::iter::once(0)).collect();
    CreateWindowExW(
        0, class_w.as_ptr(), ptr::null(),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | style,
        x, y, w, h, parent, id as usize as *mut c_void, instance, ptr::null(),
    )
}

unsafe fn mk_label(parent: HWND, text: &str, x: i32, y: i32, w: i32, h: i32, id: u16) -> HWND {
    let instance = GetModuleHandleW(ptr::null());
    let class_w: Vec<u16> = "STATIC".encode_utf16().chain(std::iter::once(0)).collect();
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    CreateWindowExW(
        0, class_w.as_ptr(), text_w.as_ptr(),
        WS_CHILD | WS_VISIBLE, x, y, w, h, parent, id as usize as *mut c_void, instance, ptr::null(),
    )
}

unsafe fn mk_check(parent: HWND, text: &str, x: i32, y: i32, w: i32, id: u16) -> HWND {
    let instance = GetModuleHandleW(ptr::null());
    let class_w: Vec<u16> = "BUTTON".encode_utf16().chain(std::iter::once(0)).collect();
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    CreateWindowExW(
        0, class_w.as_ptr(), text_w.as_ptr(),
        WS_CHILD | WS_VISIBLE | BS_AUTOCHECKBOX,
        x, y, w, 20, parent, id as usize as *mut c_void, instance, ptr::null(),
    )
}

unsafe fn set_text(hwnd: HWND, text: &str) {
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SendMessageW(hwnd, 0x000C /* WM_SETTEXT */, 0, t.as_ptr() as isize);
}
unsafe fn get_text(hwnd: HWND) -> String {
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
unsafe fn combo_add(hwnd: HWND, text: &str) {
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let _ = SendMessageW(hwnd, 0x0143 /* CB_ADDSTRING */, 0, t.as_ptr() as isize);
}
unsafe fn combo_sel(hwnd: HWND, idx: i32) {
    let _ = SendMessageW(hwnd, 0x014E /* CB_SETCURSEL */, idx as usize, 0);
}
unsafe fn combo_cur(hwnd: HWND) -> i32 {
    SendMessageW(hwnd, 0x0147 /* CB_GETCURSEL */, 0, 0) as i32
}
unsafe fn check_state(hwnd: HWND) -> bool {
    SendMessageW(hwnd, 0x00F1 /* BM_GETCHECK */, 0, 0) != 0
}
unsafe fn check_set(hwnd: HWND, on: bool) {
    let _ = SendMessageW(hwnd, 0x00F0 /* BM_SETCHECK */, if on { 1 } else { 0 }, 0);
}

// ────────────────────────────────────────────────────────────────────────
// 现代弹窗骨架
// ────────────────────────────────────────────────────────────────────────

/// 创建无边框自绘弹窗。
unsafe fn create_dialog(parent: HWND, title: &str, w: i32, h: i32) -> HWND {
    register_class_once();
    let instance = GetModuleHandleW(ptr::null());
    let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    CreateWindowExW(
        0,
        class_name().as_ptr(),
        title_w.as_ptr(),
        WS_POPUP | WS_VISIBLE,
        100, 100, w, h,
        parent, ptr::null_mut(), instance, ptr::null(),
    )
}

/// 模态循环。处理窗口框架（拖动/关闭钮/按钮 hover/背景），
/// `on_msg` 处理对话框专属消息（WM_PAINT 画内容 + 自绘按钮、WM_COMMAND 等）。
unsafe fn modal_loop<F: FnMut(HWND, &mut DlgCtx, u32, usize, isize) -> bool>(
    parent: HWND,
    hwnd: HWND,
    ctx: &mut DlgCtx,
    mut on_msg: F,
) {
    let ctx_ptr = ctx as *mut DlgCtx as isize;
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx_ptr);
    ShowWindow(hwnd, SW_SHOW);
    EnableWindow(parent, 0);
    SetFocus(hwnd);
    let mut msg = std::mem::zeroed::<MSG>();
    loop {
        let r = GetMessageW(&mut msg, ptr::null_mut(), 0, 0);
        if r == 0 || r == -1 {
            break;
        }
        let consumed = if msg.hwnd == hwnd {
            match msg.message {
                WM_MOUSEMOVE => {
                    let x = (msg.lParam & 0xFFFF) as i32;
                    let y = (msg.lParam >> 16) as i32;
                    let ch = widgets::hit_close(ctx.w, x, y);
                    let mut changed = ch != ctx.close_hover;
                    ctx.close_hover = ch;
                    for b in &mut ctx.buttons {
                        let s = if b.contains(x, y) { BtnState::Hover } else { BtnState::Normal };
                        if s != b.state {
                            b.state = s;
                            changed = true;
                        }
                    }
                    if changed {
                        let _ = InvalidateRect(hwnd, ptr::null(), 0);
                    }
                    true
                }
                WM_MOUSEWHEEL => {
                    // 文本面板：滚轮滚动（其它对话框交给默认处理）
                    if let Some(t) = &mut ctx.text {
                        let delta = (msg.wParam >> 16) as i16 as i32;
                        t.scroll = (t.scroll - (delta / 120) * 36).clamp(0, t.max_scroll);
                        let _ = InvalidateRect(hwnd, ptr::null(), 0);
                        true
                    } else {
                        false
                    }
                }
                WM_LBUTTONDOWN => {
                    let x = (msg.lParam & 0xFFFF) as i32;
                    let y = (msg.lParam >> 16) as i32;
                    if widgets::hit_close(ctx.w, x, y) {
                        DestroyWindow(hwnd);
                        true
                    } else if let Some(t) = &mut ctx.text {
                        // 文本面板滚动条：点击轨道翻页
                        if t.max_scroll > 0
                            && pt_in_rect(x, y, &t.track)
                            && !pt_in_rect(x, y, &t.thumb)
                        {
                            let page = (t.panel.bottom - t.panel.top).max(1);
                            if y < t.thumb.top {
                                t.scroll = (t.scroll - page).max(0);
                            } else if y > t.thumb.bottom {
                                t.scroll = (t.scroll + page).min(t.max_scroll);
                            }
                            let _ = InvalidateRect(hwnd, ptr::null(), 0);
                            true
                        } else {
                            // 记录按下的自绘按钮
                            for b in &mut ctx.buttons {
                                b.state = if b.contains(x, y) { BtnState::Pressed } else { BtnState::Hover };
                            }
                            on_msg(hwnd, ctx, msg.message, msg.wParam, msg.lParam)
                        }
                    } else {
                        // 记录按下的自绘按钮
                        for b in &mut ctx.buttons {
                            b.state = if b.contains(x, y) { BtnState::Pressed } else { BtnState::Hover };
                        }
                        on_msg(hwnd, ctx, msg.message, msg.wParam, msg.lParam)
                    }
                }
                _ => on_msg(hwnd, ctx, msg.message, msg.wParam, msg.lParam),
            }
        } else {
            false
        };
        if consumed {
            if !is_alive(hwnd) {
                break;
            }
            continue;
        }
        // WM_KEYDOWN：Esc 关闭
        if msg.message == WM_KEYDOWN && (msg.wParam as u16) == 0x1B {
            if is_child_of(hwnd, msg.hwnd) {
                DestroyWindow(hwnd);
                continue;
            }
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
        if !is_alive(hwnd) {
            break;
        }
    }
    EnableWindow(parent, 1);
    SetForegroundWindow(parent);
}

unsafe fn is_child_of(dialog: HWND, h: HWND) -> bool {
    let mut cur = h;
    loop {
        if cur == dialog {
            return true;
        }
        cur = windows_sys::Win32::UI::WindowsAndMessaging::GetParent(cur);
        if cur.is_null() {
            return false;
        }
    }
}

unsafe fn is_alive(hwnd: HWND) -> bool {
    IsWindow(hwnd) != 0
}

/// 画对话框背景 + 标题栏 + 自绘按钮（需调用方已 BeginPaint）。
unsafe fn paint_frame_dc(hdc: *mut c_void, ctx: &DlgCtx) {
    paint::fill_rect(hdc, &RECT { left: 0, top: 0, right: ctx.w, bottom: ctx.h }, ctx.theme.bg_primary);
    widgets::title_bar(hdc, ctx.w, &ctx.theme, &ctx.title, ctx.close_hover);
    for b in &ctx.buttons {
        b.draw(hdc, &ctx.theme);
    }
}

/// 画对话框背景 + 标题栏 + 自绘按钮。在 on_msg 的 WM_PAINT 里调用。
unsafe fn paint_frame(hwnd: HWND, ctx: &DlgCtx) {
    let mut ps = std::mem::zeroed::<PAINTSTRUCT>();
    let hdc = BeginPaint(hwnd, &mut ps);
    paint_frame_dc(hdc, ctx);
    EndPaint(hwnd, &mut ps);
}

/// 命中自绘按钮 → 返回其索引。
unsafe fn hit_btn(ctx: &DlgCtx, x: i32, y: i32) -> Option<usize> {
    ctx.buttons.iter().position(|b| b.contains(x, y))
}

fn class_name() -> &'static [u16] {
    use std::sync::OnceLock;
    static BUF: OnceLock<Vec<u16>> = OnceLock::new();
    BUF.get_or_init(|| CLASS.encode_utf16().chain(std::iter::once(0)).collect())
}

fn register_class_once() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        let instance = GetModuleHandleW(ptr::null());
        let wcex = windows_sys::Win32::UI::WindowsAndMessaging::WNDCLASSEXW {
            cbSize: std::mem::size_of::<windows_sys::Win32::UI::WindowsAndMessaging::WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS | 0x00020000 /* CS_DROPSHADOW */,
            lpfnWndProc: Some(dlg_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: ptr::null_mut(),
            hCursor: LoadCursorW(ptr::null_mut(), IDC_ARROW as *const u16),
            hbrBackground: ptr::null_mut(),
            lpszMenuName: ptr::null(),
            lpszClassName: class_name().as_ptr(),
            hIconSm: ptr::null_mut(),
        };
        RegisterClassExW(&wcex);
    });
}

unsafe extern "system" fn dlg_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match msg {
        WM_NCHITTEST => {
            // 标题栏可拖动；关闭钮区域返回 HTCLIENT（由模态循环处理点击）
            let mut pt = POINT { x: (lparam & 0xFFFF) as i32, y: ((lparam as u32) >> 16) as i32 };
            let _ = ScreenToClient(hwnd, &mut pt);
            let ctx = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgCtx;
            let w = if ctx.is_null() { 0 } else { (*ctx).w };
            if pt.y >= 0 && pt.y < widgets::TITLE_H && !widgets::hit_close(w, pt.x, pt.y) {
                HTCAPTION as isize
            } else {
                HTCLIENT as isize
            }
        }
        WM_ERASEBKGND => 1,
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let ctx = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DlgCtx;
            if !ctx.is_null() {
                let c = &(*ctx).theme;
                let brush = CreateSolidBrush(c.bg_primary.as_u32());
                paint::set_text_color(wparam as *mut c_void, c.text_primary.as_u32());
                let _ = SetBkColor(wparam as *mut c_void, c.bg_primary.as_u32());
                return brush as isize;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            // DlgCtx 由调用方栈上持有，这里不释放（此前 Box::from_raw 导致崩溃）
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn pt_in_rect(x: i32, y: i32, r: &RECT) -> bool {
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom
}

// ────────────────────────────────────────────────────────────────────────
// 文本面板：自绘、自动换行、标题/键值/正文分级、可滚动
// ────────────────────────────────────────────────────────────────────────

const TEXT_LINE_H: i32 = 22;

fn line_height(l: &TextLine) -> i32 {
    match l.kind {
        TKind::Header => TEXT_LINE_H + 6,
        _ if l.text.is_empty() => 8,
        _ => TEXT_LINE_H,
    }
}

fn text_total_height(t: &TextViewState) -> i32 {
    t.lines.iter().map(line_height).sum()
}

/// 折叠行内多余空格（去掉手写对齐用的连续空格）。
fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

/// 把 body 排版成 TextLine：`## ` 开头是标题，含冒号是键值行，其余正文。
/// 正文/键值行按面板宽度自动换行（GetTextExtentPoint32W 实测宽度，CJK 正确）。
fn build_text_lines(hdc: *mut c_void, body: &str, max_w: i32, out: &mut Vec<TextLine>) {
    for src in body.lines() {
        let s = src.trim();
        if s.is_empty() {
            out.push(TextLine { text: String::new(), kind: TKind::Body });
            continue;
        }
        if let Some(rest) = s.strip_prefix("## ") {
            out.push(TextLine { text: rest.trim().to_string(), kind: TKind::Header });
            continue;
        }
        let clean = collapse_spaces(s);
        let kind = if clean.contains(':') || clean.contains('：') {
            TKind::Kv
        } else {
            TKind::Body
        };
        wrap_line(hdc, &clean, max_w, kind, out);
    }
}

/// 把一段文本按宽度换行；首行标 `first_kind`，续行全部为 Body。
fn wrap_line(hdc: *mut c_void, text: &str, max_w: i32, first_kind: TKind, out: &mut Vec<TextLine>) {
    if text.is_empty() {
        return;
    }
    if paint::text_width(hdc, &crate::app::str_wide(text)) <= max_w {
        out.push(TextLine { text: text.to_string(), kind: first_kind });
        return;
    }
    let mut cur = String::new();
    let mut first = true;
    for ch in text.chars() {
        cur.push(ch);
        if paint::text_width(hdc, &crate::app::str_wide(&cur)) > max_w && cur.chars().count() > 1 {
            cur.pop();
            let kind = if first { first_kind } else { TKind::Body };
            out.push(TextLine { text: std::mem::take(&mut cur), kind });
            first = false;
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        let kind = if first { first_kind } else { TKind::Body };
        out.push(TextLine { text: cur, kind });
    }
}

/// 键值行拆成 (标签, 值)；值以冒号开头，去前导空格。
fn split_kv(s: &str) -> (&str, &str) {
    if let Some(i) = s.find(':') {
        let (a, b) = s.split_at(i);
        (a, b.trim_start())
    } else if let Some(i) = s.find('：') {
        let (a, b) = s.split_at(i);
        (a, b.trim_start())
    } else {
        (s, "")
    }
}

/// 绘制文本面板（卡片 + 分级文本 + 主题滚动条）。
unsafe fn render_text_panel(hdc: *mut c_void, t: &mut TextViewState, theme: &ThemeColors) {
    let panel = t.panel;
    widgets::panel(hdc, &panel, theme);
    let panel_h = panel.bottom - panel.top;
    let total = text_total_height(t);
    t.max_scroll = (total - panel_h).max(0);
    if t.scroll > t.max_scroll {
        t.scroll = t.max_scroll;
    }
    if t.scroll < 0 {
        t.scroll = 0;
    }

    // 滚动条（右侧 10px 竖条）
    let sb_w = 10;
    t.track = RECT { left: panel.right - sb_w - 2, top: panel.top + 2, right: panel.right - 2, bottom: panel.bottom - 2 };
    t.thumb = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    if t.max_scroll > 0 {
        let sh = (t.track.bottom - t.track.top) as f64;
        let thumb_h = ((sh / (sh + t.max_scroll as f64)) * sh).max(20.0) as i32;
        let frac = t.scroll as f64 / t.max_scroll as f64;
        let thumb_top = t.track.top + (frac * (sh - thumb_h as f64)) as i32;
        t.thumb = RECT {
            left: t.track.left,
            top: thumb_top,
            right: t.track.right,
            bottom: (thumb_top + thumb_h).min(t.track.bottom),
        };
        paint::fill_rect(hdc, &t.track, theme.scrollbar_track);
        paint::fill_rect(hdc, &t.thumb, theme.scrollbar_thumb);
    }

    // 分级渲染
    let text_x = panel.left + 12;
    let text_w = (panel.right - text_x - 16).max(20);
    let mut y = panel.top - t.scroll;
    for l in &t.lines {
        let lh = line_height(l);
        if !l.text.is_empty() && y + lh >= panel.top && y < panel.bottom {
            match l.kind {
                TKind::Header => {
                    paint::draw_text_clipped_c(hdc, text_x, y, text_w, theme.info, &crate::app::str_wide(&l.text));
                }
                TKind::Kv => {
                    let (a, b) = split_kv(&l.text);
                    let aw = paint::text_width(hdc, &crate::app::str_wide(a));
                    paint::draw_text_clipped_c(hdc, text_x, y, text_w, theme.text_secondary, &crate::app::str_wide(a));
                    if !b.is_empty() {
                        paint::draw_text_clipped_c(hdc, text_x + aw + 4, y, text_w, theme.text_primary, &crate::app::str_wide(b));
                    }
                }
                TKind::Body => {
                    paint::draw_text_clipped_c(hdc, text_x, y, text_w, theme.text_primary, &crate::app::str_wide(&l.text));
                }
            }
        }
        y += lh;
        if y > panel.bottom + TEXT_LINE_H {
            break;
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// 文本对话框（Help / Shortcuts / About / Properties）
// ────────────────────────────────────────────────────────────────────────

pub unsafe fn show_help(parent: HWND) {
    let body = "## 快速上手\n\
        Ctrl+O — 打开文件\n\
        Ctrl+F — 聚焦搜索框，Enter 搜索\n\
        F3 / Shift+F3 — 下一个 / 上一个匹配\n\
        Ctrl+L — 跳到指定行\n\
        Ctrl+R — 重新加载\n\
        Home / End — 顶部 / 底部\n\
        PgUp / PgDn — 上下翻页\n\
        Ctrl+滚轮 / Ctrl± — 缩放字体\n\
        Ctrl+Shift+T — 循环切换主题\n\n\
        提示：直接把文件拖进窗口即可打开；大文件自动后台建索引，可随时取消。";
    show_text(parent, "使用说明", body, 460, 380);
}

pub unsafe fn show_shortcuts(parent: HWND) {
    let body = "## 文件操作\n\
        Ctrl+O — 打开文件\n\
        Ctrl+R — 重新加载\n\
        Ctrl+W — 关闭文件\n\
        Ctrl+I — 文件属性\n\n\
        ## 搜索\n\
        Ctrl+F — 聚焦搜索\n\
        Enter — 执行搜索\n\
        F3 / Ctrl+G — 下一个\n\
        Shift+F3 / Ctrl+Shift+G — 上一个\n\
        Esc — 清除搜索\n\n\
        ## 导航\n\
        Home / End — 顶部 / 底部\n\
        PgUp / PgDn — 翻页\n\
        ↑ / ↓ — 上一行 / 下一行\n\
        Ctrl+L — 跳行\n\n\
        ## 显示\n\
        Ctrl+Plus — 放大字体\n\
        Ctrl+Minus — 缩小字体\n\
        Ctrl+0 — 重置字体\n\
        Ctrl+Shift+T — 循环主题\n\n\
        ## 选择\n\
        Ctrl+C — 复制选中内容";
    show_text(parent, "快捷键", body, 440, 460);
}

pub unsafe fn show_about(parent: HWND) {
    let body = format!(
        "## qview\n\
         基于 Rust + Win32/GDI 构建的高性能文本浏览器\n\
         支持 GB 级超大文件的快速浏览与搜索\n\
         内存映射 · 按需加载 · 极低占用（空载 ≈ 12 MiB）\n\n\
         版本 {}\n\n\
         作者：qinwh\n\
         许可证：GPL-3.0\n\n\
         © 2026 Qinwh",
        env!("CARGO_PKG_VERSION"),
    );
    show_text(parent, "关于 qview", &body, 460, 360);
}

pub unsafe fn show_properties(parent: HWND, app: &mut App) {
    let info = build_properties_info(app);
    show_text(parent, "文件属性", &info, 560, 380);
}

fn build_properties_info(app: &App) -> String {
    match &app.bridge {
        Some(b) => {
            let idx_path = app.config.engine.index_path(&b.path).display().to_string();
            let (idx_status, idx_file) = if !app.config.engine.index_cache_enabled {
                ("索引缓存已禁用".into(), String::new())
            } else if std::path::Path::new(&idx_path).exists() {
                let sz = std::fs::metadata(&idx_path).map(|m| m.len()).unwrap_or(0);
                (format!("已缓存（{}）", paint::human_bytes(sz)), idx_path)
            } else {
                ("尚未创建".into(), idx_path)
            };
            let enc = app.config.engine.encoding.clone();
            format!(
                "文件名: {}\n路径: {}\n大小: {}\n行数: {}\n编码: {}\n行尾: {}\n索引状态: {}\n索引文件: {}",
                b.path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                b.path.display(),
                paint::human_bytes(b.size),
                b.line_count,
                enc,
                if b.uses_crlf() { "CRLF" } else { "LF" },
                idx_status,
                if idx_file.is_empty() { "（未启用）".to_string() } else { idx_file },
            )
        }
        None => "未打开文件".into(),
    }
}

/// 通用文本对话框：自绘文本面板（自动换行 + 分级配色 + 滚动）+ 确定按钮。
unsafe fn show_text(parent: HWND, title: &str, body: &str, w: i32, h: i32) {
    let hwnd = create_dialog(parent, title, w, h);
    if hwnd.is_null() {
        return;
    }
    let mut ctx = DlgCtx {
        theme: crate::app::theme_for(parent),
        title: title.to_string(),
        w,
        h,
        tab: -1,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: Vec::new(),
        anno_sel: -1,
        text: None,
    };
    ctx.buttons.push(Button::new(
        RECT { left: w / 2 - 40, top: h - 44, right: w / 2 + 40, bottom: h - 14 },
        BtnKind::Primary,
        "确定",
    ));

    // 在对话框 DC + 正文字体下排版（实测宽度换行）
    let font = paint::create_font(13, "Segoe UI");
    let mut t = TextViewState {
        lines: Vec::new(),
        scroll: 0,
        panel: RECT { left: 16, top: widgets::TITLE_H + 14, right: w - 16, bottom: h - 56 },
        max_scroll: 0,
        font,
        track: RECT { left: 0, top: 0, right: 0, bottom: 0 },
        thumb: RECT { left: 0, top: 0, right: 0, bottom: 0 },
    };
    let hdc = GetDC(hwnd);
    let old = paint::select_font_safe(hdc, font);
    let text_w = (t.panel.right - t.panel.left - 24).max(40);
    build_text_lines(hdc, body, text_w, &mut t.lines);
    t.max_scroll = (text_total_height(&t) - (t.panel.bottom - t.panel.top)).max(0);
    paint::restore_font(hdc, old);
    let _ = ReleaseDC(hwnd, hdc);
    ctx.text = Some(t);

    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        let _ = lp;
        if msg == WM_PAINT {
            let mut ps = std::mem::zeroed::<PAINTSTRUCT>();
            let hdc = BeginPaint(hwnd, &mut ps);
            if ctx.text.is_some() {
                // 先选好面板字体再画框架（按钮文字随之统一），再画文本面板
                let old = paint::select_font_safe(hdc, ctx.text.as_ref().unwrap().font);
                paint_frame_dc(hdc, ctx);
                let t = ctx.text.as_mut().unwrap();
                let theme = &ctx.theme;
                render_text_panel(hdc, t, theme);
                paint::restore_font(hdc, old);
            } else {
                paint_frame(hwnd, ctx);
            }
            EndPaint(hwnd, &mut ps);
            return true;
        }
        if msg == WM_LBUTTONUP {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            if hit_btn(ctx, x, y) == Some(BTN_OK) && ctx.buttons[BTN_OK].state == BtnState::Pressed {
                ctx.ok_clicked = true;
                DestroyWindow(hwnd);
            }
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });

    // 释放面板字体
    if let Some(t) = &ctx.text {
        if !t.font.is_null() {
            let _ = DeleteObject(t.font as *mut c_void);
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// 设置对话框（4 标签，自绘按钮 + 子控件表单）
// ────────────────────────────────────────────────────────────────────────

pub unsafe fn show_settings(parent: HWND, app: &mut App) {
    let w = 520;
    let h = 400;
    let hwnd = create_dialog(parent, "设置", w, h);
    if hwnd.is_null() {
        return;
    }
    let theme = app.theme;
    let mut ctx = DlgCtx {
        theme,
        title: "设置".to_string(),
        w,
        h,
        tab: 0,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: Vec::new(),
        anno_sel: -1,
        text: None,
    };

    // 内容区从标题栏 + 标签条下方开始（标签条占 TITLE_H+2 .. TITLE_H+28）
    let y0 = widgets::TITLE_H + 34;

    // ── 标签 0：显示 ──
    let mut g0: Vec<HWND> = Vec::new();
    g0.push(mk_label(hwnd, "字体：", 24, y0 + 2, 50, 20, 0));
    let font_combo = mk(hwnd, "COMBOBOX", CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL, 78, y0, 180, 200, ID_SET_FONT_COMBO);
    let fonts_all = crate::fontmgr::all_system_fonts();
    for f in &fonts_all {
        combo_add(font_combo, f);
    }
    {
        let cur_font = app.config.gui.font_family.clone();
        let mut idx = 0;
        for (i, f) in fonts_all.iter().enumerate() {
            if f == &cur_font {
                idx = i as i32;
                break;
            }
        }
        combo_sel(font_combo, idx);
    }
    g0.push(font_combo);
    g0.push(mk_label(hwnd, "字号：", 280, y0 + 2, 50, 20, 0));
    let fs_edit = mk(hwnd, "EDIT", ES_NUMBER, 330, y0, 50, 22, ID_SET_FONT_SIZE);
    set_text(fs_edit, &format!("{}", app.config.gui.font_size as i32));
    g0.push(fs_edit);
    g0.push(mk_label(hwnd, "行高：", 24, y0 + 32, 50, 20, 0));
    let rh_edit = mk(hwnd, "EDIT", ES_NUMBER, 78, y0 + 30, 50, 22, ID_SET_ROW_H);
    set_text(rh_edit, &format!("{}", app.config.gui.row_height as i32));
    g0.push(rh_edit);
    let cb_linenum = mk_check(hwnd, "显示行号", 24, y0 + 66, 140, ID_SET_CB_LINENUM);
    check_set(cb_linenum, app.config.gui.show_line_numbers);
    g0.push(cb_linenum);
    let cb_wrap = mk_check(hwnd, "自动换行", 24, y0 + 90, 140, ID_SET_CB_WRAP);
    check_set(cb_wrap, app.config.gui.word_wrap);
    g0.push(cb_wrap);
    let cb_ws = mk_check(hwnd, "显示空白字符", 24, y0 + 114, 140, ID_SET_CB_WS);
    check_set(cb_ws, app.config.gui.show_whitespace);
    g0.push(cb_ws);
    let cb_indent = mk_check(hwnd, "缩进参考线", 24, y0 + 138, 140, ID_SET_CB_INDENT);
    check_set(cb_indent, app.config.gui.show_indent_guides);
    g0.push(cb_indent);
    let cb_color = mk_check(hwnd, "日志级别着色", 178, y0 + 66, 140, ID_SET_CB_COLOR);
    check_set(cb_color, app.config.gui.level_coloring);
    g0.push(cb_color);
    ctx.tab_groups[0] = g0;

    // ── 标签 1：搜索 ──
    let mut g1: Vec<HWND> = Vec::new();
    let cb_case = mk_check(hwnd, "大小写敏感", 24, y0 + 8, 160, ID_SET_CB_CASE);
    check_set(cb_case, app.search.case_sensitive);
    g1.push(cb_case);
    let cb_regex = mk_check(hwnd, "正则表达式", 24, y0 + 34, 160, ID_SET_CB_REGEX);
    check_set(cb_regex, app.search.use_regex);
    g1.push(cb_regex);
    let cb_word = mk_check(hwnd, "整词匹配", 24, y0 + 60, 160, ID_SET_CB_WORD);
    check_set(cb_word, app.search.whole_word);
    g1.push(cb_word);
    g1.push(mk_label(hwnd, "搜索历史（最多 20 条，自动保存）", 24, y0 + 92, 300, 20, 0));
    ctx.tab_groups[1] = g1;

    // ── 标签 2：主题 ──
    let mut g2: Vec<HWND> = Vec::new();
    for (i, name) in crate::theme::THEME_NAMES.iter().enumerate() {
        let cb = mk(hwnd, "BUTTON", BS_AUTORADIOBUTTON, 24, y0 + 8 + i as i32 * 26, 220, 20, 2600 + i as u16);
        set_text(cb, name);
        if i as usize == app.theme_idx {
            let _ = SendMessageW(cb, 0x00F0 /* BM_SETCHECK */, 1, 0);
        }
        g2.push(cb);
    }
    g2.push(mk_label(hwnd, "主题即时生效并自动保存", 24, y0 + 8 + 6 * 26 + 8, 220, 20, 0));
    ctx.tab_groups[2] = g2;

    // ── 标签 3：引擎 ──
    let mut g3: Vec<HWND> = Vec::new();
    g3.push(mk_label(hwnd, "文本编码：", 24, y0 + 4, 90, 20, 0));
    let enc_combo = mk(hwnd, "COMBOBOX", CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL, 118, y0, 200, 300, ID_SET_ENC_COMBO);
    for (key, _) in crate::app::ENCODINGS {
        combo_add(enc_combo, key);
    }
    {
        let cur = app.config.engine.encoding.clone();
        let mut idx = 0;
        for (i, (key, _)) in crate::app::ENCODINGS.iter().enumerate() {
            if *key == cur {
                idx = i as i32;
                break;
            }
        }
        combo_sel(enc_combo, idx);
    }
    g3.push(enc_combo);
    g3.push(mk_label(hwnd, "小文件阈值：", 24, y0 + 32, 90, 20, 0));
    let small_combo = mk(hwnd, "COMBOBOX", CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL, 118, y0 + 30, 200, 300, ID_SET_SMALL_COMBO);
    let small_opts = ["1 MB", "5 MB", "10 MB", "50 MB", "100 MB"];
    let small_vals = [1u64 << 20, 5 << 20, 10 << 20, 50 << 20, 100 << 20];
    let mut small_idx = 2;
    for (i, s) in small_opts.iter().enumerate() {
        combo_add(small_combo, s);
        if small_vals[i] == app.config.engine.small_file_threshold {
            small_idx = i as i32;
        }
    }
    combo_sel(small_combo, small_idx);
    g3.push(small_combo);
    g3.push(mk_label(hwnd, "行缓存条目：", 24, y0 + 60, 90, 20, 0));
    let cache_combo = mk(hwnd, "COMBOBOX", CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL, 118, y0 + 58, 200, 300, ID_SET_CACHE_COMBO);
    for s in ["5000", "10000", "20000", "50000"] {
        combo_add(cache_combo, s);
    }
    {
        let cur = app.config.engine.line_cache_capacity;
        let mut idx = 1;
        for (i, s) in ["5000", "10000", "20000", "50000"].iter().enumerate() {
            if s.parse::<usize>().ok() == Some(cur) {
                idx = i as i32;
            }
        }
        combo_sel(cache_combo, idx);
    }
    g3.push(cache_combo);
    g3.push(mk_label(hwnd, "扫描窗口：", 24, y0 + 88, 90, 20, 0));
    let scanw_combo = mk(hwnd, "COMBOBOX", CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL, 118, y0 + 86, 200, 300, ID_SET_SCANW_COMBO);
    for s in ["16 MB", "32 MB", "64 MB", "128 MB", "256 MB"] {
        combo_add(scanw_combo, s);
    }
    let scanw_sel = match app.config.engine.scan_window_mb {
        16 => 0,
        32 => 1,
        64 => 2,
        128 => 3,
        _ => 4,
    };
    combo_sel(scanw_combo, scanw_sel);
    g3.push(scanw_combo);
    let cb_index_cache = mk_check(hwnd, "索引缓存（.qli）", 24, y0 + 116, 160, ID_SET_CB_INDEXCACHE);
    check_set(cb_index_cache, app.config.engine.index_cache_enabled);
    g3.push(cb_index_cache);
    g3.push(mk_label(hwnd, "引擎参数在下次打开文件时生效", 24, y0 + 144, 260, 20, 0));
    ctx.tab_groups[3] = g3;

    // 底部按钮（自绘）
    let btn_y = h - 44;
    ctx.buttons.push(Button::new(RECT { left: w - 150, top: btn_y, right: w - 80, bottom: btn_y + 30 }, BtnKind::Primary, "应用"));
    ctx.buttons.push(Button::new(RECT { left: w - 72, top: btn_y, right: w - 16, bottom: btn_y + 30 }, BtnKind::Neutral, "取消"));

    // 显示标签 0，隐藏其它
    for gi in 0..4 {
        let show = gi == 0;
        for &c in &ctx.tab_groups[gi] {
            ShowWindow(c, if show { SW_SHOW } else { 0 });
        }
    }

    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        if msg == WM_PAINT {
            let mut ps = std::mem::zeroed::<PAINTSTRUCT>();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_frame_dc(hdc, ctx);
            // 标签条（自绘）
            let mut r = std::mem::zeroed::<RECT>();
            GetClientRect(hwnd, &mut r);
            let tw = (r.right - r.left) / 4;
            let tab_y = widgets::TITLE_H + 2;
            for i in 0..4 {
                let rect = RECT { left: i * tw + 8, top: tab_y, right: (i + 1) * tw - 8, bottom: tab_y + 26 };
                let bg = if i == ctx.tab { ctx.theme.bg_active } else { ctx.theme.bg_tertiary };
                widgets::round_rect(hdc, &rect, 5, bg);
                // 标签文字水平 + 垂直居中
                let label = crate::app::str_wide(["显示", "搜索", "主题", "引擎"][i as usize]);
                let tw2 = paint::text_width(hdc, &label);
                let th2 = paint::text_height(hdc, &label);
                let cx = rect.left + (rect.right - rect.left - tw2) / 2;
                let cy = rect.top + (rect.bottom - rect.top - th2) / 2;
                paint::draw_text_c(hdc, cx, cy, ctx.theme.text_primary, &label);
            }
            EndPaint(hwnd, &mut ps);
            return true;
        }
        if msg == WM_LBUTTONDOWN {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            // 标签条命中
            let tab_y = widgets::TITLE_H + 2;
            if y >= tab_y && y < tab_y + 26 {
                let mut r = std::mem::zeroed::<RECT>();
                GetClientRect(hwnd, &mut r);
                let tw = (r.right - r.left) / 4;
                let tab = ((x - 8) / tw).clamp(0, 3);
                if tab != ctx.tab {
                    ctx.tab = tab;
                    for gi in 0usize..4 {
                        let show = gi as i32 == tab;
                        for &c in &ctx.tab_groups[gi] {
                            ShowWindow(c, if show { SW_SHOW } else { 0 });
                        }
                    }
                    let _ = InvalidateRect(hwnd, ptr::null(), 0);
                }
                return true;
            }
            // 自绘按钮按下
            if let Some(bi) = hit_btn(ctx, x, y) {
                if bi == BTN_OK {
                    ctx.ok_clicked = true;
                    DestroyWindow(hwnd);
                } else if bi == BTN_CANCEL {
                    DestroyWindow(hwnd);
                }
                return true;
            }
            return false;
        }
        if msg == WM_LBUTTONUP {
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });

    if ctx.ok_clicked {
        apply_settings(app, font_combo, fs_edit, rh_edit, enc_combo, small_combo, cache_combo, scanw_combo, cb_linenum, cb_wrap, cb_ws, cb_indent, cb_color, cb_case, cb_regex, cb_word, cb_index_cache);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn apply_settings(
    app: &mut App,
    font_combo: HWND, fs_edit: HWND, rh_edit: HWND,
    enc_combo: HWND, small_combo: HWND, cache_combo: HWND, scanw_combo: HWND,
    cb_linenum: HWND, cb_wrap: HWND, cb_ws: HWND, cb_indent: HWND, cb_color: HWND,
    cb_case: HWND, cb_regex: HWND, cb_word: HWND, cb_index_cache: HWND,
) {
    {
        let g = &mut app.config.gui;
        let fonts = crate::fontmgr::all_system_fonts();
        let fi = combo_cur(font_combo).max(0) as usize;
        if fi < fonts.len() {
            g.font_family = fonts[fi].clone();
        }
        if let Ok(n) = get_text(fs_edit).trim().parse::<f32>() {
            g.font_size = n.clamp(8.0, 36.0);
        }
        if let Ok(n) = get_text(rh_edit).trim().parse::<f64>() {
            g.row_height = n.clamp(14.0, 36.0);
        }
        g.show_line_numbers = check_state(cb_linenum);
        g.word_wrap = check_state(cb_wrap);
        g.show_whitespace = check_state(cb_ws);
        g.show_indent_guides = check_state(cb_indent);
        g.level_coloring = check_state(cb_color);
        app.search.case_sensitive = check_state(cb_case);
        app.search.use_regex = check_state(cb_regex);
        app.search.whole_word = check_state(cb_word);
        g.case_sensitive = app.search.case_sensitive;
        g.use_regex = app.search.use_regex;
        g.whole_word = app.search.whole_word;
        app.metrics.font_size_px = g.font_size.max(8.0) as i32;
        app.metrics.invalidate();
        app.row_h = g.row_height.max(14.0) as i32;
    }
    // 引擎页
    {
        let enc_sel = combo_cur(enc_combo).max(0) as usize;
        if let Some((key, _)) = crate::app::ENCODINGS.get(enc_sel) {
            let new_enc = key.to_string();
            if new_enc != app.config.engine.encoding {
                app.config.engine.encoding = new_enc.clone();
            }
        }
        let small_opts = [1u64 << 20, 5 << 20, 10 << 20, 50 << 20, 100 << 20];
        let si = combo_cur(small_combo).max(0) as usize;
        app.config.engine.small_file_threshold = small_opts.get(si).copied().unwrap_or(10 << 20);
        let cache_opts = [5000usize, 10000, 20000, 50000];
        let ci = combo_cur(cache_combo).max(0) as usize;
        app.config.engine.line_cache_capacity = cache_opts.get(ci).copied().unwrap_or(10000);
        let scanw_opts = [16u32, 32, 64, 128, 256];
        let swi = combo_cur(scanw_combo).max(0) as usize;
        app.config.engine.scan_window_mb = scanw_opts.get(swi).copied().unwrap_or(64);
        app.config.engine.index_cache_enabled = check_state(cb_index_cache);
    }
    app.config.save();
    crate::menu::rebuild(app);
    app.invalidate_view();
}

// ────────────────────────────────────────────────────────────────────────
// 编码切换确认
// ────────────────────────────────────────────────────────────────────────

pub unsafe fn show_encoding_confirm(parent: HWND, app: &mut App, target: &str) {
    let w = 400;
    let h = 180;
    let hwnd = create_dialog(parent, "切换编码", w, h);
    if hwnd.is_null() {
        return;
    }
    let mut ctx = DlgCtx {
        theme: app.theme,
        title: "切换编码".to_string(),
        w,
        h,
        tab: -1,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: Vec::new(),
        anno_sel: -1,
        text: None,
    };
    let msg = format!("将编码从 {} 切换到 {}？\n切换后重新加载文件。", app.config.engine.encoding, target);
    let label = mk_label(hwnd, &msg, 20, widgets::TITLE_H + 20, w - 40, 48, 0);
    ctx.tab_groups[0].push(label);
    ctx.buttons.push(Button::new(RECT { left: 40, top: h - 50, right: 170, bottom: h - 18 }, BtnKind::Primary, "切换并重新加载"));
    ctx.buttons.push(Button::new(RECT { left: 190, top: h - 50, right: 270, bottom: h - 18 }, BtnKind::Neutral, "取消"));

    let target = target.to_string();
    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        if msg == WM_PAINT {
            paint_frame(hwnd, ctx);
            return true;
        }
        if msg == WM_LBUTTONUP {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            if hit_btn(ctx, x, y) == Some(BTN_OK) && ctx.buttons[BTN_OK].state == BtnState::Pressed {
                ctx.ok_clicked = true;
                DestroyWindow(hwnd);
            } else if hit_btn(ctx, x, y) == Some(BTN_CANCEL) && ctx.buttons[BTN_CANCEL].state == BtnState::Pressed {
                DestroyWindow(hwnd);
            }
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });
    if ctx.ok_clicked {
        app.config.engine.encoding = target;
        app.config.save();
        if let Some(p) = app.path.clone() {
            app.open_path(p);
        }
    }
}

/// 状态栏编码标签点击：弹编码列表，选中后弹确认对话框。
pub unsafe fn show_encoding_menu(parent: HWND, app: &mut App, x: i32, y: i32) {
    extern "system" {
        fn ClientToScreen(hwnd: HWND, lp: *mut POINT) -> i32;
    }
    let mut screen_pt = POINT { x, y };
    let _ = ClientToScreen(parent, &mut screen_pt);
    let hmenu = CreatePopupMenu();
    for (i, (key, label)) in crate::app::ENCODINGS.iter().enumerate() {
        let flags = MF_STRING | if *key == app.config.engine.encoding { MF_CHECKED } else { 0 };
        let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
        AppendMenuW(hmenu, flags, 3001 + i as usize, wide.as_ptr());
    }
    let cmd = TrackPopupMenu(hmenu, TPM_RETURNCMD, screen_pt.x, screen_pt.y, 0, parent, std::ptr::null());
    DestroyMenu(hmenu);
    if cmd >= 3001 {
        let idx = (cmd - 3001) as usize;
        if let Some((key, _)) = crate::app::ENCODINGS.get(idx) {
            if *key != app.config.engine.encoding {
                show_encoding_confirm(parent, app, key);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// 缓存管理
// ────────────────────────────────────────────────────────────────────────

pub unsafe fn show_index_manager(parent: HWND, app: &mut App) {
    let w = 520;
    let h = 420;
    let hwnd = create_dialog(parent, "缓存管理", w, h);
    if hwnd.is_null() {
        return;
    }
    let mut ctx = DlgCtx {
        theme: app.theme,
        title: "缓存管理".to_string(),
        w,
        h,
        tab: -1,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: Vec::new(),
        anno_sel: -1,
        text: None,
    };
    let list_y = widgets::TITLE_H + 12;
    let list = mk(hwnd, "EDIT", ES_MULTILINE | ES_READONLY, 16, list_y, w - 32, h - list_y - 64, 0);
    ctx.tab_groups[0].push(list);
    ctx.buttons.push(Button::new(RECT { left: 160, top: h - 50, right: 330, bottom: h - 18 }, BtnKind::Danger, "清空缓存（保留当前）"));
    ctx.buttons.push(Button::new(RECT { left: 400, top: h - 50, right: w - 16, bottom: h - 18 }, BtnKind::Neutral, "关闭"));

    // 列出 .qli
    let mut text = String::new();
    let index_dir = app.config.engine.index_dir.clone();
    let keep = app.path.as_ref().map(|p| app.config.engine.index_path(p));
    let mut total = 0u64;
    let mut count = 0usize;
    if let Some(dir) = index_dir {
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = rd.filter_map(|e| e.ok()).collect();
            files.sort_by_key(|e| e.path());
            for f in files {
                let path = f.path();
                if path.extension().and_then(|s| s.to_str()) != Some("qli") {
                    continue;
                }
                let sz = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                total += sz;
                count += 1;
                let star = if Some(path.clone()) == keep { "★ " } else { "  " };
                text.push_str(&format!("{}{}  {}\n", star, paint::human_bytes(sz), path.file_name().and_then(|s| s.to_str()).unwrap_or("")));
            }
        }
    }
    if count == 0 {
        text.push_str("（无缓存文件）");
    }
    text.push_str(&format!("\n共 {} 个索引文件，占用 {}\n最近文件 {} 条 · 搜索历史 {} 条", count, paint::human_bytes(total), app.config.recent_files.len(), app.config.search_history.len()));
    set_text(list, &text);

    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        if msg == WM_PAINT {
            paint_frame(hwnd, ctx);
            return true;
        }
        if msg == WM_LBUTTONUP {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            let bi = hit_btn(ctx, x, y);
            if bi == Some(BTN_OK) && ctx.buttons[BTN_OK].state == BtnState::Pressed {
                // 清空缓存
                let keep = app.path.as_ref().map(|p| app.config.engine.index_path(p));
                let mut deleted = 0u64;
                if let Some(dir) = &app.config.engine.index_dir {
                    if let Ok(rd) = std::fs::read_dir(dir) {
                        for f in rd.flatten() {
                            let p = f.path();
                            if p.extension().and_then(|s| s.to_str()) != Some("qli") {
                                continue;
                            }
                            if Some(p.clone()) != keep {
                                if let Ok(m) = std::fs::metadata(&p) {
                                    deleted += m.len();
                                }
                                let _ = std::fs::remove_file(&p);
                            }
                        }
                    }
                }
                app.config.recent_files.clear();
                app.config.search_history.clear();
                app.config.save();
                crate::menu::rebuild(app);
                app.status_text = format!("已清空缓存（{} 释放）", paint::human_bytes(deleted));
                app.invalidate_view();
                DestroyWindow(hwnd);
            } else if bi == Some(BTN_CANCEL) && ctx.buttons[BTN_CANCEL].state == BtnState::Pressed {
                DestroyWindow(hwnd);
            }
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });
}

// ────────────────────────────────────────────────────────────────────────
// 批注：添加 / 编辑 + 列表
// ────────────────────────────────────────────────────────────────────────

pub unsafe fn show_annotation_edit(parent: HWND, app: &mut App, edit_id: Option<u64>) {
    let is_edit = edit_id.is_some();
    let title = if is_edit { "编辑批注" } else { "添加批注" };
    let w = 420;
    let h = 360;
    let hwnd = create_dialog(parent, title, w, h);
    if hwnd.is_null() {
        return;
    }
    let mut ctx = DlgCtx {
        theme: app.theme,
        title: title.to_string(),
        w,
        h,
        tab: -1,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: Vec::new(),
        anno_sel: -1,
        text: None,
    };

    let y0 = widgets::TITLE_H + 12;
    // 选中内容预览
    let mut preview = String::new();
    if let Some(sel) = app.selection {
        if let Some(t) = crate::selection::copy_text(app, &sel) {
            preview = t.chars().take(300).collect();
        }
    }
    if let Some(a) = app.annotations.list.iter().find(|a| a.id == edit_id.unwrap_or(0)) {
        preview = a.selected_text.clone();
    }
    ctx.tab_groups[0].push(mk_label(hwnd, if preview.is_empty() { "（请先在日志中选中内容）" } else { "选中内容：" }, 20, y0, w - 40, 18, 0));
    let preview_edit = mk(hwnd, "EDIT", ES_MULTILINE | ES_READONLY, 20, y0 + 22, w - 40, 84, 0);
    set_text(preview_edit, &preview.chars().take(400).collect::<String>());
    ctx.tab_groups[0].push(preview_edit);
    let note_label = mk_label(hwnd, "批注内容：", 20, y0 + 112, w - 40, 18, 0);
    ctx.tab_groups[0].push(note_label);
    let note_edit = mk(hwnd, "EDIT", ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN, 20, y0 + 132, w - 40, 96, 0);
    if let Some(a) = app.annotations.list.iter().find(|a| a.id == edit_id.unwrap_or(0)) {
        set_text(note_edit, &a.text);
    }
    ctx.tab_groups[0].push(note_edit);

    let btn_y = h - 50;
    ctx.buttons.push(Button::new(RECT { left: 100, top: btn_y, right: 180, bottom: btn_y + 32 }, BtnKind::Primary, "保存"));
    ctx.buttons.push(Button::new(RECT { left: 220, top: btn_y, right: 300, bottom: btn_y + 32 }, BtnKind::Neutral, "取消"));

    let mut save_clicked = false;
    let note_text: std::cell::RefCell<String> = std::cell::RefCell::new(String::new());
    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        if msg == WM_PAINT {
            paint_frame(hwnd, ctx);
            return true;
        }
        if msg == WM_LBUTTONUP {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            let bi = hit_btn(ctx, x, y);
            if bi == Some(BTN_OK) && ctx.buttons[BTN_OK].state == BtnState::Pressed {
                *note_text.borrow_mut() = get_text(note_edit);
                save_clicked = true;
                DestroyWindow(hwnd);
            } else if bi == Some(BTN_CANCEL) && ctx.buttons[BTN_CANCEL].state == BtnState::Pressed {
                DestroyWindow(hwnd);
            }
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });

    if save_clicked {
        let note = note_text.into_inner();
        if note.is_empty() {
            app.status_text = "批注内容不能为空".into();
            app.invalidate_view();
            return;
        }
        let path = match app.path.clone() {
            Some(p) => p,
            None => return,
        };
        if let Some(id) = edit_id {
            app.annotations.set_text(&path, id, note);
        } else if let Some(sel) = app.selection {
            let (sb, eb) = match crate::selection::annotation_bytes(app, &sel) {
                Some(v) => v,
                None => return,
            };
            let (sl, sc, el, ec) = sel.normalized();
            let selected_text = crate::selection::copy_text(app, &sel).unwrap_or_default();
            let a = Annotation {
                id: 0,
                file_key: qview_core::annotation::AnnotationStore::file_key(&path),
                start_byte: sb,
                end_byte: eb,
                start_line: sl,
                end_line: el,
                start_col: sc,
                end_col: ec,
                selected_text,
                text: note,
                created_at: crate::annotations::now(),
                color: 0,
                stale: false,
            };
            app.annotations.add(&path, a);
        }
        app.refresh_annotations();
        app.status_text = "批注已保存".into();
    }
}

pub unsafe fn show_annotation_list(parent: HWND, app: &mut App) {
    if app.annotations.list.is_empty() {
        app.status_text = "当前文件还没有批注".into();
        app.invalidate_view();
        return;
    }
    let w = 470;
    let h = 430;
    let hwnd = create_dialog(parent, "批注列表", w, h);
    if hwnd.is_null() {
        return;
    }
    let mut ctx = DlgCtx {
        theme: app.theme,
        title: "批注列表".to_string(),
        w,
        h,
        tab: -1,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: app.annotations.list.clone(),
        anno_sel: -1,
        text: None,
    };
    ctx.buttons.push(Button::new(RECT { left: 120, top: h - 50, right: 210, bottom: h - 18 }, BtnKind::Secondary, "编辑所选"));
    ctx.buttons.push(Button::new(RECT { left: 240, top: h - 50, right: 330, bottom: h - 18 }, BtnKind::Primary, "跳转"));
    ctx.buttons.push(Button::new(RECT { left: 380, top: h - 50, right: w - 16, bottom: h - 18 }, BtnKind::Neutral, "关闭"));

    let action: std::cell::RefCell<Option<u64>> = std::cell::RefCell::new(None);
    let want_edit: std::cell::RefCell<bool> = std::cell::RefCell::new(false);
    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        if msg == WM_PAINT {
            let mut ps = std::mem::zeroed::<PAINTSTRUCT>();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut r = std::mem::zeroed::<RECT>();
            GetClientRect(hwnd, &mut r);
            paint::fill_rect(hdc, &r, ctx.theme.bg_primary);
            widgets::title_bar(hdc, ctx.w, &ctx.theme, &ctx.title, ctx.close_hover);
            // 列表区（自绘行）
            let row_h = 56;
            let top = widgets::TITLE_H + 12;
            let mut y = top;
            for (i, a) in ctx.anno.iter().enumerate() {
                if y > h - 60 {
                    break;
                }
                let bg = if i as i32 == ctx.anno_sel { ctx.theme.bg_active } else { ctx.theme.bg_secondary };
                let row = RECT { left: 12, top: y, right: w - 12, bottom: y + row_h - 4 };
                widgets::round_rect(hdc, &row, 6, bg);
                let title = if a.stale {
                    format!("⚠️ 行 {}–{}（位置已失效）", a.start_line + 1, a.end_line + 1)
                } else {
                    format!("行 {}–{}", a.start_line + 1, a.end_line + 1)
                };
                let tc = if a.stale { ctx.theme.warning } else { ctx.theme.text_primary };
                paint::draw_text_c(hdc, row.left + 12, y + 6, tc, &crate::app::str_wide(&title));
                let preview: String = a.selected_text.chars().take(40).collect();
                paint::draw_text_clipped_c(hdc, row.left + 12, y + 24, w - 40, ctx.theme.text_secondary, &crate::app::str_wide(&preview));
                if !a.text.is_empty() {
                    let note: String = a.text.chars().take(28).collect();
                    paint::draw_text_clipped_c(hdc, row.left + 12, y + 40, w - 40, ctx.theme.info, &crate::app::str_wide(&note));
                }
                y += row_h;
            }
            for b in &ctx.buttons {
                b.draw(hdc, &ctx.theme);
            }
            EndPaint(hwnd, &mut ps);
            return true;
        }
        if msg == WM_LBUTTONDOWN {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            let row_h = 56;
            let top = widgets::TITLE_H + 12;
            if x >= 12 && x <= w - 12 && y >= top && y < h - 60 {
                let idx = (y - top) / row_h;
                if (idx as usize) < ctx.anno.len() {
                    ctx.anno_sel = idx;
                    let _ = InvalidateRect(hwnd, ptr::null(), 0);
                }
            }
            return true;
        }
        if msg == WM_LBUTTONUP {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            let bi = hit_btn(ctx, x, y);
            if let Some(bi) = bi {
                match bi {
                    0 => {
                        // 编辑所选
                        if ctx.anno_sel >= 0 {
                            *want_edit.borrow_mut() = true;
                            *action.borrow_mut() = Some(ctx.anno[ctx.anno_sel as usize].id);
                            DestroyWindow(hwnd);
                        }
                    }
                    1 => {
                        // 跳转
                        if ctx.anno_sel >= 0 {
                            *action.borrow_mut() = Some(ctx.anno[ctx.anno_sel as usize].id);
                            DestroyWindow(hwnd);
                        }
                    }
                    _ => { DestroyWindow(hwnd); }
                }
            }
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });

    if let Some(id) = action.into_inner() {
        if want_edit.into_inner() {
            show_annotation_edit(parent, app, Some(id));
        } else {
            if let Some(a) = app.annotations.list.iter().find(|a| a.id == id) {
                app.scroll.goto_line(a.start_line);
                app.selection = Some(crate::selection::Selection {
                    start_line: a.start_line, start_col: a.start_col,
                    end_line: a.end_line, end_col: a.end_col,
                });
                app.invalidate_view();
                app.status_text = format!("跳转到批注 · 行 {}", a.start_line + 1);
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// 捐赠（GDI+ 二维码）
// ────────────────────────────────────────────────────────────────────────

pub unsafe fn show_donate(parent: HWND) {
    use windows_sys::Win32::Graphics::GdiPlus::{
        GdipCreateBitmapFromFile, GdipCreateFromHDC, GdipDeleteGraphics, GdipDisposeImage,
        GdipDrawImageI, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBitmap, GpGraphics,
        GpImage,
    };

    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        DebugEventCallback: 0,
        SuppressBackgroundThread: 0,
        SuppressExternalCodecs: 0,
    };
    let mut token = 0usize;
    let gdi_ok = GdiplusStartup(&mut token, &input, std::ptr::null_mut()) == 0;

    let w = 500;
    let h = 420;
    let hwnd = create_dialog(parent, "❤ 捐赠", w, h);
    if hwnd.is_null() {
        if gdi_ok {
            GdiplusShutdown(token);
        }
        return;
    }
    let mut ctx = DlgCtx {
        theme: crate::app::theme_for(parent),
        title: "❤ 捐赠".to_string(),
        w,
        h,
        tab: -1,
        tab_groups: Default::default(),
        buttons: Vec::new(),
        close_hover: false,
        ok_clicked: false,
        anno: Vec::new(),
        anno_sel: -1,
        text: None,
    };
    ctx.buttons.push(Button::new(RECT { left: 150, top: h - 50, right: w - 150, bottom: h - 18 }, BtnKind::Primary, "先白嫖着，下次一定"));

    // 加载二维码
    let mut wechat_bmp: *mut GpBitmap = std::ptr::null_mut();
    let mut alipay_bmp: *mut GpBitmap = std::ptr::null_mut();
    let mut wechat_ok = false;
    let mut alipay_ok = false;
    for p in asset_candidates("donate_wechat.png") {
        let pw: Vec<u16> = p.display().to_string().encode_utf16().chain(std::iter::once(0)).collect();
        if GdipCreateBitmapFromFile(pw.as_ptr(), &mut wechat_bmp) == 0 {
            wechat_ok = true;
            break;
        }
    }
    for p in asset_candidates("donate_alipay.png") {
        let pw: Vec<u16> = p.display().to_string().encode_utf16().chain(std::iter::once(0)).collect();
        if GdipCreateBitmapFromFile(pw.as_ptr(), &mut alipay_bmp) == 0 {
            alipay_ok = true;
            break;
        }
    }
    let wechat: *mut GpImage = wechat_bmp as *mut GpImage;
    let alipay: *mut GpImage = alipay_bmp as *mut GpImage;

    modal_loop(parent, hwnd, &mut ctx, |hwnd, ctx, msg, _wp, lp| {
        if msg == WM_PAINT {
            let mut ps = std::mem::zeroed::<PAINTSTRUCT>();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut r = std::mem::zeroed::<RECT>();
            GetClientRect(hwnd, &mut r);
            paint::fill_rect(hdc, &r, ctx.theme.bg_primary);
            widgets::title_bar(hdc, ctx.w, &ctx.theme, &ctx.title, ctx.close_hover);
            let title = crate::app::str_wide("❤ 为作者续命 ❤");
            paint::draw_text_c(hdc, (r.right - r.left) / 2 - 60, widgets::TITLE_H + 16, ctx.theme.error, &title);
            if gdi_ok {
                let mut graphics: *mut GpGraphics = std::ptr::null_mut();
                if GdipCreateFromHDC(hdc, &mut graphics) == 0 {
                    for (img, ok, x, label) in [
                        (wechat, wechat_ok, 60, "微信支付"),
                        (alipay, alipay_ok, 260, "支付宝"),
                    ] {
                        let box_rect = RECT { left: x, top: 100, right: x + 180, bottom: 280 };
                        if ok {
                            let _ = GdipDrawImageI(graphics, img, x, 100);
                        } else {
                            widgets::round_rect(hdc, &box_rect, 8, ctx.theme.bg_tertiary);
                            paint::draw_text_c(hdc, x + 45, 180, ctx.theme.text_disabled, &crate::app::str_wide("二维码缺失"));
                        }
                        paint::draw_text_c(hdc, x + 50, 290, ctx.theme.text_primary, &crate::app::str_wide(label));
                    }
                    GdipDeleteGraphics(graphics);
                }
            }
            let footer = crate::app::str_wide("如果 qview 帮到了你，欢迎请作者喝杯咖啡");
            paint::draw_text_c(hdc, (r.right - r.left) / 2 - 130, 340, ctx.theme.text_secondary, &footer);
            for b in &ctx.buttons {
                b.draw(hdc, &ctx.theme);
            }
            EndPaint(hwnd, &mut ps);
            return true;
        }
        if msg == WM_LBUTTONUP {
            let x = (lp & 0xFFFF) as i32;
            let y = (lp >> 16) as i32;
            if hit_btn(ctx, x, y) == Some(BTN_OK) && ctx.buttons[BTN_OK].state == BtnState::Pressed {
                DestroyWindow(hwnd);
            }
            for b in &mut ctx.buttons {
                b.state = BtnState::Normal;
            }
            let _ = InvalidateRect(hwnd, ptr::null(), 0);
            return true;
        }
        false
    });

    if !wechat.is_null() {
        GdipDisposeImage(wechat);
    }
    if !alipay.is_null() {
        GdipDisposeImage(alipay);
    }
    if gdi_ok {
        GdiplusShutdown(token);
    }
}

fn asset_candidates(name: &str) -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        v.push(cwd.join("assets").join(name));
        v.push(cwd.join("gui").join("assets").join(name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("assets").join(name));
            v.push(dir.join("gui").join("assets").join(name));
        }
    }
    v
}
