//! 顶层应用状态 + AppDelegate + 主循环。
//!
//! 线程模型：全部 AppKit 只跑主线程。引擎 `Arc<Mutex<Engine>>` 在 Bridge，
//! core 后台索引/搜索用 rayon 线程，主线程只短暂 lock 调 submit/poll/read_line。
//!
//! ## 重入不变量（关键）
//!
//! `runModal`（NSOpenPanel / NSAlert）会跑嵌套 runloop，再次触发 timer → 可能
//! 二次进入 `with_app` 拿到第二个 `&mut App`（UB）。
//!
//! 硬规则：**先在 `with_app` 短闭包内取出所需数据（如窗口引用），再开 modal，
//! modal 返回后再进 `with_app` 应用结果**。绝不在 `with_app` 闭包内调用任何
//! modal 对话框。

use std::cell::{OnceCell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSBorderType, NSButton, NSMenuDelegate, NSModalResponse, NSScreen, NSScrollView,
    NSSearchField, NSTextField, NSToolbar, NSToolbarDelegate, NSToolbarItem,
    NSToolbarItemIdentifier, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWindowToolbarStyle,
};
use objc2_core_graphics::CGContext;
use objc2_core_text::CTLine;
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString,
};

use crate::bridge::Bridge;
use crate::config::AppConfig;
use crate::selection::{Selection, TextPoint};
use crate::settings_sheet::SettingsSheet;
use crate::theme::{theme_by_name, theme_names, Rgba, ThemeColors};
use crate::view::LogView;
use crate::window::RootView;

/// 全局单例指针（`Box::into_raw` 的 App）。
pub static mut APP_PTR: *mut App = ptr::null_mut();

/// 在主线程用 `&mut App` 安全访问全局应用状态。
///
/// 注意：ObjC 回调（timer / action / drawRect）只会发生在主线程，但 `runModal`
/// 等会嵌套 runloop → 禁止在闭包内调用任何 modal（见本模块顶部不变量）。
#[inline]
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> R {
    unsafe {
        let p = APP_PTR;
        assert!(!p.is_null(), "APP_PTR is null");
        let app = &mut *p;
        f(app)
    }
}

/// 布局常量：顶部标题栏 + 统一工具栏高度（FullSizeContentView 下内容从这以下开始）。
pub const TOOLBAR_H: f64 = 52.0;
pub const STATUSBAR_H: f64 = 26.0;
pub const GUTTER_W: f64 = 72.0;
/// 行号/色条左侧内边距。
pub const TEXT_LEFT_PAD: f64 = 8.0;
pub const HIT_BAR_W: f64 = 3.0;

/// 从后台进度消息（如 "indexing... 42%" / "searching... 42%"）解析百分比。
fn parse_progress_pct(msg: &str) -> Option<f64> {
    let tail = msg.trim().rsplit(' ').next()?;
    let num = tail.strip_suffix('%')?.trim();
    num.parse::<f64>().ok().map(|n| n.clamp(0.0, 100.0))
}

/// 一次渲染中一个可见行（已算好几何与内容，避免二次读行）。
struct VisRow {
    row: u64,
    y: f64,
    h: f64,
    text: String,
    matches: Vec<(usize, usize)>,
    current_matches: Vec<(usize, usize)>,
    text_color: crate::theme::Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

pub struct App {
    pub config: AppConfig,
    pub bridge: Option<Bridge>,
    pub theme_name: String,
    pub font_size: f64,
    pub row_h: f64,
    pub show_line_numbers: bool,
    pub word_wrap: bool,
    pub show_whitespace: bool,
    pub level_coloring: bool,
    pub show_indent_guides: bool,
    pub view_w: f64,
    pub view_h: f64,
    pub max_content_w: f64,
    /// 最近一次渲染实测到的内容高（≥ 估算值，防止滚动到底出现空洞）。
    pub rendered_content_h: f64,
    pub hit_lines: Vec<u64>,
    pub search_active: bool,
    pub indexing_active: bool,
    pub font: Option<crate::text::Font>,
    pub current_line: u64,
    /// 鼠标悬停的行（渲染高亮；None = 未悬停）。
    pub hover_line: Option<u64>,
    /// 拖选文本选择（Phase 4）。
    pub selection: Selection,

    /// AppDelegate 强引用（菜单/工具栏 action target，保证对象存活）。
    pub delegate: OnceCell<Retained<AppDelegate>>,

    // ---- UI handles（setup_ui 填充）----
    pub window: OnceCell<Retained<NSWindow>>,
    pub root_view: OnceCell<Retained<RootView>>,
    pub log_view: OnceCell<Retained<LogView>>,
    pub scroll_view: OnceCell<Retained<NSScrollView>>,
    /// 主工具栏（搜索/跳转输入框句柄在 AppDelegate ivars，见 app.rs）。
    pub toolbar: OnceCell<Retained<NSToolbar>>,
    pub status_left: OnceCell<Retained<NSTextField>>,
    pub status_mid: OnceCell<Retained<NSTextField>>,
    pub status_right: OnceCell<Retained<NSTextField>>,
    pub progress: OnceCell<Retained<objc2_app_kit::NSProgressIndicator>>,
    pub btn_cancel: OnceCell<Retained<NSButton>>,
    /// 瞬态状态栏提示：(文本, 起始时刻, 持续秒数)；过期即清除。
    pub status_flash: Option<(String, std::time::Instant, f32)>,
    pub open_recent_menu: OnceCell<Retained<objc2_app_kit::NSMenu>>,
    pub theme_submenu: OnceCell<Retained<objc2_app_kit::NSMenu>>,
    pub view_submenu: OnceCell<Retained<objc2_app_kit::NSMenu>>,
    pub search_submenu: OnceCell<Retained<objc2_app_kit::NSMenu>>,
    /// 打开的设置 sheet（同一时刻至多一个；sheet 结束后清空）。
    pub settings_sheet: OnceCell<Retained<SettingsSheet>>,
}

impl App {
    pub fn new() -> Self {
        let config = AppConfig::load();
        let mut app = Self {
            config,
            bridge: None,
            theme_name: "Dark Pro".into(),
            font_size: 13.0,
            row_h: 18.0,
            show_line_numbers: true,
            word_wrap: false,
            show_whitespace: false,
            level_coloring: true,
            show_indent_guides: false,
            view_w: 800.0,
            view_h: 600.0,
            max_content_w: 0.0,
            rendered_content_h: 0.0,
            hit_lines: Vec::new(),
            search_active: false,
            indexing_active: false,
            font: None,
            current_line: 0,
            hover_line: None,
            selection: Selection::empty(),
            delegate: OnceCell::new(),
            window: OnceCell::new(),
            root_view: OnceCell::new(),
            log_view: OnceCell::new(),
            scroll_view: OnceCell::new(),
            toolbar: OnceCell::new(),
            status_left: OnceCell::new(),
            status_mid: OnceCell::new(),
            status_right: OnceCell::new(),
            progress: OnceCell::new(),
            btn_cancel: OnceCell::new(),
            status_flash: None,
            open_recent_menu: OnceCell::new(),
            theme_submenu: OnceCell::new(),
            view_submenu: OnceCell::new(),
            search_submenu: OnceCell::new(),
            settings_sheet: OnceCell::new(),
        };
        app.theme_name = app.config.gui.theme.clone();
        app.font_size = app.config.gui.font_size as f64;
        app.row_h = app.config.gui.row_height;
        app.show_line_numbers = app.config.gui.show_line_numbers;
        app.word_wrap = app.config.gui.word_wrap;
        app.show_whitespace = app.config.gui.show_whitespace;
        app.level_coloring = app.config.gui.level_coloring;
        app.show_indent_guides = app.config.gui.show_indent_guides;
        app.rebuild_font();
        app
    }

    pub fn theme(&self) -> ThemeColors {
        theme_by_name(&self.theme_name)
    }

    /// 主线程标记（App 方法只在主线程调用）。
    pub fn mtm_safe(&self) -> MainThreadMarker {
        MainThreadMarker::new().expect("App methods must run on the main thread")
    }

    pub fn rebuild_font(&mut self) {
        let family = if self.config.gui.font_family.is_empty() {
            None
        } else {
            Some(self.config.gui.font_family.as_str())
        };
        self.font = Some(crate::text::Font::with_family(family, self.font_size));
        let f = self.font.as_ref().unwrap();
        if self.row_h < f.line_height() {
            self.row_h = f.line_height();
        }
    }

