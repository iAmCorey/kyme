//! 候选窗所在的 GPUI 窗口：创建、测量、贴到光标旁、显示与隐藏。
//!
//! GPUI 没有公开「移动窗口」的 API，这里通过 raw window handle 拿到 NSWindow 自己摆放。

use gpui::{
    App, AppContext, AsyncApp, Bounds, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind,
    WindowOptions, point, px, size,
};
use kyme_core::AppearanceSettings;
use kyme_ui::{CandidateView, Content};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSEvent, NSScreen, NSView, NSWindow, NSWindowStyleMask};
use objc2_core_graphics::{CGWindowLevelForKey, CGWindowLevelKey};
use objc2_foundation::{NSPoint, NSPointInRect, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 候选窗和输入行之间的间距。
const GAP: f64 = 4.0;
/// 设置里没指定字体时用的中文字体。
const DEFAULT_FONT: &str = "PingFang SC";

pub struct PanelWindow {
    handle: WindowHandle<CandidateView>,
    window: Retained<NSWindow>,
}

impl PanelWindow {
    pub fn open(cx: &mut App) -> Self {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(0.0), px(0.0)),
                size(px(300.0), px(66.0)),
            ))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        };
        let handle =
            cx.open_window(options, |_, cx| cx.new(|_| CandidateView::new(DEFAULT_FONT))).expect("创建候选窗失败");
        let window =
            handle.update(cx, |_, window, _| ns_window(window)).ok().flatten().expect("拿不到候选窗的 NSWindow");

        // GPUI 的 PopUp 是带标题栏样式的面板，改成无边框；保留「不抢焦点」
        window.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
        // 层级放到光标层下面一点，盖得住 Spotlight 这类浮窗
        let level = CGWindowLevelForKey(CGWindowLevelKey::CursorWindowLevelKey) - 100;
        window.setLevel(level as isize);
        window.setHasShadow(true);
        // demo 先只支持键盘选词，点击直接穿透到下面的 App
        window.setIgnoresMouseEvents(true);
        PanelWindow { handle, window }
    }

    pub fn show(&mut self, app: &AsyncApp, content: Content, appearance: &AppearanceSettings, caret: NSRect) {
        let handle = self.handle;
        let appearance = appearance.clone();
        let measured = app.update(|cx| {
            handle.update(cx, |view, window, cx| {
                view.set(content, appearance);
                cx.notify();
                view.measure(window)
            })
        });
        let Ok(Ok(size)) = measured else {
            log::warn!("更新候选窗失败");
            return;
        };
        let size = NSSize::new(size.width.to_f64(), size.height.to_f64());
        let frame = place(caret, size, self.window.mtm());
        let current = self.window.frame();
        // 位置和大小都没变就不动窗口，免得 WindowServer 和 GPUI 白白重排一次
        if current != frame {
            self.window.setFrame_display(frame, false);
            if current.size != frame.size {
                self.window.invalidateShadow();
            }
        }
        if !self.window.isVisible() {
            self.window.orderFront(None);
        }
    }

    pub fn visible_frame(&self) -> Option<NSRect> {
        self.window.isVisible().then(|| self.window.frame())
    }

    pub fn hide(&mut self) {
        if self.window.isVisible() {
            self.window.orderOut(None);
        }
    }
}

fn ns_window(window: &gpui::Window) -> Option<Retained<NSWindow>> {
    // gpui::Window 自己也有个 window_handle()，这里要的是 raw-window-handle 的
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI 给出的 ns_view 指向一个活着的 NSView，并且这里在主线程
    let view: &NSView = unsafe { appkit.ns_view.cast().as_ref() };
    view.window()
}

/// 放在输入行下方；下方放不下就放上方，左右不超出屏幕。拿不到光标位置时跟着鼠标。
fn place(caret: NSRect, size: NSSize, mtm: MainThreadMarker) -> NSRect {
    let caret = if caret == NSRect::ZERO { NSRect::new(NSEvent::mouseLocation(), NSSize::ZERO) } else { caret };
    let screen = NSScreen::screens(mtm)
        .iter()
        .find(|screen| NSPointInRect(caret.origin, screen.frame()))
        .or_else(|| NSScreen::mainScreen(mtm));
    let Some(area) = screen.map(|screen| screen.visibleFrame()) else {
        return NSRect::new(caret.origin, size);
    };

    let mut y = caret.origin.y - size.height - GAP;
    if y < area.min().y {
        y = caret.max().y + GAP;
    }
    let x = caret.origin.x.min(area.max().x - size.width).max(area.min().x);
    NSRect::new(NSPoint::new(x, y), size)
}
