//! IMKInputController 子类。IMK 为每个输入会话创建一个实例，按键从这里进来，
//! 具体处理都转给 [`crate::state`]。输入法菜单里 Kyme 自己的项也在这里。

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, sel};
use objc2_app_kit::{NSEvent, NSEventMask, NSMenu, NSMenuItem};
use objc2_foundation::ns_string;
use objc2_input_method_kit::IMKInputController;

use crate::{settings_window, state};

define_class!(
    // SAFETY: IMKInputController 没有额外的子类化要求，这个类也没有实现 Drop。
    #[unsafe(super(IMKInputController, NSObject))]
    #[name = "KymeInputController"]
    pub struct KymeInputController;

    impl KymeInputController {
        #[unsafe(method(handleEvent:client:))]
        fn handle_event(&self, event: Option<&NSEvent>, sender: Option<&AnyObject>) -> bool {
            match (event, sender) {
                (Some(event), Some(client)) => state::handle_event(event, client),
                _ => false,
            }
        }

        #[unsafe(method(recognizedEvents:))]
        fn recognized_events(&self, _sender: Option<&AnyObject>) -> usize {
            (NSEventMask::KeyDown | NSEventMask::FlagsChanged).bits() as usize
        }

        #[unsafe(method(activateServer:))]
        fn activate_server(&self, _sender: Option<&AnyObject>) {
            state::activate();
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, sender: Option<&AnyObject>) {
            if let Some(client) = sender {
                state::finish_composition(client);
            }
        }

        #[unsafe(method(commitComposition:))]
        fn commit_composition(&self, sender: Option<&AnyObject>) {
            if let Some(client) = sender {
                state::finish_composition(client);
            }
        }

        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            input_menu()
        }

        #[unsafe(method(showPreferences:))]
        fn show_preferences(&self, _sender: Option<&AnyObject>) {
            settings_window::open();
        }
    }

    unsafe impl NSObjectProtocol for KymeInputController {}
);

impl KymeInputController {
    /// IMKServer 按 Info.plist 里的类名找控制器，要先把这个类注册进运行时。
    pub fn register() {
        let _ = Self::class();
    }
}

/// 输入法菜单（菜单栏里输入法图标的下拉菜单）里 Kyme 自己的项。
fn input_menu() -> Option<Retained<NSMenu>> {
    let mtm = MainThreadMarker::new()?;
    let menu = NSMenu::new(mtm);
    // action 是 showPreferences: 的菜单项，用户选中时 IMK 会调控制器的 showPreferences:
    // SAFETY: 标题和快捷键都是 NSString，showPreferences: 是 IMKInputController 的方法
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("Kyme 设置…"),
            Some(sel!(showPreferences:)),
            ns_string!(""),
        )
    };
    menu.addItem(&item);
    Some(menu)
}
