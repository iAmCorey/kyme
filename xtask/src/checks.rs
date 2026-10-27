//! 直接交给 cargo 的三项：格式、clippy、单元测试。

use std::path::Path;
use std::process::Command;

use crate::report::Outcome;
use crate::util;

pub fn fmt(root: &Path) -> Outcome {
    let out = match util::capture(cargo(root).args(["fmt", "--all", "--check"])) {
        Ok(_) => return Outcome::pass("和 rustfmt.toml 一致"),
        Err(out) => out,
    };
    // 「Diff in /路径/文件.rs:行号:」
    let mut files: Vec<String> = out
        .lines()
        .filter_map(|line| line.strip_prefix("Diff in "))
        .filter_map(|line| line.trim_end_matches(':').rsplit_once(':').map(|(file, _)| file))
        .map(|file| Path::new(file).strip_prefix(root).unwrap_or(Path::new(file)).display().to_string())
        .collect();
    files.dedup();
    if files.is_empty() {
        return Outcome::fail("rustfmt 出错了").with_details(util::tail(&out, 20));
    }
    Outcome::fail(format!("{} 个文件没格式化，运行 cargo fmt --all", files.len())).with_details(files)
}

pub fn clippy(root: &Path) -> Outcome {
    match util::capture(cargo(root).args(["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])) {
        Ok(_) => Outcome::pass("没有警告"),
        Err(out) => Outcome::fail("有警告或编译错误").with_details(diagnostics(&out)),
    }
}

pub fn test(root: &Path) -> Outcome {
    match util::capture(cargo(root).args(["test", "--workspace", "-q"])) {
        Ok(out) => {
            // 「test result: ok. 18 passed; 0 failed; …」
            let passed: usize = out
                .lines()
                .filter_map(|line| line.strip_prefix("test result: ok. "))
                .filter_map(|line| line.split_once(' ')?.0.parse::<usize>().ok())
                .sum();
            Outcome::pass(format!("{passed} 个测试全部通过"))
        }
        Err(out) => {
            let failed: Vec<String> = out
                .lines()
                .filter(|line| line.starts_with("---- ") || line.contains("panicked at") || line.starts_with("error"))
                .map(str::to_string)
                .collect();
            let details = if failed.is_empty() { util::tail(&out, 30) } else { failed };
            Outcome::fail("有测试没通过，或者没编译过").with_details(details)
        }
    }
}

/// 用跑 xtask 的同一个 cargo（cargo run 会把它的路径放在 CARGO 里）。
fn cargo(root: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root);
    command
}

/// 编译器输出里的错误、警告和它们的位置，去掉依赖库的「未来版本不兼容」提示。
fn diagnostics(out: &str) -> Vec<String> {
    let lines: Vec<String> = out
        .lines()
        .filter(|line| line.starts_with("error") || line.starts_with("warning") || line.trim_start().starts_with("-->"))
        .filter(|line| !line.contains("future version of Rust") && !line.contains("future-incompat"))
        .take(60)
        .map(str::to_string)
        .collect();
    if lines.is_empty() { util::tail(out, 30) } else { lines }
}
