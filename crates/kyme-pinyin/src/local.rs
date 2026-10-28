//! 本地词库：自定义词库和导入的外部词库。
//!
//! 每行认几种常见写法；没写拼音的按主词库补上读音，没写词频的按「比较常用」算。和主词库共用音节表和
//! 代价尺度，组句时和用户词一样只进照着打的读法（见 engine.rs）：加了再多的词，打对的拼音也不会被纠错改掉。

use crate::dict::{Builder, Dictionary, TYPICAL_WEIGHT, body, weight};
use crate::syllable::SylId;

/// 导入的外部词库一律按这个词频：外部词库的词频尺度各不相同，搜狗细胞词库干脆没有。
pub const IMPORTED_WEIGHT: u32 = TYPICAL_WEIGHT;
/// 没写词频的词（多半在自定义词库里）按这个算：比自带词库里八成多的词常用。
const DEFAULT_WEIGHT: u32 = 10_000;
/// 补读音时，主词库里的词最多查几个字。
const MAX_PIECE_CHARS: usize = 8;
/// 补读音时每多切一段的惩罚，偏向整词：「银行」按整词读 yin hang，不拆成「银」「行」读 yin xing。
const PIECE_PENALTY: f64 = 1.5;

/// 本地词库里的一行。
#[derive(Debug, PartialEq)]
pub struct Row<'a> {
    pub text: &'a str,
    /// 每个音节一项，照写的原样，没写拼音是空的。
    pub pinyin: Vec<&'a str>,
    pub weight: Option<u32>,
}

/// 读一行，认这几种写法：
/// - Rime 的 `词<Tab>拼音<Tab>词频`，音节用空格隔开，后两列可以不写；
/// - 用空格隔开的「词 拼音」「拼音 词 词频」、搜狗导出的 `'pin'yin 词`：带汉字的一项是词，纯数字是词频，
///   其余是拼音，音节用空格或 `'` 隔开。
///
/// 空行、`#` 开头的注释和没有汉字的行返回 None。
pub fn parse_line(line: &str) -> Option<Row<'_>> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    if line.contains('\t') {
        let mut cols = line.split('\t');
        let text = cols.next()?.trim();
        let pinyin = syllables(cols.next().unwrap_or("")).collect();
        let weight = cols.next().and_then(weight);
        return (!text.is_empty()).then_some(Row { text, pinyin, weight });
    }
    let mut row = Row { text: "", pinyin: Vec::new(), weight: None };
    for token in line.split_whitespace() {
        if !token.is_ascii() {
            if row.text.is_empty() {
                row.text = token;
            }
        } else if let Some(weight) = weight(token) {
            row.weight = Some(weight);
        } else {
            row.pinyin.extend(syllables(token));
        }
    }
    (!row.text.is_empty()).then_some(row)
}

/// 拼音里的各个音节：用空格或 `'` 隔开。
pub(crate) fn syllables(code: &str) -> impl Iterator<Item = &str> {
    code.split([' ', '\'']).filter(|syl| !syl.is_empty())
}

/// 一个词库文件里的全部词条：有 YAML 头就去掉，再逐行读。
pub fn rows(content: &str) -> impl Iterator<Item = Row<'_>> {
    body(content).lines().filter_map(parse_line)
}

/// 把这些词库文件建成本地词库。读音按 `reading` 定，定不出来的词不要。
pub(crate) fn build(main: &Dictionary, sources: &[&str]) -> Dictionary {
    let mut words = Builder::default();
    for row in sources.iter().flat_map(|source| rows(source)) {
        if let Some(ids) = reading(main, row.text, &row.pinyin) {
            words.push(row.text, &ids, row.weight.unwrap_or(DEFAULT_WEIGHT));
        }
    }
    main.sibling(words)
}

/// 定本地词的读音：写了拼音的照用，每个字一个音节，音节没隔开的按字数切开；没写拼音、怎么切都对不上的，
/// 按主词库补。补不出来是 None。
pub(crate) fn reading(dict: &Dictionary, text: &str, pinyin: &[&str]) -> Option<Vec<SylId>> {
    if !pinyin.is_empty()
        && let Some(ids) = split(dict, text, pinyin)
    {
        return Some(ids);
    }
    fill(dict, text)
}

/// 把写的拼音切成和字数一样多的音节。空格或 `'` 隔开的每一段里可以有几个音节，但音节不跨段。
/// 切法不止一种时，挑和每个字在主词库里的读音对得上最多的：「西安」写成 xian 是 xi an。
fn split(dict: &Dictionary, text: &str, pinyin: &[&str]) -> Option<Vec<SylId>> {
    let table = dict.table();
    let syllable = |spelling: &str| table.exact(spelling).or_else(|| table.exact(&spelling.to_ascii_lowercase()));
    // 常见的情况：一段一个音节，正好每个字一个
    let count = text.chars().count();
    if pinyin.len() == count
        && let Some(ids) = pinyin.iter().map(|syl| syllable(syl)).collect()
    {
        return Some(ids);
    }
    let letters = pinyin.concat().to_ascii_lowercase();
    if !letters.bytes().all(|b| b.is_ascii_lowercase()) {
        return None;
    }
    let chars: Vec<&str> = text.char_indices().map(|(at, c)| &text[at..at + c.len_utf8()]).collect();
    // 每一段在哪里结束：音节不能跨过这些位置
    let ends: Vec<usize> = pinyin.iter().scan(0, |at, syl| Some(*at + syl.len()).inspect(|end| *at = *end)).collect();
    let crosses = |start: usize, end: usize| ends.iter().any(|&at| start < at && at < end);
    // best[at][k]：前 at 个字母切成 k 个音节时，读音对得上的字最多几个，和最后一个音节从哪里开始、是哪个
    let len = letters.len();
    let mut best: Vec<Vec<Option<(usize, usize, SylId)>>> = vec![vec![None; count + 1]; len + 1];
    best[0][0] = Some((0, 0, 0));
    for start in 0..len {
        for k in 0..count {
            let Some((score, _, _)) = best[start][k] else { continue };
            for end in start + 1..=len.min(start + table.max_len()) {
                if crosses(start, end) {
                    break;
                }
                let Some(id) = table.exact(&letters[start..end]) else { continue };
                let score = score + usize::from(dict.find(chars[k], &[id]).is_some());
                if best[end][k + 1].is_none_or(|(old, _, _)| score > old) {
                    best[end][k + 1] = Some((score, start, id));
                }
            }
        }
    }
    let mut ids = vec![0; count];
    let mut at = len;
    for k in (0..count).rev() {
        let (_, start, id) = best[at][k + 1]?;
        ids[k] = id;
        at = start;
    }
    Some(ids)
}

