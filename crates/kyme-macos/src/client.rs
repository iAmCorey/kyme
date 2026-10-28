//! 对 IMK 客户端（正在输入的那个 App）的调用。

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::{NSAttributedString, NSNotFound, NSRange, NSRect, NSString};

/// 客户端 App 的 bundle ID，写诊断日志用。拿不到时是「未知 App」。
pub fn app_name(client: Option<&AnyObject>) -> String {
    let Some(client) = client else { return "未知 App".into() };
    // SAFETY: IMKTextInput 的 bundleIdentifier 返回 NSString 或 nil，先确认 App 支持再调
    let id: Option<Retained<NSString>> =
        if responds(client, sel!(bundleIdentifier)) { unsafe { msg_send![client, bundleIdentifier] } } else { None };
    id.map_or_else(|| "未知 App".into(), |id| id.to_string())
}

/// App 实现了这个方法没有：IMKTextInput 的方法不是每个 App 都有。
fn responds(client: &AnyObject, selector: Sel) -> bool {
    // SAFETY: respondsToSelector: 是 NSObject 协议的方法
    unsafe { msg_send![client, respondsToSelector: selector] }
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

/// 光标前最多 `limit` 个字（按 UTF-16 算）的文字，AI 组句当前文用。正在组字时从组字那一段前面算起。
/// App 不支持、读不到时是 None。读到的内容只交给 AI，不写日志。
pub fn text_before_cursor(client: &AnyObject, limit: usize) -> Option<String> {
    let not_found = NSNotFound as usize;
    if !responds(client, sel!(selectedRange)) || !responds(client, sel!(attributedSubstringFromRange:)) {
        return None;
    }
    // SAFETY: 都是 IMKTextInput 协议的方法，上面确认过 App 支持，参数和返回值的类型与声明一致
    unsafe {
        let marked: NSRange =
            if responds(client, sel!(markedRange)) { msg_send![client, markedRange] } else { no_replacement() };
        let selected: NSRange = msg_send![client, selectedRange];
        let cursor =
            if marked.location != not_found && marked.length > 0 { marked.location } else { selected.location };
        if cursor == not_found || cursor == 0 {
            return None;
        }
        let start = cursor.saturating_sub(limit);
        let text: Option<Retained<NSAttributedString>> =
            msg_send![client, attributedSubstringFromRange: NSRange::new(start, cursor - start)];
        text.map(|text| text.string().to_string())
    }
}
