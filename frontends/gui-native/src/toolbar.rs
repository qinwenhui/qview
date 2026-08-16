//! 自绘工具栏：打开/关闭/重载 + 搜索开关（Aa/.*/\b）+ 搜索框（EDIT 子控件）
//! + 查找/清空/停止 + 上一/下一 + 行号跳转。全部画进全窗口双缓冲，
//! 鼠标命中测试由 msg.rs 调用 `hit` / `dispatch`。

use std::ffi::c_void;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::HFONT;
use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;

use crate::app::{str_wide, App, SearchFlag};
use crate::paint;
use crate::theme::Rgb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolbarAction {
    Open,
    Close,
    Reload,
    Case,
    Regex,
    Word,
    Find,
    Clear,
    Stop,
    Prev,
    Next,
    Goto,
}

pub struct ToolbarLayout {
    pub buttons: Vec<(ToolbarAction, RECT)>,
    pub search_rect: RECT,
    pub goto_rect: RECT,
}

const Y: i32 = 4;
const H: i32 = 28;
const GAP: i32 = 4;

/// 计算工具栏布局（`searching` 决定是否显示「停止」）。
pub fn layout(w: i32, searching: bool) -> ToolbarLayout {
    let mut buttons: Vec<(ToolbarAction, RECT)> = Vec::with_capacity(16);
    let mut x = 8;

    let push = |buttons: &mut Vec<(ToolbarAction, RECT)>, act: ToolbarAction, bw: i32, x: &mut i32| {
        buttons.push((act, RECT { left: *x, top: Y, right: *x + bw, bottom: Y + H }));
        *x += bw + GAP;
    };

    for (act, bw) in [(ToolbarAction::Open, 56), (ToolbarAction::Close, 56), (ToolbarAction::Reload, 56)] {
        push(&mut buttons, act, bw, &mut x);
    }
    x += GAP;
    for (act, bw) in [(ToolbarAction::Case, 36), (ToolbarAction::Regex, 36), (ToolbarAction::Word, 36)] {
        push(&mut buttons, act, bw, &mut x);
    }
    x += GAP;

    // 右侧：Go + 行号框（右对齐，固定）
    let right_edge = w - 8;
    let go_btn = RECT { left: right_edge - 40, top: Y, right: right_edge, bottom: Y + H };
    let goto_rect = RECT { left: right_edge - 40 - 70 - GAP, top: Y + 2, right: right_edge - 40 - GAP, bottom: Y + H - 2 };
    buttons.push((ToolbarAction::Goto, go_btn));

    // 搜索后的按钮（紧跟搜索框）：查找 / 清空 / 停止 / 上一 / 下一
    let mut after: Vec<(ToolbarAction, i32)> = vec![
        (ToolbarAction::Find, 56),
        (ToolbarAction::Clear, 48),
    ];
    if searching {
        after.push((ToolbarAction::Stop, 44));
    }
    after.push((ToolbarAction::Prev, 44));
    after.push((ToolbarAction::Next, 44));
    let after_total: i32 = after.iter().map(|(_, bw)| bw + GAP).sum();

    // 搜索框宽度：约 260，但收窄以避免与右侧按钮冲突
    let avail = goto_rect.left - x - after_total - GAP;
    let search_right = (x + 260).min(x + avail.max(40));
    let search_rect = RECT {
        left: x,
        top: Y + 2,
        right: search_right,
        bottom: Y + H - 2,
    };

    // 铺搜索后的按钮（从左到右）
    let mut sx = search_right + GAP;
    for (act, bw) in after {
        push(&mut buttons, act, bw, &mut sx);
    }

    ToolbarLayout { buttons, search_rect, goto_rect }
}

/// 命中测试（y 在工具栏范围内才调用）
pub fn hit(x: i32, y: i32, w: i32, searching: bool) -> Option<ToolbarAction> {
    let lay = layout(w, searching);
    for (act, r) in &lay.buttons {
        if x >= r.left && x <= r.right && y >= r.top && y <= r.bottom {
            return Some(*act);
        }
    }
    None
}

/// 绘制工具栏到缓冲 DC。
pub fn draw(hdc: *mut c_void, w: i32, app: &mut App) {
    let lay = layout(w, app.search.searching);
    let hover = app.toolbar_hover;
    let pressed = app.toolbar_pressed;
    let font = ensure_btn_font(app);
    let old = paint::select_font_safe(hdc, font);
    for (act, r) in &lay.buttons {
        draw_button(hdc, *act, r, hover == Some(*act), pressed == Some(*act), app);
    }
    paint::restore_font(hdc, old);
}

