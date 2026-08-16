//! 顶层应用状态 + 主窗口 + 子控件。

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    SelectObject, SRCCOPY, HFONT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, WS_CHILD, WS_CLIPSIBLINGS, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE, WS_EX_CLIENTEDGE, ES_AUTOHSCROLL,
    CW_USEDEFAULT, GetClientRect, GetWindowTextW, MoveWindow,
};

extern "system" {
    fn InvalidateRect(hwnd: *mut c_void, rect: *const c_void, erase: i32) -> i32;
}

use crate::config;
use crate::engine_bridge::{Bridge, SearchState};
use crate::msg;
use crate::scroll::ScrollState;
use crate::theme::{self, ThemeColors};
use crate::view::{render_view, ViewMetrics};

pub mod ctrl {
    pub const BTN_OPEN: u16 = 2001;
    pub const BTN_CLOSE: u16 = 2002;
    pub const BTN_RELOAD: u16 = 2003;
    pub const EDIT_SEARCH: u16 = 2101;
    pub const BTN_SEARCH: u16 = 2102;
    pub const BTN_PREV: u16 = 2103;
    pub const BTN_NEXT: u16 = 2104;
    pub const EDIT_GOTO: u16 = 2201;
    pub const BTN_GOTO: u16 = 2202;
    pub const BTN_FONT_BIGGER: u16 = 2301;
    pub const BTN_FONT_SMALLER: u16 = 2302;
}

pub const TOOLBAR_H: i32 = 36;
pub const SEARCHBAR_H: i32 = 30;
pub const STATUSBAR_H: i32 = 24;

/// 编码清单：(配置键, 显示名)
pub const ENCODINGS: &[(&str, &str)] = &[
    ("UTF-8", "UTF-8 (Unicode)"),
    ("GBK", "GBK (简体中文)"),
    ("GB18030", "GB18030 (国标)"),
    ("GB2312", "GB2312 (简体中文)"),
    ("Big5", "Big5 (繁体中文)"),
    ("Shift_JIS", "Shift_JIS (日文)"),
    ("EUC-JP", "EUC-JP (日文)"),
    ("EUC-KR", "EUC-KR (韩文)"),
    ("windows-1252", "Windows-1252 (西欧)"),
    ("UTF-16LE", "UTF-16LE (Unicode)"),
    ("UTF-16BE", "UTF-16BE (Unicode)"),
];

pub struct App {
    pub hwnd: HWND,
    pub h_btn_open: HWND,
    pub h_btn_close: HWND,
    pub h_btn_reload: HWND,
    pub h_edit_search: HWND,
    pub h_btn_search: HWND,
    pub h_btn_prev: HWND,
    pub h_btn_next: HWND,
    pub h_edit_goto: HWND,
    pub h_btn_goto: HWND,
    pub h_btn_font_inc: HWND,
    pub h_btn_font_dec: HWND,
    pub btn_font: Option<HFONT>,
    pub toolbar_hover: Option<crate::toolbar::ToolbarAction>,
    pub toolbar_pressed: Option<crate::toolbar::ToolbarAction>,
    /// 视图排版结果（绘制 + 命中测试）
    pub view: crate::view::ViewLayout,
    pub wrap_factor: f64,
    pub char_width_cache: crate::layout::CharWidthCache,
    pub annotations: crate::annotations::Annotations,
    pub selection: Option<crate::selection::Selection>,
    pub selecting: bool,
    // 复用缓冲（热路径零分配）
    pub utf16_scratch: Vec<u16>,
    pub width_scratch: Vec<i32>,
    pub row_scratch: Vec<crate::layout::VisualRow>,
    pub byte_unit_scratch: Vec<usize>,
    pub path: Option<PathBuf>,
    pub bridge: Option<Bridge>,
    pub scroll: ScrollState,
    pub metrics: ViewMetrics,
    pub search: SearchState,
    /// 解析后的搜索查询（高亮与引擎一致），查询/开关变化时重建
    pub parsed_q: Option<qview_core::search::Query>,
    /// 当前命中字节偏移（engine.search.current()），每帧更新
    pub current_hit_byte: Option<u64>,
    pub first_visible_line: u64,
    pub last_visible_line: u64,
    pub row_h: i32,
    pub font_size: i32,
    pub status_text: String,
    pub search_status: String,
    pub file_size: u64,
    pub file_lines: u64,
    pub annotation_count: usize,
    pub status_font: Option<HFONT>,
    pub progress_cancel_rect: RECT,
    pub status_rects: crate::statusbar::StatusRects,
    pub vsb_thumb: RECT,
    pub hsb_thumb: RECT,
    pub vsb_track: RECT,
    pub hsb_track: RECT,
    // 主题与配置
    pub config: config::AppConfig,
    pub theme: ThemeColors,
    pub theme_idx: usize,
    // 全窗口双缓冲
    pub full_back_dc: *mut c_void,
    pub full_back_bmp: *mut c_void,
    pub full_back_old: *mut c_void,
    pub full_back_w: i32,
    pub full_back_h: i32,
}

