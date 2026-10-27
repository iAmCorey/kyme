//! 安装包检查：用 scripts/build-app.sh 组装 target/Kyme.app（不安装），
//! 核对 Info.plist、可执行文件、图标、本地化字符串、词库和签名。

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::report::Outcome;
use crate::util;

/// Info.plist 里必须有、而且不能为空的字段。
const REQUIRED: [&str; 7] = [
    "CFBundleIdentifier",
    "CFBundleExecutable",
    "CFBundleIconFile",
    "CFBundleShortVersionString",
    "InputMethodConnectionName",
    "InputMethodServerControllerClass",
    "tsInputMethodIconFileKey",
];
/// 输入模式里引用图标文件的字段。
const MODE_ICONS: [&str; 3] =
    ["tsInputModeMenuIconFileKey", "tsInputModeAlternateMenuIconFileKey", "tsInputModePaletteIconFileKey"];

pub fn run(root: &Path) -> Outcome {
    if let Err(out) = util::capture(Command::new(root.join("scripts/build-app.sh")).current_dir(root)) {
        return Outcome::fail("组装 Kyme.app 失败").with_details(util::tail(&out, 20));
    }
    let app = root.join("target/Kyme.app");
    let contents = app.join("Contents");
    let info = match plist(&contents.join("Info.plist")) {
        Ok(info) => info,
        Err(err) => return Outcome::fail(err),
    };

    let mut problems = Vec::new();
    check_info(&info, &contents, &mut problems);
    check_strings(&contents.join("Resources"), &mut problems);
    let dict_files = check_dict(root, &contents.join("Resources/dict"), &mut problems);
    if let Err(out) = util::capture(Command::new("codesign").args(["--verify", "--strict"]).arg(&app)) {
        problems.push(format!("签名校验没通过：{}", out.trim()));
    }

    let mut notes = Vec::new();
    let executable = contents.join("MacOS").join(info["CFBundleExecutable"].as_str().unwrap_or_default());
    if let Some(archs) = util::stdout_of(Command::new("lipo").arg("-archs").arg(&executable)) {
        notes.push(format!("架构：{archs}"));
    }
    let size = util::stdout_of(Command::new("du").arg("-sk").arg(&app));
    if let Some(kb) = size.as_deref().and_then(|s| s.split_whitespace().next()).and_then(|kb| kb.parse::<f64>().ok()) {
        notes.push(format!("大小：{:.1}MB", kb / 1024.0));
    }

    if problems.is_empty() {
        Outcome::pass(format!("Info.plist、图标、词库（{dict_files} 个文件）和签名都没问题")).with_details(notes)
    } else {
        let summary = format!("{} 个问题", problems.len());
        problems.extend(notes);
        Outcome::fail(summary).with_details(problems)
    }
}

fn plist(path: &Path) -> Result<Value, String> {
    let json = util::capture(Command::new("plutil").args(["-convert", "json", "-o", "-"]).arg(path))
        .map_err(|out| format!("{} 不是合法的 plist：{}", path.display(), out.trim()))?;
    serde_json::from_str(&json).map_err(|err| format!("解析 {} 失败：{err}", path.display()))
}