    /// 构建窗口 / 视图 / 工具栏 / 状态栏 / 菜单，并打开命令行传入的文件。
    ///
    /// 必须在 `run()` 里、`app.run()` 之前调用（此时 APP_PTR 已就绪，但还没有
    /// 进入 runloop，所以不会与任何 ObjC 回调竞争）。
    pub fn setup_ui(&mut self, mtm: MainThreadMarker, delegate: &Retained<AppDelegate>) {
        unsafe {
            // ---- 窗口 ----
            // FullSizeContentView：内容延伸到标题栏下，统一工具栏（Unified）浮在上面。
            let style = NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::FullSizeContentView;
            // 恢复上次窗口尺寸（无效值回退默认）
            let ws = self.config.gui.window_size;
            let (w, h) = if ws[0] >= 400.0 && ws[1] >= 300.0 {
                (ws[0] as f64, ws[1] as f64)
            } else {
                (1280.0, 820.0)
            };
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(w, h)),
                style,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setTitle(&crate::util::ns_string("qview"));
            window.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
            // 统一工具栏外观
            window.setTitlebarAppearsTransparent(true);
            window.setToolbarStyle(NSWindowToolbarStyle::Unified);
            window.setMinSize(NSSize::new(800.0, 500.0));
            // 上次最大化 → 恢复到主屏可见区
            if self.config.gui.window_maximized {
                if let Some(screen) = NSScreen::mainScreen(mtm) {
                    let vf = screen.visibleFrame();
                    window.setFrame_display(vf, false);
                }
            }

            // ---- 根视图 ----
            let root = RootView::new(mtm);
            window.setContentView(Some(&root));

            // ---- 日志视图 + 滚动区 ----
            let log_view = LogView::new(mtm);
            // 把日志视图句柄存进 delegate ivars：滚动 bounds 通知（同步回调）要用，
            // 且不能经 `with_app`（可能在其它 with_app 闭包内触发 → 重入 UB）。
            *delegate.ivars().log_view.borrow_mut() = Some(log_view.clone());
            let scroll = NSScrollView::new(mtm);
            scroll.setHasVerticalScroller(true);
            scroll.setHasHorizontalScroller(true);
            scroll.setAutohidesScrollers(false);
            scroll.setBorderType(NSBorderType::NoBorder);
            scroll.setDrawsBackground(false);
            scroll.setDocumentView(Some(&log_view));
            root.addSubview(&scroll);

            // 观察 clip view 的 bounds 变更：滚动（尤其横向）时强制整块重绘。
            // 默认 copy-on-scroll 只会重绘露出的窄条，行号栏/内容会残留滚动前的
            // 画面，直到点击（触发 setNeedsDisplay）才正确。这里用 selector 观察者，
            // 回调直接经 delegate ivars 取 log_view，避免 `with_app` 重入。
            let center = objc2_foundation::NSNotificationCenter::defaultCenter();
            center.addObserver_selector_name_object(
                &**delegate as &AnyObject,
                sel!(clipViewBoundsChanged:),
                Some(&objc2_app_kit::NSViewBoundsDidChangeNotification),
                Some(&*scroll.contentView() as &AnyObject),
            );

            // ---- 工具栏 / 状态栏 ----
            // 先建工具栏（把搜索/跳转框存进 AppDelegate ivars），再挂到窗口。
            let toolbar = crate::toolbar::create_toolbar(mtm, delegate, &self.config.search_history);
            window.setToolbar(Some(&toolbar));
            let _ = self.toolbar.set(toolbar);
            crate::statusbar::create_statusbar(self, mtm, &root, delegate);

            // ---- 菜单 ----
            let menu = crate::menu::build_main_menu(mtm, delegate, self);
            let app_obj = NSApplication::sharedApplication(mtm);
            app_obj.setMainMenu(Some(&menu));

            // ---- 保存句柄（必须在 open_path / layout 之前）----
            let _ = self.delegate.set(delegate.clone());
            let _ = self.window.set(window.clone());
            let _ = self.root_view.set(root);
            // 日志区作为默认首响应者：Cmd+C / Cmd+A / 方向键路由到日志视图。
            // （搜索/跳转输入框仍可通过点击/Cmd+F 取得焦点。须在 log_view 被
            // set 移入之前借用。）
            window.setInitialFirstResponder(Some(&log_view));
            let _ = self.log_view.set(log_view.clone());
            let _ = self.scroll_view.set(scroll);

            // ---- 初始布局 + 菜单勾选 ----
            self.layout_controls();
            self.update_status();
            crate::menu::sync_recent_menu(self);
            crate::menu::sync_theme_checks(self);
            crate::menu::sync_view_checks(self);
            crate::menu::sync_search_checks(self);

            // ---- 打开命令行传入的文件 ----
            if let Some(p) = std::env::args().nth(1) {
                if let Err(e) = self.open_path(PathBuf::from(p)) {
                    crate::dialogs::show_error(&e);
                }
            }

            // ---- 激活并显示 ----
            if !self.config.gui.window_maximized {
                window.center();
            }
            window.makeKeyAndOrderFront(None);
            // 窗口成为 key 后把首响应者指回日志区（initialFirstResponder 在首次
            // 显示时生效，这里再显式设一次兜底）。
            window.makeFirstResponder(Some(&log_view));
            app_obj.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            app_obj.activate();
        }
    }

    /// 总行数。
    pub fn total_lines(&self) -> u64 {
        self.bridge.as_ref().map(|b| b.total_lines()).unwrap_or(0)
    }

    /// 布局用行高估算（滚动/跳转，渲染用逐行精确累积）。
    fn estimated_row_step(&self) -> f64 {
        crate::layout::estimated_row_step(self.row_h, self.word_wrap)
    }

    /// 内容区可用宽度（= 视口宽 - 行号栏 - 左内边距），渲染/命中测试共享。
    pub fn content_avail_width(&self) -> f64 {
        let gutter_w = if self.show_line_numbers { GUTTER_W } else { 24.0 };
        (self.view_w - (gutter_w + TEXT_LEFT_PAD)).max(40.0)
    }

    /// 打开文件（跨线程安全：只换 bridge）。
    ///
    /// 失败时返回错误信息，**不弹窗**——调用方必须在 `with_app` 闭包之外展示。
    pub fn open_path(&mut self, path: PathBuf) -> Result<(), String> {
        if self.bridge.is_some() {
            self.close_file();
        }
        let bridge = match Bridge::open(&path, &self.config.engine) {
            Ok(b) => b,
            Err(e) => return Err(format!("无法打开文件:\n{}\n\n{}", path.display(), e)),
        };
        self.bridge = Some(bridge);
        self.current_line = 0;
        self.selection.clear();
        self.hit_lines.clear();
        self.search_active = false;
        self.indexing_active = self.bridge.as_ref().map_or(false, |b| b.indexing_active());
        self.max_content_w = 0.0;
        self.config.add_recent(path.clone());
        self.config.save();
        // 更新窗口标题
        if let Some(w) = self.window.get() {
            w.setTitle(&crate::util::ns_string(&path.display().to_string()));
        }
        self.set_log_view_size();
        self.goto_line(0);
        self.update_status();
        // 同步 Open Recent 菜单
        crate::menu::sync_recent_menu(self);
        Ok(())
    }

    pub fn close_file(&mut self) {
        self.bridge = None;
        self.selection.clear();
        self.hit_lines.clear();
        self.search_active = false;
        self.indexing_active = false;
        if let Some(w) = self.window.get() {
            w.setTitle(&crate::util::ns_string("qview"));
        }
        self.set_log_view_size();
        self.update_status();
    }

    /// 设置文档视图尺寸（内容宽/高）。
    ///
    /// 内容高 = max(估算高, 渲染实测高)。换行模式内容宽 = 视口宽（无横向滚动），
    /// 非换行模式内容宽 = 最长行宽 + 边距。
    pub fn set_log_view_size(&mut self) {
        let Some(log_view) = self.log_view.get().cloned() else {
            return;
        };
        let clip_w = self.view_w.max(1.0);
        let estimate = crate::layout::estimate_content_h(self.total_lines(), self.row_h, self.word_wrap);
        let content_h = estimate.max(self.rendered_content_h);
        let content_w = if self.word_wrap {
            clip_w
        } else {
            (self.max_content_w + GUTTER_W + 200.0).max(clip_w)
        };
        log_view.setFrameSize(NSSize::new(content_w, content_h));
    }