impl App {
    pub fn new() -> Self {
        let config = config::AppConfig::load();
        let theme_idx = theme::find_index(&config.gui.theme);
        let theme = theme::builtin(theme_idx);
        let font_px = config.gui.font_size.max(8.0) as i32;
        let row_h = config.gui.row_height.max(14.0) as i32;
        Self {
            hwnd: ptr::null_mut(),
            h_btn_open: ptr::null_mut(), h_btn_close: ptr::null_mut(), h_btn_reload: ptr::null_mut(),
            h_edit_search: ptr::null_mut(), h_btn_search: ptr::null_mut(),
            h_btn_prev: ptr::null_mut(), h_btn_next: ptr::null_mut(),
            h_edit_goto: ptr::null_mut(), h_btn_goto: ptr::null_mut(),
            h_btn_font_inc: ptr::null_mut(), h_btn_font_dec: ptr::null_mut(),
            btn_font: None,
            toolbar_hover: None,
            toolbar_pressed: None,
            view: crate::view::ViewLayout::default(),
            wrap_factor: 1.0,
            char_width_cache: crate::layout::CharWidthCache::default(),
            annotations: crate::annotations::Annotations::load(),
            selection: None,
            selecting: false,
            utf16_scratch: Vec::new(),
            width_scratch: Vec::new(),
            row_scratch: Vec::new(),
            byte_unit_scratch: Vec::new(),
            path: None, bridge: None,
            scroll: ScrollState::default(),
            metrics: ViewMetrics { font_size_px: font_px, ..Default::default() },
            search: SearchState::default(),
            parsed_q: None,
            current_hit_byte: None,
            first_visible_line: 0,
            last_visible_line: 0,
            row_h, font_size: font_px,
            status_text: "就绪".into(), search_status: String::new(),
            file_size: 0, file_lines: 0, annotation_count: 0,
            status_font: None,
            progress_cancel_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            status_rects: crate::statusbar::StatusRects::default(),
            vsb_thumb: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            hsb_thumb: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            vsb_track: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            hsb_track: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            config, theme, theme_idx,
            full_back_dc: ptr::null_mut(), full_back_bmp: ptr::null_mut(),
            full_back_old: ptr::null_mut(), full_back_w: 0, full_back_h: 0,
        }
    }

    pub fn create_main_window(&mut self, instance: *mut c_void) -> HWND {
        unsafe {
            let cfg = &self.config;
            let (w, h) = (cfg.gui.window_size[0] as i32, cfg.gui.window_size[1] as i32);
            let (w, h) = (w.max(640), h.max(420));

            let title = "qview";
            let title_wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let hwnd = CreateWindowExW(
                0, msg::class_name_wide().as_ptr(), title_wide.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPSIBLINGS,
                CW_USEDEFAULT, CW_USEDEFAULT, w, h,
                ptr::null_mut(), ptr::null_mut(), instance, ptr::null(),
            );
            if hwnd.is_null() { return ptr::null_mut(); }
            self.hwnd = hwnd;
            msg::set_window_user_data(hwnd, self);
            crate::menu::rebuild(self);
            self.create_controls(instance);
            let _ = InvalidateRect(hwnd, ptr::null(), 1);
            hwnd
        }
    }

    fn create_controls(&mut self, instance: *mut c_void) {
        unsafe {
            let edit: Vec<u16> = "EDIT".encode_utf16().chain(std::iter::once(0)).collect();
            let w = client_w(self.hwnd);
            let lay = crate::toolbar::layout(w, false);
            // 搜索框（多行 EDIT，Enter=搜索，Shift+Enter=换行）
            self.h_edit_search = make_edit_multi(instance, self.hwnd, lay.search_rect, ctrl::EDIT_SEARCH, &edit);
            // 跳行框（单行）
            self.h_edit_goto = make_edit(instance, self.hwnd, lay.goto_rect, ctrl::EDIT_GOTO, &edit);
            crate::msg::subclass_edit(self.h_edit_search, crate::msg::EditKind::Search);
            crate::msg::subclass_edit(self.h_edit_goto, crate::msg::EditKind::Goto);
        }
    }

