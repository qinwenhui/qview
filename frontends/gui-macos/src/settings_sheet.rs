//! 完整 4 标签页设置 sheet（显示 / 搜索 / 主题 / 引擎）。
//!
//! 用 `NSPanel` + 旧式 `beginSheet:modalForWindow:modalDelegate:didEndSelector:contextInfo:`
//!（经 `msg_send!`，避免 block2），Apply / Cancel 按钮 target 指向本 SettingsSheet。
//! 重入纪律：按钮 action 各自新建 `with_app` 闭包；`endSheet` 是异步的，不会嵌套
//! runloop。主题单选即时应用（action → AppDelegate `selectTheme:`）。

use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSButtonType, NSControlStateValueOff,
    NSControlStateValueOn, NSFontManager, NSPanel, NSPopUpButton, NSSlider, NSTabView,
    NSTabViewItem, NSTextField, NSView, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use crate::app::{with_app, App, AppDelegate};
use crate::util::ns_string;

const PANEL_W: f64 = 640.0;
const PANEL_H: f64 = 480.0;
const TAB_W: f64 = 616.0;
const TAB_H: f64 = 400.0;

/// 引擎编码选项（与 qview-core `resolve_encoding` 支持的标签一致）。
const ENCODINGS: &[&str] = &[
    "UTF-8",
    "GBK",
    "GB18030",
    "GB2312",
    "Big5",
    "Shift_JIS",
    "EUC-JP",
    "EUC-KR",
    "windows-1252",
    "UTF-16LE",
    "UTF-16BE",
];
/// 小文件阈值选项（MB → 字节）。
const THRESHOLDS_MB: &[(u64, &str)] = &[
    (1, "1 MB"),
    (5, "5 MB"),
    (10, "10 MB"),
    (50, "50 MB"),
    (100, "100 MB"),
];
/// 行缓存容量选项。
const CACHE_CAPS: &[(usize, &str)] = &[
    (5000, "5000"),
    (10000, "10000"),
    (20000, "20000"),
    (50000, "50000"),
];

#[derive(Default)]
pub struct SettingsSheetIvars {
    panel: OnceCell<Retained<NSPanel>>,
    font_popup: OnceCell<Retained<NSPopUpButton>>,
    font_size_slider: OnceCell<Retained<NSSlider>>,
    font_size_label: OnceCell<Retained<NSTextField>>,
    row_h_slider: OnceCell<Retained<NSSlider>>,
    row_h_label: OnceCell<Retained<NSTextField>>,
    cb_line_numbers: OnceCell<Retained<NSButton>>,
    cb_word_wrap: OnceCell<Retained<NSButton>>,
    cb_whitespace: OnceCell<Retained<NSButton>>,
    cb_level_coloring: OnceCell<Retained<NSButton>>,
    cb_indent_guides: OnceCell<Retained<NSButton>>,
    cb_case_sensitive: OnceCell<Retained<NSButton>>,
    cb_regex: OnceCell<Retained<NSButton>>,
    cb_whole_word: OnceCell<Retained<NSButton>>,
    theme_radios: RefCell<Vec<Retained<NSButton>>>,
    encoding_popup: OnceCell<Retained<NSPopUpButton>>,
    threshold_popup: OnceCell<Retained<NSPopUpButton>>,
    cb_cache: OnceCell<Retained<NSButton>>,
    cache_popup: OnceCell<Retained<NSPopUpButton>>,
    index_dir_label: OnceCell<Retained<NSTextField>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[name = "QLogSettingsSheet"]
    #[ivars = SettingsSheetIvars]
    pub struct SettingsSheet;

    unsafe impl NSObjectProtocol for SettingsSheet {}

    impl SettingsSheet {
        /// 应用：读取控件快照 → with_app 应用 → 关 sheet。
        #[unsafe(method(applySettings:))]
        fn apply_settings(&self, _sender: Option<&AnyObject>) {
            let snap = self.read_snapshot();
            with_app(|app| apply_snapshot(app, &snap));
            self.end_sheet();
        }

        /// 取消：直接关 sheet（不应用）。
        #[unsafe(method(cancelSettings:))]
        fn cancel_settings(&self, _sender: Option<&AnyObject>) {
            self.end_sheet();
        }