/// 按主词库补读音：把词切成主词库里有的词，取代价最小的切法，整词有就是整词。
/// 同一个写法有几个读音的（行 xing、hang），取最常用的那个。
fn fill(dict: &Dictionary, text: &str) -> Option<Vec<SylId>> {
    // 每个字的起点，最后加上结尾；cost[i]、back[i] 是读到第 i 个字为止的最小代价和最后一段
    let bounds: Vec<usize> = text.char_indices().map(|(at, _)| at).chain([text.len()]).collect();
    let chars = bounds.len() - 1;
    let mut cost = vec![f64::INFINITY; chars + 1];
    let mut back: Vec<Option<(usize, u32)>> = vec![None; chars + 1];
    cost[0] = 0.0;
    for start in 0..chars {
        if cost[start].is_infinite() {
            continue;
        }
        for end in start + 1..=chars.min(start + MAX_PIECE_CHARS) {
            if let Some(entry) = dict.find_text(&text[bounds[start]..bounds[end]]) {
                let total = cost[start] + dict.cost(entry) + PIECE_PENALTY;
                if total < cost[end] {
                    cost[end] = total;
                    back[end] = Some((start, entry));
                }
            }
        }
    }
    let mut pieces = Vec::new();
    let mut end = chars;
    while end > 0 {
        let (start, entry) = back[end]?;
        pieces.push(entry);
        end = start;
    }
    Some(pieces.iter().rev().flat_map(|&entry| dict.syllables(entry).iter().copied()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_common_line_formats() {
        let row = |text, pinyin: &[&'static str], weight| Some(Row { text, pinyin: pinyin.to_vec(), weight });
        assert_eq!(parse_line("鲲鹏\tkun peng\t100"), row("鲲鹏", &["kun", "peng"], Some(100)), "Rime");
        assert_eq!(parse_line("鲲鹏\tkun peng"), row("鲲鹏", &["kun", "peng"], None));
        assert_eq!(parse_line("鲲鹏"), row("鲲鹏", &[], None), "只写词");
        assert_eq!(parse_line("'kun'peng 鲲鹏"), row("鲲鹏", &["kun", "peng"], None), "搜狗导出的");
        assert_eq!(parse_line("kun'peng 鲲鹏 1"), row("鲲鹏", &["kun", "peng"], Some(1)), "QQ 导出的");
        assert_eq!(parse_line("南宁 nan ning"), row("南宁", &["nan", "ning"], None), "nan 是音节，不是数字");
        assert_eq!(parse_line("# 注释"), None);
        assert_eq!(parse_line("name: custom"), None, "YAML 头里没有汉字的行");
    }

    #[test]
    fn missing_pinyin_is_filled_from_the_main_dictionary() {
        let main =
            Dictionary::from_rime_sources(&["行\txing\t1000\n行\thang\t100\n银\tyin\t500\n银行\tyin hang\t800\n\
                                                   曾\tceng\t900\n曾\tzeng\t100\n"]);
        let local = build(&main, &["银行行长\n曾行\tzeng xing\n银行\tyin\n"]);
        let read = |entry| local.syllables(entry).iter().map(|&id| main.table().name(id)).collect::<Vec<_>>();
        let reading = |text| (0..local.len() as u32).filter(|&e| local.text(e) == text).map(read).collect::<Vec<_>>();
        assert_eq!(reading("银行行长"), Vec::<Vec<&str>>::new(), "「长」主词库里没有，补不出来就不要");
        assert_eq!(reading("曾行"), [["zeng", "xing"]], "写了拼音就照用");
        assert_eq!(reading("银行"), [["yin", "hang"]], "拼音和字数对不上，按整词补");
    }

    #[test]
    fn unseparated_pinyin_is_split_by_the_number_of_characters() {
        let main = Dictionary::from_rime_sources(&["西\txi\t1000\n安\tan\t1000\n先\txian\t1000\n下\txia\t1000\n\
                                                   嗯\tn\t10\n曾\tceng\t900\n曾\tzeng\t100\n小\txiao\t900\n"]);
        let read = |text, pinyin: &[&str]| Some(main.table().spell(&reading(&main, text, pinyin)?));
        assert_eq!(read("西安", &["xian"]).as_deref(), Some("xi an"), "不切成 xia n");
        assert_eq!(read("曾小", &["zengxiao"]).as_deref(), Some("zeng xiao"));
        assert_eq!(read("曾小", &["Zeng", "Xiao"]).as_deref(), Some("zeng xiao"), "大写也认");
        assert_eq!(read("曾小", &["zen", "gxiao"]).as_deref(), Some("ceng xiao"), "音节不跨段，切不开就按主词库补");
    }
}