fn ensure_btn_font(app: &mut App) -> HFONT {
    if app.btn_font.is_none() {
        app.btn_font = Some(paint::create_font(12, "Segoe UI"));
    }
    app.btn_font.unwrap()
}

fn btn_bg(act: ToolbarAction, hovered: bool, pressed: bool, app: &App) -> Rgb {
    let t = &app.theme;
    // 固定语义色按钮
    let base = match act {
        ToolbarAction::Open => t.btn_primary,
        ToolbarAction::Close => t.btn_danger,
        ToolbarAction::Find => t.btn_success,
        ToolbarAction::Stop => t.btn_danger,
        _ => t.bg_tertiary,
    };
    // 搜索开关：激活时高亮
    let base = match act {
        ToolbarAction::Case if app.search.case_sensitive => t.btn_primary,
        ToolbarAction::Regex if app.search.use_regex => t.btn_primary,
        ToolbarAction::Word if app.search.whole_word => t.btn_primary,
        _ => base,
    };
    if pressed {
        t.bg_active
    } else if hovered {
        t.bg_hover
    } else {
        base
    }
}

fn draw_button(hdc: *mut c_void, act: ToolbarAction, r: &RECT, hovered: bool, pressed: bool, app: &App) {
    let bg = btn_bg(act, hovered, pressed, app);
    crate::widgets::round_rect(hdc, r, 5, bg);
    let text_color = match act {
        ToolbarAction::Case if app.search.case_sensitive => app.theme.bg_primary,
        ToolbarAction::Regex if app.search.use_regex => app.theme.bg_primary,
        ToolbarAction::Word if app.search.whole_word => app.theme.bg_primary,
        _ => app.theme.text_primary,
    };
    let label = match act {
        ToolbarAction::Open => "打开",
        ToolbarAction::Close => "关闭",
        ToolbarAction::Reload => "重载",
        ToolbarAction::Case => "Aa",
        ToolbarAction::Regex => ".*",
        ToolbarAction::Word => "\\b",
        ToolbarAction::Find => "查找",
        ToolbarAction::Clear => "清空",
        ToolbarAction::Stop => "停止",
        ToolbarAction::Prev => "<上一",
        ToolbarAction::Next => "下一>",
        ToolbarAction::Goto => "Go",
    };
    // 文本水平 + 垂直居中
    let label_w = str_wide(label);
    let tw = paint::text_width(hdc, &label_w);
    let th = paint::text_height(hdc, &label_w);
    let cx = r.left + (r.right - r.left - tw) / 2;
    let cy = r.top + (r.bottom - r.top - th) / 2;
    paint::draw_text_c(hdc, cx.max(r.left + 2), cy, text_color, &label_w);
}

/// 点击工具栏按钮后的动作。
pub fn dispatch(act: ToolbarAction, app: &mut App) {
    match act {
        ToolbarAction::Open => {
            if let Some(p) = crate::shell::pick_file() {
                app.open_path(p);
            }
        }
        ToolbarAction::Close => app.close_file(),
        ToolbarAction::Reload => {
            if let Some(p) = app.path.clone() {
                app.open_path(p);
            }
        }
        ToolbarAction::Case => app.toggle_search_flag(SearchFlag::Case),
        ToolbarAction::Regex => app.toggle_search_flag(SearchFlag::Regex),
        ToolbarAction::Word => app.toggle_search_flag(SearchFlag::Word),
        ToolbarAction::Find => app.submit_search(),
        ToolbarAction::Clear => app.clear_search(),
        ToolbarAction::Stop => {
            if let Some(ref b) = app.bridge {
                b.cancel_search();
                app.search.searching = false;
                app.invalidate_view();
            }
        }
        ToolbarAction::Prev => app.jump_hit(-1),
        ToolbarAction::Next => app.jump_hit(1),
        ToolbarAction::Goto => app.submit_goto(),
    }
}

extern "system" {
    fn InvalidateRect(hwnd: *mut c_void, rect: *const c_void, erase: i32) -> i32;
}

/// 只重绘工具栏区域（顶部 TOOLBAR_H 像素）
pub fn invalidate(app: &App) {
    unsafe {
        let mut r = std::mem::zeroed::<RECT>();
        let _ = GetClientRect(app.hwnd, &mut r);
        let inv = RECT { left: 0, top: 0, right: r.right, bottom: crate::app::TOOLBAR_H };
        let _ = InvalidateRect(app.hwnd, &inv as *const RECT as *const c_void, 0);
    }
}