    pub fn relayout(&mut self) {
        unsafe {
            let w = client_w(self.hwnd);
            let lay = crate::toolbar::layout(w, self.search.searching);
            if !self.h_edit_search.is_null() {
                let _ = MoveWindow(self.h_edit_search, lay.search_rect.left, lay.search_rect.top,
                    lay.search_rect.right - lay.search_rect.left, lay.search_rect.bottom - lay.search_rect.top, 1);
            }
            if !self.h_edit_goto.is_null() {
                let _ = MoveWindow(self.h_edit_goto, lay.goto_rect.left, lay.goto_rect.top,
                    lay.goto_rect.right - lay.goto_rect.left, lay.goto_rect.bottom - lay.goto_rect.top, 1);
            }
            // 销毁旧全窗口缓冲
            self.destroy_full_backbuf();
        }
    }

    pub unsafe fn destroy_full_backbuf(&mut self) {
        if !self.full_back_dc.is_null() {
            SelectObject(self.full_back_dc, self.full_back_old);
            DeleteObject(self.full_back_bmp);
            DeleteDC(self.full_back_dc);
            self.full_back_dc = ptr::null_mut();
            self.full_back_bmp = ptr::null_mut();
        }
    }

    unsafe fn acquire_full_backbuf(&mut self, dc: *mut c_void, w: i32, h: i32) -> *mut c_void {
        if self.full_back_w != w || self.full_back_h != h || self.full_back_dc.is_null() {
            self.destroy_full_backbuf();
            self.full_back_dc = CreateCompatibleDC(dc);
            self.full_back_bmp = CreateCompatibleBitmap(dc, w, h);
            self.full_back_old = SelectObject(self.full_back_dc, self.full_back_bmp);
            self.full_back_w = w;
            self.full_back_h = h;
        }
        self.full_back_dc
    }

    /// 切换主题：更新色板 + 写回配置 + 全窗口重绘。
    pub fn switch_theme(&mut self, idx: usize) {
        let idx = idx % theme::THEME_NAMES.len();
        self.theme_idx = idx;
        self.theme = theme::builtin(idx);
        self.config.gui.theme = theme::THEME_NAMES[idx].to_string();
        self.config.save();
        unsafe { let _ = InvalidateRect(self.hwnd, ptr::null(), 1); }
    }

    /// 循环切换主题（Ctrl+Shift+T）。
    pub fn cycle_theme(&mut self) {
        let n = theme::THEME_NAMES.len();
        self.switch_theme((self.theme_idx + 1) % n);
    }

    pub fn open_path(&mut self, path: PathBuf) {
        let engine_cfg = self.config.engine.clone();
        match Bridge::open(&path, &engine_cfg) {
            Ok(b) => {
                self.file_size = b.size; self.file_lines = b.line_count;
                let bg = b.indexing_active();
                self.status_text = if bg {
                    format!("索引中... · {}", crate::paint::human_bytes(b.size))
                } else {
                    format!("已打开 · {} 行 · {}", b.line_count, crate::paint::human_bytes(b.size))
                };
                self.bridge = Some(b); self.path = Some(path.clone());
                self.scroll.reset();
                self.selection = None;
                self.config.add_recent(path);
                self.config.save();
                crate::menu::rebuild(self);
                self.refresh_annotations();
                unsafe { let _ = InvalidateRect(self.hwnd, ptr::null(), 1); }
            }
            Err(e) => { self.status_text = format!("打开失败: {}", e);
                unsafe { let _ = InvalidateRect(self.hwnd, ptr::null(), 1); }
            }
        }
    }

    pub fn submit_search(&mut self) {
        let q = unsafe {
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(self.h_edit_search, buf.as_mut_ptr(), buf.len() as i32);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        };
        self.search.query = q.clone();
        self.search.cursor = 0;
        self.search.total = 0;
        self.search_status.clear();

        let crlf = self.bridge.as_ref().map_or(false, |b| b.uses_crlf());
        if q.is_empty() {
            self.parsed_q = None;
            self.current_hit_byte = None;
            self.search.searching = false;
            self.search.status.clear();
        } else {
            let opts = qview_core::search::SearchOptions {
                case_sensitive: self.search.case_sensitive,
                use_regex: self.search.use_regex,
                whole_word: self.search.whole_word,
                crlf,
            };
            // 缓存解析查询，供视图高亮与引擎搜索保持一致
            self.parsed_q = qview_core::search::parse_query(&q, &opts).ok();
            if let Some(ref mut b) = self.bridge {
                let _ = b.submit_search(q, opts);
            }
            self.search.searching = true;
            self.search.status = "搜索中...".into();
            self.config.add_search_history(self.search.query.clone());
            self.config.save();
        }
        unsafe { let _ = InvalidateRect(self.hwnd, ptr::null(), 1); }
    }

