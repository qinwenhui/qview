//! 原生统一工具栏：NSToolbar + NSToolbarDelegate（项在标题栏 Unified 工具栏里）。
//!
//! 不再手工摆按钮。搜索框 / 跳转输入框的句柄存入 AppDelegate ivars（见 app.rs），
//! 这样 NSToolbarDelegate 的同步回调（可能在 setup_ui 内 setToolbar 时触发，此时
//! 已持有 `&mut App`）**不会重入 with_app**。

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSImage, NSSearchField, NSTextField, NSToolbar, NSToolbarDisplayMode, NSToolbarItem,
    NSToolbarItemIdentifier,
};
use objc2_foundation::{NSArray, NSSize, NSString};

use crate::app::{App, AppDelegate};

pub const ID_OPEN: &str = "qlog.open";
pub const ID_RELOAD: &str = "qlog.reload";
pub const ID_CLOSE: &str = "qlog.close";
pub const ID_SEARCH: &str = "qlog.search";
pub const ID_PREV: &str = "qlog.prev";
pub const ID_NEXT: &str = "qlog.next";
pub const ID_GOTO: &str = "qlog.goto";
pub const ID_FONT_DEC: &str = "qlog.font_dec";
pub const ID_FONT_INC: &str = "qlog.font_inc";
pub const ID_STOP: &str = "qlog.stop";

/// 工具栏 autosave identifier（决定配置持久化命名空间）。
const TOOLBAR_ID: &str = "QLogMainToolbar";

/// 弹性间隔占位，构建标识列表时替换为 NSToolbarFlexibleSpaceItemIdentifier。
const FLEX: &str = "\u{0}flex";

/// 默认项顺序：左侧打开/重载/关闭，中间弹性间隔 + 搜索，右侧导航/字体。
const DEFAULT_IDS: &[&str] = &[
    ID_OPEN,
    ID_RELOAD,
    ID_CLOSE,
    FLEX,
    ID_SEARCH,
    FLEX,
    ID_PREV,
    ID_NEXT,
    ID_GOTO,
    ID_FONT_DEC,
    ID_FONT_INC,
];

/// 创建搜索框 + 跳转输入框（存 AppDelegate ivars）并返回已配好的 NSToolbar。
/// 由 setup_ui 调用后挂到窗口（`window.setToolbar`）。
pub fn create_toolbar(
    mtm: MainThreadMarker,
    delegate: &Retained<AppDelegate>,
    search_history: &[String],
) -> Retained<NSToolbar> {
    unsafe {
        // 搜索框（进 NSSearchToolbarItem）
        let search = NSSearchField::new(mtm);
        search.setTarget(Some(&**delegate as &AnyObject));
        search.setAction(Some(sel!(submitSearch:)));
        search.setSendsSearchStringImmediately(false);
        search.setSendsWholeSearchString(false);
        search.setPlaceholderString(Some(&crate::util::ns_string("搜索（Enter 提交，Esc 取消）")));
        // 搜索历史下拉：自动保存命名空间 + 上限，启动时用 config 里已有的历史填充
        search.setRecentsAutosaveName(Some(&crate::util::ns_string("QLogSearchRecents")));
        search.setMaximumRecents(20);
        if !search_history.is_empty() {
            let owned: Vec<Retained<NSString>> = search_history
                .iter()
                .map(|s| crate::util::ns_string(s))
                .collect();
            search.setRecentSearches(&NSArray::from_retained_slice(&owned));
        }
        *delegate.ivars().search_field.borrow_mut() = Some(search);

        // 跳转行号输入框（自定义 view 项）
        let goto = NSTextField::new(mtm);
        goto.setEditable(true);
        goto.setBezeled(true);
        goto.setDrawsBackground(true);
        goto.setTarget(Some(&**delegate as &AnyObject));
        goto.setAction(Some(sel!(gotoSubmit:)));
        goto.setPlaceholderString(Some(&crate::util::ns_string("行号")));
        *delegate.ivars().goto_field.borrow_mut() = Some(goto);

        let toolbar = NSToolbar::initWithIdentifier(
            NSToolbar::alloc(mtm),
            &crate::util::ns_string(TOOLBAR_ID),
        );
        toolbar.setDelegate(Some(ProtocolObject::from_ref(&**delegate)));
        toolbar.setAllowsUserCustomization(false);
        // 仅图标：去掉按钮文字，工具栏更紧凑。搜索框自带放大镜图标。
        toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
        toolbar
    }
}

