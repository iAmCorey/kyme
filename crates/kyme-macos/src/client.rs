//! 对 IMK 客户端（正在输入的那个 App）的调用。

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, sel};
use objc2_foundation::{NSNotFound, NSRange, NSRect, NSString};

/// 客户端 App 的 bundle ID，写诊断日志用。拿不到时是「未知 App」。
pub fn app_name(client: Option<&AnyObject>) -> String {
    let Some(client) = client else { return "未知 App".into() };
    // SAFETY: respondsToSelector: 是 NSObject 协议的方法；IMKTextInput 的 bundleIdentifier 返回 NSString 或 nil
    let id: Option<Retained<NSString>> = unsafe {
        let responds: bool = msg_send![client, respondsToSelector: sel!(bundleIdentifier)];
        if responds { msg_send![client, bundleIdentifier] } else { None }
    };
    id.map_or_else(|| "未知 App".into(), |id| id.to_string())
}

fn no_replacement() -> NSRange {
    NSRange::new(NSNotFound as usize, 0)
}

/// 上屏。会顺带替换掉当前的 marked text。
pub fn insert_text(client: &AnyObject, text: &str) {
    let text = NSString::from_str(text);
    // SAFETY: IMKTextInput 协议的方法，参数类型与声明一致
    unsafe {
        let _: () = msg_send![client, insertText: &*text, replacementRange: no_replacement()];
    }
}

/// 显示输入中的拼音（带下划线的那段）。传空串表示结束输入。
pub fn set_marked_text(client: &AnyObject, text: &str) {
    let text = NSString::from_str(text);
    let cursor = NSRange::new(text.length(), 0);
    // SAFETY: 同上
    unsafe {
        let _: () = msg_send![
            client,
            setMarkedText: &*text,
            selectionRange: cursor,
            replacementRange: no_replacement()
        ];
    }
}

/// 输入位置所在行的矩形，屏幕坐标（原点在左下）。有的 App 会返回全零。
pub fn caret_rect(client: &AnyObject) -> NSRect {
    let mut rect = NSRect::ZERO;
    // SAFETY: 同上；返回的属性字典不需要
    unsafe {
        let _: Option<Retained<AnyObject>> = msg_send![
            client,
            attributesForCharacterIndex: 0usize,
            lineHeightRectangle: &mut rect
        ];
    }
    rect
}