        /// 滑块值标签实时更新（tag 0=字号, 1=行高）。
        #[unsafe(method(sliderChanged:))]
        fn slider_changed(&self, sender: Option<&AnyObject>) {
            let Some(sender) = sender else { return };
            let Some(slider) = sender.downcast_ref::<NSSlider>() else { return };
            let s = format!("{:.0}", slider.doubleValue());
            match slider.tag() {
                0 => {
                    if let Some(l) = self.ivars().font_size_label.get() {
                        l.setStringValue(&ns_string(&s));
                    }
                }
                _ => {
                    if let Some(l) = self.ivars().row_h_label.get() {
                        l.setStringValue(&ns_string(&s));
                    }
                }
            }
        }
    }
);

/// 从 sheet 控件读取到的完整设置快照。
struct SettingsSnapshot {
    font_family: String,
    font_size: f64,
    row_height: f64,
    show_line_numbers: bool,
    word_wrap: bool,
    show_whitespace: bool,
    level_coloring: bool,
    show_indent_guides: bool,
    case_sensitive: bool,
    use_regex: bool,
    whole_word: bool,
    encoding: String,
    small_file_threshold: u64,
    index_cache_enabled: bool,
    line_cache_capacity: usize,
}

impl SettingsSheet {
    /// 弹出设置 sheet。若已打开则忽略。
    pub fn present(app: &mut App, delegate: &Retained<AppDelegate>) {
        if app.settings_sheet.get().is_some() {
            return;
        }
        let Some(window) = app.window.get().cloned() else {
            return;
        };
        let mtm = MainThreadMarker::new().expect("settings sheet on main thread");
        let this = Self::alloc(mtm).set_ivars(SettingsSheetIvars::default());
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build_panel(mtm, app, delegate);
        let _ = app.settings_sheet.set(this.clone());
        let panel = this.ivars().panel.get().expect("panel built").clone();
        unsafe {
            let _: () = msg_send![
                &*window,
                beginSheet:&*panel,
                modalForWindow:&*window,
                modalDelegate:&**delegate as &AnyObject,
                didEndSelector:sel!(sheetDidEnd:returnCode:contextInfo:),
                contextInfo:std::ptr::null_mut::<std::ffi::c_void>()
            ];
        }
    }

    /// 菜单切换主题后，同步 sheet 里的单选状态。
    pub fn sync_theme_radios(&self, theme: &str) {
        for r in self.ivars().theme_radios.borrow().iter() {
            let on = r.title().to_string() == theme;
            r.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    fn end_sheet(&self) {
        if let Some(panel) = self.ivars().panel.get() {
            if let Some(parent) = panel.sheetParent() {
                parent.endSheet(panel);
            }
        }
    }

    fn read_snapshot(&self) -> SettingsSnapshot {
        let iv = self.ivars();
        let popup_str = |p: &OnceCell<Retained<NSPopUpButton>>| -> String {
            p.get()
                .and_then(|p| p.selectedItem())
                .map(|i| i.title().to_string())
                .unwrap_or_default()
        };
        let checkbox = |c: &OnceCell<Retained<NSButton>>| -> bool {
            c.get()
                .map(|b| b.state() == NSControlStateValueOn)
                .unwrap_or(false)
        };
        let font_size = iv
            .font_size_slider
            .get()
            .map(|s| s.doubleValue())
            .unwrap_or(13.0);
        let row_height = iv
            .row_h_slider
            .get()
            .map(|s| s.doubleValue())
            .unwrap_or(18.0);

        let threshold_label = popup_str(&iv.threshold_popup);
        let small_file_threshold = THRESHOLDS_MB
            .iter()
            .find(|(_, l)| *l == threshold_label)
            .map(|(v, _)| *v * 1024 * 1024)
            .unwrap_or(10 * 1024 * 1024);
        let cap_label = popup_str(&iv.cache_popup);
        let line_cache_capacity = CACHE_CAPS
            .iter()
            .find(|(_, l)| *l == cap_label)
            .map(|(v, _)| *v)
            .unwrap_or(10000);

        SettingsSnapshot {
            font_family: popup_str(&iv.font_popup),
            font_size,
            row_height,
            show_line_numbers: checkbox(&iv.cb_line_numbers),
            word_wrap: checkbox(&iv.cb_word_wrap),
            show_whitespace: checkbox(&iv.cb_whitespace),
            level_coloring: checkbox(&iv.cb_level_coloring),
            show_indent_guides: checkbox(&iv.cb_indent_guides),
            case_sensitive: checkbox(&iv.cb_case_sensitive),
            use_regex: checkbox(&iv.cb_regex),
            whole_word: checkbox(&iv.cb_whole_word),
            // 主题已在单选点击时即时应用（selectTheme:），这里仅保证快照一致
            encoding: popup_str(&iv.encoding_popup),
            small_file_threshold,
            index_cache_enabled: checkbox(&iv.cb_cache),
            line_cache_capacity,
        }
    }

    // ---------------------------------------------------------------------
    // 面板构建
    // ---------------------------------------------------------------------

    fn build_panel(&self, mtm: MainThreadMarker, app: &App, delegate: &Retained<AppDelegate>) {
        {
            let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_W, PANEL_H)),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            );
            panel.setTitle(&ns_string("设置"));
            let content = NSView::new(mtm);
            content.setFrame(NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(PANEL_W, PANEL_H),
            ));
            panel.setContentView(Some(&content));

