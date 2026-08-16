//! 主菜单栏：App / File / Edit / View / Search / Settings / Help。
//!
//! 全部菜单项 target 指向 AppDelegate（见 app.rs），action 经 with_app 分发。
//! Open Recent 是动态菜单（menuNeedsUpdate 时重建）。

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSMenu, NSMenuItem, NSEventModifierFlags, NSControlStateValueOn,
    NSControlStateValueOff,
};

use crate::app::{with_app, App, AppDelegate};

const MODS_EMPTY: NSEventModifierFlags = NSEventModifierFlags::empty();
const MODS_CMD: NSEventModifierFlags = NSEventModifierFlags::Command;
const MODS_CMD_SHIFT: NSEventModifierFlags =
    NSEventModifierFlags(NSEventModifierFlags::Command.0 | NSEventModifierFlags::Shift.0);
const MODS_SHIFT: NSEventModifierFlags = NSEventModifierFlags::Shift;

// 功能键 keyEquivalent（Unicode 私有区）
const KEY_F1: &str = "\u{F704}";
const KEY_F3: &str = "\u{F703}";
const KEY_HOME: &str = "\u{F729}";
const KEY_END: &str = "\u{F72B}";
const KEY_PAGEUP: &str = "\u{F72C}";
const KEY_PAGEDOWN: &str = "\u{F72D}";

