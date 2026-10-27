//! 开发用的命令行工具，目前只有一个命令：
//!
//! `cargo xtask qa`：发版前的整套检查。各项查什么、怎么加用例、发版流程见 docs/QA.md。

#[cfg(target_os = "macos")]
mod bundle;
mod checks;
mod eval;
mod gates;
#[cfg(target_os = "macos")]
mod macos;
mod manual;
mod release;
mod report;
mod scenario;
mod util;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kyme_core::pinyin::{Dictionary, Engine};

use crate::gates::Gates;
use crate::report::{Outcome, Report, Status};

const USAGE: &str = "\
用法：cargo xtask qa [--manual] [--release] [--only 检查,...]

  不带参数   自动检查：格式、clippy、单元测试、评测、按键场景、安装包、macOS 接入层。不动输入法设置，也不占屏幕
  --manual   再逐项过一遍人工检查清单（qa/manual.toml）
  --release  发版用：以上全部，外加发版要求（LICENSE、代码已提交）
  --only     只跑列出的检查，比如 --only eval,scenarios

检查：fmt clippy test eval scenarios bundle macos release manual";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("qa") => qa(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Fmt,
    Clippy,
    Test,
    Eval,
    Scenarios,
    Bundle,
    Macos,
    Release,
    Manual,
}

impl Check {
    /// 按这个顺序跑：快的、不占屏幕的在前。
    const ALL: [Check; 9] = [
        Check::Fmt,
        Check::Clippy,
        Check::Test,
        Check::Eval,
        Check::Scenarios,
        Check::Bundle,
        Check::Macos,
        Check::Release,
        Check::Manual,
    ];

    fn id(self) -> &'static str {
        match self {
            Check::Fmt => "fmt",
            Check::Clippy => "clippy",
            Check::Test => "test",
            Check::Eval => "eval",
            Check::Scenarios => "scenarios",
            Check::Bundle => "bundle",
            Check::Macos => "macos",
            Check::Release => "release",
            Check::Manual => "manual",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Check::Fmt => "格式",
            Check::Clippy => "clippy",
            Check::Test => "单元测试",
            Check::Eval => "评测",
            Check::Scenarios => "按键场景",
            Check::Bundle => "安装包",
            Check::Macos => "macOS 接入层",
            Check::Release => "发版要求",
            Check::Manual => "人工检查",
        }
    }
}

struct Options {
    manual: bool,
    release: bool,
    only: Option<Vec<Check>>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Options, String> {
        let mut options = Options { manual: false, release: false, only: None };
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--manual" => options.manual = true,
                "--release" => options.release = true,
                "--only" => {
                    let list = args.next().ok_or("--only 后面要写检查名")?;
                    let checks = list
                        .split(',')
                        .map(|id| Check::ALL.into_iter().find(|c| c.id() == id).ok_or(format!("没有叫 {id} 的检查")))
                        .collect::<Result<_, _>>()?;
                    options.only = Some(checks);
                }
                other => return Err(format!("不认识的参数 {other}")),
            }
        }
        Ok(options)
    }

    fn checks(&self) -> Vec<Check> {
        Check::ALL
            .into_iter()
            .filter(|check| match &self.only {
                Some(only) => only.contains(check),
                None => match check {
                    Check::Release => self.release,
                    Check::Manual => self.manual || self.release,
                    _ => true,
                },
            })
            .collect()
    }
}

/// 各项检查共用的东西。
pub struct Context {
    pub root: PathBuf,
    pub gates: Gates,
    engine: Option<Result<Loaded, String>>,
}

/// 载入好的引擎，评测、按键场景和 macOS 接入层共用。
#[derive(Clone)]
pub struct Loaded {
    pub engine: Arc<Engine>,
    pub entries: usize,
    pub load_time: Duration,
}

impl Context {
    /// 第一次用时先确保词库下载好，再载入；之后直接复用。
    pub fn engine(&mut self) -> Result<Loaded, String> {
        self.engine.get_or_insert_with(|| load_engine(&self.root)).clone()
    }
}

/// 和输入法一样：读 data/rime-ice/ 下全部 .dict.yaml，按文件名排序。安装包里放的也是这些文件。
fn load_engine(root: &Path) -> Result<Loaded, String> {
    util::capture(Command::new(root.join("scripts/fetch-dict.sh")).current_dir(root))
        .map_err(|out| format!("下载词库失败：{}", util::tail(&out, 3).join(" / ")))?;
    let dir = root.join("data/rime-ice");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|err| format!("读不了 {}：{err}", dir.display()))?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.to_string_lossy().ends_with(".dict.yaml"))
        .collect();
    paths.sort();
    let started = Instant::now();
    let dict = Dictionary::load_rime(&paths).map_err(|err| err.to_string())?;
    let load_time = started.elapsed();
    let entries = dict.len();
    Ok(Loaded { engine: Arc::new(Engine::new(dict)), entries, load_time })
}

fn qa(args: &[String]) -> ExitCode {
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(err) => {
            eprintln!("{err}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask 在仓库根目录下").to_path_buf();
    let gates = match Gates::load(&root.join("qa/gates.toml")) {
        Ok(gates) => gates,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(2);
        }
    };
    let checks = options.checks();
    let scope: Vec<&str> = checks.iter().map(|check| check.title()).collect();
    let mut report = Report::new(report::header(&root, &scope.join("、")));
    let mut ctx = Context { root, gates, engine: None };

    for (index, &check) in checks.iter().enumerate() {
        println!("[{}/{}] {}", index + 1, checks.len(), check.title());
        let started = Instant::now();
        let outcome = run(check, &mut ctx);
        report::print(&outcome, started.elapsed());
        report.push(check.title(), outcome, started.elapsed());
    }

    let written = report.write(&ctx.root.join("target/qa"));
    let failed = report.titles(Status::Fail);
    let warned = report.titles(Status::Warn);
    println!();
    if !failed.is_empty() {
        println!("❌ 没通过：{}", failed.join("、"));
    } else if !warned.is_empty() {
        println!("⚠️ 都通过了，但这几项要看一下：{}", warned.join("、"));
    } else {
        println!("✅ 全部通过");
    }
    match written {
        Ok(path) => println!("报告：{}", path.strip_prefix(&ctx.root).unwrap_or(&path).display()),
        Err(err) => eprintln!("{err}"),
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn run(check: Check, ctx: &mut Context) -> Outcome {
    match check {
        Check::Fmt => checks::fmt(&ctx.root),
        Check::Clippy => checks::clippy(&ctx.root),
        Check::Test => checks::test(&ctx.root),
        Check::Eval => eval::run(ctx),
        Check::Scenarios => scenario::run(ctx),
        Check::Release => release::run(&ctx.root),
        Check::Manual => manual::run(&ctx.root),
        #[cfg(target_os = "macos")]
        Check::Bundle => bundle::run(&ctx.root),
        #[cfg(target_os = "macos")]
        Check::Macos => macos::run(ctx),
        #[cfg(not(target_os = "macos"))]
        Check::Bundle | Check::Macos => Outcome::skip("目前只有 macOS 版"),
    }
}
