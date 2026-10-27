//! 人工检查清单（qa/manual.toml）：自动测试顾不到的 App 兼容性和观感。
//! 逐项提示操作和期望，记下结果；只能在终端里交互着跑。

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use serde::Deserialize;

use crate::report::{Outcome, Status};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Checklist {
    check: Vec<Item>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    app: String,
    steps: String,
    expect: String,
}

pub fn run(root: &Path) -> Outcome {
    let path = root.join("qa/manual.toml");
    let parsed = fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|text| toml::from_str::<Checklist>(&text).map_err(|err| err.to_string()));
    let items = match parsed {
        Ok(list) => list.check,
        Err(err) => return Outcome::fail(format!("读不了 {}：{err}", path.display())),
    };
    if !io::stdin().is_terminal() {
        return Outcome::skip(format!("{} 项要在终端里交互着过：cargo xtask qa --manual", items.len()));
    }

    // 测的时候输入法多半是 Kyme，这里只收数字和回车：中英文状态下它们都直接交给终端
    println!("      先在要测的 App 里切到 Kyme 拼音。这里用数字键回答，中英文状态下都能输入。");
    let mut lines = io::stdin().lock().lines();
    let mut ask = |prompt: &str| {
        print!("      {prompt}");
        let _ = io::stdout().flush();
        lines.next().and_then(Result::ok).map(|line| line.trim().to_string())
    };
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    for (index, item) in items.iter().enumerate() {
        println!("\n      [{}/{}] {}", index + 1, items.len(), item.app);
        println!("      操作：{}", item.steps);
        println!("      期望：{}", item.expect);
        let answer = loop {
            match ask("1 通过　2 不通过　3 跳过 > ") {
                Some(answer) if matches!(answer.as_str(), "1" | "2" | "3") => break answer,
                Some(_) => continue,
                // 输入结束了，剩下的都算跳过
                None => break "3".into(),
            }
        };
        match answer.as_str() {
            "1" => {}
            "2" => {
                let note = ask("哪里不对？> ").unwrap_or_default();
                failed.push(format!("不通过 {}：{note}", item.app));
            }
            _ => skipped.push(format!("跳过 {}", item.app)),
        }
    }

    let summary = format!(
        "{} 项：{} 项通过，{} 项不通过，{} 项跳过",
        items.len(),
        items.len() - failed.len() - skipped.len(),
        failed.len(),
        skipped.len()
    );
    let status = if !failed.is_empty() {
        Status::Fail
    } else if !skipped.is_empty() {
        Status::Warn
    } else {
        Status::Pass
    };
    failed.extend(skipped);
    Outcome::new(status, summary).with_details(failed)
}
