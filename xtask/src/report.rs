//! 每项检查的结果，以及写到 target/qa/ 下的 Markdown 报告。

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::util;

/// 终端里每项最多打印这么多行细节，完整的在报告里。
const MAX_PRINTED: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn icon(self) -> &'static str {
        match self {
            Status::Pass => "✅",
            Status::Warn => "⚠️",
            Status::Fail => "❌",
            Status::Skip => "⏭️",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::Pass => "通过",
            Status::Warn => "要注意",
            Status::Fail => "没通过",
            Status::Skip => "跳过",
        }
    }
}

pub struct Outcome {
    pub status: Status,
    /// 一句话结论，显示在终端和报告的汇总表里。
    pub summary: String,
    /// 细节：没通过的用例、测到的数据等。终端里只在没通过或要注意时打印。
    pub details: Vec<String>,
}

impl Outcome {
    pub fn new(status: Status, summary: impl Into<String>) -> Self {
        Outcome { status, summary: summary.into(), details: Vec::new() }
    }

    pub fn pass(summary: impl Into<String>) -> Self {
        Self::new(Status::Pass, summary)
    }

    pub fn fail(summary: impl Into<String>) -> Self {
        Self::new(Status::Fail, summary)
    }

    pub fn skip(summary: impl Into<String>) -> Self {
        Self::new(Status::Skip, summary)
    }

    pub fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

/// 一项检查跑完后在终端里打出结论。
pub fn print(outcome: &Outcome, elapsed: Duration) {
    let Outcome { status, summary, details } = outcome;
    println!("      {} {}：{summary}（{}）", status.icon(), status.label(), util::seconds(elapsed));
    if matches!(status, Status::Fail | Status::Warn) {
        for line in details.iter().take(MAX_PRINTED) {
            println!("         {line}");
        }
        if details.len() > MAX_PRINTED {
            println!("         ……还有 {} 行，见报告", details.len() - MAX_PRINTED);
        }
    }
}

struct Entry {
    title: &'static str,
    outcome: Outcome,
    elapsed: Duration,
}

pub struct Report {
    header: Vec<String>,
    entries: Vec<Entry>,
}

impl Report {
    pub fn new(header: Vec<String>) -> Self {
        Report { header, entries: Vec::new() }
    }

    pub fn push(&mut self, title: &'static str, outcome: Outcome, elapsed: Duration) {
        self.entries.push(Entry { title, outcome, elapsed });
    }

    /// 结果是 `status` 的那些检查。
    pub fn titles(&self, status: Status) -> Vec<&'static str> {
        self.entries.iter().filter(|e| e.outcome.status == status).map(|e| e.title).collect()
    }

    /// 写到 `dir/report-<时间>.md`，同时更新 `dir/latest.md`。
    pub fn write(&self, dir: &Path) -> Result<PathBuf, String> {
        let stamp = util::stdout_of(Command::new("date").arg("+%Y%m%d-%H%M%S")).unwrap_or_else(|| "now".into());
        let path = dir.join(format!("report-{stamp}.md"));
        let text = self.render();
        fs::create_dir_all(dir)
            .and_then(|()| fs::write(&path, &text))
            .and_then(|()| fs::write(dir.join("latest.md"), &text))
            .map_err(|err| format!("写不了报告 {}：{err}", path.display()))?;
        Ok(path)
    }

    fn render(&self) -> String {
        let mut out = String::from("# Kyme QA 报告\n\n");
        for line in &self.header {
            let _ = writeln!(out, "- {line}");
        }
        out.push_str("\n| 检查 | 结果 | 说明 | 耗时 |\n|---|---|---|---|\n");
        for Entry { title, outcome, elapsed } in &self.entries {
            let status = outcome.status;
            let summary = outcome.summary.replace('|', "\\|");
            let _ = writeln!(
                out,
                "| {title} | {} {} | {summary} | {} |",
                status.icon(),
                status.label(),
                util::seconds(*elapsed)
            );
        }
        for Entry { title, outcome, .. } in self.entries.iter().filter(|e| !e.outcome.details.is_empty()) {
            let _ = writeln!(out, "\n## {title}\n\n```text");
            for line in &outcome.details {
                let _ = writeln!(out, "{line}");
            }
            out.push_str("```\n");
        }
        out
    }
}

/// 报告开头：测的是哪个版本、什么时候、在什么机器上、跑了哪些检查。
pub fn header(root: &Path, scope: &str) -> Vec<String> {
    let commit = match util::git_head(root) {
        Some(head) if util::git_dirty(root) => format!("{head}（工作区有未提交的改动）"),
        Some(head) => head,
        None => "还没有提交".into(),
    };
    let mut lines = vec![format!("版本：{}", env!("CARGO_PKG_VERSION")), format!("提交：{commit}")];
    if let Some(now) = util::stdout_of(Command::new("date").arg("+%Y-%m-%d %H:%M")) {
        lines.push(format!("时间：{now}"));
    }
    let os = util::stdout_of(Command::new("sw_vers").arg("-productVersion"));
    let cpu = util::stdout_of(Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]));
    if let (Some(os), Some(cpu)) = (os, cpu) {
        lines.push(format!("机器：macOS {os}，{cpu}"));
    }
    lines.push(format!("范围：{scope}"));
    lines
}
