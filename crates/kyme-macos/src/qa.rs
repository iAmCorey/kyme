//! `Kyme --qa`：自测模式，给 `cargo xtask qa` 用。
//!
//! 启动真正的输入会话、macOS 接入代码和候选窗，但不连系统的输入法服务（IMK），而是换上一个假的输入框：
//! 从标准输入一行行读按键（JSON），合成 NSEvent 交给 [`state::handle_event`]，再把这个键接没接、
//! 假输入框里上屏和正在输入的文字、候选窗的位置写回标准输出。不切输入法，不抢焦点，不写日志。
//!
//! 为什么不在真的 App 里模拟打字：程序合成的字母键经系统转给输入法时，输入法收到的是控制字符，
//! 认不出是哪个字母（真实键盘没有这个问题）。各个 App 里的表现交给人工检查清单。

use std::cell::{Cell, OnceCell, RefCell};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Instant;

use dispatch2::DispatchQueue;
use gpui::Application;
use kyme_core::SettingsFile;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType, NSScreen};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize, NSString};
use serde::{Deserialize, Serialize};

use crate::state;

/// xtask 发来的指令，和 xtask/src/macos.rs 里的一致。
#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Request {
    /// 合成一个按键事件交给输入会话，参数的含义见 xtask/src/scenario.rs 的 Press。
    Key { flags_changed: bool, code: u16, chars: String, shift: bool, command: bool },
    /// 清空假输入框。
    Clear,
    /// 只读状态。
    State,
}

/// 每条指令之后的状态，和 xtask/src/macos.rs 里的一致。坐标是屏幕坐标，原点在主屏左上角。
#[derive(Serialize)]
struct Reply {
    /// 输入法接了这个键（没接的话，App 会自己处理它）。
    handled: bool,
    /// 上屏的文字，从上次清空起依次接起来。
    inserted: String,
    /// 正在输入的文字（marked text）。
    marked: String,
    caret: [f64; 4],
    /// 候选窗的位置，没显示时是 None。
    panel: Option<[f64; 4]>,
    /// 处理这个键花了多久（微秒）。
    elapsed_us: u64,
}

struct ClientIvars {
    inserted: RefCell<String>,
    marked: RefCell<String>,
    caret: Cell<NSRect>,
}

define_class!(
    // SAFETY: NSObject 没有子类化要求，这个类也没有实现 Drop。
    /// 假输入框：实现输入会话会调用的那几个 IMKTextInput 方法，记下输入法往里写了什么。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "KymeQAClient"]
    #[ivars = ClientIvars]
    struct FakeClient;

    impl FakeClient {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text(&self, text: &NSString, _replacement: NSRange) {
            self.ivars().inserted.borrow_mut().push_str(&text.to_string());
            self.ivars().marked.borrow_mut().clear();
        }

        #[unsafe(method(setMarkedText:selectionRange:replacementRange:))]
        fn set_marked_text(&self, text: &NSString, _selection: NSRange, _replacement: NSRange) {
            *self.ivars().marked.borrow_mut() = text.to_string();
        }

        #[unsafe(method(attributesForCharacterIndex:lineHeightRectangle:))]
        fn attributes(&self, _index: usize, rect: *mut NSRect) -> *mut AnyObject {
            // SAFETY: 调用方传来的是一个可写的 NSRect
            unsafe { *rect = self.ivars().caret.get() };
            std::ptr::null_mut()
        }
    }

    unsafe impl NSObjectProtocol for FakeClient {}
);

impl FakeClient {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // 光标放在主屏中间，候选窗贴在它下面，不会碰到屏幕边缘
        let area = NSScreen::mainScreen(mtm)
            .map_or(NSRect::new(NSPoint::ZERO, NSSize::new(1440.0, 900.0)), |screen| screen.visibleFrame());
        let caret = NSRect::new(
            NSPoint::new(area.origin.x + area.size.width / 2.0 - 150.0, area.origin.y + area.size.height / 2.0),
            NSSize::new(2.0, 20.0),
        );
        let ivars = ClientIvars { inserted: RefCell::default(), marked: RefCell::default(), caret: Cell::new(caret) };
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: NSObject 的指定初始化方法
        unsafe { msg_send![super(this), init] }
    }
}

