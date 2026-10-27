//! 拼音评测：eval/pinyin.tsv 里每一条的首选都要对，单条转换和词库载入的耗时不能超过门槛。
//!
//! 每一条都从第一个字母打到最后一个，每一步输入框显示的（preedit）都要是打的字母，纠过错的也一样。
//! 「本来就对」这一类是照着打的输入，还要求每一步引擎理解的拼音（spelling）也和打的字母一样，
//! 也就是没被当成打错。同样的要求再放到最常用的音节两两拼接上（那按、下岸……），被改掉的比例不能超过门槛。
//!
//! eval/pending.tsv（可选，格式相同）放暂时还做不到的用例：只报告不卡发版，哪条做到了会提示挪过去。

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use kyme_core::pinyin::{Conversion, Engine, SylId};

use crate::report::Outcome;
use crate::{Context, util};

/// 照着打的那一类：打对了的字母不能被纠错吃掉或改掉（naan 曾被显示成 nan、读成「难」）。
const SPELLED: &str = "本来就对";

struct Case {
    category: String,
    input: String,
    expected: String,
}

fn load(path: &Path) -> Result<Vec<Case>, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|(index, line)| {
            let mut cols = line.split('\t');
            match (cols.next(), cols.next(), cols.next()) {
                (Some(category), Some(input), Some(expected)) => {
                    Ok(Case { category: category.into(), input: input.into(), expected: expected.into() })
                }
                _ => Err(format!("{} 第 {} 行要有类别、输入、期望三列，用 Tab 分隔", path.display(), index + 1)),
            }
        })
        .collect()
}

struct Evaluation {
    /// （类别, 打对的, 总数），按类别在文件里第一次出现的顺序。
    tally: Vec<(String, usize, usize)>,
    hits: Vec<String>,
    misses: Vec<String>,
    slowest: Duration,
}

fn evaluate(engine: &Engine, cases: &[Case]) -> Evaluation {
    let mut result = Evaluation { tally: Vec::new(), hits: Vec::new(), misses: Vec::new(), slowest: Duration::ZERO };
    for Case { category, input, expected } in cases {
        let started = Instant::now();
        let conversion = engine.convert(input);
        result.slowest = result.slowest.max(started.elapsed());

        let index = match result.tally.iter().position(|(c, ..)| c == category) {
            Some(index) => index,
            None => {
                result.tally.push((category.clone(), 0, 0));
                result.tally.len() - 1
            }
        };
        result.tally[index].2 += 1;
        let top: Vec<&str> = conversion.candidates.iter().take(3).map(|c| c.text.as_str()).collect();
        if top.first() == Some(&expected.as_str()) {
            result.tally[index].1 += 1;
            result.hits.push(format!("[{category}] {input} → {expected}"));
        } else {
            result.misses.push(format!(
                "[{category}] {input} → {}（期望 {expected}，读成 {}）",
                top.join(" "),
                conversion.spelling
            ));
        }
    }
    result
}

/// 拿常用音节两两拼接的个数：80 个，6400 个输入。
const COMMON_SYLLABLES: usize = 80;

/// 从第一个字母打到最后一个，每一步从转换结果里取一份拼音，去掉分隔符后都要和已经打的字母一样。
/// 返回第一处不一样的地方：已经打的字母，和那份拼音。
fn first_changed<'a>(engine: &Engine, input: &'a str, pinyin: fn(Conversion) -> String) -> Option<(&'a str, String)> {
    (1..=input.len()).map(|end| &input[..end]).find_map(|typed| {
        let got = pinyin(engine.convert(typed));
        (got.replace('\'', "") != typed.replace('\'', "")).then_some((typed, got))
    })
}

/// 引擎理解的拼音（spelling）没改打的字母，否则返回第一处被改的地方。
fn first_altered(engine: &Engine, input: &str) -> Option<String> {
    first_changed(engine, input, |c| c.spelling).map(|(typed, read)| format!("打到 {typed} 时被读成了 {read}"))
}

/// 输入框显示的（preedit）就是打的字母，纠过错的也一样，否则返回第一处不一样的地方。
fn first_misshown(engine: &Engine, input: &str) -> Option<String> {
    first_changed(engine, input, |c| c.preedit).map(|(typed, shown)| format!("打到 {typed} 时显示成了 {shown}"))
}

/// 照着打的那一类，每条只报第一处。
fn altered_prefixes(engine: &Engine, cases: &[Case]) -> Vec<String> {
    cases.iter().filter(|case| case.category == SPELLED).filter_map(|case| first_altered(engine, &case.input)).collect()
}