/// NSToolbarDelegate 回调：按 identifier 创建工具栏项。
pub unsafe fn item_for_identifier(
    mtm: MainThreadMarker,
    id: &NSToolbarItemIdentifier,
    delegate: &AppDelegate,
) -> Retained<NSToolbarItem> {
    let id_str = id.to_string();
    match id_str.as_str() {
        ID_OPEN => make_image_item(mtm, id, "folder", "打开", sel!(openDocument:), delegate),
        ID_RELOAD => make_image_item(
            mtm,
            id,
            "arrow.clockwise",
            "重新加载",
            sel!(reloadDocument:),
            delegate,
        ),
        ID_CLOSE => make_image_item(mtm, id, "xmark", "关闭", sel!(closeDocument:), delegate),
        ID_PREV => make_image_item(
            mtm,
            id,
            "chevron.up",
            "查找上一个",
            sel!(findPrevious:),
            delegate,
        ),
        ID_NEXT => make_image_item(
            mtm,
            id,
            "chevron.down",
            "查找下一个",
            sel!(findNext:),
            delegate,
        ),
        ID_FONT_DEC => make_image_item(
            mtm,
            id,
            "textformat.size.smaller",
            "字体减小",
            sel!(fontSmaller:),
            delegate,
        ),
        ID_FONT_INC => make_image_item(
            mtm,
            id,
            "textformat.size.larger",
            "字体加大",
            sel!(fontBigger:),
            delegate,
        ),
        ID_STOP => make_image_item(
            mtm,
            id,
            "xmark.circle.fill",
            "停止",
            sel!(cancelTasks:),
            delegate,
        ),
        ID_SEARCH => make_search_item(mtm, id, delegate),
        ID_GOTO => make_goto_item(mtm, id, delegate),
        _ => NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), id),
    }
}

/// 默认项列表（供 toolbarDefaultItemIdentifiers:）。
pub fn default_item_identifiers() -> Retained<NSArray<NSToolbarItemIdentifier>> {
    identifiers_with_stop(false)
}

/// 允许项列表（默认项 + 停止项，供 toolbarAllowedItemIdentifiers:）。
pub fn allowed_item_identifiers() -> Retained<NSArray<NSToolbarItemIdentifier>> {
    identifiers_with_stop(true)
}

/// 显示/隐藏"停止"项（索引/搜索进行中）。通过重建 itemIdentifiers 实现。
pub fn set_stop_visible(app: &mut App, visible: bool) {
    let Some(toolbar) = app.toolbar.get() else { return };
    let ids = toolbar.itemIdentifiers().to_vec();
    let has = ids.iter().any(|s| s.to_string() == ID_STOP);
    if visible == has {
        return;
    }
    let mut new_ids = ids;
    if visible {
        new_ids.push(crate::util::ns_string(ID_STOP));
    } else {
        new_ids.retain(|s| s.to_string() != ID_STOP);
    }
    let refs: Vec<&NSString> = new_ids.iter().map(|s| &**s).collect();
    toolbar.setItemIdentifiers(&NSArray::from_slice(&refs));
}

// ---------------------------------------------------------------------------
// 项构造
// ---------------------------------------------------------------------------

fn make_image_item(
    mtm: MainThreadMarker,
    id: &NSToolbarItemIdentifier,
    symbol: &str,
    label: &str,
    action: Sel,
    delegate: &AppDelegate,
) -> Retained<NSToolbarItem> {
    let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), id);
    item.setLabel(&crate::util::ns_string(label));
    item.setToolTip(Some(&crate::util::ns_string(label)));
    // setTarget / setAction 是 unsafe 方法
    unsafe {
        item.setTarget(Some(&**delegate as &AnyObject));
        item.setAction(Some(action));
    }
    if let Some(img) =
        NSImage::imageWithSystemSymbolName_accessibilityDescription(&crate::util::ns_string(symbol), None)
    {
        item.setImage(Some(&img));
    }
    item
}

fn make_search_item(
    mtm: MainThreadMarker,
    id: &NSToolbarItemIdentifier,
    delegate: &AppDelegate,
) -> Retained<NSToolbarItem> {
    // 不用 NSSearchToolbarItem：它在 objc2 下搜索框宽度得不到布局（实测 frame 宽为 0），
    // 会渲染成一片空白。改用与跳转框相同的"自定义视图项"套路，min/maxSize 固定宽度。
    let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), id);
    item.setLabel(&crate::util::ns_string("搜索"));
    if let Some(field) = delegate.ivars().search_field.borrow().clone() {
        item.setView(Some(&field));
        item.setMinSize(NSSize::new(220.0, 24.0));
        item.setMaxSize(NSSize::new(280.0, 24.0));
    }
    item
}

#[allow(deprecated)] // 固定 70×24 尺寸由 min/maxSize 确定，避免系统按内容自动放宽
fn make_goto_item(
    mtm: MainThreadMarker,
    id: &NSToolbarItemIdentifier,
    delegate: &AppDelegate,
) -> Retained<NSToolbarItem> {
    let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), id);
    item.setLabel(&crate::util::ns_string("跳转到行"));
    if let Some(field) = delegate.ivars().goto_field.borrow().clone() {
        item.setView(Some(&field));
        item.setMinSize(NSSize::new(70.0, 24.0));
        item.setMaxSize(NSSize::new(70.0, 24.0));
    }
    item
}

/// 构建标识数组：默认项 + 可选停止项。
fn identifiers_with_stop(with_stop: bool) -> Retained<NSArray<NSToolbarItemIdentifier>> {
    // NSToolbarFlexibleSpaceItemIdentifier 是 extern 静态 NSString，其值就是
    // "NSToolbarFlexibleSpaceItemIdentifier"，用等值字符串即可（AppKit 按值比较）。
    let mut owned: Vec<Retained<NSString>> = DEFAULT_IDS
        .iter()
        .map(|s| {
            if *s == FLEX {
                crate::util::ns_string("NSToolbarFlexibleSpaceItemIdentifier")
            } else {
                crate::util::ns_string(s)
            }
        })
        .collect();
    if with_stop {
        owned.push(crate::util::ns_string(ID_STOP));
    }
    NSArray::from_retained_slice(&owned)
}