            let tab = NSTabView::new(mtm);
            tab.setFrame(NSRect::new(
                NSPoint::new(12.0, 54.0),
                NSSize::new(TAB_W, TAB_H),
            ));
            content.addSubview(&tab);

            let display_tab = self.build_display_tab(mtm, app);
            let search_tab = self.build_search_tab(mtm, app);
            let theme_tab = self.build_theme_tab(mtm, app, delegate);
            let engine_tab = self.build_engine_tab(mtm, app);
            add_tab(&tab, &ns_string("显示"), &display_tab);
            add_tab(&tab, &ns_string("搜索"), &search_tab);
            add_tab(&tab, &ns_string("主题"), &theme_tab);
            add_tab(&tab, &ns_string("引擎"), &engine_tab);

            // 取消（Esc）按钮
            let cancel = NSButton::new(mtm);
            cancel.setTitle(&ns_string("取消"));
            cancel.setKeyEquivalent(&ns_string("\u{1b}"));
            unsafe {
                cancel.setTarget(Some(self as &AnyObject));
                cancel.setAction(Some(sel!(cancelSettings:)));
            }
            cancel.setFrame(NSRect::new(
                NSPoint::new(PANEL_W - 184.0, 12.0),
                NSSize::new(84.0, 28.0),
            ));
            content.addSubview(&cancel);

            // 应用（Return）按钮
            let apply = NSButton::new(mtm);
            apply.setTitle(&ns_string("应用"));
            apply.setKeyEquivalent(&ns_string("\r"));
            unsafe {
                apply.setTarget(Some(self as &AnyObject));
                apply.setAction(Some(sel!(applySettings:)));
            }
            apply.setFrame(NSRect::new(
                NSPoint::new(PANEL_W - 92.0, 12.0),
                NSSize::new(80.0, 28.0),
            ));
            content.addSubview(&apply);

