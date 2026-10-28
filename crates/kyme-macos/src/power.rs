//! 系统睡眠、唤醒、锁屏这类事件，只写诊断日志，用来查「熄屏唤醒后打不了字」。

use std::ptr::NonNull;

use block2::RcBlock;
use core_foundation::base::{CFType, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceScreensDidSleepNotification,
    NSWorkspaceScreensDidWakeNotification, NSWorkspaceSessionDidBecomeActiveNotification,
    NSWorkspaceSessionDidResignActiveNotification, NSWorkspaceWillSleepNotification,
};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSString, ns_string};

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

/// 安全输入开着时（密码框、1Password 这类会开），系统不把按键交给任何输入法。开着的话顺便记下是谁开的。
pub fn secure_input() -> String {
    // SAFETY: 没有参数的 HIToolbox 查询
    if unsafe { IsSecureEventInputEnabled() } == 0 {
        return "安全输入关".into();
    }
    match secure_input_owner() {
        Some(owner) => format!("安全输入开（{owner}）"),
        None => "安全输入开".into(),
    }
}

/// 开着安全输入的进程：会话信息里的 kCGSSessionSecureInputPID，能换成 App 的 bundle ID 就换。
fn secure_input_owner() -> Option<String> {
    // SAFETY: 按 Copy 规则返回，可能是 NULL；不是 NULL 就交给 CFDictionary 释放
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    if session.is_null() {
        return None;
    }
    let session: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_create_rule(session) };
    let pid =
        session.find(CFString::from_static_string("kCGSSessionSecureInputPID"))?.downcast::<CFNumber>()?.to_i32()?;
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid).and_then(|app| app.bundleIdentifier());
    Some(app.map_or_else(|| format!("pid {pid}"), |app| format!("{app}，pid {pid}")))
}

/// 开始记录。观察者要活到进程结束，所以不释放。
pub fn watch() {
    let workspace = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: 这些是 AppKit 导出的通知名常量
    let events = unsafe {
        [
            (NSWorkspaceWillSleepNotification, "将要睡眠"),
            (NSWorkspaceDidWakeNotification, "唤醒"),
            (NSWorkspaceScreensDidSleepNotification, "熄屏"),
            (NSWorkspaceScreensDidWakeNotification, "亮屏"),
            (NSWorkspaceSessionDidResignActiveNotification, "会话切走"),
            (NSWorkspaceSessionDidBecomeActiveNotification, "会话切回"),
        ]
    };
    for (name, label) in events {
        observe(&workspace, name, label);
    }
    // 锁屏、解锁只有分布式通知
    let distributed = NSDistributedNotificationCenter::defaultCenter();
    observe(&distributed, ns_string!("com.apple.screenIsLocked"), "锁屏");
    observe(&distributed, ns_string!("com.apple.screenIsUnlocked"), "解锁");
}

fn observe(center: &NSNotificationCenter, name: &NSString, label: &'static str) {
    let block = RcBlock::new(move |_: NonNull<NSNotification>| log::info!("{label}，{}", secure_input()));
    // SAFETY: 没有指定发送者和队列，通知在发出它的线程上送达；块只写日志
    let observer = unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) };
    std::mem::forget(observer);
}
