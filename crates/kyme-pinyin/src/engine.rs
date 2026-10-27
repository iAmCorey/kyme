//! 拼音 → 候选：音节切分（含简拼、末尾没打完的音节、打字纠错）、查词、按一元词频组整句。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::dict::{Dictionary, LoadError, read_file};
use crate::emoji::Emoji;
use crate::syllable::{SylId, SylSet, SyllableTable};

/// 一个词最多跨几个音节。
const MAX_WORD_SYLS: usize = 8;
/// 组句时每多切出一个词的惩罚，用来偏向长词。
const WORD_PENALTY: f64 = 1.5;
/// 简拼（只打声母）时每个音节的惩罚。
const ABBR_PENALTY: f64 = 1.5;
/// 简拼的字母本身就是音节（a、e、o）时，按简拼理解要罚得更重。
const ABBR_SYLLABLE_PENALTY: f64 = 3.0;
/// 末尾音节还没打完时的惩罚。
const PARTIAL_PENALTY: f64 = 1.0;
/// 末尾已经是完整音节、却按更长的音节去联想时的惩罚。
const PARTIAL_COMPLETE_PENALTY: f64 = 4.0;
/// 各种打字错误按纠正后的音节算，再加这么多惩罚。
const SWAP_PENALTY: f64 = 2.5;
const CROSS_SWAP_PENALTY: f64 = 3.0;
/// 按错相邻键最容易误伤正常输入（把简拼 shj 改成 shi），罚得最重。
const NEIGHBOR_PENALTY: f64 = 4.0;
const MISSING_PENALTY: f64 = 3.0;
/// 同一个键连按了两次（nihhao、zhoong）。随便多出一个别的字母的情况不纠：
/// 它和简拼、正常输入太容易混（nhao 会被当成 hao）。
const DOUBLED_PENALTY: f64 = 2.0;
/// 连按的两下跨在两个音节之间（womenndou）。nn、gg 后面去掉一个字母常常正好是 an、eng 这种元音音节，
/// 容易把 zhanneng 读成 zhan'eng；可罚到 3.0 又纠不回 womenndou，所以和同一个音节里罚得一样。
const DOUBLED_ACROSS_PENALTY: f64 = 2.0;
/// 纠错得来的整句，要比照着打的读法好出这么多才用：手误本来就少见，打对了的拼音不能被「纠」成别的。
/// 太小会改掉打对了的输入（naan 是「那按」，读成 nan「难」只好 5.3；打到一半的 naann 好 6.0）；
/// 太大会纠不回明显的手误（xiaang 读成 xiang「想」比照着打的 xia'ang「下昂」好 10.6）。
/// 最后一个音节没打完时也用这个值：打到一半是每按一个键都有的状态，比最后一下按错常见得多，
/// 从宽会让拼音在打字过程中跳来跳去（打到 lain 显示成 lian，再打一个 a 又变回 lai'na）。
const TYPO_MARGIN: f64 = 7.0;
/// 拼音里的元音字母。一个元音都没有的输入是在打简拼（rgzn），不做纠错。
const VOWELS: &[u8] = b"aeiouv";
/// 认不出的字母原样保留的代价，只在别无选择时才会走到。
const RAW_COST: f64 = 40.0;
/// 组句时每一段取几个词参与比较。
const LATTICE_WIDTH: usize = 3;
/// 候选列表的上限。
const MAX_CANDIDATES: usize = 60;

#[derive(Clone, Copy, Debug)]
enum Constraint {
    /// 打全了的音节。
    Exact(SylId),
    /// 简拼或末尾没打完的音节：以某个前缀开头的任意音节。
    Prefix { set: SylSet, initial: u8 },
}

impl Constraint {
    fn matches(&self, id: SylId) -> bool {
        match self {
            Constraint::Exact(exact) => *exact == id,
            Constraint::Prefix { set, .. } => set.contains(id),
        }
    }