            let _ = self.ivars().panel.set(panel);
        }
    }

    fn build_display_tab(&self, mtm: MainThreadMarker, app: &App) -> Retained<NSView> {
        let v = NSView::new(mtm);
        v.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(TAB_W, TAB_H),
        ));
        unsafe {
            // 等宽字体
            v.addSubview(&make_label(mtm, "等宽字体", 16.0, 352.0, 84.0, 24.0));
            let font_popup = NSPopUpButton::new(mtm);
            font_popup.setFrame(NSRect::new(
                NSPoint::new(104.0, 350.0),
                NSSize::new(300.0, 26.0),
            ));
            let families = available_mono_families(mtm);
            let current = app.config.gui.font_family.clone();
            for fam in &families {
                font_popup.addItemWithTitle(&ns_string(fam));
            }
            if !families.iter().any(|f| *f == current) {
                font_popup.addItemWithTitle(&ns_string(&current));
            }
            font_popup.selectItemWithTitle(&ns_string(&current));
            v.addSubview(&font_popup);
            let _ = self.ivars().font_popup.set(font_popup);

            // 字号
            v.addSubview(&make_label(mtm, "字号", 16.0, 310.0, 84.0, 24.0));
            let fs = NSSlider::new(mtm);
            fs.setFrame(NSRect::new(
                NSPoint::new(104.0, 314.0),
                NSSize::new(200.0, 24.0),
            ));
            fs.setMinValue(8.0);
            fs.setMaxValue(32.0);
            fs.setDoubleValue(app.font_size);
            fs.setTag(0);
            fs.setTarget(Some(self as &AnyObject));
            fs.setAction(Some(sel!(sliderChanged:)));
            v.addSubview(&fs);
            let fs_label = make_label(mtm, &format!("{:.0}", app.font_size), 314.0, 310.0, 40.0, 24.0);
            v.addSubview(&fs_label);
            let _ = self.ivars().font_size_slider.set(fs);
            let _ = self.ivars().font_size_label.set(fs_label);

            // 行高
            v.addSubview(&make_label(mtm, "行高", 16.0, 268.0, 84.0, 24.0));
            let rh = NSSlider::new(mtm);
            rh.setFrame(NSRect::new(
                NSPoint::new(104.0, 272.0),
                NSSize::new(200.0, 24.0),
            ));
            rh.setMinValue(14.0);
            rh.setMaxValue(36.0);
            rh.setDoubleValue(app.row_h);
            rh.setTag(1);
            rh.setTarget(Some(self as &AnyObject));
            rh.setAction(Some(sel!(sliderChanged:)));
            v.addSubview(&rh);
            let rh_label = make_label(mtm, &format!("{:.0}", app.row_h), 314.0, 268.0, 40.0, 24.0);
            v.addSubview(&rh_label);
            let _ = self.ivars().row_h_slider.set(rh);
            let _ = self.ivars().row_h_label.set(rh_label);

            // 勾选项
            let cb_line = make_checkbox(mtm, "显示行号", app.show_line_numbers, 16.0, 226.0, 220.0);
            let cb_wrap = make_checkbox(mtm, "自动换行", app.word_wrap, 16.0, 196.0, 220.0);
            let cb_ws = make_checkbox(mtm, "显示空白", app.show_whitespace, 16.0, 166.0, 220.0);
            let cb_level = make_checkbox(mtm, "级别着色", app.level_coloring, 16.0, 136.0, 220.0);
            let cb_indent = make_checkbox(mtm, "缩进参考线", app.show_indent_guides, 16.0, 106.0, 220.0);
            v.addSubview(&cb_line);
            v.addSubview(&cb_wrap);
            v.addSubview(&cb_ws);
            v.addSubview(&cb_level);
            v.addSubview(&cb_indent);
            let _ = self.ivars().cb_line_numbers.set(cb_line);
            let _ = self.ivars().cb_word_wrap.set(cb_wrap);
            let _ = self.ivars().cb_whitespace.set(cb_ws);
            let _ = self.ivars().cb_level_coloring.set(cb_level);
            let _ = self.ivars().cb_indent_guides.set(cb_indent);
        }
        v
    }

    fn build_search_tab(&self, mtm: MainThreadMarker, app: &App) -> Retained<NSView> {
        let v = NSView::new(mtm);
        v.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(TAB_W, TAB_H),
        ));
        let cb_case = make_checkbox(mtm, "区分大小写", app.config.gui.case_sensitive, 16.0, 340.0, 240.0);
        let cb_regex = make_checkbox(mtm, "正则表达式", app.config.gui.use_regex, 16.0, 300.0, 240.0);
        let cb_whole = make_checkbox(mtm, "整词匹配", app.config.gui.whole_word, 16.0, 260.0, 240.0);
        v.addSubview(&cb_case);
        v.addSubview(&cb_regex);
        v.addSubview(&cb_whole);
        let _ = self.ivars().cb_case_sensitive.set(cb_case);
        let _ = self.ivars().cb_regex.set(cb_regex);
        let _ = self.ivars().cb_whole_word.set(cb_whole);
        v
    }

    fn build_theme_tab(&self, mtm: MainThreadMarker, app: &App, delegate: &Retained<AppDelegate>) -> Retained<NSView> {
        let v = NSView::new(mtm);
        v.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(TAB_W, TAB_H),
        ));
        unsafe {
            let names = crate::theme::theme_names();
            for (i, name) in names.iter().enumerate() {
                let r = NSButton::new(mtm);
                r.setTitle(&ns_string(name));
                r.setButtonType(NSButtonType::Radio);
                r.setState(if *name == app.theme_name {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                r.setFrame(NSRect::new(
                    NSPoint::new(16.0, 350.0 - i as f64 * 34.0),
                    NSSize::new(240.0, 26.0),
                ));
                r.setTarget(Some(&**delegate as &AnyObject));
                r.setAction(Some(sel!(selectTheme:)));
                v.addSubview(&r);
                self.ivars().theme_radios.borrow_mut().push(r);
            }
        }
        v
    }

    fn build_engine_tab(&self, mtm: MainThreadMarker, app: &App) -> Retained<NSView> {
        let v = NSView::new(mtm);
        v.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(TAB_W, TAB_H),
        ));
        {
            // 编码
            v.addSubview(&make_label(mtm, "编码", 16.0, 352.0, 84.0, 24.0));
            let enc = NSPopUpButton::new(mtm);
            enc.setFrame(NSRect::new(
                NSPoint::new(104.0, 350.0),
                NSSize::new(240.0, 26.0),
            ));
            for e in ENCODINGS {
                enc.addItemWithTitle(&ns_string(e));
            }
            enc.selectItemWithTitle(&ns_string(&app.config.engine.encoding));
            v.addSubview(&enc);
            let _ = self.ivars().encoding_popup.set(enc);

            // 小文件阈值
            v.addSubview(&make_label(mtm, "小文件阈值", 16.0, 310.0, 84.0, 24.0));
            let th = NSPopUpButton::new(mtm);
            th.setFrame(NSRect::new(
                NSPoint::new(104.0, 308.0),
                NSSize::new(200.0, 26.0),
            ));
            let cur_th = app.config.engine.small_file_threshold;
            for (_, l) in THRESHOLDS_MB {
                th.addItemWithTitle(&ns_string(l));
            }
            let match_th = THRESHOLDS_MB
                .iter()
                .find(|(v, _)| *v == cur_th / (1024 * 1024))
                .map(|(_, l)| l.to_string());
            match match_th {
                Some(l) => th.selectItemWithTitle(&ns_string(&l)),
                None => {
                    th.addItemWithTitle(&ns_string(&format!("{} MB", cur_th / (1024 * 1024))));
                    th.selectItemAtIndex(0);
                }
            }
            v.addSubview(&th);
            let _ = self.ivars().threshold_popup.set(th);

            // 索引缓存勾选
            let cb_cache = make_checkbox(mtm, "索引缓存", app.config.engine.index_cache_enabled, 16.0, 268.0, 220.0);
            v.addSubview(&cb_cache);
            let _ = self.ivars().cb_cache.set(cb_cache);

            // 行缓存容量
            v.addSubview(&make_label(mtm, "行缓存容量", 16.0, 226.0, 84.0, 24.0));
            let cap = NSPopUpButton::new(mtm);
            cap.setFrame(NSRect::new(
                NSPoint::new(104.0, 224.0),
                NSSize::new(200.0, 26.0),
            ));
            for (_, l) in CACHE_CAPS {
                cap.addItemWithTitle(&ns_string(l));
            }
            let cur_cap = app.config.engine.line_cache_capacity;
            let match_cap = CACHE_CAPS
                .iter()
                .find(|(v, _)| *v == cur_cap)
                .map(|(_, l)| l.to_string());
            match match_cap {
                Some(l) => cap.selectItemWithTitle(&ns_string(&l)),
                None => {
                    cap.addItemWithTitle(&ns_string(&cur_cap.to_string()));
                    cap.selectItemAtIndex(0);
                }
            }
            v.addSubview(&cap);
            let _ = self.ivars().cache_popup.set(cap);

            // 索引目录（只读）
            v.addSubview(&make_label(mtm, "索引目录", 16.0, 184.0, 84.0, 24.0));
            let dir = app
                .config
                .engine
                .index_dir
                .clone()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "未设置".to_string());
            let dir_label = make_label(mtm, &dir, 104.0, 184.0, 480.0, 24.0);
            v.addSubview(&dir_label);
            let _ = self.ivars().index_dir_label.set(dir_label);
        }
        v
    }
}

