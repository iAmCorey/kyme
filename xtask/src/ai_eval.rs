//! `cargo xtask ai-eval [服务商] [--no-context]`：拿 eval/context.tsv 的用例实测 AI 预测选词。
//!
//! 每条用例是光标前的文字、接着打的拼音和想要的那句。用真的输入会话打一遍拼音，拿输入法会发给模型的那几句让它挑，
//! 发出去的和打字时一模一样。报告引擎首选和模型
//! 各对了多少、按把握度决定换不换首选时对了多少，还有延迟和 token。用的是输入法那份 AI 设置：设置文件里选的
//! 服务商（命令后面写服务商可以换一家，写法同设置文件的 provider），ai.toml 里这一家的 Key 和模型。
//! `--no-context` 不给前文，像在空的输入框里打字：主要看「首选就对」的会不会被改错。
//! 要联网、要 Key，所以不放进 qa。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kyme_ai::Client;
use kyme_core::pinyin::Engine;
use kyme_core::{AiFile, AiProvider, AiQuery, Key, Modifiers, Session, Settings, SettingsFile};

use crate::util;
/// 模型的把握度到这个数才换掉引擎的首选。0 是模型挑什么就用什么。
const THRESHOLDS: [f64; 4] = [0.0, 0.3, 0.5, 0.7];
/// 引擎首选本来就对的那一类：模型不能把它改错。
const ALREADY_RIGHT: &str = "首选就对";
/// 一条用例出错时最多再试几次。
const RETRIES: usize = 2;

struct Case {
    category: String,
    context: String,
    pinyin: String,
    expected: String,
}

/// 一条用例的结果。
struct Result {
    engine_right: bool,
    /// 想要的那句在给模型挑的整句里。
    covered: bool,
    /// 模型挑的对不对，和它的把握度（聊天模型不给）。
    picked_right: bool,
    confidence: Option<f64>,
    /// 输入法会不会拿它挑的换首选（`Pick::trusted`）。
    trusted: bool,
    elapsed: Duration,
    tokens: u64,
}

pub fn run(root: &Path, engine: &Arc<Engine>, provider: Option<&str>, no_context: bool) -> ExitCode {
    let client = match client(provider) {
        Ok(client) => client,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(2);
        }
    };
    let cases = match load(&root.join("eval/context.tsv")) {
        Ok(cases) => cases,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::from(2);
        }
    };
    let how = if no_context { "，不给前文" } else { "" };
    println!("{} 条用例，{} 的 {}{how}\n", cases.len(), client.provider().name(), client.model());
    let mut results = Vec::new();
    let mut failed = 0;
    for case in &cases {
        let query = match query(engine, &case.pinyin) {
            Ok(query) => query,
            Err(reason) => {
                println!("  ! {}「{}」输入法不会拿去问 AI（{reason}），跳过", case.pinyin, case.context);
                failed += 1;
                continue;
            }
        };
        let texts: Vec<&str> = query.sentences.iter().map(|sentence| sentence.text.as_str()).collect();
        let context = if no_context { "" } else { case.context.as_str() };
        let attempt = || client.pick(context, &case.pinyin, &texts);
        let mut outcome = attempt();
        for retry in 1..=RETRIES {
            if outcome.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500 * retry as u64));
            outcome = attempt();
        }
        let (pick, elapsed) = match outcome {
            Ok(result) => result,
            Err(err) => {
                println!("  ! {}「{}」试了 {} 次都出错，跳过：{err}", case.pinyin, case.context, RETRIES + 1);
                failed += 1;
                continue;
            }
        };
        let chosen = texts[pick.index];
        let result = Result {
            engine_right: texts.first() == Some(&case.expected.as_str()),
            covered: texts.contains(&case.expected.as_str()),
            picked_right: chosen == case.expected,
            confidence: pick.confidence,
            trusted: pick.trusted(),
            elapsed,
            tokens: pick.input_tokens,
        };
        if !result.picked_right {
            let first = texts.first().copied().unwrap_or("（没有整句）");
            println!(
                "  ✗ {}「{}」{}：要「{}」，模型挑了「{chosen}」（把握 {}），引擎首选「{first}」{}",
                case.category,
                case.context,
                case.pinyin,
                case.expected,
                pick.confidence.map_or("没给".into(), |confidence| format!("{confidence:.2}")),
                if result.covered { "" } else { "，想要的不在候选里" }
            );
        }
        results.push((case, result));
    }
    if failed > 0 {
        println!("  {failed} 条跳过了，下面只算其余的");
    }
    if !results.is_empty() {
        report(&results);
    }
    ExitCode::SUCCESS
}

