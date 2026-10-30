//! 按改动挑检查：看哪些文件还没推送（工作区的修改、没跟踪的新文件、本地还没推送的提交），
//! 只跑和它们有关的检查。全部跑用 `--all`，发版用 `--release`。

use std::path::Path;
use std::process::Command;

use crate::{Check, util};

/// 改到哪里要跑哪些检查，按路径前缀匹配。Rust 代码另外都要过格式、clippy 和单元测试。
const RULES: &[(&str, &[Check])] = &[
    // 引擎改了，首选和按键场景的结果都可能变；两边用同一个引擎，接入层对照不会因此对不上
    ("crates/kyme-pinyin/", &[Check::Eval, Check::Scenarios]),
    // 扩展词库登记的校验值由评测核对
    ("crates/kyme-core/src/extra.rs", &[Check::Eval]),
    ("crates/kyme-core/", &[Check::Scenarios, Check::Macos]),
    ("crates/kyme-ui/", &[Check::Macos]),
    ("crates/kyme-macos/bundle/", &[Check::Bundle]),
    ("crates/kyme-macos/", &[Check::Macos]),
    ("xtask/src/eval.rs", &[Check::Eval]),
    ("xtask/src/scenario.rs", &[Check::Scenarios, Check::Macos]),
    ("xtask/src/macos.rs", &[Check::Macos]),
    ("xtask/src/bundle.rs", &[Check::Bundle]),
    ("eval/", &[Check::Eval]),
    ("qa/scenarios.tsv", &[Check::Scenarios, Check::Macos]),
    ("qa/gates.toml", &[Check::Eval, Check::Macos]),
    ("data/", &[Check::Eval, Check::Scenarios, Check::Bundle]),
    ("scripts/fetch-dict.sh", &[Check::Eval, Check::Bundle]),
    ("scripts/build-app.sh", &[Check::Bundle, Check::Macos]),
    // 版本号要和 Info.plist 对得上
    ("Cargo.toml", &[Check::Bundle]),
    ("LICENSE", &[Check::Bundle]),
];

/// 还没推送的改动碰到的文件。没设上游的分支和 origin/main 比；git 出错、比不了时返回 None，调用方全跑：
/// 查不出改了什么，不能当成什么都没改。
pub fn files(root: &Path) -> Option<Vec<String>> {
    let git = |args: &[&str]| util::stdout_of(Command::new("git").args(args).current_dir(root));
    let unpushed = git(&["diff", "--name-only", "@{upstream}...HEAD"])
        .or_else(|| git(&["diff", "--name-only", "origin/main...HEAD"]))?;
    let working = git(&["diff", "--name-only", "HEAD"])?;
    let untracked = git(&["ls-files", "--others", "--exclude-standard"])?;
    let mut files: Vec<String> =
        [unpushed, working, untracked].iter().flat_map(|out| out.lines()).map(str::to_string).collect();
    files.sort();
    files.dedup();
    Some(files)
}

/// 这些文件要跑哪些检查，按 `Check::ALL` 的顺序。
pub fn checks(files: &[String]) -> Vec<Check> {
    let rust = |file: &str| {
        file.ends_with(".rs") || ["Cargo.toml", "Cargo.lock", "rustfmt.toml"].iter().any(|name| file.ends_with(name))
    };
    let mut wanted: Vec<Check> = Vec::new();
    for file in files {
        if rust(file) {
            wanted.extend([Check::Fmt, Check::Clippy, Check::Test]);
        }
        for (prefix, checks) in RULES {
            if file.starts_with(prefix) {
                wanted.extend_from_slice(checks);
            }
        }
    }
    Check::ALL.into_iter().filter(|check| wanted.contains(check)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(files: &[&str]) -> Vec<&'static str> {
        checks(&files.iter().map(|file| file.to_string()).collect::<Vec<_>>()).into_iter().map(Check::id).collect()
    }

    #[test]
    fn picks_checks_by_what_changed() {
        assert!(picked(&["README.md", "docs/QA.md"]).is_empty(), "只改文档什么都不跑");
        assert_eq!(picked(&["crates/kyme-pinyin/src/engine.rs"]), ["fmt", "clippy", "test", "eval", "scenarios"]);
        assert_eq!(picked(&["crates/kyme-ui/src/settings.rs"]), ["fmt", "clippy", "test", "macos"]);
        assert_eq!(picked(&["eval/pinyin.tsv"]), ["eval"]);
        assert_eq!(picked(&["crates/kyme-macos/bundle/Info.plist"]), ["bundle", "macos"]);
    }
}
