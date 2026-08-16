//! 主窗口根视图：flipped NSView，作为窗口 contentView 并承载滚动区 / 工具栏 / 状态栏。
//! 同时注册为文件拖放目标（把 .log 拖到窗口即可打开）。

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSDraggingInfo, NSDragOperation, NSPasteboard, NSPasteboardTypeFileURL, NSView,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObjectProtocol, NSString, NSURL};

use crate::util::ns_string;

pub struct RootViewIvars {}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "QLogRootView"]
    #[ivars = RootViewIvars]
    pub struct RootView;

    unsafe impl NSObjectProtocol for RootView {}

    impl RootView {
        /// flipped：原点在左上、y 向下，子视图布局简单。
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// 拖拽进入：仅当拖的是文件时显示可放置光标。
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            let pb = sender.draggingPasteboard();
            let has_file = unsafe {
                pb.types()
                    .map_or(false, |types| types.containsObject(&NSPasteboardTypeFileURL))
            };
            if has_file {
                NSDragOperation::Copy
            } else {
                NSDragOperation::None
            }
        }

        /// 拖拽放置：读取拖入的文件路径并打开。
        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let pb = sender.draggingPasteboard();
            if let Some(p) = read_dragged_path(&pb) {
                // 遵守重入不变量：open_path 只换 bridge 不弹窗，错误在 with_app 外展示
                let result = crate::app::with_app(|app| app.open_path(PathBuf::from(p)));
                if let Err(e) = result {
                    crate::dialogs::show_error(&e);
                }
                true
            } else {
                false
            }
        }
    }
);

impl RootView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RootViewIvars {});
        unsafe {
            let this: Retained<Self> = msg_send![super(this), init];
            // 注册为文件拖放目标
            let types = NSArray::arrayWithObject(&*NSPasteboardTypeFileURL);
            this.registerForDraggedTypes(&types);
            this
        }
    }
}

/// 从拖放 pasteboard 读取第一个文件路径。
///
/// 现代类型 `NSPasteboardTypeFileURL` 的 property list 是 NSString 数组（file URL
/// 字符串），把它解析成文件系统路径；再兜底尝试 `stringForType`。
fn read_dragged_path(pb: &NSPasteboard) -> Option<String> {
    unsafe {
        if let Some(pl) = pb.propertyListForType(&NSPasteboardTypeFileURL) {
            // NSArray 泛型参数必须是 AnyObject 才能 downcast（见 DowncastTarget 文档）
            if let Some(arr) = pl.downcast_ref::<NSArray>() {
                for i in 0..arr.count() {
                    let obj = arr.objectAtIndex(i);
                    if let Some(s) = obj.downcast_ref::<NSString>() {
                        if let Some(p) = url_string_to_path(&s.to_string()) {
                            return Some(p);
                        }
                    }
                }
            }
        }
        if let Some(s) = pb.stringForType(&NSPasteboardTypeFileURL) {
            if let Some(p) = url_string_to_path(&s.to_string()) {
                return Some(p);
            }
        }
        None
    }
}

/// 把 file URL 字符串（file:///…）解析为文件系统路径。
fn url_string_to_path(s: &str) -> Option<String> {
    let url = NSURL::URLWithString(&ns_string(s))?;
    let path = url.path()?;
    Some(path.to_string())
}