/// 输入法打这串拼音时会拿去问 AI 的那几句，第一句是首选。不会问的（比如首选用了纠错）返回原因。
fn query(engine: &Arc<Engine>, pinyin: &str) -> std::result::Result<AiQuery, &'static str> {
    let mut session = Session::new(engine.clone());
    for c in pinyin.chars() {
        session.handle_key(Key::Char(c), Modifiers::default());
    }
    session.ai_query()
}

/// 输入法那份 AI 设置建出来的客户端。`provider` 是命令后面写的服务商，没写用设置文件里选的。
fn client(provider: Option<&str>) -> std::result::Result<Client, String> {
    let path = settings_path().ok_or("找不到主目录")?;
    let settings = SettingsFile::new(path.clone()).reload().unwrap_or_else(|| Ok(Settings::default()))?;
    let provider = match provider {
        None => settings.ai.provider,
        Some(id) => AiProvider::ALL.into_iter().find(|provider| provider.id() == id).ok_or_else(|| {
            let ids: Vec<&str> = AiProvider::ALL.iter().map(|provider| provider.id()).collect();
            format!("没有「{id}」这一家，可以写 {}", ids.join("、"))
        })?,
    };
    let services = AiFile::beside(&path).reload().unwrap_or(Ok(Default::default()))?;
    Client::new(provider, services.get(provider))
        .map_err(|err| format!("{}{err}：在设置窗口的「AI」页填好再跑", provider.name()))
}

/// 输入法的设置文件，ai.toml 在它旁边。
fn settings_path() -> Option<PathBuf> {
    Some(std::env::home_dir()?.join("Library/Application Support/Kyme/settings.toml"))
}

/// 一种问法的汇总。
fn report(results: &[(&Case, Result)]) {
    let total = results.len();
    let count =
        |keep: &dyn Fn(&Case, &Result) -> bool| results.iter().filter(|(case, result)| keep(case, result)).count();
    let context = count(&|case, _| case.category != ALREADY_RIGHT);
    let right = total - context;
    println!(
        "  引擎首选对了 {}/{total}（要看上下文的 {}/{context}，首选就对的 {}/{right}）；想要的在候选里 {}/{total}",
        count(&|_, result| result.engine_right),
        count(&|case, result| case.category != ALREADY_RIGHT && result.engine_right),
        count(&|case, result| case.category == ALREADY_RIGHT && result.engine_right),
        count(&|_, result| result.covered),
    );
    // `used` 的那些用模型挑的，其余留着引擎首选
    let row = |how: &str, used: &dyn Fn(&Result) -> bool| {
        let final_right = |result: &Result| if used(result) { result.picked_right } else { result.engine_right };
        println!(
            "  {how}：对了 {}/{total}，修好 {}，改错 {}（用了模型 {} 次）",
            count(&|_, result| final_right(result)),
            count(&|_, result| !result.engine_right && final_right(result)),
            count(&|_, result| result.engine_right && !final_right(result)),
            count(&|_, result| used(result)),
        );
    };
    // 聊天模型不给把握度，各个门槛都当它有把握
    for threshold in THRESHOLDS {
        row(&format!("把握度 ≥ {threshold:.1} 才换"), &|result| {
            result.confidence.is_none_or(|c| c >= threshold)
        });
    }
    row("照输入法的规则换", &|result| result.trusted);
    let mut elapsed: Vec<Duration> = results.iter().map(|(_, result)| result.elapsed).collect();
    elapsed.sort();
    let at = |q: f64| util::percentile(&elapsed, q).unwrap_or_default();
    let tokens: u64 = results.iter().map(|(_, result)| result.tokens).sum();
    println!(
        "  延迟 p50 {}，p95 {}，最慢 {}；共 {tokens} 个输入 token（每条 {:.0} 个）",
        util::millis(at(0.5)),
        util::millis(at(0.95)),
        util::millis(at(1.0)),
        tokens as f64 / total.max(1) as f64,
    );
}

fn load(path: &Path) -> std::result::Result<Vec<Case>, String> {
    let rows = util::tsv(path, "类别、光标前的文字、拼音、想要的那句四列")?;
    Ok(rows
        .into_iter()
        .map(|[category, context, pinyin, expected]| Case { category, context, pinyin, expected })
        .collect())
}