    fn initial(&self, table: &SyllableTable) -> u8 {
        match self {
            Constraint::Exact(id) => table.initial(*id),
            Constraint::Prefix { initial, .. } => *initial,
        }
    }
}

/// 一个音节在输入串里占的位置。
#[derive(Clone, Copy, Debug)]
struct Seg {
    start: usize,
    end: usize,
    constraint: Constraint,
    penalty: f64,
    /// 纠错得来的音节，预编辑里显示纠正后的拼写。
    fixed: Option<SylId>,
}

impl Seg {
    fn exact(start: usize, end: usize, id: SylId) -> Self {
        Seg { start, end, constraint: Constraint::Exact(id), penalty: 0.0, fixed: None }
    }

    fn fixed(start: usize, end: usize, id: SylId, penalty: f64) -> Self {
        Seg { start, end, constraint: Constraint::Exact(id), penalty, fixed: Some(id) }
    }

    fn prefix(start: usize, end: usize, set: SylSet, initial: u8, penalty: f64) -> Self {
        Seg { start, end, constraint: Constraint::Prefix { set, initial }, penalty, fixed: None }
    }
}

/// 某个位置上的一种切法：通常是一个音节；跨两个音节打反时要两个音节一起改，所以是一对。
#[derive(Clone, Copy, Debug)]
struct Choice {
    first: Seg,
    second: Option<Seg>,
}

impl Choice {
    fn single(seg: Seg) -> Self {
        Choice { first: seg, second: None }
    }

    fn end(&self) -> usize {
        self.second.map_or(self.first.end, |seg| seg.end)
    }
}

/// 从某个位置开始、连续若干音节组成的一段，对应词库里的一个词。
#[derive(Clone, Debug)]
struct Span {
    end: usize,
    segs: Vec<Seg>,
    penalty: f64,
}

/// 全部是打全了的音节时，取出它们的 id。
fn exact_ids(segs: &[Seg]) -> Option<Vec<SylId>> {
    segs.iter()
        .map(|seg| match seg.constraint {
            Constraint::Exact(id) => Some(id),
            Constraint::Prefix { .. } => None,
        })
        .collect()
}

/// 切音节时允许哪些读法。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// 只认打全了的音节，最后一个可以没打完：「照着打的」读法，给纠错当参照。
    Literal,
    /// 再加上简拼。
    Plain,
    /// 再加上纠错。
    Corrected,
}

/// 某一种读法下的最优整句。
struct Solution {
    cost: f64,
    /// 整句用到了纠错。
    corrected: bool,
    /// 最后一处纠错改到哪个字节为止。
    last_fix_end: Option<usize>,
    /// 整句以没打完的音节结尾时，这个音节从哪个字节开始。
    unfinished_from: Option<usize>,
    conversion: Conversion,
}

/// `window` 本身不是音节时，只改一处就能变成的音节和对应的惩罚：
/// 相邻两个字母打反、按错相邻的键、漏了一个字母、同一个键连按了两次。
/// 最后一个字母不当成多打的：它更可能是下一个音节的开头（zhongg 是还没打完的 zhongguo）。
fn corrections(table: &SyllableTable, window: &str) -> Vec<(SylId, f64)> {
    let mut found: Vec<(SylId, f64)> = Vec::new();
    let mut add = |id: SylId, penalty: f64| match found.iter_mut().find(|(seen, _)| *seen == id) {
        Some(entry) => entry.1 = entry.1.min(penalty),
        None => found.push((id, penalty)),
    };
    // 拼音只有 ASCII 小写字母，改动后仍是合法 UTF-8
    let syllable = |letters: &[u8]| std::str::from_utf8(letters).ok().and_then(|s| table.exact(s));

    let mut letters = window.as_bytes().to_vec();
    for i in 0..letters.len() - 1 {
        if letters[i] != letters[i + 1] {
            letters.swap(i, i + 1);
            if let Some(id) = syllable(&letters) {
                add(id, SWAP_PENALTY);
            }
            letters.swap(i, i + 1);
        }
    }
    for i in 0..letters.len() {
        let typed = letters[i];
        for &key in neighbors(typed) {
            letters[i] = key;
            if let Some(id) = syllable(&letters) {
                add(id, NEIGHBOR_PENALTY);
            }
        }
        letters[i] = typed;
    }
    for &id in table.missing_letter(window) {
        add(id, MISSING_PENALTY);
    }
    if letters.len() >= 3 {
        // 末尾一串相同的字母（zhongg 的 gg）删哪个都等于删最后一个，一起跳过
        let last = letters[letters.len() - 1];
        let tail_run = letters.iter().rev().take_while(|&&c| c == last).count();
        for i in 0..letters.len() - tail_run {
            if letters[i] != letters[i + 1] {
                continue;
            }
            let mut shorter = letters.clone();
            shorter.remove(i);
            if let Some(id) = syllable(&shorter) {
                add(id, DOUBLED_PENALTY);
            }
        }
    }
    found
}