thread_local! {
    static CLIENT: OnceCell<Retained<FakeClient>> = const { OnceCell::new() };
}

/// 启动自测模式，直到标准输入关闭。
pub fn run() {
    // 用临时的设置文件：结果不能依赖你自己的设置
    let settings = std::env::temp_dir().join(format!("kyme-qa-{}.toml", std::process::id()));
    let loading = crate::load_engine_in_background();
    Application::new().run(move |cx| {
        crate::boot(cx, loading, SettingsFile::new(settings.clone()));
        thread::spawn(move || serve(settings));
    });
}

/// 在后台线程里读指令，每条派到主线程上执行，主线程照常跑事件循环（候选窗、中英提示的计时都靠它）。
fn serve(settings: PathBuf) {
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines().map_while(Result::ok) {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(request) => on_main(move |mtm| execute(mtm, request)),
            Err(err) => Err(format!("看不懂的指令 {line}：{err}")),
        };
        let line = serde_json::to_string(&reply).unwrap_or_default();
        if writeln!(stdout, "{line}").and_then(|()| stdout.flush()).is_err() {
            break;
        }
    }
    let _ = std::fs::remove_file(settings);
    std::process::exit(0);
}

/// 在主线程上执行 `work`，等它做完。
fn on_main<R: Send>(work: impl FnOnce(MainThreadMarker) -> R + Send) -> R {
    let mut result = None;
    DispatchQueue::main().exec_sync(|| result = Some(work(MainThreadMarker::new().expect("主队列在主线程上"))));
    result.expect("exec_sync 会等 work 做完")
}

fn execute(mtm: MainThreadMarker, request: Request) -> Result<Reply, String> {
    CLIENT.with(|client| {
        let client = client.get_or_init(|| FakeClient::new(mtm));
        let ivars = client.ivars();
        let mut handled = false;
        let mut elapsed_us = 0;
        match request {
            Request::Key { flags_changed, code, chars, shift, command } => {
                let event = key_event(flags_changed, code, &chars, shift, command).ok_or("合成按键事件失败")?;
                let started = Instant::now();
                handled = state::handle_event(&event, client);
                elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            }
            Request::Clear => {
                ivars.inserted.borrow_mut().clear();
                ivars.marked.borrow_mut().clear();
            }
            Request::State => {}
        }
        Ok(Reply {
            handled,
            inserted: ivars.inserted.borrow().clone(),
            marked: ivars.marked.borrow().clone(),
            caret: top_left(ivars.caret.get(), mtm),
            panel: state::panel_frame().map(|frame| top_left(frame, mtm)),
            elapsed_us,
        })
    })
}

fn key_event(flags_changed: bool, code: u16, chars: &str, shift: bool, command: bool) -> Option<Retained<NSEvent>> {
    let mut flags = NSEventModifierFlags::empty();
    if shift {
        flags |= NSEventModifierFlags::Shift;
    }
    if command {
        flags |= NSEventModifierFlags::Command;
    }
    let kind = if flags_changed { NSEventType::FlagsChanged } else { NSEventType::KeyDown };
    let chars = NSString::from_str(chars);
    // 只用到 Shift 和 Command：「不算修饰键的字符」除了 Command 都和 characters 一样，这里不需要区分
    NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind,
        NSPoint::ZERO,
        flags,
        0.0,
        0,
        None,
        &chars,
        &chars,
        false,
        code,
    )
}

/// AppKit 的屏幕坐标（原点在主屏左下角）换成原点在主屏左上角的。
fn top_left(rect: NSRect, mtm: MainThreadMarker) -> [f64; 4] {
    let height = NSScreen::screens(mtm).firstObject().map_or(0.0, |screen| screen.frame().size.height);
    [rect.origin.x, height - rect.origin.y - rect.size.height, rect.size.width, rect.size.height]
}
