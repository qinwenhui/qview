//! 菜单：运行时整条重建 + 命令分发。
//!
//! 状态变化（主题 / 显示开关 / 搜索开关 / 最近文件 / 打开文件）都会整条重建
//! 菜单（`rebuild` → `SetMenu` 换新 handle，系统自动释放旧的），保证勾选、
//! 单选、动态子菜单永远与状态一致。重建不在热路径，代价可忽略。

use std::ffi::c_void;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateMenu, CreatePopupMenu, SetMenu,
    MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING,
};

use crate::app::App;
use crate::theme;

pub const IDM_FILE_OPEN: u16 = 1001;
pub const IDM_FILE_CLOSE: u16 = 1002;
pub const IDM_FILE_RELOAD: u16 = 1003;
pub const IDM_FILE_EXIT: u16 = 1004;
pub const IDM_FILE_PROPERTIES: u16 = 1005;
pub const IDM_RECENT_BASE: u16 = 1100; // +0..9 最近文件

pub const IDM_VIEW_TOP: u16 = 1201;
pub const IDM_VIEW_BOTTOM: u16 = 1202;
pub const IDM_VIEW_GOTO: u16 = 1203;
pub const IDM_THEME_BASE: u16 = 1210; // +0..5 主题 radio
pub const IDM_VIEW_LINENUMS: u16 = 1221;
pub const IDM_VIEW_WORDWRAP: u16 = 1222;
pub const IDM_VIEW_WHITESPACE: u16 = 1223;
pub const IDM_VIEW_INDENTGUIDE: u16 = 1224;
pub const IDM_VIEW_LEVELCOLOR: u16 = 1225;
pub const IDM_VIEW_FONT_BIGGER: u16 = 1226;
pub const IDM_VIEW_FONT_SMALLER: u16 = 1227;
pub const IDM_VIEW_FONT_RESET: u16 = 1228;

pub const IDM_SEARCH_FIND: u16 = 1301;
pub const IDM_SEARCH_NEXT: u16 = 1302;
pub const IDM_SEARCH_PREV: u16 = 1303;
pub const IDM_SEARCH_CASE: u16 = 1304;
pub const IDM_SEARCH_REGEX: u16 = 1305;
pub const IDM_SEARCH_WORD: u16 = 1306;

pub const IDM_TOOLS_CACHE: u16 = 1401;
pub const IDM_TOOLS_SETTINGS: u16 = 1402;

pub const IDM_DONATE: u16 = 1501;

pub const IDM_HELP_HELP: u16 = 1601;
pub const IDM_HELP_SHORTCUTS: u16 = 1602;
pub const IDM_HELP_ABOUT: u16 = 1603;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 截断过长路径（显示用）。
fn short_path(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if s.chars().count() <= 48 {
        s
    } else {
        let tail: String = s.chars().skip(s.chars().count() - 47).collect();
        format!("…{}", tail)
    }
}