    pub fn submit_goto(&mut self) {
        let s = unsafe {
            let mut buf = [0u16; 32];
            let n = GetWindowTextW(self.h_edit_goto, buf.as_mut_ptr(), buf.len() as i32);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        };
        if let Ok(n) = s.trim().parse::<u64>() {
            if n > 0 {
                self.scroll.goto_line(n - 1);
                unsafe { let _ = InvalidateRect(self.hwnd, ptr::null(), 1); }
            }
        }
    }

    /// 关闭当前文件。
    pub fn close_file(&mut self) {
        self.bridge = None;
        self.path = None;
        self.status_text = "已关闭".into();
        self.selection = None;
        self.refresh_annotations();
        self.invalidate_view();
    }

    /// 刷新当前文件批注列表与计数。
    pub fn refresh_annotations(&mut self) {
        let path = self.path.clone();
        self.annotations.reload(path.as_deref());
        self.annotation_count = self
            .path
            .as_ref()
            .map(|p| self.annotations.store.count(p))
            .unwrap_or(0);
        self.invalidate_view();
    }

    pub fn paint(&mut self, hdc: *mut c_void, rect: &RECT) {
        unsafe {
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            if w <= 0 || h <= 0 { return; }

            // === 全窗口双缓冲 ===
            let buf = self.acquire_full_backbuf(hdc, w, h);
            // 填充背景
            crate::paint::fill_rect(buf, &RECT { left: 0, top: 0, right: w, bottom: h }, self.theme.bg_primary);

            // --- 工具栏（自绘，编辑框是子控件浮在上面）---
            crate::toolbar::draw(buf, w, self);

            let view_top = TOOLBAR_H + 2;
            let status_top = h - STATUSBAR_H;
            let progress_top = if crate::statusbar::has_progress(self) {
                status_top - crate::statusbar::PROGRESS_H
            } else {
                status_top
            };

            // --- 主视图 ---
            let vr = RECT { left: 0, top: view_top, right: w, bottom: progress_top - 1 };
            render_view(buf, &vr, self);

            // --- 状态栏（进度行 + 状态行）---
            let mut status_rects = self.status_rects;
            crate::statusbar::render(buf, w, progress_top, status_top, h, self, &mut status_rects);
            self.status_rects = status_rects;

            // === BitBlt 全窗口 → 屏幕 ===
            BitBlt(hdc, rect.left, rect.top, w, h, buf, 0, 0, SRCCOPY);
        }
    }

    pub fn total_lines(&self) -> u64 {
        self.bridge.as_ref().map(|b| b.total_lines()).unwrap_or(0)
    }

    /// 只重绘视图区+状态栏（跳过工具栏，避免子控件闪烁）。
    pub fn invalidate_view(&self) {
        unsafe {
            let mut r = std::mem::zeroed::<RECT>();
            let _ = GetClientRect(self.hwnd, &mut r);
            let top = TOOLBAR_H + SEARCHBAR_H + 2;
            let inv = RECT { left: 0, top, right: r.right, bottom: r.bottom };
            let _ = InvalidateRect(self.hwnd, &inv as *const RECT as *const c_void, 0);
        }
    }

    pub fn font_inc(&mut self) {
        self.metrics.font_size_px = (self.metrics.font_size_px + 1).min(36);
        self.metrics.invalidate();
        self.invalidate_view();
    }
    pub fn font_dec(&mut self) {
        if self.metrics.font_size_px > 8 {
            self.metrics.font_size_px -= 1;
        }
        self.metrics.invalidate();
        self.invalidate_view();
    }
    pub fn font_reset(&mut self) {
        self.metrics.font_size_px = self.config.gui.font_size.max(8.0) as i32;
        self.metrics.invalidate();
        self.invalidate_view();
    }

