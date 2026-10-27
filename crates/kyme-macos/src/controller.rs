//! IMKInputController 子类。IMK 为每个输入会话创建一个实例，按键从这里进来，
//! 具体处理都转给 [`crate::state`]。

use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{ClassType, define_class};
use objc2_app_kit::{NSEvent, NSEventMask};
use objc2_input_method_kit::IMKInputController;

use crate::state;

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
    }

    unsafe impl NSObjectProtocol for KymeInputController {}
);

impl KymeInputController {
    /// IMKServer 按 Info.plist 里的类名找控制器，要先把这个类注册进运行时。
    pub fn register() {
        let _ = Self::class();
    }
}