fn check_info(info: &Value, contents: &Path, problems: &mut Vec<String>) {
    let text = |key: &str| info.get(key).and_then(Value::as_str).unwrap_or_default();
    for key in REQUIRED.into_iter().filter(|key| text(key).is_empty()) {
        problems.push(format!("Info.plist 缺少 {key}"));
    }
    let version = env!("CARGO_PKG_VERSION");
    if text("CFBundleShortVersionString") != version {
        problems.push(format!(
            "Info.plist 的版本是 {}，Cargo.toml 是 {version}，两边要一致",
            text("CFBundleShortVersionString")
        ));
    }
    if info.get("LSBackgroundOnly").and_then(Value::as_bool) != Some(true) {
        problems.push("Info.plist 的 LSBackgroundOnly 要是 true，否则输入法会出现在程序坞里".into());
    }

    let executable = contents.join("MacOS").join(text("CFBundleExecutable"));
    match fs::read(&executable) {
        Ok(bytes) => {
            let runnable = fs::metadata(&executable).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
            if !runnable {
                problems.push(format!("{} 没有可执行权限", executable.display()));
            }
            // IMK 按 Info.plist 里的类名找控制器，两边改名要同步
            let class = text("InputMethodServerControllerClass");
            if !class.is_empty() && !bytes.windows(class.len()).any(|window| window == class.as_bytes()) {
                problems.push(format!("可执行文件里没有 Info.plist 写的控制器类 {class}"));
            }
        }
        Err(err) => problems.push(format!("读不了可执行文件 {}：{err}", executable.display())),
    }

    let bundle_id = text("CFBundleIdentifier");
    // App 图标可以不写扩展名，系统会补上 .icns
    let app_icon = match text("CFBundleIconFile") {
        name if !name.is_empty() && Path::new(name).extension().is_none() => format!("{name}.icns"),
        name => name.to_string(),
    };
    let mut icons = vec![text("tsInputMethodIconFileKey").to_string(), app_icon];
    match info.pointer("/ComponentInputModeDict/tsInputModeListKey").and_then(Value::as_object) {
        None => problems.push("Info.plist 里没有输入模式（ComponentInputModeDict.tsInputModeListKey）".into()),
        Some(modes) => {
            for (id, mode) in modes {
                if !id.starts_with(bundle_id) {
                    problems.push(format!("输入模式 {id} 要以 bundle ID {bundle_id} 开头"));
                }
                icons.extend(MODE_ICONS.iter().filter_map(|key| Some(mode.get(key)?.as_str()?.to_string())));
            }
            let visible = info.pointer("/ComponentInputModeDict/tsVisibleInputModeOrderedArrayKey");
            for id in visible.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                if !modes.contains_key(id) {
                    problems.push(format!("tsVisibleInputModeOrderedArrayKey 里的 {id} 不在输入模式列表里"));
                }
            }
        }
    }
    icons.sort();
    icons.dedup();
    for icon in icons.iter().filter(|icon| !icon.is_empty()) {
        if !contents.join("Resources").join(icon).is_file() {
            problems.push(format!("Info.plist 引用的图标 {icon} 不在 Resources 里"));
        }
    }
}

/// 各语言的 InfoPlist.strings 也得是合法的 plist，不然系统设置里显示不出名字。
fn check_strings(resources: &Path, problems: &mut Vec<String>) {
    let localized = fs::read_dir(resources).into_iter().flatten().filter_map(Result::ok).map(|entry| entry.path());
    for dir in localized.filter(|path| path.extension().is_some_and(|ext| ext == "lproj")) {
        for file in fs::read_dir(&dir).into_iter().flatten().filter_map(Result::ok).map(|entry| entry.path()) {
            if file.extension().is_some_and(|ext| ext == "strings")
                && let Err(out) = util::capture(Command::new("plutil").arg("-lint").arg(&file))
            {
                problems.push(format!("{} 有问题：{}", file.display(), out.trim()));
            }
        }
    }
}

/// 安装包里的 dict/ 要和 data/rime-ice/ 一模一样（文件名和大小）。返回文件个数。
/// 隐藏文件不算：build-app.sh 的 `cp data/rime-ice/*` 不拷它们，Finder 又会往文件夹里放 .DS_Store。
fn check_dict(root: &Path, bundled: &Path, problems: &mut Vec<String>) -> usize {
    let source = dict_files(&root.join("data/rime-ice"));
    let bundled = dict_files(bundled);
    if bundled.is_empty() {
        problems.push("安装包里没有词库（Resources/dict/）".into());
    } else if bundled != source {
        problems.push(format!("安装包里的词库 {bundled:?} 和 data/rime-ice/ 的 {source:?} 不一致"));
    }
    bundled.len()
}

fn dict_files(dir: &Path) -> BTreeMap<String, u64> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.metadata().ok()?.len())))
        .filter(|(name, _)| !name.starts_with('.'))
        .collect()
}