    /// 跳到下一个/上一个命中（delta=+1/-1），带视口锚定语义。
    ///
    /// 视口里已高亮的命中 → 顺着它相对跳转；视口里没有（用户滚去别处）→
    /// 把搜索锚定到视口顶行之后第一个命中再开始找。
    pub fn jump_hit(&mut self, delta: i64) {
        if self.bridge.as_ref().map_or(true, |b| b.search_len() == 0) {
            return;
        }

        // 当前命中是否落在视口内（逻辑行 vs 首/末可见逻辑行）
        let cursor_visible = self
            .bridge
            .as_ref()
            .and_then(|b| b.search_current())
            .map_or(false, |m| {
                let l = self.bridge.as_ref().map(|b| b.hit_line_of(&m)).unwrap_or(0);
                l >= self.first_visible_line && l <= self.last_visible_line
            });

        if !cursor_visible {
            let max_line = self.total_lines().saturating_sub(1);
            let top_line = self.first_visible_line.min(max_line);
            let seek_ok = self.bridge.as_ref().map_or(false, |b| {
                let first_byte = b.line_start_byte(top_line);
                b.search_seek_to_byte(first_byte)
            });
            if seek_ok {
                if delta > 0 {
                    // 锚定的那个命中就是“下一个”
                    let anchored = self.bridge.as_ref().map(|b| b.search_cursor()).unwrap_or(0);
                    let m = self.bridge.as_ref().and_then(|b| b.search_jump(anchored));
                    if let Some(m) = m {
                        let total = self.bridge.as_ref().map(|b| b.search_len()).unwrap_or(0);
                        let (line, line_start) = {
                            let b = self.bridge.as_ref().unwrap();
                            let l = b.hit_line_of(&m);
                            (l, b.line_start_byte(l))
                        };
                        self.anchor_to_match(line, line_start, m.byte);
                        self.search.cursor = anchored;
                        self.search.status = format!("{}/{} 条匹配", anchored + 1, total);
                        self.current_hit_byte = Some(m.byte);
                        self.invalidate_view();
                    }
                    return;
                }
                // delta < 0：落到下面的相对跳转，以锚定游标为基准
            } else {
                // 视口之后没有命中 → 环绕
                let wrap_msg = if delta > 0 { "已到最后 · " } else { "已到头 · " };
                let m = self.bridge.as_ref().and_then(|b| {
                    if delta > 0 { b.search_first() } else { b.search_last() }
                });
                if let Some(m) = m {
                    let total = self.bridge.as_ref().map(|b| b.search_len()).unwrap_or(0);
                    let cursor = self.bridge.as_ref().map(|b| b.search_cursor()).unwrap_or(0);
                    let (line, line_start) = {
                        let b = self.bridge.as_ref().unwrap();
                        let l = b.hit_line_of(&m);
                        (l, b.line_start_byte(l))
                    };
                    self.anchor_to_match(line, line_start, m.byte);
                    self.search.cursor = cursor;
                    self.search.status = format!("{}{}/{} 条匹配", wrap_msg, cursor + 1, total);
                    self.current_hit_byte = Some(m.byte);
                    self.invalidate_view();
                }
                return;
            }
        }

        // 相对导航
        let m = self.bridge.as_ref().and_then(|b| b.search_jump_by(delta));
        if let Some(m) = m {
            let total = self.bridge.as_ref().map(|b| b.search_len()).unwrap_or(0);
            let cursor = self.bridge.as_ref().map(|b| b.search_cursor()).unwrap_or(0);
            let (line, line_start) = {
                let b = self.bridge.as_ref().unwrap();
                let l = b.hit_line_of(&m);
                (l, b.line_start_byte(l))
            };
            self.anchor_to_match(line, line_start, m.byte);
            self.search.cursor = cursor;
            self.search.status = format!("{}/{} 条匹配", cursor + 1, total);
            self.current_hit_byte = Some(m.byte);
            self.invalidate_view();
        }
    }

    /// 把视口锚定到命中附近。`scroll.y` 是「有效可视行」，而命中给的是逻辑行，
    /// 所以用 `逻辑行 × wrap_factor` 换算；长行内再按「命中前的字节数 ÷ 每行
    /// 可容纳字节数」估算子行，让命中落在视口上 1/3 处（近似，渲染再精确夹取）。
    fn anchor_to_match(&mut self, line: u64, line_start: u64, byte: u64) {
        let rel = byte.saturating_sub(line_start);
        let mut row = (line as f64 * self.wrap_factor.max(1.0)) as i64;
        if self.config.gui.word_wrap && rel > 0 {
            let avg_cw = self.char_width_cache.ascii[48].max(6) as f64;
            let content_w = (self.view.text_right - self.view.content_x).max(40) as f64;
            let per_row = (content_w / avg_cw).max(8.0);
            row += (rel as f64 / per_row) as i64;
        }
        let page = self.scroll.page_size_lines.max(1);
        self.scroll.y = (row - page / 3).max(0);
    }