/// 跨两个音节打反（nhiao → ni'hao）：交换相邻两个字母后，正好在交换处分成两个音节。
/// 返回 (分界位置, 前一个音节, 后一个音节)。
fn cross_swaps(table: &SyllableTable, window: &str) -> Vec<(usize, SylId, SylId)> {
    let syllable = |letters: &[u8]| std::str::from_utf8(letters).ok().and_then(|s| table.exact(s));
    let mut letters = window.as_bytes().to_vec();
    let mut found = Vec::new();
    for split in 1..letters.len() {
        // 不交换也能在这里切成两个音节的话，用不着纠错
        let already = table.exact(&window[..split]).is_some() && table.exact(&window[split..]).is_some();
        if already || letters[split - 1] == letters[split] {
            continue;
        }
        letters.swap(split - 1, split);
        let (first, second) = letters.split_at(split);
        if let (Some(a), Some(b)) = (syllable(first), syllable(second)) {
            found.push((split, a, b));
        }
        letters.swap(split - 1, split);
    }
    found
}

/// QWERTY 键盘上和 `key` 相邻的字母键。
fn neighbors(key: u8) -> &'static [u8] {
    match key {
        b'q' => b"wa",
        b'w' => b"qeas",
        b'e' => b"wrsd",
        b'r' => b"etdf",
        b't' => b"ryfg",
        b'y' => b"tugh",
        b'u' => b"yihj",
        b'i' => b"uojk",
        b'o' => b"ipkl",
        b'p' => b"ol",
        b'a' => b"qwsz",
        b's' => b"awedzx",
        b'd' => b"serfxc",
        b'f' => b"drtgcv",
        b'g' => b"ftyhvb",
        b'h' => b"gyujbn",
        b'j' => b"huiknm",
        b'k' => b"jiolm",
        b'l' => b"kop",
        b'z' => b"asx",
        b'x' => b"zsdc",
        b'c' => b"xdfv",
        b'v' => b"cfgb",
        b'b' => b"vghn",
        b'n' => b"bhjm",
        b'm' => b"njk",
        _ => b"",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    /// 选中它会消耗掉输入串的前多少个字节。
    pub consumed: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversion {
    /// 按最优切分加了分隔符的拼音，例如 `ni'hao`。
    pub preedit: String,
    pub candidates: Vec<Candidate>,
}

/// 组句路径上的一步。
#[derive(Clone)]
enum Step {
    Separator,
    Raw,
    Word { entry: u32, segs: Vec<Seg> },
}

pub struct Engine {
    dict: Dictionary,
    emoji: Emoji,
}

impl Engine {
    pub fn new(dict: Dictionary) -> Self {
        Engine { dict, emoji: Emoji::default() }
    }

    /// 读一个数据目录：全部 `.dict.yaml`（按文件名排序）和 emoji 对照表 `emoji.txt`。
    /// 输入法读安装包里的 dict/，QA 读仓库的 data/rime-ice/，两处放的是同一套文件。
    pub fn load_dir(dir: &Path) -> Result<Self, LoadError> {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|source| LoadError::new(dir, source))?
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.to_string_lossy().ends_with(".dict.yaml"))
            .collect();
        paths.sort();
        let dict = Dictionary::load_rime(&paths)?;
        let emoji = Emoji::parse(&read_file(&dir.join("emoji.txt"))?);
        Ok(Engine::new(dict).with_emoji(emoji))
    }

    /// 带上 emoji 对照表。`convert` 不用它，由输入会话决定要不要把 emoji 放进候选。
    pub fn with_emoji(mut self, emoji: Emoji) -> Self {
        self.emoji = emoji;
        self
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dict
    }

    pub fn emoji(&self) -> &Emoji {
        &self.emoji
    }

    /// 把一串拼音（小写字母，可用 `'` 分隔）转成候选。第一个候选是整句最优解。
    pub fn convert(&self, input: &str) -> Conversion {
        if input.is_empty() {
            return Conversion::default();
        }
        // 一个元音都没有是在打简拼（rgzn），不纠错
        if !input.bytes().any(|b| VOWELS.contains(&b)) {
            return self.solve(input, Reading::Plain).conversion;
        }
        let best = self.solve(input, Reading::Corrected);
        if !best.corrected {
            return best.conversion;
        }
        // 用了纠错的话，要比照着打的读法明显更合理才算数（naanni 照着打是 na'an'ni「那按你」，
        // 不能被读成 nan'ni「南宁」）。拼不通的输入只能逐个字母原样保留，照着打的那边代价极高
        let literal = self.solve(input, Reading::Literal);
        if best.cost + TYPO_MARGIN < literal.cost {
            return best.conversion;
        }
        let mut plain = self.solve(input, Reading::Plain).conversion;
        // 照着打的读法以没打完的音节结尾、纠错改的又正是它：分不清是还没打完还是最后一下按错了
        // （suho 可能要接着打 suhong，也可能是想打「说」）。拼音照着打的显示，纠错的整句排第二，按 2 就能选
        let at_tail = matches!((literal.unfinished_from, best.last_fix_end), (Some(tail), Some(end)) if end > tail);
        if at_tail
            && let Some(fixed) = best.conversion.candidates.into_iter().next()
            && !plain.candidates.iter().any(|c| c.text == fixed.text)
        {
            plain.candidates.insert(plain.candidates.len().min(1), fixed);
        }
        plain
    }

    fn solve(&self, input: &str, reading: Reading) -> Solution {
        let bytes = input.as_bytes();
        let n = bytes.len();
        // 每个位置能切出的单个音节只算一次，各个起点的切分共用
        let options: Vec<Vec<Choice>> = (0..n).map(|pos| self.seg_options(input, pos, reading)).collect();

        // cost[i]：走到第 i 个字节的最小代价；back[i]：最后一步从哪儿来、走的是什么
        let mut cost = vec![f64::INFINITY; n + 1];
        let mut back: Vec<Option<(usize, Step)>> = vec![None; n + 1];
        cost[0] = 0.0;
        let mut spans_from_start = Vec::new();
        for i in 0..n {
            let here = cost[i];
            if here.is_infinite() {
                continue;
            }
            if bytes[i] == b'\'' {
                relax(&mut cost, &mut back, i, i + 1, here, || Step::Separator);
                continue;
            }
            let spans = self.spans_from(input, &options, i);
            for span in &spans {
                for (entry, word_cost) in self.lookup(span, LATTICE_WIDTH) {
                    let total = here + word_cost + span.penalty + WORD_PENALTY;
                    relax(&mut cost, &mut back, i, span.end, total, || Step::Word { entry, segs: span.segs.clone() });
                }
            }
            relax(&mut cost, &mut back, i, i + 1, here + RAW_COST, || Step::Raw);
            if i == 0 {
                spans_from_start = spans;
            }
        }

        // 每个位置都能靠 Raw 一步步走到，所以终点一定可达
        let total = cost[n];
        let mut steps = Vec::new();
        let mut end = n;
        while end > 0 {
            let (start, step) = back[end].take().expect("每个位置都可达");
            steps.push((start, end, step));
            end = start;
        }
        steps.reverse();

        // 沿最优路径拼出整句，同时给拼音加分隔符：纠过错的音节显示纠正后的拼写，
        // 用户自己打的分隔符原样保留
        let table = self.dict.table();
        let mut sentence = String::new();
        let mut preedit = String::with_capacity(n + 8);
        let mut after_syllable = false;
        let mut boundaries = HashSet::new();
        let mut sentence_corrected = false;
        let mut last_fix_end = None;
        let mut unfinished_from = None;
        for (start, end, step) in &steps {
            boundaries.insert(*end);
            match step {
                Step::Word { entry, segs } => {
                    sentence.push_str(self.dict.text(*entry));
                    for seg in segs {
                        boundaries.insert(seg.end);
                        sentence_corrected |= seg.fixed.is_some();
                        if seg.fixed.is_some() {
                            last_fix_end = Some(seg.end);
                        }
                        let unfinished = seg.end == n && matches!(seg.constraint, Constraint::Prefix { .. });
                        unfinished_from = unfinished.then_some(seg.start);
                        if after_syllable {
                            preedit.push('\'');
                        }
                        match seg.fixed {
                            Some(id) => preedit.push_str(table.name(id)),
                            None => preedit.push_str(&input[seg.start..seg.end]),
                        }
                        after_syllable = true;
                    }
                }
                Step::Raw => {
                    let raw = &input[*start..*end];
                    sentence.push_str(raw);
                    if after_syllable {
                        preedit.push('\'');
                    }
                    preedit.push_str(raw);
                    after_syllable = false;
                }
                Step::Separator => {
                    preedit.push('\'');
                    after_syllable = false;
                }
            }
        }

        // 其余候选：从开头起能组成的词，覆盖得越长越靠前，同样长度按代价排。
        // 只要在首选读法的音节边界上结束的（kaihui 不出「开花」，那会剩下打不出来的 ui）；
        // 首选没用到纠错，就说明打得没错，也不出纠错得来的词
        let mut words_from_start: Vec<(usize, f64, u32)> = Vec::new();
        for span in &spans_from_start {
            let corrected = span.segs.iter().any(|seg| seg.fixed.is_some());
            if !boundaries.contains(&span.end) || (corrected && !sentence_corrected) {
                continue;
            }
            for (entry, cost) in self.lookup(span, MAX_CANDIDATES) {
                words_from_start.push((span.end, cost + span.penalty, entry));
            }
        }
        words_from_start.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.total_cmp(&b.1)));

        let mut candidates = vec![Candidate { text: sentence, consumed: n }];
        let mut seen: HashSet<String> = candidates.iter().map(|c| c.text.clone()).collect();
        for (end, _, entry) in words_from_start {
            if candidates.len() >= MAX_CANDIDATES {
                break;
            }
            let text = self.dict.text(entry);
            if seen.insert(text.to_string()) {
                candidates.push(Candidate { text: text.to_string(), consumed: end });
            }
        }

        Solution {
            cost: total,
            corrected: sentence_corrected,
            last_fix_end,
            unfinished_from,
            conversion: Conversion { preedit, candidates },
        }
    }

    /// 从 `start` 出发能切出的所有音节序列（最多 MAX_WORD_SYLS 个音节）。
    fn spans_from(&self, input: &str, options: &[Vec<Choice>], start: usize) -> Vec<Span> {
        let mut spans = Vec::new();
        self.extend_spans(input, options, start, &mut Vec::new(), false, &mut spans);
        spans
    }

    fn extend_spans(
        &self,
        input: &str,
        options: &[Vec<Choice>],
        pos: usize,
        path: &mut Vec<Seg>,
        corrected: bool,
        spans: &mut Vec<Span>,
    ) {
        let bytes = input.as_bytes();
        let depth = path.len();
        for choice in &options[pos] {
            // 一个词里最多纠一处错，两处都错的情况很少见，放开会让切法成倍增加
            let fixes = choice.first.fixed.is_some();
            if corrected && fixes {
                continue;
            }
            path.push(choice.first);
            path.extend(choice.second);
            // 词库里没有以这几个音节开头的词，就不必再往下切
            if self.has_words_starting_with(path) {
                let end = choice.end();
                spans.push(Span { end, segs: path.clone(), penalty: path.iter().map(|s| s.penalty).sum() });
                let mut next = end;
                while next < bytes.len() && bytes[next] == b'\'' {
                    next += 1;
                }
                if next < bytes.len() && path.len() < MAX_WORD_SYLS {
                    self.extend_spans(input, options, next, path, corrected || fixes, spans);
                }
            }
            path.truncate(depth);
        }
    }

    fn has_words_starting_with(&self, segs: &[Seg]) -> bool {
        let table = self.dict.table();
        let initials: Vec<u8> = segs.iter().map(|seg| seg.constraint.initial(table)).collect();
        self.dict.has_abbr_prefix(&initials) && exact_ids(segs).is_none_or(|ids| self.dict.has_exact_prefix(&ids))
    }

    /// 在 `pos` 处按 `reading` 允许的各种切法：打全了的音节、纠错得来的音节、
    /// 末尾没打完的音节、简拼。
    fn seg_options(&self, input: &str, pos: usize, reading: Reading) -> Vec<Choice> {
        let table = self.dict.table();
        let bytes = input.as_bytes();
        let run_end = pos + bytes[pos..].iter().take_while(|b| b.is_ascii_lowercase()).count();
        let mut options = Vec::new();
        if run_end == pos {
            return options;
        }
        let max_len = table.max_len();

        for end in pos + 1..=run_end.min(pos + max_len) {
            if let Some(id) = table.exact(&input[pos..end]) {
                options.push(Choice::single(Seg::exact(pos, end, id)));
            }
        }

        if reading == Reading::Corrected {
            // 这一段本身不是音节，但改一处就能对上：打反、按错相邻键、漏一个字母、连按两次
            for end in pos + 2..=run_end.min(pos + max_len + 1) {
                let window = &input[pos..end];
                if table.exact(window).is_none() {
                    for (id, penalty) in corrections(table, window) {
                        options.push(Choice::single(Seg::fixed(pos, end, id, penalty)));
                    }
                }
            }

            // 跨在两个音节之间连按了两次（womenndou）：这一段的第一个字母是多出来的
            if pos > 0 && bytes[pos - 1] == bytes[pos] {
                for end in pos + 2..=run_end.min(pos + max_len + 1) {
                    if let Some(id) = table.exact(&input[pos + 1..end]) {
                        options.push(Choice::single(Seg::fixed(pos, end, id, DOUBLED_ACROSS_PENALTY)));
                    }
                }
            }

            // 跨两个音节打反：nhiao → ni'hao
            for end in pos + 3..=run_end.min(pos + 2 * max_len) {
                for (split, first, second) in cross_swaps(table, &input[pos..end]) {
                    options.push(Choice {
                        first: Seg::fixed(pos, pos + split, first, CROSS_SWAP_PENALTY),
                        second: Some(Seg::fixed(pos + split, end, second, 0.0)),
                    });
                }
            }
        }

        // 一段字母的末尾（输入结尾或分隔符前）可以是还没打完的音节
        if run_end - pos <= max_len {
            let tail = &input[pos..run_end];
            if let Some(set) = table.prefix(tail) {
                let complete = table.exact(tail).is_some();
                if !complete || set.len() > 1 {
                    let penalty = if complete { PARTIAL_COMPLETE_PENALTY } else { PARTIAL_PENALTY };
                    options.push(Choice::single(Seg::prefix(pos, run_end, *set, bytes[pos], penalty)));
                }
            }
        }

        if reading == Reading::Literal {
            return options;
        }
        // 简拼：只打一个字母，或者只打 zh、ch、sh 这样的声母
        for end in pos + 1..=run_end.min(pos + 2) {
            let letters = &input[pos..end];
            if end - pos == 2 && !table.is_initial(letters) {
                continue;
            }
            let Some(set) = table.prefix(letters) else { continue };
            // 末尾没打完的音节已经给出了同样的约束
            let duplicate = options.iter().any(|o| {
                o.second.is_none() && o.first.end == end && matches!(o.first.constraint, Constraint::Prefix { .. })
            });
            if duplicate {
                continue;
            }
            let penalty = if table.exact(letters).is_some() { ABBR_SYLLABLE_PENALTY } else { ABBR_PENALTY };
            options.push(Choice::single(Seg::prefix(pos, end, *set, bytes[pos], penalty)));
        }
        options
    }

    /// 查出与这一段匹配的词，按代价从低到高，最多 `limit` 个。
    fn lookup(&self, span: &Span, limit: usize) -> Vec<(u32, f64)> {
        let exact = exact_ids(&span.segs);
        if let Some(entries) = exact.as_deref().and_then(|ids| self.dict.exact(ids)) {
            return entries.take(limit).map(|entry| (entry, self.dict.cost(entry))).collect();
        }

        let table = self.dict.table();
        let initials: Vec<u8> = span.segs.iter().map(|seg| seg.constraint.initial(table)).collect();
        let mut found = Vec::new();
        for entry in self.dict.abbr(&initials) {
            let ids = self.dict.syllables(entry);
            if ids.iter().zip(&span.segs).all(|(&id, seg)| seg.constraint.matches(id)) {
                found.push((entry, self.dict.cost(entry)));
                if found.len() >= limit {
                    break;
                }
            }
        }
        found
    }
}