/// 构造完整菜单栏（读 app 当前状态）。
pub unsafe fn build_menu_bar(app: &App) -> *mut c_void {
    let bar = CreateMenu();

    // ── 文件 ──
    let file = CreatePopupMenu();
    AppendMenuW(file, MF_STRING, IDM_FILE_OPEN as usize, wide("&打开文件...\tCtrl+O").as_ptr());
    AppendMenuW(file, MF_STRING, IDM_FILE_CLOSE as usize, wide("&关闭\tCtrl+W").as_ptr());
    AppendMenuW(file, MF_STRING, IDM_FILE_RELOAD as usize, wide("&重新加载\tCtrl+R").as_ptr());
    // 最近打开子菜单
    let recent = CreatePopupMenu();
    if app.config.recent_files.is_empty() {
        AppendMenuW(recent, MF_GRAYED | MF_STRING, 0, wide("（无）").as_ptr());
    } else {
        for (i, p) in app.config.recent_files.iter().enumerate().take(10) {
            let label = format!("&{} {}", i + 1, short_path(p));
            AppendMenuW(recent, MF_STRING, (IDM_RECENT_BASE + i as u16) as usize, wide(&label).as_ptr());
        }
    }
    AppendMenuW(file, MF_POPUP, recent as usize, wide("最近打开 ▸").as_ptr());
    AppendMenuW(file, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(file, MF_STRING, IDM_FILE_PROPERTIES as usize, wide("文件属性...\tCtrl+I").as_ptr());
    AppendMenuW(file, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(file, MF_STRING, IDM_FILE_EXIT as usize, wide("&退出\tAlt+F4").as_ptr());
    AppendMenuW(bar, MF_POPUP, file as usize, wide("&文件").as_ptr());

    // ── 视图 ──
    let view = CreatePopupMenu();
    AppendMenuW(view, MF_STRING, IDM_VIEW_TOP as usize, wide("跳转到顶部\tHome").as_ptr());
    AppendMenuW(view, MF_STRING, IDM_VIEW_BOTTOM as usize, wide("跳转到底部\tEnd").as_ptr());
    AppendMenuW(view, MF_STRING, IDM_VIEW_GOTO as usize, wide("跳转到行...\tCtrl+L").as_ptr());
    AppendMenuW(view, MF_SEPARATOR, 0, std::ptr::null());
    // 主题子菜单（radio）
    let theme_menu = CreatePopupMenu();
    for (i, name) in theme::THEME_NAMES.iter().enumerate() {
        let flags = MF_STRING
            | if i == app.theme_idx { MF_CHECKED } else { 0 };
        AppendMenuW(theme_menu, flags, (IDM_THEME_BASE + i as u16) as usize, wide(name).as_ptr());
    }
    AppendMenuW(view, MF_POPUP, theme_menu as usize, wide("主题 ▸").as_ptr());
    AppendMenuW(view, MF_SEPARATOR, 0, std::ptr::null());
    let g = &app.config.gui;
    append_check(view, IDM_VIEW_LINENUMS, "显示行号", g.show_line_numbers);
    append_check(view, IDM_VIEW_WORDWRAP, "自动换行", g.word_wrap);
    append_check(view, IDM_VIEW_WHITESPACE, "显示空白字符", g.show_whitespace);
    append_check(view, IDM_VIEW_INDENTGUIDE, "缩进参考线", g.show_indent_guides);
    append_check(view, IDM_VIEW_LEVELCOLOR, "日志级别着色", g.level_coloring);
    AppendMenuW(view, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(view, MF_STRING, IDM_VIEW_FONT_BIGGER as usize, wide("字体放大\tCtrl++").as_ptr());
    AppendMenuW(view, MF_STRING, IDM_VIEW_FONT_SMALLER as usize, wide("字体缩小\tCtrl+-").as_ptr());
    AppendMenuW(view, MF_STRING, IDM_VIEW_FONT_RESET as usize, wide("字体重置\tCtrl+0").as_ptr());
    AppendMenuW(bar, MF_POPUP, view as usize, wide("视图(&V)").as_ptr());

    // ── 搜索 ──
    let search = CreatePopupMenu();
    AppendMenuW(search, MF_STRING, IDM_SEARCH_FIND as usize, wide("查找...\tCtrl+F").as_ptr());
    AppendMenuW(search, MF_STRING, IDM_SEARCH_NEXT as usize, wide("下一个\tF3").as_ptr());
    AppendMenuW(search, MF_STRING, IDM_SEARCH_PREV as usize, wide("上一个\tShift+F3").as_ptr());
    AppendMenuW(search, MF_SEPARATOR, 0, std::ptr::null());
    append_check(search, IDM_SEARCH_CASE, "大小写敏感", app.search.case_sensitive);
    append_check(search, IDM_SEARCH_REGEX, "正则表达式", app.search.use_regex);
    append_check(search, IDM_SEARCH_WORD, "整词匹配", app.search.whole_word);
    AppendMenuW(bar, MF_POPUP, search as usize, wide("搜索(&S)").as_ptr());

    // ── 工具 ──
    let tools = CreatePopupMenu();
    AppendMenuW(tools, MF_STRING, IDM_TOOLS_CACHE as usize, wide("缓存管理...").as_ptr());
    AppendMenuW(tools, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(tools, MF_STRING, IDM_TOOLS_SETTINGS as usize, wide("设置...").as_ptr());
    AppendMenuW(bar, MF_POPUP, tools as usize, wide("工具(&T)").as_ptr());

    // ── 捐赠（顶级）──
    AppendMenuW(bar, MF_STRING, IDM_DONATE as usize, wide("❤ 捐赠").as_ptr());

    // ── 帮助 ──
    let help = CreatePopupMenu();
    AppendMenuW(help, MF_STRING, IDM_HELP_HELP as usize, wide("使用说明\tF1").as_ptr());
    AppendMenuW(help, MF_STRING, IDM_HELP_SHORTCUTS as usize, wide("快捷键一览").as_ptr());
    AppendMenuW(help, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(help, MF_STRING, IDM_HELP_ABOUT as usize, wide("关于 qview").as_ptr());
    AppendMenuW(bar, MF_POPUP, help as usize, wide("帮助(&H)").as_ptr());

    bar
}

unsafe fn append_check(menu: *mut c_void, id: u16, label: &str, checked: bool) {
    let flags = MF_STRING | if checked { MF_CHECKED } else { 0 };
    AppendMenuW(menu, flags, id as usize, wide(label).as_ptr());
}

/// 整条重建并挂到窗口。
pub fn rebuild(app: &App) {
    unsafe {
        let h = build_menu_bar(app);
        SetMenu(app.hwnd, h as *mut c_void);
    }
}

/// 菜单命令分发。
pub fn dispatch(id: u16, app: &mut App, hwnd: *mut c_void) {
    unsafe {
        match id {
            IDM_FILE_OPEN => {
                if let Some(p) = crate::shell::pick_file() {
                    app.open_path(p);
                }
            }
            IDM_FILE_CLOSE => app.close_file(),
            IDM_FILE_RELOAD => {
                if let Some(p) = app.path.clone() {
                    app.open_path(p);
                }
            }
            IDM_FILE_PROPERTIES => {
                crate::dlg::show_properties(hwnd, app);
            }
            IDM_FILE_EXIT => {
                use windows_sys::Win32::UI::WindowsAndMessaging::PostQuitMessage;
                PostQuitMessage(0);
            }
            IDM_SEARCH_FIND => crate::msg::focus_search(app),
            IDM_SEARCH_NEXT => {
                app.jump_hit(1);
            }
            IDM_SEARCH_PREV => {
                app.jump_hit(-1);
            }
            IDM_SEARCH_CASE => {
                app.search.case_sensitive = !app.search.case_sensitive;
                app.config.gui.case_sensitive = app.search.case_sensitive;
                app.config.save();
                crate::menu::rebuild(app);
            }
            IDM_SEARCH_REGEX => {
                app.search.use_regex = !app.search.use_regex;
                app.config.gui.use_regex = app.search.use_regex;
                app.config.save();
                crate::menu::rebuild(app);
            }
            IDM_SEARCH_WORD => {
                app.search.whole_word = !app.search.whole_word;
                app.config.gui.whole_word = app.search.whole_word;
                app.config.save();
                crate::menu::rebuild(app);
            }
            IDM_VIEW_TOP => {
                app.scroll.top();
                app.invalidate_view();
            }
            IDM_VIEW_BOTTOM => {
                app.scroll.bottom(app.total_lines());
                app.invalidate_view();
            }
            IDM_VIEW_GOTO => crate::msg::focus_goto(app),
            IDM_VIEW_LINENUMS => {
                app.config.gui.show_line_numbers = !app.config.gui.show_line_numbers;
                app.config.save();
                crate::menu::rebuild(app);
                app.invalidate_view();
            }
            IDM_VIEW_WORDWRAP => {
                app.config.gui.word_wrap = !app.config.gui.word_wrap;
                app.config.save();
                crate::menu::rebuild(app);
                app.metrics.invalidate();
                app.invalidate_view();
            }
            IDM_VIEW_WHITESPACE => {
                app.config.gui.show_whitespace = !app.config.gui.show_whitespace;
                app.config.save();
                crate::menu::rebuild(app);
                app.invalidate_view();
            }
            IDM_VIEW_INDENTGUIDE => {
                app.config.gui.show_indent_guides = !app.config.gui.show_indent_guides;
                app.config.save();
                crate::menu::rebuild(app);
                app.invalidate_view();
            }
            IDM_VIEW_LEVELCOLOR => {
                app.config.gui.level_coloring = !app.config.gui.level_coloring;
                app.config.save();
                crate::menu::rebuild(app);
                app.invalidate_view();
            }
            IDM_VIEW_FONT_BIGGER => {
                app.font_inc();
            }
            IDM_VIEW_FONT_SMALLER => {
                app.font_dec();
            }
            IDM_VIEW_FONT_RESET => {
                app.font_reset();
            }
            IDM_TOOLS_CACHE => {
                crate::dlg::show_index_manager(hwnd, app);
            }
            IDM_TOOLS_SETTINGS => {
                crate::dlg::show_settings(hwnd, app);
            }
            IDM_DONATE => {
                crate::dlg::show_donate(hwnd);
            }
            IDM_HELP_HELP => {
                crate::dlg::show_help(hwnd);
            }
            IDM_HELP_SHORTCUTS => {
                crate::dlg::show_shortcuts(hwnd);
            }
            IDM_HELP_ABOUT => {
                crate::dlg::show_about(hwnd);
            }
            _ => {
                // 最近文件 / 主题
                if (IDM_RECENT_BASE..IDM_RECENT_BASE + 10).contains(&id) {
                    let idx = (id - IDM_RECENT_BASE) as usize;
                    if let Some(p) = app.config.recent_files.get(idx) {
                        let p = p.clone();
                        app.open_path(p);
                    }
                } else if (IDM_THEME_BASE..IDM_THEME_BASE + 6).contains(&id) {
                    let idx = (id - IDM_THEME_BASE) as usize;
                    app.switch_theme(idx);
                    crate::menu::rebuild(app);
                }
            }
        }
    }
}

/// 菜单 ID 是否本应用范围内。
pub fn matches(id: u16) -> bool {
    (1001..=1005).contains(&id)
        || (IDM_RECENT_BASE..IDM_RECENT_BASE + 10).contains(&id)
        || (1201..=1228).contains(&id)
        || (IDM_THEME_BASE..IDM_THEME_BASE + 6).contains(&id)
        || (1301..=1306).contains(&id)
        || (1401..=1402).contains(&id)
        || id == IDM_DONATE
        || (1601..=1603).contains(&id)
}
