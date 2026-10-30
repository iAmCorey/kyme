//! 拼音评测：eval/pinyin.tsv 里每一条的首选都要对，单条转换和词库载入的耗时不能超过门槛。
//!
//! 每一条都从第一个字母打到最后一个，每一步输入框显示的（preedit）都要是打的字母，纠过错的也一样。
//! 「本来就对」这一类是照着打的输入，还要求每一步引擎理解的拼音（spelling）也和打的字母一样，
//! 也就是没被当成打错。同样的要求再放到最常用的音节两两拼接上（那按、下岸……），被改掉的比例不能超过门槛。
//! 再带上一万个故意捣乱的本地词和用户词跑一遍：被改的不能变多，单条转换也不能慢过门槛。
//!
//! eval/pending.tsv（可选，格式相同）放暂时还做不到的用例：只报告不卡发版，哪条做到了会提示挪过去。

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use kyme_core::pinyin::{Candidate, Conversion, Engine, MAX_WORDS, SylId, SyllableTable, UserDict, UserWord};
use kyme_core::{EXTRA_DICTS, InputSettings};

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
    let rows = util::tsv(path, "类别、输入、期望三列")?;
    Ok(rows.into_iter().map(|[category, input, expected]| Case { category, input, expected }).collect())
}

struct Evaluation {
    /// （类别, 打对的, 总数），按类别在文件里第一次出现的顺序。
    tally: Vec<(String, usize, usize)>,
    hits: Vec<String>,
    misses: Vec<String>,
    slowest: Duration,
}

