//! 底部状态栏：左（文件名）+ 中（匹配/行数/瞬态提示）+ 进度条 + 取消按钮 + 右（大小/行数/编码）。
//! frame 由 App::layout_controls 布置。

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{sel, MainThreadMarker};
use objc2_app_kit::{
    NSBezelStyle, NSButton, NSImage, NSProgressIndicator, NSProgressIndicatorStyle,
    NSTextField,
};

use crate::app::{App, AppDelegate};
use crate::window::RootView;

pub fn create_statusbar(
    app: &mut App,
    mtm: MainThreadMarker,
    root: &RootView,
    delegate: &Retained<AppDelegate>,
) {
    let sl = make_label(mtm, "未打开文件");
    root.addSubview(&sl);
    let sm = make_label(mtm, "");
    root.addSubview(&sm);
    let sr = make_label(mtm, "");
    root.addSubview(&sr);

    let pr = NSProgressIndicator::new(mtm);
    pr.setStyle(NSProgressIndicatorStyle::Bar);
    pr.setIndeterminate(true);
    pr.setHidden(true);
    root.addSubview(&pr);

    // 取消按钮：索引/搜索进行中显示，点击取消后台任务（cancelTasks:）。
    let cancel = NSButton::new(mtm);
    unsafe {
        cancel.setTarget(Some(&**delegate as &AnyObject));
        cancel.setAction(Some(sel!(cancelTasks:)));
    }
    cancel.setBezelStyle(NSBezelStyle::Toolbar);
    cancel.setBordered(false);
    cancel.setTitle(&crate::util::ns_string("停止"));
    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &crate::util::ns_string("stop.circle"),
        None,
    ) {
        cancel.setImage(Some(&img));
    }
    cancel.setHidden(true);
    root.addSubview(&cancel);

    let _ = app.status_left.set(sl);
    let _ = app.status_mid.set(sm);
    let _ = app.status_right.set(sr);
    let _ = app.progress.set(pr);
    let _ = app.btn_cancel.set(cancel);
}

fn make_label(mtm: MainThreadMarker, text: &str) -> Retained<NSTextField> {
    let f = NSTextField::new(mtm);
    f.setStringValue(&crate::util::ns_string(text));
    f.setEditable(false);
    f.setSelectable(false);
    f.setBezeled(false);
    f.setDrawsBackground(false);
    f
}