// ---------------------------------------------------------------------------
// 应用快照
// ---------------------------------------------------------------------------

fn apply_snapshot(app: &mut App, snap: &SettingsSnapshot) {
    let old_wrap = app.word_wrap;

    app.config.gui.font_family = snap.font_family.clone();
    app.font_size = snap.font_size;
    app.config.gui.font_size = snap.font_size as f32;
    app.row_h = snap.row_height;
    app.config.gui.row_height = snap.row_height;
    app.show_line_numbers = snap.show_line_numbers;
    app.config.gui.show_line_numbers = snap.show_line_numbers;
    app.word_wrap = snap.word_wrap;
    app.config.gui.word_wrap = snap.word_wrap;
    app.show_whitespace = snap.show_whitespace;
    app.config.gui.show_whitespace = snap.show_whitespace;
    app.level_coloring = snap.level_coloring;
    app.config.gui.level_coloring = snap.level_coloring;
    app.show_indent_guides = snap.show_indent_guides;
    app.config.gui.show_indent_guides = snap.show_indent_guides;
    app.config.gui.case_sensitive = snap.case_sensitive;
    app.config.gui.use_regex = snap.use_regex;
    app.config.gui.whole_word = snap.whole_word;
    app.config.engine.encoding = snap.encoding.clone();
    app.config.engine.small_file_threshold = snap.small_file_threshold;
    app.config.engine.index_cache_enabled = snap.index_cache_enabled;
    app.config.engine.line_cache_capacity = snap.line_cache_capacity;

    app.save_config_now();
    app.rebuild_font();
    app.max_content_w = 0.0;
    app.rendered_content_h = 0.0;
    // 换行模式回到 x=0（无横向滚动）
    if snap.word_wrap && !old_wrap {
        if let Some(scroll_view) = app.scroll_view.get() {
            let clip = scroll_view.contentView();
            clip.scrollToPoint(objc2_foundation::NSPoint::new(
                0.0,
                clip.bounds().origin.y,
            ));
            scroll_view.reflectScrolledClipView(&clip);
        }
    }
    app.set_log_view_size();
    crate::menu::sync_view_checks(app);
    crate::menu::sync_search_checks(app);
    if let Some(lv) = app.log_view.get() {
        lv.setNeedsDisplay(true);
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn add_tab(tab: &NSTabView, title: &NSString, view: &NSView) {
    unsafe {
        let item = NSTabViewItem::initWithIdentifier(NSTabViewItem::alloc(), None);
        item.setLabel(title);
        item.setView(Some(view));
        tab.addTabViewItem(&item);
    }
}

fn make_label(mtm: MainThreadMarker, text: &str, x: f64, y: f64, w: f64, h: f64) -> Retained<NSTextField> {
    let f = NSTextField::new(mtm);
    f.setStringValue(&ns_string(text));
    f.setEditable(false);
    f.setSelectable(false);
    f.setBezeled(false);
    f.setDrawsBackground(false);
    f.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)));
    f
}

fn make_checkbox(
    mtm: MainThreadMarker,
    title: &str,
    on: bool,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSButton> {
    let b = NSButton::new(mtm);
    b.setTitle(&ns_string(title));
    b.setButtonType(NSButtonType::Switch);
    b.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
    b.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(w, 24.0)));
    b
}

/// 系统已安装的等宽字体族（用于字体下拉）。
fn available_mono_families(mtm: MainThreadMarker) -> Vec<String> {
    let fm = NSFontManager::sharedFontManager(mtm);
    let families = fm.availableFontFamilies();
    let mut out: Vec<String> = families
        .iter()
        .map(|f| f.to_string())
        .filter(|n| {
            let n = n.to_lowercase();
            n.contains("mono")
                || n.contains("menlo")
                || n.contains("courier")
                || n.contains("monaco")
                || n.contains("source code")
                || n.contains("jetbrains")
        })
        .collect();
    out.sort();
    out.dedup();
    for c in ["Menlo", "SF Mono", "Monaco"] {
        if !out.iter().any(|n| n.eq_ignore_ascii_case(c)) {
            out.insert(0, c.to_string());
        }
    }
    out
}