    /// 清除搜索：清状态 + 取消后台任务 + 清空搜索框。
    pub fn clear_search(&mut self) {
        self.search.cancel();
        self.parsed_q = None;
        self.current_hit_byte = None;
        if let Some(ref b) = self.bridge {
            b.cancel_search();
        }
        unsafe {
            if !self.h_edit_search.is_null() {
                let empty: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
                let _ = windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW(
                    self.h_edit_search, empty.as_ptr(),
                );
            }
        }
        self.invalidate_view();
    }

    /// 搜索结果到达后：把游标锚到视口顶行之后第一个命中，让“下一个”从用户
    /// 正在看的地方开始。
    ///
    /// 注意用 `first_visible_line`（逻辑行，由渲染发布），不能直接用 scroll.y
    /// —— 那是「有效可视行」，换行时 ≠ 逻辑行。
    pub fn anchor_search_to_viewport(&mut self) {
        let b = match &self.bridge {
            Some(b) => b,
            None => return,
        };
        if b.search_len() == 0 {
            return;
        }
        let top_line = self.first_visible_line.min(self.total_lines().saturating_sub(1));
        let first_byte = b.line_start_byte(top_line);
        let _ = b.search_seek_to_byte(first_byte);
        self.search.cursor = b.search_cursor();
        self.current_hit_byte = b.search_current().map(|m| m.byte);
    }

    /// 当前搜索切换一个开关（Aa / .* / \b），下次 Enter/查找生效。
    pub fn toggle_search_flag(&mut self, flag: SearchFlag) {
        let g = &mut self.config.gui;
        match flag {
            SearchFlag::Case => { self.search.case_sensitive = !self.search.case_sensitive; g.case_sensitive = self.search.case_sensitive; }
            SearchFlag::Regex => { self.search.use_regex = !self.search.use_regex; g.use_regex = self.search.use_regex; }
            SearchFlag::Word => { self.search.whole_word = !self.search.whole_word; g.whole_word = self.search.whole_word; }
        }
        self.config.save();
        crate::menu::rebuild(self);
    }
}

/// 搜索开关
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SearchFlag {
    Case,
    Regex,
    Word,
}

/// 把 &str 转成带结尾 \0 的 UTF-16。
pub fn str_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 从父窗口取当前主题（对话框用）。
pub fn theme_for(parent: *mut c_void) -> ThemeColors {
    unsafe {
        let app = crate::msg::get_window_user_data(parent);
        if app.is_null() {
            theme::builtin(0)
        } else {
            (*app).theme
        }
    }
}

fn client_w(hwnd: HWND) -> i32 {
    unsafe {
        let mut r = std::mem::zeroed::<RECT>();
        let _ = GetClientRect(hwnd, &mut r);
        r.right
    }
}

unsafe fn make_edit(
    instance: *mut c_void, parent: HWND, r: RECT, id: u16, class: &[u16],
) -> HWND {
    let txt: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
    CreateWindowExW(
        WS_EX_CLIENTEDGE, class.as_ptr(), txt.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | ES_AUTOHSCROLL as u32,
        r.left, r.top, r.right - r.left, r.bottom - r.top,
        parent, id as usize as *mut c_void, instance, ptr::null(),
    )
}

/// 多行 EDIT（搜索框：Enter=搜索，Shift+Enter=换行，由子类化拦截）
unsafe fn make_edit_multi(
    instance: *mut c_void, parent: HWND, r: RECT, id: u16, class: &[u16],
) -> HWND {
    use windows_sys::Win32::UI::WindowsAndMessaging as wm;
    let txt: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
    CreateWindowExW(
        WS_EX_CLIENTEDGE, class.as_ptr(), txt.as_ptr(),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS
            | (wm::ES_MULTILINE | wm::ES_AUTOVSCROLL | wm::ES_WANTRETURN | wm::ES_LEFT) as u32,
        r.left, r.top, r.right - r.left, r.bottom - r.top,
        parent, id as usize as *mut c_void, instance, ptr::null(),
    )
}
