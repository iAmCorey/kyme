//! 跑外部命令、读 git 状态、读用例表、格式化耗时之类的小工具。

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// 跑一个命令，返回 stdout 和 stderr 拼在一起的输出；退出码不是 0 时把输出放进 Err。
pub fn capture(command: &mut Command) -> Result<String, String> {
    match command.output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            if output.status.success() { Ok(text) } else { Err(text) }
        }
        Err(err) => Err(format!("运行 {} 失败：{err}", command.get_program().to_string_lossy())),
    }
}

/// 命令成功时的 stdout，去掉首尾空白。
pub fn stdout_of(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// 最后 `n` 个非空行。
pub fn tail(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].iter().map(|line| line.to_string()).collect()
}

/// 当前提交的短哈希；还没有提交时是 None。
pub fn git_head(root: &Path) -> Option<String> {
    stdout_of(Command::new("git").args(["rev-parse", "--short", "HEAD"]).current_dir(root))
}

/// 工作区有没有未提交的改动（含未跟踪的文件）。
pub fn git_dirty(root: &Path) -> bool {
    stdout_of(Command::new("git").args(["status", "--porcelain"]).current_dir(root)).is_some_and(|s| !s.is_empty())
}

/// 读一个 Tab 分隔的用例表：跳过空行和 `#` 开头的注释，每行取前 `N` 列，不够的报第几行，`columns` 说明各列。
pub fn tsv<const N: usize>(path: &Path, columns: &str) -> Result<Vec<[String; N]>, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|(index, line)| {
            let fields: Vec<String> = line.split('\t').take(N).map(String::from).collect();
            fields.try_into().map_err(|_| format!("{} 第 {} 行要有{columns}，用 Tab 分隔", path.display(), index + 1))
        })
        .collect()
}

/// 排好序的耗时里的第 `q` 分位（0 到 1），空的是 None。
pub fn percentile(sorted: &[Duration], q: f64) -> Option<Duration> {
    sorted.get((sorted.len().saturating_sub(1) as f64 * q).round() as usize).copied()
}

pub fn as_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn millis(duration: Duration) -> String {
    format!("{:.1}ms", as_ms(duration))
}

pub fn seconds(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}