    /// 跳转到某行：目标行顶距视口 1/3 屏。
    pub fn goto_line(&mut self, line: u64) {
        let Some(scroll_view) = self.scroll_view.get() else {
            return;
        };
        let total = self.total_lines();
        if total == 0 {
            return;
        }
        let line = line.min(total.saturating_sub(1));
        self.current_line = line;
        let clip_h = self.view_h;
        let y = crate::layout::estimate_line_y(line, self.row_h, self.word_wrap) - clip_h / 3.0;
        let y = y.max(0.0);
        let clip = scroll_view.contentView();
        clip.scrollToPoint(NSPoint::new(0.0, y));
        scroll_view.reflectScrolledClipView(&clip);
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    /// 跳到顶部/底部。
    pub fn goto_top(&mut self) {
        self.goto_line(0);
    }
    pub fn goto_end(&mut self) {
        let total = self.total_lines();
        self.goto_line(total);
    }

    /// 翻页：向上移动约 0.9×视口高（按估算行高折算行数）。
    pub fn page_up(&mut self) {
        let step = (self.view_h * 0.9 / self.estimated_row_step().max(1.0)).floor() as u64;
        self.goto_line(self.current_line.saturating_sub(step.max(1)));
    }

    /// 翻页：向下移动约 0.9×视口高（按估算行高折算行数）。
    pub fn page_down(&mut self) {
        let step = (self.view_h * 0.9 / self.estimated_row_step().max(1.0)).floor() as u64;
        self.goto_line(self.current_line.saturating_add(step.max(1)));
    }

    // -------------------------------------------------------------------
    // 搜索
    // -------------------------------------------------------------------

    /// 提交搜索。失败返回错误信息，**不弹窗**。
    pub fn submit_search(&mut self, q: String) -> Result<(), String> {
        let Some(bridge) = self.bridge.as_mut() else {
            return Ok(());
        };
        let opts = qview_core::search::SearchOptions {
            case_sensitive: self.config.gui.case_sensitive,
            use_regex: self.config.gui.use_regex,
            whole_word: self.config.gui.whole_word,
            crlf: bridge.uses_crlf(),
        };
        bridge
            .submit_search(q.clone(), opts)
            .map_err(|e| format!("搜索失败: {e}"))?;
        self.search_active = true;
        self.config.add_search_history(q);
        self.config.save();
        // 同步最近搜索到搜索框下拉（config 为权威源）
        if let Some(d) = self.delegate.get() {
            if let Some(f) = d.ivars().search_field.borrow().as_ref() {
                let owned: Vec<Retained<NSString>> = self
                    .config
                    .search_history
                    .iter()
                    .map(|s| crate::util::ns_string(s))
                    .collect();
                f.setRecentSearches(&NSArray::from_retained_slice(&owned));
            }
        }
        Ok(())
    }

    pub fn search_next(&mut self) {
        let Some(bridge) = self.bridge.as_mut() else {
            return;
        };
        if bridge.hits.is_empty() {
            return;
        }
        bridge.cursor = (bridge.cursor + 1) % bridge.hits.len();
        if let Some(line) = bridge.hit_line(bridge.cursor) {
            self.goto_line(line);
        }
        self.update_hit_lines();
    }

    pub fn search_prev(&mut self) {
        let Some(bridge) = self.bridge.as_mut() else {
            return;
        };
        if bridge.hits.is_empty() {
            return;
        }
        bridge.cursor = if bridge.cursor == 0 {
            bridge.hits.len() - 1
        } else {
            bridge.cursor - 1
        };
        if let Some(line) = bridge.hit_line(bridge.cursor) {
            self.goto_line(line);
        }
        self.update_hit_lines();
    }

    pub fn cancel_search(&mut self) {
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.engine.lock().unwrap().cancel_search();
            bridge.hits.clear();
            bridge.cursor = 0;
            bridge.last_query.clear();
        }
        self.search_active = false;
        self.hit_lines.clear();
        self.set_search_field_text("");
        self.update_status();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    /// 由 hit 列表生成去重排序的行号（供左侧色条/导航）。
    pub fn update_hit_lines(&mut self) {
        let Some(bridge) = self.bridge.as_ref() else {
            self.hit_lines.clear();
            return;
        };
        if bridge.hits.is_empty() {
            self.hit_lines.clear();
            return;
        }
        let engine = bridge.engine.lock().unwrap();
        let mut lines: Vec<u64> = bridge
            .hits
            .iter()
            .map(|&b| engine.index.line_of_byte(b))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        self.hit_lines = lines;
    }

    // -------------------------------------------------------------------
    // 轮询（timer）
    // -------------------------------------------------------------------

    pub fn poll_tasks(&mut self) {
        let mut changed = false;
        let mut index_finished = false;
        let mut search_finished = false;
        if let Some(bridge) = self.bridge.as_mut() {
            // 索引进度
            if bridge.poll_index() {
                index_finished = true;
                changed = true;
            } else if bridge.indexing_active() {
                changed = true;
            }
            // 搜索进度
            let before = bridge.hits.len();
            search_finished = bridge.poll_search();
            if bridge.hits.len() != before || search_finished {
                changed = true;
            }
        }
        if index_finished {
            self.indexing_active = false;
            self.flash_status("索引完成", 2.5);
        }
        if search_finished {
            self.search_active = false;
            let n = self.bridge.as_ref().map_or(0, |b| b.hits.len());
            if n > 0 {
                self.flash_status(&format!("找到 {} 条匹配", n), 3.0);
            } else {
                self.flash_status("未找到匹配", 2.5);
            }
        }
        if changed {
            self.update_hit_lines();
            self.set_log_view_size();
            if let Some(lv) = self.log_view.get() {
                lv.setNeedsDisplay(true);
            }
        }
        self.update_status();
    }

    // -------------------------------------------------------------------
    // 视图布局
    // -------------------------------------------------------------------

    /// 内容区顶部内边距：统一工具栏 + 标题栏的实际高度（FullSizeContentView 下
    /// 内容会延伸到标题栏，必须让滚动区从工具栏底边之下开始，否则被工具栏盖住）。
    ///
    /// 优先用 `contentLayoutRect`（KVO 感知，内容视图坐标系）推算；窗口尚未布局
    /// 或结果不合理时回退到常量 `TOOLBAR_H`。兼容 flipped（origin.y=顶边距）与
    /// 非 flipped（底左原点）两种语义，取落在合理区间的那一个。
    fn toolbar_top_inset(&self) -> f64 {
        let Some(w) = self.window.get() else { return TOOLBAR_H };
        let r = w.contentLayoutRect();
        let content_h = self
            .root_view
            .get()
            .map(|rv| rv.bounds().size.height)
            .unwrap_or(0.0);
        if r.size.width <= 0.0 || r.size.height <= 0.0 || content_h <= 0.0 {
            return TOOLBAR_H;
        }
        let flipped_top = r.origin.y;
        let bottom_left_top = content_h - r.origin.y - r.size.height;
        let sane = |v: f64| (40.0..=140.0).contains(&v);
        if sane(flipped_top) {
            flipped_top
        } else if sane(bottom_left_top) {
            bottom_left_top
        } else {
            TOOLBAR_H
        }
    }

    /// 根据窗口内容区尺寸摆放工具栏 / 滚动区 / 状态栏。
    pub fn layout_controls(&mut self) {
        let Some(root) = self.root_view.get() else { return };
        let bounds = root.bounds();
        let w = bounds.size.width;
        let h = bounds.size.height;
        self.view_w = w;
        let top = self.toolbar_top_inset();
        self.view_h = h - top - STATUSBAR_H;

        // 滚动区：顶部工具栏下方，底部状态栏上方
        if let Some(scroll) = self.scroll_view.get() {
            scroll.setFrame(NSRect::new(
                NSPoint::new(0.0, top),
                NSSize::new(w, self.view_h),
            ));
        }
        // 状态栏
        if let Some(sl) = self.status_left.get() {
            sl.setFrame(NSRect::new(
                NSPoint::new(8.0, h - STATUSBAR_H + 5.0),
                NSSize::new(220.0, 16.0),
            ));
        }
        if let Some(sm) = self.status_mid.get() {
            sm.setFrame(NSRect::new(
                NSPoint::new(240.0, h - STATUSBAR_H + 5.0),
                NSSize::new(220.0, 16.0),
            ));
        }
        if let Some(pr) = self.progress.get() {
            pr.setFrame(NSRect::new(
                NSPoint::new(480.0, h - STATUSBAR_H + 7.0),
                NSSize::new(120.0, 8.0),
            ));
        }
        // 取消按钮：进度条右侧
        if let Some(bc) = self.btn_cancel.get() {
            bc.setFrame(NSRect::new(
                NSPoint::new(606.0, h - STATUSBAR_H + 1.0),
                NSSize::new(54.0, 22.0),
            ));
        }
        if let Some(sr) = self.status_right.get() {
            sr.setFrame(NSRect::new(
                NSPoint::new(w - 280.0, h - STATUSBAR_H + 5.0),
                NSSize::new(272.0, 16.0),
            ));
        }
        // 更新文档尺寸
        self.set_log_view_size();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    // -------------------------------------------------------------------
    // 渲染
    // -------------------------------------------------------------------

    pub fn render_log_view(&mut self, dirty: NSRect) {
        let Some(ctx) = objc2_app_kit::NSGraphicsContext::currentContext() else {
            return;
        };
        let ctx = ctx.CGContext();
        let Some(lv) = self.log_view.get() else { return };
        let visible = lv.visibleRect();
        let clip_w = visible.size.width;
        let clip_h = visible.size.height;
        let scroll_y = visible.origin.y;
        let h_scroll = visible.origin.x;
        let total = self.total_lines();

        let theme = self.theme();
        let row_h = self.row_h;
        let font = self.font.as_ref().unwrap();

        // 背景
        let bg = theme.bg_primary.to_cgcolor();
        CGContext::set_fill_color_with_color(Some(&ctx), Some(&bg));
        CGContext::fill_rect(Some(&ctx), dirty);

        if total == 0 || self.bridge.is_none() {
            // 空状态提示
            if self.bridge.is_none() {
                let text = "拖入或打开一个日志文件（Cmd+O）";
                crate::text::draw_string(
                    &ctx,
                    font,
                    text,
                    TEXT_LEFT_PAD - h_scroll,
                    scroll_y + font.ascent + 10.0,
                    &theme.text_secondary,
                );
            }
            return;
        }

        // 可见行范围（估算；渲染时用逐行 visual_rows 精确累积 y_cursor）
        let est_step = self.estimated_row_step();
        let first = crate::layout::first_visible_line(scroll_y, row_h, self.word_wrap);
        let buffer: u64 = if self.word_wrap { 8 } else { 2 };
        let est_visible = crate::layout::estimate_visible_lines(clip_h, row_h, self.word_wrap);
        let last = (first + est_visible + buffer).min(total);

        let gutter_w = if self.show_line_numbers { GUTTER_W } else { 24.0 };

        // 命中行二分查找用
        let hit_lines = &self.hit_lines;
        let search_state = (
            self.search_active,
            self.bridge.as_ref().map(|b| b.last_query.clone()).unwrap_or_default(),
            self.config.gui.case_sensitive,
            self.config.gui.use_regex,
            self.config.gui.whole_word,
            self.bridge.as_ref().map(|b| b.hits.get(b.cursor).copied()).flatten(),
            self.bridge.as_ref().map_or(false, |b| b.uses_crlf()),
        );

        let current_hit_byte = search_state.5;

        // 交替行背景色
        let bg_alt = theme.bg_secondary.to_cgcolor();
        let bg_primary = theme.bg_primary.to_cgcolor();
        let hit_bar = theme.search_highlight.to_cgcolor();

        let show_line_numbers = self.show_line_numbers;
        let level_coloring = self.level_coloring;
        let show_whitespace = self.show_whitespace;
        let show_indent_guides = self.show_indent_guides;
        let wrap_mode = self.word_wrap;
        let avail = self.content_avail_width();
        let opts = qview_core::search::SearchOptions {
            case_sensitive: search_state.2,
            use_regex: search_state.3,
            whole_word: search_state.4,
            crlf: search_state.6,
        };

        // ---- 收集可见行几何与内容 ----
        let line_h_base = row_h;
        let mut rows: Vec<VisRow> = Vec::new();
        let mut row = first;
        let mut y_cursor = first as f64 * est_step;
        let mut max_w_changed = false;
        let mut content_grew = false;
        while row < last {
            // 读取该行文本（换行模式下需用它算实际折行段数）
            let display = {
                let bridge = self.bridge.as_ref().unwrap();
                bridge.read_display_line(
                    row,
                    &search_state.1,
                    &opts,
                    if search_state.0 { current_hit_byte } else { None },
                )
            };

            let visual_rows = if wrap_mode {
                font.visual_rows(&display.text, avail)
            } else {
                1
            };
            let line_h = visual_rows as f64 * line_h_base;

            // 完全在视口上方 → 跳过（仍累积 y_cursor）
            if y_cursor + line_h < scroll_y {
                y_cursor += line_h;
                row += 1;
                continue;
            }
            // 已越过视口底部 → 停止
            if y_cursor > scroll_y + clip_h {
                break;
            }

            // 级别着色
            let text_color = if level_coloring {
                detect_level(&display.text)
                    .map(|lv| level_color(lv, &theme))
                    .unwrap_or(theme.text_primary)
            } else {
                theme.text_primary
            };

            // 当前命中行：整行背景加 current 高亮
            let (matches, current_matches) = if display.is_current {
                (
                    Vec::<(usize, usize)>::new(),
                    display.matches.clone(),
                )
            } else {
                (display.matches, Vec::<(usize, usize)>::new())
            };

            // 更新最长行宽（仅未换行模式；布局延后到循环外）
            if !wrap_mode {
                let w = font.measure_width(&display.text);
                if w > self.max_content_w {
                    self.max_content_w = w;
                    max_w_changed = true;
                }
            }

            rows.push(VisRow {
                row,
                y: y_cursor,
                h: line_h,
                text: display.text,
                matches,
                current_matches,
                text_color,
            });

            y_cursor += line_h;
            if y_cursor > self.rendered_content_h {
                self.rendered_content_h = y_cursor;
                content_grew = true;
            }
            row += 1;
        }

        // ---- 绘制：背景 + 行号栏 ----
        let hover_fill = theme.bg_hover.with_alpha(153).to_cgcolor();
        let current_fill = theme.bg_tertiary.with_alpha(80).to_cgcolor();
        let current_line = self.current_line;
        let hover_line = self.hover_line;

        for v in &rows {
            // 基础行背景
            let row_even = v.row % 2 == 0;
            let alt = if row_even { &bg_primary } else { &bg_alt };
            CGContext::set_fill_color_with_color(Some(&ctx), Some(alt));
            CGContext::fill_rect(
                Some(&ctx),
                NSRect {
                    origin: NSPoint::new(0.0, v.y),
                    size: NSSize::new(clip_w + 200.0, v.h),
                },
            );

            // 当前行 / 悬停行高亮
            let fill = if v.row == current_line {
                Some(&current_fill)
            } else if hover_line == Some(v.row) {
                Some(&hover_fill)
            } else {
                None
            };
            if let Some(f) = fill {
                CGContext::set_fill_color_with_color(Some(&ctx), Some(f));
                CGContext::fill_rect(
                    Some(&ctx),
                    NSRect {
                        origin: NSPoint::new(0.0, v.y),
                        size: NSSize::new(clip_w + 200.0, v.h),
                    },
                );
            }

            // 命中行左侧色条
            if hit_lines.binary_search(&v.row).is_ok() {
                CGContext::set_fill_color_with_color(Some(&ctx), Some(&hit_bar));
                CGContext::fill_rect(
                    Some(&ctx),
                    NSRect {
                        origin: NSPoint::new(0.0, v.y + 2.0),
                        size: NSSize::new(HIT_BAR_W, (v.h - 4.0).max(1.0)),
                    },
                );
            }
        }

        // 行号栏背景 + 分隔线（盖住行背景的左段）
        if show_line_numbers {
            let gutter_bg = theme.line_number_bg.to_cgcolor();
            CGContext::set_fill_color_with_color(Some(&ctx), Some(&gutter_bg));
            CGContext::fill_rect(
                Some(&ctx),
                NSRect {
                    origin: NSPoint::new(0.0, scroll_y),
                    size: NSSize::new(gutter_w, clip_h),
                },
            );
            let sep = theme.line_number.with_alpha(77).to_cgcolor();
            CGContext::set_fill_color_with_color(Some(&ctx), Some(&sep));
            CGContext::fill_rect(
                Some(&ctx),
                NSRect {
                    origin: NSPoint::new(gutter_w - 1.0, scroll_y),
                    size: NSSize::new(1.0, clip_h),
                },
            );
        }

        // 行号（固定不随横向滚动移动）+ 命中色条已在行号栏背景之上
        if show_line_numbers {
            for v in &rows {
                let num = (v.row + 1).to_string();
                // 右对齐到 gutter_w - 8
                let num_w = font.measure_width(&num);
                let x = (gutter_w - 8.0 - num_w).max(0.0);
                crate::text::draw_string(&ctx, font, &num, x, v.y + font.ascent, &theme.line_number);
            }
        }

        // ---- 绘制：内容区（裁剪到行号栏右侧）----
        if rows.is_empty() {
            return;
        }
        let guide_color = theme.line_number.with_alpha(64);
        let guide_char_w = if show_indent_guides {
            font.measure_width(" ")
        } else {
            0.0
        };
        let tx = gutter_w + TEXT_LEFT_PAD - h_scroll;
        CGContext::save_g_state(Some(&ctx));
        CGContext::clip_to_rect(
            Some(&ctx),
            NSRect {
                origin: NSPoint::new(gutter_w, scroll_y),
                size: NSSize::new((clip_w - gutter_w).max(1.0), clip_h),
            },
        );

        let sel_color = theme.selection_bg;

        for v in &rows {
            let baseline = v.y + font.ascent;

            // 缩进参考线
            if show_indent_guides {
                draw_indent_guides(&ctx, &v.text, tx, v.y, v.h, guide_char_w, &guide_color);
            }

            // 选区背景（文字之下；换行模式按视觉段对齐）
            if let Some((s, e)) = self.selection.selected_range_for_line(v.row, v.text.len()) {
                if e > s {
                    let sel_u16 = crate::util::byte_ranges_to_utf16(&v.text, &[(s, e)]);
                    let sel_w = if wrap_mode { avail } else { f64::MAX };
                    font.draw_selection_rects(
                        &ctx,
                        tx,
                        baseline,
                        &v.text,
                        &sel_u16,
                        &sel_color,
                        sel_w,
                        line_h_base,
                    );
                }
            }

            if wrap_mode {
                font.draw_line_wrapped(
                    &ctx,
                    tx,
                    baseline,
                    &v.text,
                    &v.text_color,
                    None,
                    &v.matches,
                    &v.current_matches,
                    &theme.search_highlight,
                    &theme.search_current,
                    show_whitespace,
                    avail,
                    line_h_base,
                );
            } else {
                font.draw_line(
                    &ctx,
                    tx,
                    baseline,
                    &v.text,
                    &v.text_color,
                    None,
                    &v.matches,
                    &v.current_matches,
                    &theme.search_highlight,
                    &theme.search_current,
                    show_whitespace,
                );
            }
        }
        CGContext::restore_g_state(Some(&ctx));

        if max_w_changed || content_grew {
            self.set_log_view_size();
        }
    }

    /// 由文档 y 坐标（LogView 本地坐标，已含滚动偏移）定位所在行。
    /// 与渲染使用同样的 y_cursor 累积，保证悬停/点击命中与视觉一致。
    pub fn line_at_y(&self, y: f64) -> Option<u64> {
        self.line_and_offset_at_y(y).map(|(line, _)| line)
    }

    /// 同 `line_at_y`，额外返回该行内距顶部的 y 偏移（用于换行时选视觉段）。
    fn line_and_offset_at_y(&self, y: f64) -> Option<(u64, f64)> {
        let total = self.total_lines();
        if total == 0 || self.bridge.is_none() {
            return None;
        }
        let row_h = self.row_h;
        let wrap = self.word_wrap;
        let est_step = crate::layout::estimated_row_step(row_h, wrap);
        let first = crate::layout::first_visible_line(y, row_h, wrap);
        let mut row = first;
        let mut y_cursor = first as f64 * est_step;
        let font = self.font.as_ref()?;
        let avail = self.content_avail_width();
        while row < total {
            let text = self.bridge.as_ref()?.read_line(row);
            let vr = if wrap { font.visual_rows(&text, avail) } else { 1 };
            let h = vr as f64 * row_h;
            if y < y_cursor + h {
                return Some((row, y - y_cursor));
            }
            y_cursor += h;
            row += 1;
        }
        None
    }

    /// 由 LogView 本地坐标（含滚动偏移）命中一个 `TextPoint`（行 + 字节偏移）。
    ///
    /// 流程：y 定位行 → 行内 y 偏移选视觉段（换行）→ `string_index_for_position`
    /// 把 x 换算成 UTF-16 索引 → 转回 UTF-8 字节。与渲染共享 `content_avail_width`
    /// 和 `wrapped_segments`，保证命中与视觉一致。
    pub fn hit_test(&self, point: objc2_foundation::NSPoint) -> Option<TextPoint> {
        let total = self.total_lines();
        if total == 0 || self.bridge.is_none() {
            return None;
        }
        let (line, offset_y) = match self.line_and_offset_at_y(point.y) {
            Some(v) => v,
            None => {
                // 视口下方空白：定位到最后一行末尾
                let last = total - 1;
                let text = self.bridge.as_ref()?.read_line(last);
                return Some(TextPoint { line: last, byte: text.len() });
            }
        };
        let text = self.bridge.as_ref()?.read_line(line);
        let font = self.font.as_ref()?;
        let wrap = self.word_wrap;
        let avail = self.content_avail_width();
        // 文本左缘（渲染用 tx = gutter + pad - h_scroll，这里补回 h_scroll）
        let h_scroll = self.log_view.get().map(|lv| lv.visibleRect().origin.x).unwrap_or(0.0);
        let gutter_w = if self.show_line_numbers { GUTTER_W } else { 24.0 };
        let text_left = gutter_w + TEXT_LEFT_PAD - h_scroll;
        let x_rel = point.x - text_left;

        let u16_idx = unsafe {
            if wrap {
                match font.wrapped_segments(&text, avail) {
                    Some((_, segs)) => {
                        let vr = font.visual_rows(&text, avail);
                        let seg_idx = (offset_y / self.row_h).floor() as usize;
                        let seg_idx = seg_idx.min(vr.saturating_sub(1));
                        let (_, ctline) = &segs[seg_idx];
                        crate::text::point_to_offset(ctline, x_rel)
                    }
                    // 空行（无分段）：从头开始
                    None => 0,
                }
            } else {
                // 不换行：建单行 CTLine 做命中
                let attr = crate::text::plain_attr(&text, font.as_ctfont());
                let ctline = CTLine::with_attributed_string(&attr);
                crate::text::point_to_offset(&ctline, x_rel)
            }
        };
        let byte = crate::text::utf16_to_byte(&text, u16_idx);
        Some(TextPoint { line, byte })
    }

    /// 选中 `pt` 所在行的一个"词"（连续非空白），返回 `[s, e)` 字节区间。
    pub fn word_at(&self, pt: TextPoint) -> Option<(usize, usize)> {
        let text = self.bridge.as_ref()?.read_line(pt.line);
        let bytes = text.as_bytes();
        let len = bytes.len();
        let mut pos = pt.byte.min(len);
        // 落到字符边界（多字节字符时向左对齐）
        while pos > 0 && !text.is_char_boundary(pos) {
            pos -= 1;
        }
        // 在空白上 → 向右找第一个非空白（词首）
        while pos < len && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= len {
            return None;
        }
        let mut start = pos;
        while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
            start -= 1;
        }
        let mut end = pos;
        while end < len && !bytes[end].is_ascii_whitespace() {
            end += 1;
        }
        if end <= start {
            None
        } else {
            Some((start, end))
        }
    }

    /// 把选区文本拼成字符串（空选区返回空串）。
    pub fn selection_copy_string(&self) -> String {
        let Some(bridge) = self.bridge.as_ref() else {
            return String::new();
        };
        self.selection.copy_string(|line| bridge.read_line(line))
    }

    /// 清除选区（Esc）。
    pub fn clear_selection(&mut self) {
        if !self.selection.active {
            return;
        }
        self.selection.clear();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    /// 全选并复制（Cmd+A / 菜单）。
    pub fn select_all_lines(&mut self) {
        let total = self.total_lines();
        if total == 0 || self.bridge.is_none() {
            return;
        }
        let last = total - 1;
        let last_len = self.bridge.as_ref().unwrap().read_line(last).len();
        self.selection.anchor = TextPoint { line: 0, byte: 0 };
        self.selection.focus = TextPoint { line: last, byte: last_len };
        self.selection.active = true;
        let s = self.selection_copy_string();
        crate::util::copy_to_clipboard(&s);
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    // -------------------------------------------------------------------
    // 状态栏
    // -------------------------------------------------------------------

    /// 状态栏瞬态提示：显示约 `secs` 秒后自动消失（poll_tasks 每 100ms 检查）。
    pub fn flash_status(&mut self, msg: &str, secs: f32) {
        self.status_flash = Some((msg.to_string(), std::time::Instant::now(), secs.max(1.0)));
        self.update_status();
    }

    pub fn update_status(&mut self) {
        let Some(sl) = self.status_left.get().cloned() else { return };
        let Some(sm) = self.status_mid.get().cloned() else { return };
        let Some(sr) = self.status_right.get().cloned() else { return };
        let Some(pr) = self.progress.get().cloned() else { return };

        // 瞬态提示过期即清除
        let flash = if let Some((_, start, secs)) = &self.status_flash {
            if start.elapsed().as_secs_f32() < *secs {
                self.status_flash.as_ref().map(|(msg, _, _)| msg.clone())
            } else {
                self.status_flash = None;
                None
            }
        } else {
            None
        };

        let path = self.bridge.as_ref().map(|b| b.path.clone());
        let size = self.bridge.as_ref().map(|b| b.size);
        let total = self.total_lines();
        let hits = self
            .bridge
            .as_ref()
            .map(|b| b.hits.len())
            .unwrap_or(0);
        let cursor = self.bridge.as_ref().map(|b| b.cursor).unwrap_or(0);

        let left = if let Some(p) = &path {
            let name = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string());
            name
        } else {
            "未打开文件".to_string()
        };
        sl.setStringValue(&crate::util::ns_string(&left));

        let mid = if let Some(f) = flash {
            f
        } else if self.indexing_active {
            "正在建立索引…".to_string()
        } else if self.search_active {
            if hits > 0 {
                format!("{}/{} 条匹配", cursor + 1, hits)
            } else {
                "搜索中…".to_string()
            }
        } else {
            format!("{} 行", total)
        };
        sm.setStringValue(&crate::util::ns_string(&mid));

        // 右区：大小 │ 行数 │ 编码（编码来自 config.engine.encoding）
        let encoding = self.config.engine.encoding.clone();
        let right = match size {
            Some(sz) => format!(
                "{} │ {} 行 │ {}",
                crate::util::human_bytes(sz),
                total,
                encoding
            ),
            None => String::new(),
        };
        sr.setStringValue(&crate::util::ns_string(&right));

        // 进度条：能解析出百分比 → 确定模式；否则不确定动画
        let busy = self.indexing_active || self.search_active;
        if busy {
            let msg = self.bridge.as_ref().and_then(|b| b.progress_message());
            let pct = msg.as_deref().and_then(parse_progress_pct);
            pr.setHidden(false);
            match pct {
                Some(p) => {
                    pr.setIndeterminate(false);
                    pr.setDoubleValue(p);
                    unsafe { pr.stopAnimation(None) };
                }
                None => {
                    pr.setIndeterminate(true);
                    unsafe { pr.startAnimation(None) };
                }
            }
        } else {
            unsafe { pr.stopAnimation(None) };
            pr.setHidden(true);
        }

        // 取消按钮：索引/搜索进行中显示
        if let Some(bc) = self.btn_cancel.get() {
            bc.setHidden(!busy);
        }

        // 工具栏"停止"项：索引/搜索进行中显示
        crate::toolbar::set_stop_visible(self, busy);
    }

    // -------------------------------------------------------------------
    // 其它操作
    // -------------------------------------------------------------------

    pub fn font_bigger(&mut self) {
        if self.font_size < 28.0 {
            self.font_size += 1.0;
            self.config.gui.font_size = self.font_size as f32;
            self.rebuild_font();
            self.save_config_now();
            self.set_log_view_size();
            if let Some(lv) = self.log_view.get() {
                lv.setNeedsDisplay(true);
            }
        }
    }
    pub fn font_smaller(&mut self) {
        if self.font_size > 8.0 {
            self.font_size -= 1.0;
            self.config.gui.font_size = self.font_size as f32;
            self.rebuild_font();
            self.save_config_now();
            self.set_log_view_size();
            if let Some(lv) = self.log_view.get() {
                lv.setNeedsDisplay(true);
            }
        }
    }
    pub fn font_reset(&mut self) {
        self.font_size = 13.0;
        self.config.gui.font_size = 13.0;
        self.rebuild_font();
        self.save_config_now();
        self.set_log_view_size();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    pub fn cycle_theme(&mut self) {
        let names = theme_names();
        let idx = names.iter().position(|n| *n == self.theme_name).unwrap_or(0);
        let next = names[(idx + 1) % names.len()];
        self.apply_theme(next.to_string());
    }
    pub fn apply_theme(&mut self, name: String) {
        self.theme_name = name;
        self.config.gui.theme = self.theme_name.clone();
        self.save_config_now();
        crate::menu::sync_theme_checks(self);
        // 设置 sheet 打开时同步单选状态（菜单/单选共用同一入口）
        if let Some(sheet) = self.settings_sheet.get() {
            sheet.sync_theme_radios(&self.theme_name);
        }
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }

    pub fn toggle_line_numbers(&mut self) {
        self.show_line_numbers = !self.show_line_numbers;
        self.config.gui.show_line_numbers = self.show_line_numbers;
        self.save_config_now();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
        crate::menu::sync_view_checks(self);
    }
    pub fn toggle_word_wrap(&mut self) {
        self.word_wrap = !self.word_wrap;
        self.config.gui.word_wrap = self.word_wrap;
        self.save_config_now();
        self.max_content_w = 0.0;
        self.rendered_content_h = 0.0;
        // 换行模式无横向滚动 → 回到 x=0
        if self.word_wrap {
            if let Some(scroll_view) = self.scroll_view.get() {
                let clip = scroll_view.contentView();
                clip.scrollToPoint(NSPoint::new(0.0, clip.bounds().origin.y));
                scroll_view.reflectScrolledClipView(&clip);
            }
        }
        self.set_log_view_size();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }
    pub fn toggle_whitespace(&mut self) {
        self.show_whitespace = !self.show_whitespace;
        self.config.gui.show_whitespace = self.show_whitespace;
        self.save_config_now();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }
    pub fn toggle_level_coloring(&mut self) {
        self.level_coloring = !self.level_coloring;
        self.config.gui.level_coloring = self.level_coloring;
        self.save_config_now();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
    }
    pub fn toggle_indent_guides(&mut self) {
        self.show_indent_guides = !self.show_indent_guides;
        self.config.gui.show_indent_guides = self.show_indent_guides;
        self.save_config_now();
        if let Some(lv) = self.log_view.get() {
            lv.setNeedsDisplay(true);
        }
        crate::menu::sync_view_checks(self);
    }

    pub fn toggle_case_sensitive(&mut self) {
        self.config.gui.case_sensitive = !self.config.gui.case_sensitive;
        self.save_config_now();
        crate::menu::sync_search_checks(self);
        self.resubmit_search();
    }
    pub fn toggle_regex(&mut self) {
        self.config.gui.use_regex = !self.config.gui.use_regex;
        self.save_config_now();
        crate::menu::sync_search_checks(self);
        self.resubmit_search();
    }
    pub fn toggle_whole_word(&mut self) {
        self.config.gui.whole_word = !self.config.gui.whole_word;
        self.save_config_now();
        crate::menu::sync_search_checks(self);
        self.resubmit_search();
    }

    fn resubmit_search(&mut self) {
        let q = self.bridge.as_ref().map(|b| b.last_query.clone()).unwrap_or_default();
        if !q.is_empty() {
            let q2 = q.clone();
            let _ = self.submit_search(q2);
        }
    }

    pub fn save_config_now(&mut self) {
        self.config.save();
    }

    /// 复制当前行文本到剪贴板（Cmd+C / 菜单 Copy）。
    pub fn copy_current_line(&mut self) {
        let Some(bridge) = self.bridge.as_ref() else { return };
        let line = self.current_line.min(self.total_lines().saturating_sub(1));
        let text = bridge.read_line(line);
        crate::util::copy_to_clipboard(&text);
    }

    /// 复制：有选区复制选区，否则复制当前行。返回复制的文本（用于状态栏提示）。
    /// 剪贴板写入与选区读取都在主线程，跨行/长行直接整段写入。
    pub fn copy_selection_or_line(&mut self) -> Option<String> {
        if !self.selection.is_empty() {
            let s = self.selection_copy_string();
            crate::util::copy_to_clipboard(&s);
            Some(s)
        } else {
            let Some(bridge) = self.bridge.as_ref() else {
                return None;
            };
            let line = self.current_line.min(self.total_lines().saturating_sub(1));
            let text = bridge.read_line(line);
            crate::util::copy_to_clipboard(&text);
            Some(text)
        }
    }

    /// 设置工具栏搜索框文字（句柄在 AppDelegate ivars）。
    pub fn set_search_field_text(&mut self, s: &str) {
        if let Some(d) = self.delegate.get() {
            if let Some(f) = d.ivars().search_field.borrow().as_ref() {
                f.setStringValue(&crate::util::ns_string(s));
            }
        }
    }

    pub fn focus_search(&mut self) {
        let field = self.delegate.get().map(|d| d.ivars().search_field.borrow().clone()).flatten();
        if let Some(f) = field {
            unsafe { f.selectText(None) };
            if let Some(w) = self.window.get() {
                w.makeFirstResponder(Some(&f));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 级别检测（镜像 egui viewer.rs 的 level_color / has_level）
// ---------------------------------------------------------------------------

pub fn detect_level(text: &str) -> Option<Level> {
    let upper = text.to_uppercase();
    let checks: &[(&[&str], Level)] = &[
        (&["ERROR", "FATAL", "CRIT"], Level::Error),
        (&["WARN", "WARNING"], Level::Warn),
        (&["INFO", "NOTICE"], Level::Info),
        (&["DEBUG"], Level::Debug),
        (&["TRACE"], Level::Trace),
    ];
    for (words, lv) in checks {
        for w in *words {
            if has_level(&upper, w) {
                return Some(*lv);
            }
        }
    }
    None
}

fn has_level(upper: &str, level: &str) -> bool {
    if upper.contains(&format!("[{}]", level)) {
        return true;
    }
    if upper.contains(&format!("{}:", level)) {
        return true;
    }
    if upper.contains(&format!("\"{}\"", level)) {
        return true;
    }
    if upper.contains(&format!("<{}>", level)) {
        return true;
    }
    if upper.contains(&format!(" {} ", level)) {
        return true;
    }
    if upper.starts_with(&format!("{} ", level)) {
        return true;
    }
    if upper.ends_with(&format!(" {}", level)) {
        return true;
    }
    false
}

pub fn level_color(lv: Level, theme: &ThemeColors) -> Rgba {
    match lv {
        Level::Error => theme.level_error,
        Level::Warn => theme.level_warn,
        Level::Info => theme.level_info,
        Level::Debug => theme.level_debug,
        Level::Trace => theme.level_trace,
    }
}

// ---------------------------------------------------------------------------
// 缩进参考线
// ---------------------------------------------------------------------------

const INDENT_TAB: usize = 4;

/// 数前导空白折算成的列数（tab 跳到下一制表位）。
fn count_leading_whitespace_cols(text: &str) -> usize {
    let mut col = 0;
    for c in text.chars() {
        match c {
            ' ' => col += 1,
            '\t' => col = (col / INDENT_TAB + 1) * INDENT_TAB,
            _ => break,
        }
    }
    col
}

/// 在每个缩进制表位画 1px 细竖线（仅在有前导空白的行）。
fn draw_indent_guides(
    ctx: &objc2_core_graphics::CGContext,
    text: &str,
    tx: f64,
    y: f64,
    h: f64,
    char_w: f64,
    color: &Rgba,
) {
    if char_w <= 0.0 {
        return;
    }
    let col = count_leading_whitespace_cols(text);
    if col < INDENT_TAB {
        return;
    }
    let cg = color.to_cgcolor();
    objc2_core_graphics::CGContext::set_fill_color_with_color(Some(ctx), Some(&cg));
    let mut stop = INDENT_TAB;
    while stop <= col {
        let x = tx + stop as f64 * char_w - 0.5;
        objc2_core_graphics::CGContext::fill_rect(
            Some(ctx),
            objc2_foundation::NSRect {
                origin: objc2_foundation::NSPoint::new(x, y),
                size: objc2_foundation::NSSize::new(1.0, h),
            },
        );
        stop += INDENT_TAB;
    }
}

// ---------------------------------------------------------------------------
// AppDelegate
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct AppDelegateIvars {
    /// 工具栏搜索框 / 跳转输入框。NSToolbarDelegate 的同步回调（可能在
    /// setup_ui 内 setToolbar 时触发，此时已持有 `&mut App`）不能重入
    /// with_app，所以句柄直接存 ivars。
    pub search_field: RefCell<Option<Retained<NSSearchField>>>,
    pub goto_field: RefCell<Option<Retained<NSTextField>>>,
    /// 日志视图句柄：NSClipView 滚动（bounds 变更）通知会同步触发，可能在
    /// 其它 `with_app` 闭包内回调，故不经 App 直接存这里，避免重入 UB。
    pub log_view: RefCell<Option<Retained<LogView>>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "QLogAppDelegate"]
    #[ivars = AppDelegateIvars]
    pub struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            // 窗口已显示：此时 contentLayoutRect 才是准确的（工具栏/标题栏布局完成），
            // 重摆一次滚动区/状态栏，避免内容被统一工具栏盖住。
            crate::app::with_app(|app| app.layout_controls());
            // 窗口/视图/菜单在 run() 里已创建，这里只注册 100ms 轮询 timer。
            unsafe {
                let _timer = objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.1,
                    &**self as &AnyObject,
                    sel!(pollBackgroundTasks:),
                    None,
                    true,
                );
            }
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            crate::app::with_app(|app| {
                app.save_config_now();
            });
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _sender: &NSApplication) -> bool {
            true
        }

        /// Dock 图标点击重开：若窗口已关闭则重新显示。
        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, has_visible_windows: bool) -> bool {
            if !has_visible_windows {
                crate::app::with_app(|app| {
                    if let Some(w) = app.window.get() {
                        w.makeKeyAndOrderFront(None);
                    }
                });
            }
            true
        }

        /// 把文件拖到 Dock 图标：打开第一个存在的文件。
        #[unsafe(method(application:openFiles:))]
        fn open_files(&self, _sender: &NSApplication, filenames: &NSArray<NSString>) {
            let paths: Vec<String> = (0..filenames.count())
                .map(|i| filenames.objectAtIndex(i).to_string())
                .collect();
            if let Some(p) = paths.into_iter().find(|p| std::path::Path::new(p).exists()) {
                // 重入不变量：open_path 不弹窗，错误在 with_app 外展示
                let result = crate::app::with_app(|app| app.open_path(PathBuf::from(p)));
                if let Err(e) = result {
                    crate::dialogs::show_error(&e);
                }
            }
        }
    }

    unsafe impl NSWindowDelegate for AppDelegate {
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self, _notification: &NSNotification) {
            crate::app::with_app(|app| {
                app.layout_controls();
                // 记录窗口尺寸 + 最大化状态（变更即存）
                if let Some(w) = app.window.get() {
                    let f = w.frame();
                    let maximized = w.isZoomed();
                    app.config.set_window_state(
                        [f.size.width as f32, f.size.height as f32],
                        maximized,
                    );
                }
                app.save_config_now();
            });
        }
    }

    unsafe impl NSToolbarDelegate for AppDelegate {
        // 返回 Retained 的方法须用 method_id（走 RetainedReturnValue 编码路径）。
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn toolbar_item_for_identifier(
            &self,
            _toolbar: &NSToolbar,
            item_identifier: &NSToolbarItemIdentifier,
            _will_be_inserted: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            let mtm = MainThreadMarker::new().expect("toolbar delegate on main thread");
            Some(unsafe { crate::toolbar::item_for_identifier(mtm, item_identifier, self) })
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn toolbar_default_items(
            &self,
            _toolbar: &NSToolbar,
        ) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            crate::toolbar::default_item_identifiers()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn toolbar_allowed_items(
            &self,
            _toolbar: &NSToolbar,
        ) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            crate::toolbar::allowed_item_identifiers()
        }
    }

    unsafe impl NSMenuDelegate for AppDelegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &objc2_app_kit::NSMenu) {
            // Open Recent 动态刷新
            crate::menu::maybe_refresh_recent(menu);
        }
    }

    impl AppDelegate {
        #[unsafe(method(pollBackgroundTasks:))]
        fn poll_background_tasks(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.poll_tasks());
        }

        /// NSClipView 的 bounds 变更（垂直/横向滚动）→ 强制日志视图整块重绘。
        ///
        /// 不经 `with_app`：通知是同步回调，可能在其它 `with_app` 闭包内
        /// （如 goto_line 的 scrollToPoint）触发，重入会拿到第二个 `&mut App`。
        #[unsafe(method(clipViewBoundsChanged:))]
        fn clip_view_bounds_changed(&self, _note: &NSNotification) {
            if let Some(lv) = self.ivars().log_view.borrow().as_ref() {
                lv.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(openDocument:))]
        fn open_document(&self, _sender: Option<&AnyObject>) {
            crate::app::open_document_modal();
        }

        #[unsafe(method(reloadDocument:))]
        fn reload_document(&self, _sender: Option<&AnyObject>) {
            let p = crate::app::with_app(|app| app.bridge.as_ref().map(|b| b.path.clone()));
            if let Some(p) = p {
                let result = crate::app::with_app(|app| app.open_path(p));
                if let Err(e) = result {
                    crate::dialogs::show_error(&e);
                }
            }
        }

        #[unsafe(method(closeDocument:))]
        fn close_document(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.close_file());
        }

        /// 停止按钮：取消后台索引 + 搜索。
        #[unsafe(method(cancelTasks:))]
        fn cancel_tasks(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| {
                if let Some(bridge) = app.bridge.as_mut() {
                    bridge.engine.lock().unwrap().cancel_index();
                }
                app.cancel_search();
                app.indexing_active = false;
                app.update_status();
                if let Some(lv) = app.log_view.get() {
                    lv.setNeedsDisplay(true);
                }
            });
        }

        #[unsafe(method(find:))]
        fn find(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| {
                app.focus_search();
            });
        }

        #[unsafe(method(findNext:))]
        fn find_next(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.search_next());
        }

        #[unsafe(method(findPrevious:))]
        fn find_previous(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.search_prev());
        }

        /// 搜索框回车：提交搜索。
        #[unsafe(method(submitSearch:))]
        fn submit_search(&self, _sender: Option<&AnyObject>) {
            let q = self
                .ivars()
                .search_field
                .borrow()
                .as_ref()
                .map(|f| f.stringValue().to_string())
                .unwrap_or_default();
            if q.is_empty() {
                crate::app::with_app(|app| app.cancel_search());
            } else {
                let result = crate::app::with_app(|app| app.submit_search(q));
                if let Err(e) = result {
                    crate::dialogs::show_error(&e);
                }
            }
        }

        /// 跳转框回车：跳到指定行。
        #[unsafe(method(gotoSubmit:))]
        fn goto_submit(&self, _sender: Option<&AnyObject>) {
            let s = self
                .ivars()
                .goto_field
                .borrow()
                .as_ref()
                .map(|f| f.stringValue().to_string())
                .unwrap_or_default();
            if let Ok(n) = s.trim().parse::<u64>() {
                if n > 0 {
                    crate::app::with_app(|app| app.goto_line(n - 1));
                }
            }
        }

        #[unsafe(method(gotoLine:))]
        fn goto_line(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::prompt_goto();
        }

        #[unsafe(method(goTop:))]
        fn go_top(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.goto_top());
        }

        #[unsafe(method(goEnd:))]
        fn go_end(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.goto_end());
        }

        #[unsafe(method(fontBigger:))]
        fn font_bigger(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.font_bigger());
        }
        #[unsafe(method(fontSmaller:))]
        fn font_smaller(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.font_smaller());
        }
        #[unsafe(method(fontReset:))]
        fn font_reset(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.font_reset());
        }

        #[unsafe(method(toggleLineNumbers:))]
        fn toggle_line_numbers(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_line_numbers());
        }
        #[unsafe(method(toggleWordWrap:))]
        fn toggle_word_wrap(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_word_wrap());
        }
        #[unsafe(method(toggleWhitespace:))]
        fn toggle_whitespace(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_whitespace());
        }
        #[unsafe(method(toggleLevelColoring:))]
        fn toggle_level_coloring(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_level_coloring());
        }
        #[unsafe(method(toggleCaseSensitive:))]
        fn toggle_case_sensitive(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_case_sensitive());
        }
        #[unsafe(method(toggleRegex:))]
        fn toggle_regex(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_regex());
        }
        #[unsafe(method(toggleWholeWord:))]
        fn toggle_whole_word(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_whole_word());
        }

        #[unsafe(method(switchTheme:))]
        fn switch_theme(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.cycle_theme());
        }

        #[unsafe(method(selectTheme:))]
        fn select_theme(&self, sender: Option<&AnyObject>) {
            if let Some(sender) = sender {
                // 来源：菜单项（NSMenuItem）或设置 sheet 的主题单选（NSButton）
                let name = if let Some(item) = sender.downcast_ref::<objc2_app_kit::NSMenuItem>() {
                    item.title().to_string()
                } else if let Some(btn) = sender.downcast_ref::<objc2_app_kit::NSButton>() {
                    btn.title().to_string()
                } else {
                    return;
                };
                crate::app::with_app(|app| app.apply_theme(name));
            }
        }

        #[unsafe(method(copyLine:))]
        fn copy_line(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.copy_current_line());
        }

        /// 复制：优先选区，否则复制当前行（响应链兜底，LogView 为 first responder
        /// 时其 `copy:` 先被调用）。
        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| {
                let n = app.copy_selection_or_line();
                if let Some(s) = n {
                    if app.selection.is_empty() {
                        app.flash_status("已复制当前行", 1.2);
                    } else {
                        let chars = s.chars().count();
                        app.flash_status(&format!("已复制选区（{} 字符）", chars), 1.2);
                    }
                }
            });
        }

        #[unsafe(method(showProperties:))]
        fn show_properties(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::show_properties();
        }

        #[unsafe(method(showSettings:))]
        fn show_settings(&self, _sender: Option<&AnyObject>) {
            // 从 App 取窗口 + delegate，再进 with_app 呈现 sheet（重入不变量：
            // present 只同步建面板 + beginSheet，不跑嵌套 runloop）。
            crate::app::with_app(|app| {
                if let Some(d) = app.delegate.get().cloned() {
                    crate::settings_sheet::SettingsSheet::present(app, &d);
                }
            });
        }

        /// beginSheet:...didEndSelector: 的结束回调：清理 App 里持有的 sheet 引用。
        #[unsafe(method(sheetDidEnd:returnCode:contextInfo:))]
        fn sheet_did_end(
            &self,
            _sheet: &NSWindow,
            _return_code: NSModalResponse,
            _context: *mut c_void,
        ) {
            crate::app::with_app(|app| {
                let _ = app.settings_sheet.take();
            });
        }

        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::show_about();
        }

        #[unsafe(method(showHelp:))]
        fn show_help(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::show_help();
        }

        #[unsafe(method(showShortcuts:))]
        fn show_shortcuts(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::show_shortcuts();
        }

        #[unsafe(method(openConfigDir:))]
        fn open_config_dir(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::open_config_dir();
        }

        #[unsafe(method(openRecent:))]
        fn open_recent(&self, sender: Option<&AnyObject>) {
            if let Some(sender) = sender {
                if let Some(item) = sender.downcast_ref::<objc2_app_kit::NSMenuItem>() {
                    if let Some(ro) = item.representedObject() {
                        if let Some(s) = ro.downcast_ref::<objc2_foundation::NSString>() {
                            let p = s.to_string();
                            let result = crate::app::with_app(|app| app.open_path(PathBuf::from(p)));
                            if let Err(e) = result {
                                crate::dialogs::show_error(&e);
                            }
                        }
                    }
                }
            }
        }

        #[unsafe(method(manageIndexes:))]
        fn manage_indexes(&self, _sender: Option<&AnyObject>) {
            crate::dialogs::manage_indexes();
        }

        #[unsafe(method(pageUp:))]
        fn page_up(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.page_up());
        }

        #[unsafe(method(pageDown:))]
        fn page_down(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.page_down());
        }

        #[unsafe(method(toggleIndentGuides:))]
        fn toggle_indent_guides(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.toggle_indent_guides());
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::default());
        unsafe { msg_send![super(this), init] }
    }
}

/// 打开文件对话框（遵守重入不变量：modal 期间不持有 &mut App）。
fn open_document_modal() {
    let window = with_app(|app| app.window.get().cloned());
    if let Some(path) = crate::dialogs::pick_file(&window) {
        let result = with_app(|app| app.open_path(path));
        if let Err(e) = result {
            crate::dialogs::show_error(&e);
        }
    }
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

pub fn run() {
    let mtm = MainThreadMarker::new().unwrap();
    let app_obj = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm);
    app_obj.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    // 建立全局 App 单例
    let app = Box::new(App::new());
    let app_ptr = Box::into_raw(app);
    unsafe {
        APP_PTR = app_ptr;
    }

    // 创建窗口 / 视图 / 菜单（用直接指针访问，避免 with_app 双重借用）
    unsafe {
        (&mut *app_ptr).setup_ui(mtm, &delegate);
    }

    app_obj.run();
}