fn evaluate(engine: &Engine, user: &UserDict, cases: &[Case]) -> Evaluation {
    let mut result = Evaluation { tally: Vec::new(), hits: Vec::new(), misses: Vec::new(), slowest: Duration::ZERO };
    for Case { category, input, expected } in cases {
        let started = Instant::now();
        let conversion = engine.convert_with(input, user);
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
/// 故意捣乱的本地词库和用户词库都用前 100 个常用音节两两拼成，各一万个词。
const ADVERSARIAL_SYLLABLES: usize = 100;
/// 看拼接的读法用前 150 个常用音节，两万多个输入，每个只转换一次（wen 排第 99、gan 第 115，wengan 也在里面）。
const READING_SYLLABLES: usize = 150;

/// 从第一个字母打到最后一个，每一步从转换结果里取一份拼音，去掉分隔符后都要和已经打的字母一样。
/// 返回第一处不一样的地方：已经打的字母，和那份拼音。
fn first_changed<'a>(
    engine: &Engine,
    user: &UserDict,
    input: &'a str,
    pinyin: fn(Conversion) -> String,
) -> Option<(&'a str, String)> {
    (1..=input.len()).map(|end| &input[..end]).find_map(|typed| {
        let got = pinyin(engine.convert_with(typed, user));
        (got.replace('\'', "") != typed.replace('\'', "")).then_some((typed, got))
    })
}

/// 引擎理解的拼音（spelling）没改打的字母，否则返回第一处被改的地方。
fn first_altered(engine: &Engine, user: &UserDict, input: &str) -> Option<String> {
    first_changed(engine, user, input, |c| c.spelling).map(|(typed, read)| format!("打到 {typed} 时被读成了 {read}"))
}

/// 输入框显示的（preedit）就是打的字母，纠过错的也一样，否则返回第一处不一样的地方。
fn first_misshown(engine: &Engine, input: &str) -> Option<String> {
    first_changed(engine, &UserDict::empty(), input, |c| c.preedit)
        .map(|(typed, shown)| format!("打到 {typed} 时显示成了 {shown}"))
}

/// 照着打的那一类，每条只报第一处。
fn altered_prefixes(engine: &Engine, user: &UserDict, cases: &[Case]) -> Vec<String> {
    cases
        .iter()
        .filter(|case| case.category == SPELLED)
        .filter_map(|case| first_altered(engine, user, &case.input))
        .collect()
}

/// 按最常用的字给音节排序，取前 `count` 个。
fn common_syllables(engine: &Engine, count: usize) -> Vec<&str> {
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
    ranked.into_iter().take(count).map(|(_, name)| name).collect()
}

/// 前 `count` 个常用音节两两拼接，每一对是（前一个, 后一个）。
fn syllable_pairs(engine: &Engine, count: usize) -> Vec<[&str; 2]> {
    let common = common_syllables(engine, count);
    common.iter().flat_map(|first| common.iter().map(move |second| [*first, *second])).collect()
}

/// 前 COMMON_SYLLABLES 个常用音节两两拼接，逐个字母打。返回（输入个数, 被改掉的）。
fn altered_pairs(engine: &Engine, user: &UserDict) -> (usize, Vec<String>) {
    let pairs = syllable_pairs(engine, COMMON_SYLLABLES);
    let altered =
        pairs.iter().filter_map(|[first, second]| first_altered(engine, user, &format!("{first}{second}"))).collect();
    (pairs.len(), altered)
}

/// 常用音节两两拼接时想打的那种读法。拼出来的字母常常还能切成别的读法（wen'gan 也能读成 weng'an「瓮安」），
/// 首选是哪种看词频，但想打的那种一定要选得到；首选也不能把打全了的音节拆成简拼（dui'shuo 读成 dui's'huo「对生活」）。
struct Readings {
    /// 拼接的个数。
    inputs: usize,
    /// 首选是别的读法的拼接个数。
    other_first: usize,
    /// 其中想打的读法在默认的第一页里的个数。
    first_page: usize,
    unreachable: Vec<String>,
    split: Vec<String>,
}

/// 候选的读音是不是 `wanted` 开头的几个音节：选了它，剩下的字母照样能按 `wanted` 打完。
fn starts_reading(table: &SyllableTable, candidate: &Candidate, wanted: &[SylId]) -> bool {
    let count = candidate.syllables.len();
    count > 0
        && wanted.starts_with(&candidate.syllables)
        && candidate.consumed == wanted[..count].iter().map(|&id| table.name(id).len()).sum::<usize>()
}

/// 前 READING_SYLLABLES 个常用音节两两拼接，看想打的读法选不选得到、首选有没有把音节拆成简拼。
fn pair_readings(engine: &Engine) -> Readings {
    let table = engine.dictionary().table();
    let page = InputSettings::default().page_size;
    let pairs = syllable_pairs(engine, READING_SYLLABLES);
    let mut result =
        Readings { inputs: pairs.len(), other_first: 0, first_page: 0, unreachable: Vec::new(), split: Vec::new() };
    for [first, second] in pairs {
        let input = format!("{first}{second}");
        let wanted: Vec<SylId> = [first, second].iter().filter_map(|name| table.exact(name)).collect();
        let conversion = engine.convert(&input);
        // 最后一段可以是没打完的音节，前面的都得是打全了的
        let parts: Vec<&str> = conversion.spelling.split('\'').collect();
        if parts[..parts.len() - 1].iter().any(|part| table.exact(part).is_none()) {
            result.split.push(format!("{input} 读成了 {}「{}」", conversion.spelling, conversion.candidates[0].text));
        }
        if conversion.candidates[0].syllables == wanted {
            continue;
        }
        result.other_first += 1;
        match conversion.candidates.iter().position(|c| starts_reading(table, c, &wanted)) {
            Some(rank) if rank < page => result.first_page += 1,
            Some(_) => {}
            None => result.unreachable.push(format!("{input} 选不到 {first}'{second}")),
        }
    }
    result
}

/// 前 100 个常用音节两两拼成的一万个读音。
fn adversarial_pairs(engine: &Engine) -> Vec<[&str; 2]> {
    syllable_pairs(engine, ADVERSARIAL_SYLLABLES).into_iter().take(MAX_WORDS).collect()
}

/// 带一万个故意捣乱的本地词的引擎：每个读音一个词，词频高得离谱，放两层，和本地词库、扩展词库都在时一样。
/// 附加的词库都只进照着打的读法，带上它们，打对的拼音只会更少被改；转换也不能因此慢过门槛。
fn adversarial_local(engine: &Engine) -> Engine {
    let source: String = adversarial_pairs(engine)
        .into_iter()
        .map(|[first, second]| format!("搅局\t{first} {second}\t100000000\n"))
        .collect();
    let layer = engine.layer(&[&source]);
    engine.with_layers(vec![layer.clone(), layer])
}

/// 带上扩展词库跑一遍评测集的结果。
struct Extra {
    /// 校验值和 kyme-core 里登记的对不上的扩展词库。
    mismatched: Vec<&'static str>,
    words: usize,
    /// 建扩展词库这一层的耗时。
    build: Duration,
    /// 照着打的输入被改的条数。
    altered: usize,
    slowest: Duration,
}

/// 设置窗口能下载的扩展词库全部载入，一个文件一层，和设置里全都打开一样，再跑一遍评测集：扩展词库只进照着打的
/// 读法，打对的拼音不能因此被改；词多了，单条转换也不能慢过门槛。用的是 fetch-dict.sh 下到 data/rime-ice-extra/
/// 的副本，它得和设置窗口下载、核对的是同一份：校验值对不上，就是改了雾凇的版本没改登记的校验值。没有副本是 None。
fn check_extra(engine: &Engine, root: &Path, cases: &[Case]) -> Option<Extra> {
    let dir = root.join("data/rime-ice-extra");
    let started = Instant::now();
    let (mut layers, mut mismatched) = (Vec::new(), Vec::new());
    for dict in &EXTRA_DICTS {
        let Ok(text) = fs::read_to_string(dir.join(dict.file())) else { continue };
        if !dict.verify(text.as_bytes()) {
            mismatched.push(dict.name);
        }
        layers.push(engine.layer(&[&text]));
    }
    if layers.is_empty() {
        return None;
    }
    let build = started.elapsed();
    let words = layers.iter().map(|layer| layer.len()).sum();
    let engine = engine.with_layers(layers);
    let no_user = UserDict::empty();
    Some(Extra {
        mismatched,
        words,
        build,
        altered: altered_prefixes(&engine, &no_user, cases).len(),
        slowest: evaluate(&engine, &no_user, cases).slowest,
    })
}

/// 一万个故意捣乱的用户词：每个读音一个词，都选过很多次、加分到顶。道理同上。
fn adversarial_user(engine: &Engine) -> UserDict {
    let table = engine.dictionary().table();
    let words = adversarial_pairs(engine)
        .into_iter()
        .filter_map(|pair| {
            let syllables = pair.iter().map(|name| table.exact(name)).collect::<Option<Vec<_>>>()?;
            Some(UserWord { text: "捣乱".into(), syllables, count: 1000.0, day: 0 })
        })
        .collect();
    UserDict::new(engine, words)
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
    let no_user = UserDict::empty();
    let result = evaluate(&loaded.engine, &no_user, &cases);
    let gates = &ctx.gates.engine;

    let misshown: Vec<String> = cases.iter().filter_map(|case| first_misshown(&loaded.engine, &case.input)).collect();
    let crowded = adversarial_local(&loaded.engine);
    let user = adversarial_user(&crowded);
    let altered = altered_prefixes(&loaded.engine, &no_user, &cases);
    let altered_with_user = altered_prefixes(&crowded, &user, &cases).len();
    // 带不带捣乱的词的两轮常用拼接各打 6400 个输入，再看一遍拼接的读法、带上扩展词库跑一遍，放在四个线程里同时跑
    let root = &ctx.root;
    let ((pairs, altered_pairs), (_, pairs_with_user), readings, extra) = std::thread::scope(|scope| {
        let with_user = scope.spawn(|| altered_pairs(&crowded, &user));
        let readings = scope.spawn(|| pair_readings(&loaded.engine));
        let extra = scope.spawn(|| check_extra(&loaded.engine, root, &cases));
        let without = altered_pairs(&loaded.engine, &no_user);
        (
            without,
            with_user.join().expect("带捣乱的词的那一轮崩了"),
            readings.join().expect("看拼接读法的那一轮崩了"),
            extra.join().expect("带扩展词库的那一轮崩了"),
        )
    });
    let altered_percent = 100.0 * altered_pairs.len() as f64 / pairs.max(1) as f64;
    let slowest_with_user = evaluate(&crowded, &user, &cases).slowest;
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
    if !readings.unreachable.is_empty() {
        problems.push(format!("常用音节拼接有 {} 个选不到想打的读法", readings.unreachable.len()));
    }
    if !readings.split.is_empty() {
        problems.push(format!("常用音节拼接有 {} 个首选把打全了的音节拆成了简拼", readings.split.len()));
    }
    if let Some(extra) = &extra {
        if !extra.mismatched.is_empty() {
            problems.push(format!(
                "{} 和 kyme-core 的 extra.rs 里登记的校验值不一致：改了雾凇的版本，校验值要跟着改",
                extra.mismatched.join("、")
            ));
        }
        if extra.altered > altered.len() {
            problems.push("带上扩展词库后，打对的拼音被改得更多了".into());
        }
        if util::as_ms(extra.slowest) > gates.max_convert_ms {
            problems.push(format!(
                "带扩展词库时单条最慢 {}，超过门槛 {}ms",
                util::millis(extra.slowest),
                gates.max_convert_ms
            ));
        }
    }
    if altered_with_user > altered.len() || pairs_with_user.len() > altered_pairs.len() {
        problems.push("带上本地词库和用户词库后，打对的拼音被改得更多了".into());
    }
    if util::as_ms(slowest_with_user) > gates.max_convert_ms {
        problems.push(format!(
            "带一万个本地词和用户词时单条最慢 {}，超过门槛 {}ms",
            util::millis(slowest_with_user),
            gates.max_convert_ms
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
    details.push(format!(
        "带一万个捣乱的本地词和用户词：常用拼接被改 {} 个，照着打的输入被改 {} 条，单条最慢 {}",
        pairs_with_user.len(),
        altered_with_user,
        util::millis(slowest_with_user)
    ));
    details.push(match &extra {
        Some(extra) => format!(
            "带上扩展词库（{} 个词，建好用时 {}）：照着打的输入被改 {} 条，单条最慢 {}",
            extra.words,
            util::millis(extra.build),
            extra.altered,
            util::millis(extra.slowest)
        ),
        None => "没有扩展词库的副本（data/rime-ice-extra/），这一项没跑".into(),
    });
    // 同一个前缀会让好几个拼接一起出错（zi'a、zi'an 都卡在 zia），只列一次
    let mut shown: Vec<&String> = altered_pairs.iter().collect();
    shown.sort();
    shown.dedup();
    details.extend(shown.into_iter().take(10).map(|line| format!("  {line}")));
    details.push(format!(
        "前 {READING_SYLLABLES} 个常用音节两两拼接 {} 个，{} 个首选是别的读法：想打的读法选不到 {} 个，在第一页 {} 个；\
         首选把音节拆成简拼 {} 个",
        readings.inputs,
        readings.other_first,
        readings.unreachable.len(),
        readings.first_page,
        readings.split.len()
    ));
    details.extend(readings.unreachable.iter().chain(&readings.split).take(10).map(|line| format!("  {line}")));

    let pending = ctx.root.join("eval/pending.tsv");
    if pending.exists() {
        match load(&pending) {
            Ok(cases) => {
                let result = evaluate(&loaded.engine, &no_user, &cases);
                details.push(format!("待做（eval/pending.tsv）：{}/{} 条已经能打对", result.hits.len(), cases.len()));
                details.extend(result.hits.iter().map(|hit| format!("  可以挪进 pinyin.tsv：{hit}")));
            }
            Err(err) => problems.push(err),
        }
    }

    if problems.is_empty() {
        let summary = format!(
            "{} 条全对，显示的都是打的字母，照着打的拼音不被改（常用拼接 {altered_percent:.2}%），常用拼接的每种读法都选得到，单条最慢 {}，词库载入 {}",
            cases.len(),
            util::millis(result.slowest),
            util::millis(loaded.load_time)
        );
        Outcome::pass(summary).with_details(details)
    } else {
        Outcome::fail(problems.join("；")).with_details(details)
    }
}