/// 构建主菜单栏。会把 Open Recent / 主题 / 视图 / 搜索子菜单句柄存进 app。
pub fn build_main_menu(
    mtm: MainThreadMarker,
    delegate: &Retained<AppDelegate>,
    app: &mut App,
) -> Retained<NSMenu> {
    unsafe {
        let bar = NSMenu::new(mtm);
        let app_obj = NSApplication::sharedApplication(mtm);

        // ---- App 菜单 ----
        let app_menu = NSMenu::new(mtm);
        add(
            &app_menu,
            mtm,
            "关于 qview",
            Some(sel!(showAbout:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &app_menu,
            mtm,
            "设置…",
            Some(sel!(showSettings:)),
            Some(&**delegate as &AnyObject),
            ",",
            MODS_CMD,
        );
        app_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &app_menu,
            mtm,
            "退出 qview",
            Some(sel!(terminate:)),
            Some(&app_obj as &AnyObject),
            "q",
            MODS_CMD,
        );
        let app_item = NSMenuItem::new(mtm);
        app_item.setTitle(&crate::util::ns_string("qview"));
        app_item.setSubmenu(Some(&app_menu));
        bar.addItem(&app_item);

        // ---- File 菜单 ----
        let file_menu = NSMenu::new(mtm);
        add(
            &file_menu,
            mtm,
            "打开…",
            Some(sel!(openDocument:)),
            Some(&**delegate as &AnyObject),
            "o",
            MODS_CMD,
        );
        // Open Recent（动态）
        let recent_menu = NSMenu::new(mtm);
        recent_menu.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
        let recent_item = NSMenuItem::new(mtm);
        recent_item.setTitle(&crate::util::ns_string("最近打开"));
        recent_item.setSubmenu(Some(&recent_menu));
        file_menu.addItem(&recent_item);
        file_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &file_menu,
            mtm,
            "重新加载",
            Some(sel!(reloadDocument:)),
            Some(&**delegate as &AnyObject),
            "r",
            MODS_CMD,
        );
        add(
            &file_menu,
            mtm,
            "关闭",
            Some(sel!(closeDocument:)),
            Some(&**delegate as &AnyObject),
            "w",
            MODS_CMD,
        );
        add(
            &file_menu,
            mtm,
            "文件属性",
            Some(sel!(showProperties:)),
            Some(&**delegate as &AnyObject),
            "i",
            MODS_CMD,
        );
        bar.addItem(&top_level_item(mtm, "文件", &file_menu));

        // ---- 工具菜单 ----
        let tools_menu = NSMenu::new(mtm);
        add(
            &tools_menu,
            mtm,
            "缓存管理…",
            Some(sel!(manageIndexes:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        bar.addItem(&top_level_item(mtm, "工具", &tools_menu));

        // ---- Edit 菜单 ----
        let edit_menu = NSMenu::new(mtm);
        // 复制：target = nil → 走响应链。LogView 为 first responder 时其 copy:
        // 复制选区（否则当前行）；聚焦在文本框时由文本框处理。
        add(
            &edit_menu,
            mtm,
            "复制",
            Some(sel!(copy:)),
            None,
            "c",
            MODS_CMD,
        );
        let select_all = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &crate::util::ns_string("全选"),
            Some(sel!(selectAll:)),
            &crate::util::ns_string("a"),
        );
        select_all.setKeyEquivalentModifierMask(MODS_CMD);
        // target = nil → 走响应链（LogView selectAll: 全选复制；文本框全选）
        edit_menu.addItem(&select_all);
        edit_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &edit_menu,
            mtm,
            "查找",
            Some(sel!(find:)),
            Some(&**delegate as &AnyObject),
            "f",
            MODS_CMD,
        );
        add(
            &edit_menu,
            mtm,
            "查找下一个",
            Some(sel!(findNext:)),
            Some(&**delegate as &AnyObject),
            KEY_F3,
            MODS_EMPTY,
        );
        add(
            &edit_menu,
            mtm,
            "查找上一个",
            Some(sel!(findPrevious:)),
            Some(&**delegate as &AnyObject),
            KEY_F3,
            MODS_SHIFT,
        );
        // 备用快捷键（对齐 egui 的 Ctrl+G / Ctrl+Shift+G，macOS 用 Cmd 表示）
        add(
            &edit_menu,
            mtm,
            "查找下一个 (⌘G)",
            Some(sel!(findNext:)),
            Some(&**delegate as &AnyObject),
            "g",
            MODS_CMD,
        );
        add(
            &edit_menu,
            mtm,
            "查找上一个 (⇧⌘G)",
            Some(sel!(findPrevious:)),
            Some(&**delegate as &AnyObject),
            "g",
            MODS_CMD_SHIFT,
        );
        bar.addItem(&top_level_item(mtm, "编辑", &edit_menu));

        // ---- View 菜单 ----
        let view_menu = NSMenu::new(mtm);
        add(
            &view_menu,
            mtm,
            "到顶部",
            Some(sel!(goTop:)),
            Some(&**delegate as &AnyObject),
            KEY_HOME,
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "到底部",
            Some(sel!(goEnd:)),
            Some(&**delegate as &AnyObject),
            KEY_END,
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "跳到行…",
            Some(sel!(gotoLine:)),
            Some(&**delegate as &AnyObject),
            "l",
            MODS_CMD,
        );
        view_menu.addItem(&NSMenuItem::separatorItem(mtm));
        // 主题子菜单
        let theme_menu = NSMenu::new(mtm);
        for name in crate::theme::theme_names() {
            let item = make_item(
                mtm,
                name,
                Some(sel!(selectTheme:)),
                Some(&**delegate as &AnyObject),
                "",
                MODS_EMPTY,
            );
            theme_menu.addItem(&item);
        }
        let theme_item = NSMenuItem::new(mtm);
        theme_item.setTitle(&crate::util::ns_string("主题"));
        theme_item.setSubmenu(Some(&theme_menu));
        view_menu.addItem(&theme_item);
        add(
            &view_menu,
            mtm,
            "切换主题",
            Some(sel!(switchTheme:)),
            Some(&**delegate as &AnyObject),
            "t",
            MODS_CMD_SHIFT,
        );
        view_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &view_menu,
            mtm,
            "显示行号",
            Some(sel!(toggleLineNumbers:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "自动换行",
            Some(sel!(toggleWordWrap:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "显示空白",
            Some(sel!(toggleWhitespace:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "级别着色",
            Some(sel!(toggleLevelColoring:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "缩进参考线",
            Some(sel!(toggleIndentGuides:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        view_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &view_menu,
            mtm,
            "上一页",
            Some(sel!(pageUp:)),
            Some(&**delegate as &AnyObject),
            KEY_PAGEUP,
            MODS_EMPTY,
        );
        add(
            &view_menu,
            mtm,
            "下一页",
            Some(sel!(pageDown:)),
            Some(&**delegate as &AnyObject),
            KEY_PAGEDOWN,
            MODS_EMPTY,
        );
        bar.addItem(&top_level_item(mtm, "视图", &view_menu));

        // ---- Search 菜单 ----
        let search_menu = NSMenu::new(mtm);
        add(
            &search_menu,
            mtm,
            "查找",
            Some(sel!(find:)),
            Some(&**delegate as &AnyObject),
            "f",
            MODS_CMD,
        );
        add(
            &search_menu,
            mtm,
            "查找下一个",
            Some(sel!(findNext:)),
            Some(&**delegate as &AnyObject),
            KEY_F3,
            MODS_EMPTY,
        );
        add(
            &search_menu,
            mtm,
            "查找上一个",
            Some(sel!(findPrevious:)),
            Some(&**delegate as &AnyObject),
            KEY_F3,
            MODS_SHIFT,
        );
        search_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &search_menu,
            mtm,
            "区分大小写",
            Some(sel!(toggleCaseSensitive:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        add(
            &search_menu,
            mtm,
            "正则表达式",
            Some(sel!(toggleRegex:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        add(
            &search_menu,
            mtm,
            "整词匹配",
            Some(sel!(toggleWholeWord:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        bar.addItem(&top_level_item(mtm, "搜索", &search_menu));

        // ---- Settings 菜单 ----
        let settings_menu = NSMenu::new(mtm);
        add(
            &settings_menu,
            mtm,
            "字体加大",
            Some(sel!(fontBigger:)),
            Some(&**delegate as &AnyObject),
            "=",
            MODS_CMD,
        );
        add(
            &settings_menu,
            mtm,
            "字体减小",
            Some(sel!(fontSmaller:)),
            Some(&**delegate as &AnyObject),
            "-",
            MODS_CMD,
        );
        add(
            &settings_menu,
            mtm,
            "字体重置",
            Some(sel!(fontReset:)),
            Some(&**delegate as &AnyObject),
            "0",
            MODS_CMD,
        );
        settings_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &settings_menu,
            mtm,
            "打开配置目录",
            Some(sel!(openConfigDir:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        bar.addItem(&top_level_item(mtm, "设置", &settings_menu));

        // ---- Help 菜单 ----
        let help_menu = NSMenu::new(mtm);
        add(
            &help_menu,
            mtm,
            "帮助",
            Some(sel!(showHelp:)),
            Some(&**delegate as &AnyObject),
            KEY_F1,
            MODS_EMPTY,
        );
        add(
            &help_menu,
            mtm,
            "快捷键",
            Some(sel!(showShortcuts:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        help_menu.addItem(&NSMenuItem::separatorItem(mtm));
        add(
            &help_menu,
            mtm,
            "关于",
            Some(sel!(showAbout:)),
            Some(&**delegate as &AnyObject),
            "",
            MODS_EMPTY,
        );
        bar.addItem(&top_level_item(mtm, "帮助", &help_menu));

        // ---- 存子菜单句柄 ----
        let _ = app.open_recent_menu.set(recent_menu);
        let _ = app.theme_submenu.set(theme_menu);
        let _ = app.view_submenu.set(view_menu);
        let _ = app.search_submenu.set(search_menu);

        bar
    }
}

/// 顶层（带标题）菜单项。
fn top_level_item(mtm: MainThreadMarker, title: &str, sub: &NSMenu) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(&crate::util::ns_string(title));
    item.setSubmenu(Some(sub));
    item
}

/// 创建带 action/target/keyEquivalent 的菜单项。
unsafe fn make_item(
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    target: Option<&AnyObject>,
    key: &str,
    mods: NSEventModifierFlags,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::initWithTitle_action_keyEquivalent(
        NSMenuItem::alloc(mtm),
        &crate::util::ns_string(title),
        action,
        &crate::util::ns_string(key),
    );
    if let Some(t) = target {
        item.setTarget(Some(t));
    }
    if !mods.is_empty() {
        item.setKeyEquivalentModifierMask(mods);
    }
    item
}

/// 往菜单里加一项。
unsafe fn add(
    menu: &NSMenu,
    mtm: MainThreadMarker,
    title: &str,
    action: Option<Sel>,
    target: Option<&AnyObject>,
    key: &str,
    mods: NSEventModifierFlags,
) {
    menu.addItem(&make_item(mtm, title, action, target, key, mods));
}

// ---------------------------------------------------------------------------
// 勾选状态同步
// ---------------------------------------------------------------------------

pub fn sync_theme_checks(app: &mut App) {
    let Some(menu) = app.theme_submenu.get() else { return };
    let count = menu.numberOfItems();
    for i in 0..count {
        let Some(item) = menu.itemAtIndex(i) else { continue };
        let on = item.title().to_string() == app.theme_name;
        item.setState(if on {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }
}

pub fn sync_view_checks(app: &mut App) {
    let Some(menu) = app.view_submenu.get() else { return };
    set_check(menu, "显示行号", app.show_line_numbers);
    set_check(menu, "自动换行", app.word_wrap);
    set_check(menu, "显示空白", app.show_whitespace);
    set_check(menu, "级别着色", app.level_coloring);
    set_check(menu, "缩进参考线", app.show_indent_guides);
}

pub fn sync_search_checks(app: &mut App) {
    let Some(menu) = app.search_submenu.get() else { return };
    set_check(menu, "区分大小写", app.config.gui.case_sensitive);
    set_check(menu, "正则表达式", app.config.gui.use_regex);
    set_check(menu, "整词匹配", app.config.gui.whole_word);
}

fn set_check(menu: &NSMenu, title: &str, on: bool) {
    let count = menu.numberOfItems();
    for i in 0..count {
        let Some(item) = menu.itemAtIndex(i) else { continue };
        if item.title().to_string() == title {
            item.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Open Recent
// ---------------------------------------------------------------------------

/// 用配置里的最近文件重建 Open Recent 菜单（打开文件后调用）。
pub fn sync_recent_menu(app: &mut App) {
    let Some(menu) = app.open_recent_menu.get().cloned() else { return };
    rebuild_recent_items(app, &menu);
}

/// menuNeedsUpdate 回调：下拉时刷新。
pub fn maybe_refresh_recent(menu: &NSMenu) {
    with_app(|app| rebuild_recent_items(app, menu));
}

fn rebuild_recent_items(app: &mut App, menu: &NSMenu) {
    unsafe {
        menu.removeAllItems();
        // 剔除不存在的文件（可能被移动/删除）
        let recent: Vec<String> = app
            .config
            .recent_files()
            .into_iter()
            .filter(|p| std::path::Path::new(p).exists())
            .collect();
        if recent.is_empty() {
            let placeholder = NSMenuItem::new(app.mtm_safe());
            placeholder.setTitle(&crate::util::ns_string("（无）"));
            placeholder.setEnabled(false);
            menu.addItem(&placeholder);
            return;
        }
        let delegate = app.delegate.get();
        for path in recent {
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            let item = make_item(
                app.mtm_safe(),
                &name,
                Some(sel!(openRecent:)),
                delegate.map(|d| &**d as &AnyObject),
                "",
                MODS_EMPTY,
            );
            item.setRepresentedObject(Some(&crate::util::ns_string(&path)));
            menu.addItem(&item);
        }
    }
}