fn relax(
    cost: &mut [f64],
    back: &mut [Option<(usize, Step)>],
    from: usize,
    to: usize,
    total: f64,
    step: impl FnOnce() -> Step,
) {
    if total < cost[to] {
        cost[to] = total;
        back[to] = Some((from, step()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
你\tni\t1000
好\thao\t1000
你好\tni hao\t500
拟好\tni hao\t20
是\tshi\t2000
世界\tshi jie\t300
界\tjie\t100
西\txi\t300
安\tan\t300
西安\txi an\t50
先\txian\t800
想\txiang\t900
中\tzhong\t900
国\tguo\t700
人\tren\t800
中国\tzhong guo\t600
中国人\tzhong guo ren\t200
鸟\tniao\t5000
说\tshuo\t2000
苏\tsu\t500
红\thong\t500
那\tna\t900
按\tan\t400
难\tnan\t1000
南宁\tnan ning\t300
横眉冷对千夫指\theng mei leng dui qian fu zhi\t100
";

    fn engine() -> Engine {
        Engine::new(Dictionary::from_rime_sources(&[SAMPLE]))
    }

    fn texts(conversion: &Conversion) -> Vec<&str> {
        conversion.candidates.iter().map(|c| c.text.as_str()).collect()
    }

    #[test]
    fn single_word() {
        let c = engine().convert("nihao");
        assert_eq!(texts(&c)[..2], ["你好", "拟好"]);
        assert_eq!(c.preedit, "ni'hao");
    }

    #[test]
    fn sentence_from_words() {
        let c = engine().convert("nihaoshijie");
        assert_eq!(c.candidates[0].text, "你好世界");
        assert_eq!(c.preedit, "ni'hao'shi'jie");
    }

    #[test]
    fn prefix_words_follow_full_matches() {
        let c = engine().convert("nihaoshijie");
        let first_word = c.candidates.iter().find(|c| c.text == "你好").unwrap();
        assert_eq!(first_word.consumed, "nihao".len());
    }

    #[test]
    fn exact_syllable_beats_completion() {
        let c = engine().convert("xian");
        assert_eq!(c.candidates[0].text, "先");
        assert!(texts(&c).contains(&"西安"));
    }

    #[test]
    fn separator_forces_split() {
        let c = engine().convert("xi'an");
        assert_eq!(c.candidates[0].text, "西安");
        assert_eq!(c.preedit, "xi'an");
    }

    #[test]
    fn abbreviations() {
        assert_eq!(engine().convert("zgr").candidates[0].text, "中国人");
        assert_eq!(engine().convert("nh").candidates[0].text, "你好");
    }

    #[test]
    fn unfinished_last_syllable() {
        assert_eq!(engine().convert("zhongg").candidates[0].text, "中国");
    }

    #[test]
    fn swapped_letters_are_corrected() {
        let c = engine().convert("zhnog");
        assert_eq!(c.candidates[0].text, "中");
        assert_eq!(c.preedit, "zhong", "预编辑显示纠正后的拼写");
        let c = engine().convert("nihoa");
        assert_eq!(c.candidates[0].text, "你好");
        assert_eq!(c.preedit, "ni'hao");
        assert_eq!(engine().convert("xinag").candidates[0].text, "想");
    }

    #[test]
    fn other_typos_are_corrected() {
        let e = engine();
        assert_eq!(e.convert("zhonf").candidates[0].text, "中", "按错相邻键");
        assert_eq!(e.convert("zhngguo").candidates[0].text, "中国", "漏字母");
        assert_eq!(e.convert("nihhao").candidates[0].text, "你好", "连按两次");
        let c = e.convert("nhiao");
        assert_eq!(c.candidates[0].text, "你好", "跨音节打反");
        assert_eq!(c.preedit, "ni'hao");
    }

    #[test]
    fn doubled_letters_on_a_syllable_boundary_are_kept() {
        // naan 是 na'an，不是 nan 多按了一个 a；不然 naanni 会被吃掉一个 a 变成「南宁」
        let c = engine().convert("naan");
        assert_eq!(c.preedit, "na'an");
        assert_eq!(c.candidates[0].text, "那按");
        let c = engine().convert("naanni");
        assert_eq!(c.preedit, "na'an'ni");
        assert_eq!(c.candidates[0].text, "那按你");
    }

    #[test]
    fn unfinished_tail_keeps_typed_letters_and_offers_the_fix_second() {
        // suho 可能要接着打 suhong，也可能是「说」打反了：拼音照着打的显示，「说」排第二
        let c = engine().convert("suho");
        assert_eq!(c.preedit, "su'ho");
        assert_eq!(texts(&c)[1], "说");
    }

    #[test]
    fn consonant_only_input_is_not_corrected() {
        // 全是辅音就是在打简拼，不能被纠成别的
        assert_eq!(engine().convert("zgr").candidates[0].text, "中国人");
    }

    #[test]
    fn correct_input_offers_no_corrected_words() {
        // nihao 去掉 h 就是 niao，但打对了的输入不该冒出「鸟」
        let c = engine().convert("nihao");
        assert!(!texts(&c).contains(&"鸟"), "{:?}", texts(&c));
    }

    #[test]
    fn words_longer_than_the_exact_index() {
        // 7 个音节，全拼索引放不下，要靠简拼索引查到
        let c = engine().convert("hengmeilengduiqianfuzhi");
        assert_eq!(c.candidates[0].text, "横眉冷对千夫指");
    }

    #[test]
    fn unknown_letters_stay_raw() {
        let c = engine().convert("vv");
        assert_eq!(c.candidates[0].text, "vv");
        assert_eq!(c.preedit, "vv");
    }
}