/// 按最常用的字给音节排序，取前 COMMON_SYLLABLES 个两两拼接。返回（输入个数, 被改掉的）。
fn altered_pairs(engine: &Engine) -> (usize, Vec<String>) {
    let dict = engine.dictionary();
    let table = dict.table();
    let mut ranked: Vec<(f64, &str)> = (0..table.len())
        .filter_map(|index| {
            let id = SylId::try_from(index).ok()?;
            let best = dict.exact(&[id])?.next()?;
            Some((dict.cost(best), table.name(id)))
        })
        .collect();
    ranked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let common: Vec<&str> = ranked.into_iter().take(COMMON_SYLLABLES).map(|(_, name)| name).collect();
    let altered = common
        .iter()
        .flat_map(|first| common.iter().map(move |second| format!("{first}{second}")))
        .filter_map(|input| first_altered(engine, &input))
        .collect();
    (common.len() * common.len(), altered)
}

pub fn run(ctx: &mut Context) -> Outcome {
    let loaded = match ctx.engine() {
        Ok(loaded) => loaded,
        Err(err) => return Outcome::fail(err),
    };
    let cases = match load(&ctx.root.join("eval/pinyin.tsv")) {
        Ok(cases) => cases,
        Err(err) => return Outcome::fail(err),
    };
    let result = evaluate(&loaded.engine, &cases);
    let gates = &ctx.gates.engine;

    let misshown: Vec<String> = cases.iter().filter_map(|case| first_misshown(&loaded.engine, &case.input)).collect();
    let altered = altered_prefixes(&loaded.engine, &cases);
    let (pairs, altered_pairs) = altered_pairs(&loaded.engine);
    let altered_percent = 100.0 * altered_pairs.len() as f64 / pairs.max(1) as f64;
    let mut problems = Vec::new();
    if !result.misses.is_empty() {
        problems.push(format!("{} 条没打对", result.misses.len()));
    }
    if !misshown.is_empty() {
        problems.push(format!("{} 条输入打到一半时输入框显示的不是打的字母", misshown.len()));
    }
    if !altered.is_empty() {
        problems.push(format!("{} 条照着打的输入打到一半时拼音被改了", altered.len()));
    }
    if altered_percent > gates.max_altered_percent {
        problems.push(format!(
            "常用音节拼接有 {altered_percent:.2}% 打到一半拼音被改了，超过门槛 {}%",
            gates.max_altered_percent
        ));
    }
    if util::as_ms(result.slowest) > gates.max_convert_ms {
        problems.push(format!("单条最慢 {}，超过门槛 {}ms", util::millis(result.slowest), gates.max_convert_ms));
    }
    if util::as_ms(loaded.load_time) > gates.max_load_ms {
        problems.push(format!("词库载入 {}，超过门槛 {}ms", util::millis(loaded.load_time), gates.max_load_ms));
    }

    let mut details: Vec<String> =
        result.tally.iter().map(|(category, hit, total)| format!("{category} {hit}/{total}")).collect();
    details.push(format!("单条最慢 {}（门槛 {}ms）", util::millis(result.slowest), gates.max_convert_ms));
    details.push(format!(
        "词库 {} 条，载入 {}（门槛 {}ms）",
        loaded.entries,
        util::millis(loaded.load_time),
        gates.max_load_ms
    ));
    if !result.misses.is_empty() {
        details.push("没打对的：".into());
        details.extend(result.misses.iter().map(|miss| format!("  {miss}")));
    }
    if !misshown.is_empty() {
        details.push("输入框显示的不是打的字母：".into());
        details.extend(misshown.iter().map(|line| format!("  {line}")));
    }
    if !altered.is_empty() {
        details.push("打到一半拼音被改的：".into());
        details.extend(altered.iter().map(|line| format!("  {line}")));
    }
    details.push(format!(
        "常用音节两两拼接 {pairs} 个，{} 个打到一半拼音被改（{altered_percent:.2}%，门槛 {}%）",
        altered_pairs.len(),
        gates.max_altered_percent
    ));
    // 同一个前缀会让好几个拼接一起出错（zi'a、zi'an 都卡在 zia），只列一次
    let mut shown: Vec<&String> = altered_pairs.iter().collect();
    shown.sort();
    shown.dedup();
    details.extend(shown.into_iter().take(10).map(|line| format!("  {line}")));

    let pending = ctx.root.join("eval/pending.tsv");
    if pending.exists() {
        match load(&pending) {
            Ok(cases) => {
                let result = evaluate(&loaded.engine, &cases);
                details.push(format!("待做（eval/pending.tsv）：{}/{} 条已经能打对", result.hits.len(), cases.len()));
                details.extend(result.hits.iter().map(|hit| format!("  可以挪进 pinyin.tsv：{hit}")));
            }
            Err(err) => problems.push(err),
        }
    }

    if problems.is_empty() {
        let summary = format!(
            "{} 条全对，显示的都是打的字母，照着打的拼音不被改（常用拼接 {altered_percent:.2}%），单条最慢 {}，词库载入 {}",
            cases.len(),
            util::millis(result.slowest),
            util::millis(loaded.load_time)
        );
        Outcome::pass(summary).with_details(details)
    } else {
        Outcome::fail(problems.join("；")).with_details(details)
    }
}
