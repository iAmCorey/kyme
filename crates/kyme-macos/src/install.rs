//! `Kyme --install [--select]`：向系统注册输入法并启用，可选地直接切换过去。
//! 用的是 Text Input Sources（HIToolbox）的公开 API，和在系统设置里手动添加输入法等效。

use std::ffi::c_void;
use std::thread;
use std::time::Duration;

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::{CFBoolean, CFBooleanRef};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::string::{CFString, CFStringRef};
use core_foundation::url::{CFURL, CFURLRef};
use objc2_foundation::NSBundle;

type InputSourceRef = *const c_void;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISRegisterInputSource(location: CFURLRef) -> i32;
    fn TISCreateInputSourceList(properties: CFDictionaryRef, include_all_installed: u8) -> CFArrayRef;
    fn TISEnableInputSource(source: InputSourceRef) -> i32;
    fn TISSelectInputSource(source: InputSourceRef) -> i32;
    fn TISGetInputSourceProperty(source: InputSourceRef, key: CFStringRef) -> *const c_void;
    fn TISCopyCurrentKeyboardInputSource() -> InputSourceRef;
    static kTISPropertyBundleID: CFStringRef;
    static kTISPropertyInputSourceID: CFStringRef;
    static kTISPropertyInputSourceIsEnabled: CFStringRef;
}

/// 返回进程退出码。
pub fn install(select: bool) -> i32 {
    let bundle = NSBundle::mainBundle();
    let path = bundle.bundlePath().to_string();
    let Some(bundle_id) = bundle.bundleIdentifier().map(|id| id.to_string()) else {
        eprintln!("须在 Kyme.app 中运行：{path}");
        return 1;
    };
    let Some(url) = CFURL::from_path(&path, true) else {
        eprintln!("路径无效：{path}");
        return 1;
    };
    // SAFETY: url 是有效的 CFURL
    let status = unsafe { TISRegisterInputSource(url.as_concrete_TypeRef()) };
    println!("注册 {path}：{}", describe(status));

    let Some(modes) = retry(|| enable(&bundle_id)) else {
        eprintln!("系统中未找到 {bundle_id} 的输入模式，请注销并重新登录后重试");
        return 1;
    };
    match modes.first() {
        Some(mode) if select => select_source(mode),
        _ => 0,
    }
}

/// `Kyme --current-source`：打印当前输入源的 ID。
pub fn print_current() -> i32 {
    // SAFETY: 返回值按 Copy 规则持有，交给 CFType 释放
    let source = unsafe { TISCopyCurrentKeyboardInputSource() };
    if source.is_null() {
        return 1;
    }
    let owned: CFType = unsafe { CFType::wrap_under_create_rule(source) };
    println!("{}", source_id(owned.as_CFTypeRef()));
    0
}

/// `Kyme --select-source <id>`：切换到指定的输入源（测试完切回原来的输入法用）。
pub fn select_source(id: &str) -> i32 {
    // SAFETY: 系统常量
    let key = unsafe { kTISPropertyInputSourceID };
    let mut status = None;
    let selected = retry(|| {
        let sources = find_sources(key, id)?;
        let source = sources.iter().next()?;
        // SAFETY: source 来自上面的列表，列表还活着
        let result = unsafe { TISSelectInputSource(source.as_CFTypeRef()) };
        status = Some(result);
        (result == 0).then_some(())
    });
    match status {
        Some(status) => println!("切换到 {id}：{}", describe(status)),
        None => eprintln!("未找到输入源 {id}"),
    }
    i32::from(selected.is_none())
}

/// 启用输入法的全部输入源，返回其中输入模式（可以选中的那些）的 ID；还查不到时返回 None。
fn enable(bundle_id: &str) -> Option<Vec<String>> {
    // SAFETY: 系统常量
    let sources = find_sources(unsafe { kTISPropertyBundleID }, bundle_id)?;
    let mut modes = Vec::new();
    for source in sources.iter() {
        let source = source.as_CFTypeRef();
        let id = source_id(source);
        // 已经启用的不再启用一遍：每调一次 TISEnableInputSource，系统都会弹窗请用户确认
        if is_enabled(source) {
            println!("{id} 已启用");
        } else {
            // SAFETY: source 来自上面的列表，列表还活着
            let status = unsafe { TISEnableInputSource(source) };
            println!("启用 {id}：{}（系统将弹窗请求确认）", describe(status));
        }
        // 和 bundle ID 相同的是输入法本身，其余是它的输入模式
        if id != bundle_id {
            modes.push(id);
        }
    }
    (!modes.is_empty()).then_some(modes)
}

/// 按某个属性查输入源，包括还没启用的。
fn find_sources(key: CFStringRef, value: &str) -> Option<CFArray<CFType>> {
    // SAFETY: key 是 HIToolbox 的属性常量
    let key = unsafe { CFString::wrap_under_get_rule(key) };
    let filter = CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFString::new(value).as_CFType())]);
    // SAFETY: filter 是有效的字典；返回值按 Create 规则持有
    let list = unsafe { TISCreateInputSourceList(filter.as_concrete_TypeRef(), 1) };
    (!list.is_null()).then(|| unsafe { CFArray::wrap_under_create_rule(list) })
}

/// 系统是异步生效的：刚注册、刚启用的输入源要过一会儿才查得到、选得中。
fn retry<T>(mut attempt: impl FnMut() -> Option<T>) -> Option<T> {
    for _ in 0..20 {
        if let Some(value) = attempt() {
            return Some(value);
        }
        thread::sleep(Duration::from_millis(250));
    }
    None
}

fn describe(status: i32) -> String {
    if status == 0 { "成功".into() } else { format!("错误 {status}") }
}

fn is_enabled(source: InputSourceRef) -> bool {
    // SAFETY: 属性按 Get 规则返回，值是 CFBoolean
    unsafe {
        let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceIsEnabled);
        !value.is_null() && bool::from(CFBoolean::wrap_under_get_rule(value as CFBooleanRef))
    }
}

fn source_id(source: InputSourceRef) -> String {
    // SAFETY: 属性按 Get 规则返回，值是 CFString
    unsafe {
        let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
        if value.is_null() {
            return String::new();
        }
        CFString::wrap_under_get_rule(value as CFStringRef).to_string()
    }
}
