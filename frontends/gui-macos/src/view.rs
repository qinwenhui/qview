//! 日志视图：flipped NSView，虚拟滚动渲染（drawRect 只画可见行）。
//!
//! 作为 NSScrollView 的 documentView，尺寸 = 内容高 × 内容宽。绘制逻辑在
//! `App::render_log_view`（见 app.rs），这里负责交互：
//! - 首响应者（Cmd+C/Cmd+A/copy:/selectAll: 路由到这里）
//! - 悬停高亮（mouseMoved / mouseExited，跟踪区用 InVisibleRect）
//! - 点击/拖选文本（mouseDown / mouseDragged / rightMouseDown），Shift 延展选区
//! - 右键菜单（复制 / 全选）、Esc 清选区

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSEvent, NSEventModifierFlags, NSMenu, NSMenuItem, NSTrackingArea, NSTrackingAreaOptions,
    NSView,
};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::selection::TextPoint;

pub struct LogViewIvars {
    tracking_area: RefCell<Option<Retained<NSTrackingArea>>>,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[name = "QLogLogView"]
    #[ivars = LogViewIvars]
    pub struct LogView;

    unsafe impl NSObjectProtocol for LogView {}

    impl LogView {
        /// flipped：与 AppKit 的滚动坐标一致（原点左上、y 向下）。
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// 不透明：由 drawRect 填充整块背景。
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        /// 接收键盘输入（Cmd+C/Cmd+A 才能路由到这里）。
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            crate::app::with_app(|app| app.render_log_view(dirty));
        }

        /// 更新跟踪区：跟随可见矩形，只在悬停/移出时收到事件。
        #[unsafe(method(updateTrackingAreas))]
        fn update_tracking_areas(&self) {
            if let Some(ta) = self.ivars().tracking_area.borrow_mut().take() {
                self.removeTrackingArea(&ta);
            }
            let options = NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveInKeyWindow
                | NSTrackingAreaOptions::InVisibleRect;
            let ta = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    NSTrackingArea::alloc(),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1.0, 1.0)),
                    options,
                    Some(self as &AnyObject),
                    None,
                )
            };
            self.addTrackingArea(&ta);
            *self.ivars().tracking_area.borrow_mut() = Some(ta);
            unsafe { msg_send![super(self), updateTrackingAreas] }
        }

        /// 悬停 → 高亮所在行。
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            let lp = self.local_point(event);
            crate::app::with_app(|app| {
                let line = app.line_at_y(lp.y);
                if line != app.hover_line {
                    app.hover_line = line;
                    if let Some(lv) = app.log_view.get() {
                        lv.setNeedsDisplay(true);
                    }
                }
            });
        }

        /// 移出 → 清除悬停高亮。
        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            crate::app::with_app(|app| {
                if app.hover_line.is_some() {
                    app.hover_line = None;
                    if let Some(lv) = app.log_view.get() {
                        lv.setNeedsDisplay(true);
                    }
                }
            });
        }

        /// 按下 → 定位 current_line、成为首响应者；Shift 延展选区，否则重设起点。
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let lp = self.local_point(event);
            self.window().map(|w| w.makeFirstResponder(Some(self)));
            crate::app::with_app(|app| {
                let shift = event.modifierFlags().contains(NSEventModifierFlags::Shift);
                if let Some(pt) = app.hit_test(lp) {
                    if shift && app.selection.active {
                        // Shift 点击：保持 anchor，focus 移到新位置
                        app.selection.focus = pt;
                        app.selection.active = true;
                    } else {
                        // 新选区起点（anchor == focus → 点击不产生可见选区）
                        app.selection.anchor = pt;
                        app.selection.focus = pt;
                        app.selection.active = true;
                    }
                    if app.current_line != pt.line {
                        app.current_line = pt.line;
                    }
                    if let Some(lv) = app.log_view.get() {
                        lv.setNeedsDisplay(true);
                    }
                }
            });
        }

        /// 拖动 → 更新选区 focus；边缘自动滚动。
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let lp = self.local_point(event);
            crate::app::with_app(|app| {
                if let Some(pt) = app.hit_test(lp) {
                    app.selection.focus = pt;
                    app.selection.active = true;
                    if app.current_line != pt.line {
                        app.current_line = pt.line;
                    }
                    if let Some(lv) = app.log_view.get() {
                        lv.setNeedsDisplay(true);
                    }
                }
            });
            self.autoscroll(event);
        }

        /// 抬起 → 选区保留。
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {}

        /// 右键 → 选中光标处的词，并弹出 复制/全选 菜单。
        ///
        /// 菜单弹出会跑嵌套事件循环，故在 `with_app` 闭包之外进行。
        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            let lp = self.local_point(event);
            self.window().map(|w| w.makeFirstResponder(Some(self)));
            crate::app::with_app(|app| {
                if let Some(pt) = app.hit_test(lp) {
                    if let Some((s, e)) = app.word_at(pt) {
                        app.selection.anchor = TextPoint { line: pt.line, byte: s };
                        app.selection.focus = TextPoint { line: pt.line, byte: e };
                        app.selection.active = true;
                        app.current_line = pt.line;
                    } else {
                        app.selection.active = false;
                    }
                    if let Some(lv) = app.log_view.get() {
                        lv.setNeedsDisplay(true);
                    }
                }
            });
            let mtm = MainThreadMarker::new().expect("rightMouseDown runs on main thread");
            let menu = Self::build_context_menu(mtm);
            let _ = menu.popUpMenuPositioningItem_atLocation_inView(None, lp, Some(self));
        }

        /// 复制：有选区复制选区，否则复制当前行。
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

        /// 全选（并复制）。
        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.select_all_lines());
        }

        /// Esc → 清选区。
        #[unsafe(method(cancelOperation:))]
        fn cancel_operation(&self, _sender: Option<&AnyObject>) {
            crate::app::with_app(|app| app.clear_selection());
        }
    }
);

impl LogView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(LogViewIvars { tracking_area: RefCell::new(None) });
        unsafe { msg_send![super(this), init] }
    }

    /// 把事件窗口坐标转成 LogView 本地坐标（已含滚动偏移）。
    fn local_point(&self, event: &NSEvent) -> NSPoint {
        let p = event.locationInWindow();
        self.convertPoint_fromView(p, None)
    }

    /// 右键菜单：复制 / 全选。target 为 nil → 走响应链（LogView 已是 first responder）。
    fn build_context_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
        unsafe {
            let menu = NSMenu::new(mtm);
            let copy_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &crate::util::ns_string("复制"),
                Some(sel!(copy:)),
                &crate::util::ns_string("c"),
            );
            copy_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            menu.addItem(&copy_item);
            let select_all_item = NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &crate::util::ns_string("全选"),
                Some(sel!(selectAll:)),
                &crate::util::ns_string("a"),
            );
            select_all_item.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            menu.addItem(&select_all_item);
            menu
        }
    }
}
