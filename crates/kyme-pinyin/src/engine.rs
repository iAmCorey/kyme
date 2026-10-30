//! 拼音 → 候选：音节切分（含简拼、末尾没打完的音节、打字纠错）、查词、按一元词频组整句。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::dict::{Dictionary, LoadError, dict_files, read_file};
use crate::emoji::Emoji;
use crate::local;
use crate::pairs::Pairs;
use crate::syllable::{SylId, SylSet, SyllableTable};
use crate::user::UserDict;

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
/// 末尾已经是完整音节、却按更长的音节去联想时的惩罚。打完整的音节多半就是想要它：meine 是「没呢」，
/// 不该把 ne 联想成 neng 出「没能」。照着打的读法（判断要不要纠错时的参照）罚得轻一些：
/// 罚重了它就变贵，纠错更容易赢，打对的拼音更容易被改。
const PARTIAL_COMPLETE_PENALTY: f64 = 6.0;
const PARTIAL_COMPLETE_LITERAL_PENALTY: f64 = 4.0;
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
/// 从宽会让首选在打字过程中跳来跳去（打到 lain 读成 lian，再打一个 a 又变回 lai'na）。
const TYPO_MARGIN: f64 = 7.0;
/// 拼音里的元音字母。一个元音都没有的输入是在打简拼（rgzn），不做纠错。
const VOWELS: &[u8] = b"aeiouv";
/// 认不出的字母原样保留的代价，只在别无选择时才会走到。
const RAW_COST: f64 = 40.0;
/// 组句时每一段取几个词参与比较。
const LATTICE_WIDTH: usize = 3;
/// 候选列表里，选中后消耗同样多字母的候选最多几个。
const MAX_CANDIDATES: usize = 60;
/// 词频低于这个数的单字算生僻字，候选里排在常用的字词后面。自带词库里约四成单字低于它。
const RARE_WEIGHT: f64 = 1000.0;
/// 首选看搭配换字时，比较前几句。
const COLLOCATION_SENTENCES: usize = 8;
/// 不带用户词库时用的空词库。
static NO_USER_WORDS: UserDict = UserDict::empty();

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
    /// 纠错得来的音节：spelling 里写纠正后的拼写，preedit 照样显示打的字母。
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

    /// 夹在中间的简拼：只打了开头的字母，后面紧接着别的字母（nhao 的 n）。末尾没打完的音节不算。
    fn abbreviated(&self, bytes: &[u8]) -> bool {
        matches!(self.constraint, Constraint::Prefix { .. }) && bytes.get(self.end).is_some_and(u8::is_ascii_lowercase)
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

/// 夹在中间的简拼 `input[pos..end]` 能和前后的字母拼成完整的音节：后面接着打全了以它开头的音节（shuo 的 s，
/// 从 `pos` 起打全了的最长音节到 `syllable_end`），或者它是前面那个音节的最后一个字母（jian 的 n）。
/// 打全了的音节就是想打它，这时不按简拼读：不然 duishuo 会读成 dui's'huo「对生活」，jianda 读成 jia'n'da「加拿大」。
/// 往前越过分隔符的那几个字母本来就不是音节，不用另外管。
fn glued(table: &SyllableTable, input: &str, pos: usize, end: usize, syllable_end: usize) -> bool {
    syllable_end > end
        || (end.saturating_sub(table.max_len())..pos).any(|from| table.exact(&input[from..end]).is_some())
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
    /// 读音，学词用。emoji 和带着原样字母的整句没有读音，是空的。
    pub syllables: Vec<SylId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conversion {
    /// 显示给用户的拼音：就是打的字母，按最优切分加了分隔符，例如 `ni'hao`。
    /// 纠过错的词照原样显示，中间不加分隔符（打 `zhnog` 显示 `zhnog`）。
    pub preedit: String,
    /// 引擎理解的拼音：纠过错的音节换成纠正后的拼写（打 `zhnog` 是 `zhong`）。
    /// 字母和 preedit 不一样，就说明首选用了纠错。
    pub spelling: String,
    pub candidates: Vec<Candidate>,
}

/// 词图里的一个词：主词库的词条，附加的第几层词库的词条，或者用户词库里的词。
#[derive(Clone, Copy)]
enum Word {
    Main(u32),
    Added(u16, u32),
    User(u32),
}

/// 组句时用哪些词。
#[derive(Clone, Copy)]
enum Words<'a> {
    /// 只用主词库：纠错得来的那一段，和拿来跟照着打的读法比的纠错整句，都只用它。
    Main,
    /// 主词库，加上只进照着打的读法的附加词库和这份用户词库。
    All(&'a UserDict),
}

impl<'a> Words<'a> {
    /// 这一段用哪些词：纠错得来的那一段只用主词库，加进来的词只该让照着打的读法更占优。
    fn for_span(self, span: &Span) -> Words<'a> {
        if span.segs.iter().any(|seg| seg.fixed.is_some()) { Words::Main } else { self }
    }

    /// 取词的文字和读音时用的用户词库。只用主词库时不会查到用户词，给个空的。
    fn user(self) -> &'a UserDict {
        match self {
            Words::Main => &NO_USER_WORDS,
            Words::All(user) => user,
        }
    }
}

/// 词图上走到某个位置的一条路：代价，从哪个位置的第几条路走过来，这一步是哪个词（分隔符那一步没有词），
/// 和到这里为止的最后一个字，接下一个词时看搭配用。
#[derive(Clone, Copy)]
struct Route {
    cost: f64,
    from: usize,
    rank: usize,
    word: Option<Word>,
    last: Option<char>,
}

/// `sentences` 里往后走的一步：走到哪、代价多少。
#[derive(Clone, Copy)]
struct Hop {
    to: usize,
    cost: f64,
    /// 哪个词，和它的头尾两个字，接词时看搭配用。分隔符那一步没有词
    word: Option<(Word, char, char)>,
}

/// 组句路径上的一步。
#[derive(Clone)]
enum Step {
    Separator,
    Raw,
    Word { word: Word, segs: Vec<Seg> },
}

/// 各份数据都是共享的，复制一份很便宜。
#[derive(Clone)]
pub struct Engine {
    dict: Arc<Dictionary>,
    /// 附加的几层词库：本地词库（自定义的和导入的）、设置里打开的扩展词库。和用户词一样只用在照着打的读法上。
    added: Arc<[Arc<Dictionary>]>,
    emoji: Arc<Emoji>,
    /// 主词库里字和字的搭配，组句时看相邻两个词接得顺不顺。
    pairs: Arc<Pairs>,
}

impl Engine {
    pub fn new(dict: Dictionary) -> Self {
        let pairs = Arc::new(Pairs::new(&dict));
        Engine { dict: Arc::new(dict), added: Arc::new([]), emoji: Arc::default(), pairs }
    }

    /// 读一个数据目录：全部 `.dict.yaml`（按文件名排序）和 emoji 对照表 `emoji.txt`。
    /// 输入法读安装包里的 dict/，QA 读仓库的 data/rime-ice/，两处放的是同一套文件。
    pub fn load_dir(dir: &Path) -> Result<Self, LoadError> {
        let paths = dict_files(dir).map_err(|source| LoadError::new(dir, source))?;
        let dict = Dictionary::load_rime(&paths)?;
        let emoji = Emoji::parse(&read_file(&dir.join("emoji.txt"))?);
        Ok(Engine::new(dict).with_emoji(emoji))
    }

    /// 带上 emoji 对照表。`convert` 不用它，由输入会话决定要不要把 emoji 放进候选。
    pub fn with_emoji(mut self, emoji: Emoji) -> Self {
        self.emoji = Arc::new(emoji);
        self
    }

    /// 把几个词库文件的内容（写法见 `local::parse_line`）建成一层附加的词库，用 `with_layers` 换上。
    /// 没写拼音、拼音对不上的词按主词库补读音，补不出来的不要。词多时要一秒多，在后台建。
    pub fn layer(&self, sources: &[&str]) -> Arc<Dictionary> {
        Arc::new(local::build(&self.dict, sources))
    }

    /// 换上这几层附加的词库，其余和这个引擎共用。
    pub fn with_layers(&self, layers: Vec<Arc<Dictionary>>) -> Engine {
        Engine { added: layers.into(), ..self.clone() }
    }

    pub fn dictionary(&self) -> &Dictionary {
        &self.dict
    }

    /// 本地词库里这些词（文字，写的拼音）会按什么读音载入，和 `layer` 定读音的办法一样。
    /// 音节用空格隔开，定不出来是 None。设置窗口显示用。
    pub fn local_readings(&self, words: &[(&str, &str)]) -> Vec<Option<String>> {
        let read = |text, pinyin| local::reading(&self.dict, text, &local::syllables(pinyin).collect::<Vec<_>>());
        words.iter().map(|&(text, pinyin)| Some(self.dict.table().spell(&read(text, pinyin)?))).collect()
    }

    /// 主词库或附加的词库里文字和读音都对得上的这个词的代价，几处都有取低的。
    pub(crate) fn word_cost(&self, text: &str, syllables: &[SylId]) -> Option<f64> {
        let cost = |dict: &Dictionary| dict.find(text, syllables).map(|entry| dict.cost(entry));
        std::iter::once(&self.dict).chain(self.added.iter()).filter_map(|dict| cost(dict)).reduce(f64::min)
    }

    /// 有没有只进照着打的读法的词：附加的词库或这份用户词库。
    fn has_added_words(&self, user: &UserDict) -> bool {
        self.added.iter().any(|dict| !dict.is_empty()) || !user.is_empty()
    }

    pub fn emoji(&self) -> &Emoji {
        &self.emoji
    }

    /// 前 `count` 个最合适的整句，最好的在前，文字各不相同。只看照着打的读法和简拼，不纠错；拼不通、
    /// 要原样保留字母的输入没有整句。和 `convert_with` 一样按词频组句，再算上相邻两个词接得顺不顺。
    /// AI 在这些整句里挑接得上前文的那句。
    pub fn sentences(&self, input: &str, user: &UserDict, count: usize) -> Vec<Candidate> {
        self.ranked_sentences(input, Words::All(user), count)
    }

    /// `sentences` 的实现，用 `words` 里的词。
    fn ranked_sentences(&self, input: &str, words: Words, count: usize) -> Vec<Candidate> {
        let user = words.user();
        let bytes = input.as_bytes();
        let n = bytes.len();
        let keep = count * 2;
        let options: Vec<Vec<Choice>> = (0..n).map(|pos| self.seg_options(input, pos, Reading::Plain)).collect();
        self.k_best(user, n, count, n, |i| {
            if bytes[i] == b'\'' {
                return vec![Hop { to: i + 1, cost: 0.0, word: None }];
            }
            let mut hops = Vec::new();
            for span in self.spans_from(input, &options, i, words) {
                for (word, cost) in self.lookup(&span, keep, words.for_span(&span)) {
                    hops.extend(self.hop(user, span.end, step_cost(cost, &span), word));
                }
            }
            hops
        })
    }

    /// 读音是 `syllables` 的前 `count` 句，最好的在前：只换字、不换读音，给 `with_collocations` 挑首选。
    /// 位置按音节数，选中时消耗 `consumed` 个字节。
    fn same_reading(&self, syllables: &[SylId], words: Words, count: usize, consumed: usize) -> Vec<Candidate> {
        let user = words.user();
        let keep = count * 2;
        self.k_best(user, syllables.len(), count, consumed, |i| {
            let mut hops = Vec::new();
            let mut segs = Vec::new();
            for (j, &id) in syllables.iter().enumerate().skip(i).take(MAX_WORD_SYLS) {
                segs.push(Seg::exact(i, j + 1, id));
                // 词库里没有以这几个音节开头的词，再长也不会有
                if !self.has_words_starting_with(&segs, words) {
                    break;
                }
                let span = Span { end: j + 1, segs: segs.clone(), penalty: 0.0 };
                for (word, cost) in self.lookup(&span, keep, words) {
                    hops.extend(self.hop(user, span.end, step_cost(cost, &span), word));
                }
            }
            hops
        })
    }

    /// 走到 `to` 的一步，选了 `word`：记下这个词的头尾两个字，接词时看搭配用。文字是空的词不要。
    fn hop(&self, user: &UserDict, to: usize, cost: f64, word: Word) -> Option<Hop> {
        let text = self.word_text(user, word);
        Some(Hop { to, cost, word: Some((word, text.chars().next()?, text.chars().next_back()?)) })
    }

    /// 词图上从位置 0 走到 `n` 的前 `count` 条路，算上相邻两个词接得顺不顺（见 `Pairs`），最好的在前，文字各不相同。
    /// `hops(i)` 是从位置 `i` 往后走的每一步，选中整句时消耗 `consumed` 个字节。
    fn k_best(
        &self,
        user: &UserDict,
        n: usize,
        count: usize,
        consumed: usize,
        hops: impl Fn(usize) -> Vec<Hop>,
    ) -> Vec<Candidate> {
        // 同一句话可能有几种切法，多留一些路，去掉重复的以后还够数
        let keep = count * 2;
        // paths[i]：走到位置 i 代价最小的几条路，按代价排好
        let mut paths: Vec<Vec<Route>> = vec![Vec::new(); n + 1];
        paths[0].push(Route { cost: 0.0, from: 0, rank: 0, word: None, last: None });
        for i in 0..n {
            if paths[i].is_empty() {
                continue;
            }
            let hops = hops(i);
            for rank in 0..paths[i].len() {
                let from = paths[i][rank];
                for &Hop { to, cost, word } in &hops {
                    let mut cost = from.cost + cost;
                    // 搭配的代价不会是负的：不算它都挤不进前几名，就不用查了
                    if paths[to].len() == keep && paths[to][keep - 1].cost <= cost {
                        continue;
                    }
                    if let (Some(before), Some((_, first, _))) = (from.last, word) {
                        cost += self.pairs.cost(before, first);
                    }
                    let at = paths[to].partition_point(|route| route.cost <= cost);
                    if at < keep {
                        let (word, last) = match word {
                            Some((word, _, last)) => (Some(word), Some(last)),
                            None => (None, from.last),
                        };
                        paths[to].insert(at, Route { cost, from: i, rank, word, last });
                        paths[to].truncate(keep);
                    }
                }
            }
        }
        let mut seen = HashSet::new();
        let mut sentences = Vec::new();
        for rank in 0..paths[n].len() {
            let mut picked = Vec::new();
            let (mut at, mut rank) = (n, rank);
            while at > 0 {
                let route = paths[at][rank];
                picked.extend(route.word);
                (at, rank) = (route.from, route.rank);
            }
            picked.reverse();
            let text: String = picked.iter().map(|&word| self.word_text(user, word)).collect();
            if seen.insert(text.clone()) {
                let syllables =
                    picked.iter().flat_map(|&word| self.word_syllables(user, word).iter().copied()).collect();
                sentences.push(Candidate { text, consumed, syllables });
            }
            if sentences.len() == count {
                break;
            }
        }
        sentences
    }

    /// 把一串拼音（小写字母，可用 `'` 分隔）转成候选。第一个候选是整句最优解。
    pub fn convert(&self, input: &str) -> Conversion {
        self.convert_with(input, &NO_USER_WORDS)
    }

    /// 带上用户词库转换：常选的词更靠前，学到的新词也能出。本地词库和用户词都只用在照着打的读法上，
    /// 纠错的读法只用主词库：加进来的词只会让照着打的读法更占优，打对的拼音就更不会被改。
    pub fn convert_with(&self, input: &str, user: &UserDict) -> Conversion {
        if input.is_empty() {
            return Conversion::default();
        }
        let all = Words::All(user);
        // 一个元音都没有是在打简拼（rgzn），不纠错
        if !input.bytes().any(|b| VOWELS.contains(&b)) {
            return self.with_collocations(self.solve(input, Reading::Plain, all).conversion, all);
        }
        let best = self.solve(input, Reading::Corrected, all);
        if !best.corrected {
            return self.with_collocations(best.conversion, all);
        }
        // 用了纠错的话，要比照着打的读法明显更合理才算数（naanni 照着打是 na'an'ni「那按你」，
        // 不能被读成 nan'ni「南宁」）。拼不通的输入只能逐个字母原样保留，照着打的那边代价极高。
        // 比的时候纠错这边只用主词库：加进来的词只该让照着打的读法更占优
        let best = if !self.has_added_words(user) { best } else { self.solve(input, Reading::Corrected, Words::Main) };
        if !best.corrected {
            return self.with_collocations(self.solve(input, Reading::Plain, all).conversion, all);
        }
        let literal = self.solve(input, Reading::Literal, all);
        if best.cost + TYPO_MARGIN < literal.cost {
            // 纠错的整句只用主词库，看搭配换字时也一样
            return self.with_collocations(best.conversion, Words::Main);
        }
        let mut conversion = self.with_collocations(self.solve(input, Reading::Plain, all).conversion, all);
        // 照着打的读法以没打完的音节结尾、纠错改的又正是它：分不清是还没打完还是最后一下按错了
        // （suho 可能要接着打 suhong，也可能是想打「说」）。首选按照着打的读法，纠错的整句排第二，按 2 就能选
        let at_tail = matches!((literal.unfinished_from, best.last_fix_end), (Some(tail), Some(end)) if end > tail);
        if at_tail
            && let Some(fixed) = best.conversion.candidates.into_iter().next()
            && !conversion.candidates.iter().any(|c| c.text == fixed.text)
        {
            conversion.candidates.insert(conversion.candidates.len().min(1), fixed);
        }
        conversion
    }

    /// 首选换成读音一样、搭配更顺的那句，原来的首选排第二。只按词频组句的话，「和」比「喝」常用得多，
    /// haonanhe 组出「好难和」；看搭配，「难喝」常见、「难和」几乎没有，就换成「好难喝」。
    /// 只在首选的读音（纠过错的是纠正后的）上换字，打的拼音怎么理解不变；`words` 和组出首选时用的一样。
    fn with_collocations(&self, mut conversion: Conversion, words: Words) -> Conversion {
        let Some(first) = conversion.candidates.first() else { return conversion };
        // 一个音节只有一个字，没有词和词接得顺不顺可看；带着原样字母的整句没有读音
        if first.syllables.len() < 2 {
            return conversion;
        }
        let better = self
            .same_reading(&first.syllables, words, COLLOCATION_SENTENCES, first.consumed)
            .into_iter()
            .next()
            .filter(|better| better.text != first.text);
        if let Some(better) = better {
            conversion.candidates.retain(|candidate| candidate.text != better.text);
            conversion.candidates.insert(0, better);
        }
        conversion
    }

    /// 按 `reading` 允许的读法、用 `words` 里的词组出最优整句。纠错得来的那一段总是只用主词库。
    fn solve(&self, input: &str, reading: Reading, words: Words) -> Solution {
        let user = words.user();
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
            let spans = self.spans_from(input, &options, i, words);
            for span in &spans {
                for (word, word_cost) in self.lookup(span, LATTICE_WIDTH, words.for_span(span)) {
                    let total = here + step_cost(word_cost, span);
                    relax(&mut cost, &mut back, i, span.end, total, || Step::Word { word, segs: span.segs.clone() });
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

        // 沿最优路径拼出整句和两份拼音：preedit 显示打的字母，spelling 换成纠正后的拼写。
        // 两份都在音节之间加分隔符，用户自己打的分隔符原样保留
        let table = self.dict.table();
        let mut sentence = String::new();
        let mut syllables = Vec::new();
        let mut has_raw = false;
        let mut preedit = String::with_capacity(n + 8);
        let mut spelling = String::with_capacity(n + 8);
        let mut after_syllable = false;
        let mut boundaries = HashSet::new();
        let mut sentence_corrected = false;
        let mut last_fix_end = None;
        let mut unfinished_from = None;
        for (start, end, step) in &steps {
            boundaries.insert(*end);
            match step {
                Step::Word { word, segs } => {
                    sentence.push_str(self.word_text(user, *word));
                    syllables.extend_from_slice(self.word_syllables(user, *word));
                    // 纠过错的词整个照原样显示：纠正可能挪动了字母（nhiao 是 ni'hao），按音节切开反而对不上
                    let fixed_word = segs.iter().any(|seg| seg.fixed.is_some());
                    sentence_corrected |= fixed_word;
                    if fixed_word {
                        push_part(&mut preedit, after_syllable, &input[*start..*end]);
                    }
                    for seg in segs {
                        boundaries.insert(seg.end);
                        if seg.fixed.is_some() {
                            last_fix_end = Some(seg.end);
                        }
                        let unfinished = seg.end == n && matches!(seg.constraint, Constraint::Prefix { .. });
                        unfinished_from = unfinished.then_some(seg.start);
                        let typed = &input[seg.start..seg.end];
                        push_part(&mut spelling, after_syllable, seg.fixed.map_or(typed, |id| table.name(id)));
                        if !fixed_word {
                            push_part(&mut preedit, after_syllable, typed);
                        }
                        after_syllable = true;
                    }
                }
                Step::Raw => {
                    let raw = &input[*start..*end];
                    sentence.push_str(raw);
                    has_raw = true;
                    push_part(&mut preedit, after_syllable, raw);
                    push_part(&mut spelling, after_syllable, raw);
                    after_syllable = false;
                }
                Step::Separator => {
                    preedit.push('\'');
                    spelling.push('\'');
                    after_syllable = false;
                }
            }
        }

        // 其余候选：从开头起能组成的词，覆盖得越长越靠前，同样长度按代价排。
        // 选了以后剩下的字母要还打得出字：结束在首选读法的音节边界上；照着打的词（没纠错、没夹着简拼）
        // 也可以结束在别处，只要剩下的照着打读得通（wengan 首选是 weng'an「瓮安」，也出 wen「温」，剩下 gan；
        // kaihui 不出「开户」，那会剩下打不出来的 i）。首选没用到纠错，就说明打得没错，也不出纠错得来的词
        let readable = self.readable_rest(input);
        let rare_cost = self.dict.cost_of_weight(RARE_WEIGHT);
        let mut words_from_start: Vec<(usize, f64, bool, Word)> = Vec::new();
        for span in &spans_from_start {
            let corrected = span.segs.iter().any(|seg| seg.fixed.is_some());
            let abbreviated = span.segs.iter().any(|seg| seg.abbreviated(bytes));
            let rest_typable = boundaries.contains(&span.end) || (!corrected && !abbreviated && readable[span.end]);
            if !rest_typable || (corrected && !sentence_corrected) {
                continue;
            }
            for (word, cost) in self.lookup(span, MAX_CANDIDATES, words.for_span(span)) {
                let rare = cost > rare_cost && self.word_text(user, word).chars().count() == 1;
                words_from_start.push((span.end, cost + span.penalty, rare, word));
            }
        }
        words_from_start
            .sort_by(|(end_a, cost_a, ..), (end_b, cost_b, ..)| end_b.cmp(end_a).then(cost_a.total_cmp(cost_b)));

        // 带着原样字母的整句没有完整的读音，不拿来学词
        let syllables = if has_raw { Vec::new() } else { syllables };
        let mut candidates = vec![Candidate { text: sentence, consumed: n, syllables }];
        let mut seen: HashSet<String> = candidates.iter().map(|c| c.text.clone()).collect();
        // 每种长度各自有上限：长的词再多，也要给短一些的读法留位置（jia 也选得到 ji'a 的「机」）
        let mut per_end = vec![0; n + 1];
        let mut kept: Vec<(bool, Candidate)> = Vec::new();
        for (end, _, rare, word) in words_from_start {
            if per_end[end] >= MAX_CANDIDATES {
                continue;
            }
            let text = self.word_text(user, word);
            if seen.insert(text.to_string()) {
                per_end[end] += 1;
                let syllables = self.word_syllables(user, word).to_vec();
                kept.push((rare, Candidate { text: text.to_string(), consumed: end, syllables }));
            }
        }
        // 留下哪些不看生不生僻，只调先后：生僻字排在所有常用的字词后面，免得把另一种读法的常用字挤到后面去
        // （wengan 的「温」不排在 weng 的「蓊、𬭩、滃」后面）。排序是稳定的，其余的先后不变
        kept.sort_by_key(|&(rare, _)| rare);
        candidates.extend(kept.into_iter().map(|(_, candidate)| candidate));

        Solution {
            cost: total,
            corrected: sentence_corrected,
            last_fix_end,
            unfinished_from,
            conversion: Conversion { preedit, spelling, candidates },
        }
    }

    /// 每个位置往后剩下的字母，照着打（打全了的音节，末尾可以没打完）能不能读完。
    fn readable_rest(&self, input: &str) -> Vec<bool> {
        let bytes = input.as_bytes();
        let n = bytes.len();
        let mut readable = vec![false; n + 1];
        readable[n] = true;
        for pos in (0..n).rev() {
            readable[pos] = if bytes[pos] == b'\'' {
                readable[pos + 1]
            } else {
                self.seg_options(input, pos, Reading::Literal).iter().any(|choice| readable[choice.end()])
            };
        }
        readable
    }

    /// 从 `start` 出发能切出的所有音节序列（最多 MAX_WORD_SYLS 个音节）。
    fn spans_from(&self, input: &str, options: &[Vec<Choice>], start: usize, words: Words) -> Vec<Span> {
        let mut spans = Vec::new();
        self.extend_spans(input, options, start, &mut Vec::new(), false, words, &mut spans);
        spans
    }

    #[allow(clippy::too_many_arguments)]
    fn extend_spans(
        &self,
        input: &str,
        options: &[Vec<Choice>],
        pos: usize,
        path: &mut Vec<Seg>,
        corrected: bool,
        words: Words,
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
            if self.has_words_starting_with(path, words) {
                let end = choice.end();
                spans.push(Span { end, segs: path.clone(), penalty: path.iter().map(|s| s.penalty).sum() });
                let mut next = end;
                while next < bytes.len() && bytes[next] == b'\'' {
                    next += 1;
                }
                if next < bytes.len() && path.len() < MAX_WORD_SYLS {
                    self.extend_spans(input, options, next, path, corrected || fixes, words, spans);
                }
            }
            path.truncate(depth);
        }
    }

    fn has_words_starting_with(&self, segs: &[Seg], words: Words) -> bool {
        let table = self.dict.table();
        let initials: Vec<u8> = segs.iter().map(|seg| seg.constraint.initial(table)).collect();
        let exact = exact_ids(segs);
        let in_dict = |dict: &Dictionary| {
            dict.has_abbr_prefix(&initials) && exact.as_deref().is_none_or(|ids| dict.has_exact_prefix(ids))
        };
        // 附加的词库、学到的新词不在主词库里，也要查，不然会在切分时被剪掉。纠错的那一段也照样往下切，
        // 但查词时只查主词库，主词库的词本来就切得到，所以多切出来的段不会多出主词库的词
        in_dict(&self.dict)
            || matches!(words, Words::All(user) if self.added.iter().any(|dict| in_dict(dict))
                || (user.has_abbr_prefix(&initials) && exact.as_deref().is_none_or(|ids| user.has_exact_prefix(ids))))
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
        // 从这里起打全了的最长音节到哪儿，没有就是 pos。简拼不能是它的开头
        let syllable_end = options.iter().map(Choice::end).max().unwrap_or(pos);

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
                    let penalty = match (complete, reading) {
                        (false, _) => PARTIAL_PENALTY,
                        (true, Reading::Literal) => PARTIAL_COMPLETE_LITERAL_PENALTY,
                        (true, _) => PARTIAL_COMPLETE_PENALTY,
                    };
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
            if end < run_end && glued(table, input, pos, end, syllable_end) {
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

    /// 查出与这一段匹配的词，按代价从低到高，最多 `limit` 个。本地词库和用户词（加过分的代价）和主词库的词
    /// 一起排：主词库里排在后面的词，用户常选的话也能排进来。
    fn lookup(&self, span: &Span, limit: usize, words: Words) -> Vec<(Word, f64)> {
        let table = self.dict.table();
        let exact = exact_ids(&span.segs);
        let initials: Vec<u8> = span.segs.iter().map(|seg| seg.constraint.initial(table)).collect();
        let fits = |ids: &[SylId]| ids.iter().zip(&span.segs).all(|(&id, seg)| seg.constraint.matches(id));
        // 一个词库里对得上的词，按词频从高到低，最多 `limit` 个
        let entries = |dict: &Dictionary| -> Vec<u32> {
            match exact.as_deref().and_then(|ids| dict.exact(ids)) {
                Some(entries) => entries.take(limit).collect(),
                None => dict.abbr(&initials).filter(|&entry| fits(dict.syllables(entry))).take(limit).collect(),
            }
        };
        let mut found: Vec<(Word, f64)> =
            entries(&self.dict).into_iter().map(|entry| (Word::Main(entry), self.dict.cost(entry))).collect();
        let Words::All(user) = words else { return found };
        if !self.has_added_words(user) {
            return found;
        }
        // 同一个词可能几个词库里都有，或者是另一个读音（「的」de 和 di 都对得上简拼 d），留代价低的那个
        let mut add = |word: Word, cost: f64| {
            let text = self.word_text(user, word);
            match found.iter_mut().find(|(found, _)| self.word_text(user, *found) == text) {
                Some(slot) if cost < slot.1 => *slot = (word, cost),
                Some(_) => {}
                None => found.push((word, cost)),
            }
        };
        for (layer, dict) in self.added.iter().enumerate() {
            for entry in entries(dict) {
                add(Word::Added(layer as u16, entry), dict.cost(entry));
            }
        }
        let learned = match exact.as_deref() {
            Some(ids) => user.exact(ids),
            None => user.abbr(&initials),
        };
        for &word in learned.iter().filter(|&&word| fits(user.syllables(word))) {
            add(Word::User(word), user.cost(word));
        }
        found.sort_by(|a, b| a.1.total_cmp(&b.1));
        found.truncate(limit);
        found
    }

    fn word_text<'a>(&'a self, user: &'a UserDict, word: Word) -> &'a str {
        match word {
            Word::Main(entry) => self.dict.text(entry),
            Word::Added(layer, entry) => self.added[usize::from(layer)].text(entry),
            Word::User(learned) => user.text(learned),
        }
    }

    fn word_syllables<'a>(&'a self, user: &'a UserDict, word: Word) -> &'a [SylId] {
        match word {
            Word::Main(entry) => self.dict.syllables(entry),
            Word::Added(layer, entry) => self.added[usize::from(layer)].syllables(entry),
            Word::User(learned) => user.syllables(learned),
        }
    }
}

/// 词图上走一步的代价：词的代价、这一段切法的惩罚，再加每多一个词的惩罚。组句和求前几句都用它，两边算法一样。
fn step_cost(word_cost: f64, span: &Span) -> f64 {
    word_cost + span.penalty + WORD_PENALTY
}

/// 接上一段拼音：前面已经是音节的话，先加一个分隔符。
fn push_part(pinyin: &mut String, after_syllable: bool, part: &str) {
    if after_syllable {
        pinyin.push('\'');
    }
    pinyin.push_str(part);
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

    fn syllables(engine: &Engine, pinyin: &str) -> Vec<SylId> {
        pinyin.split(' ').map(|name| engine.dictionary().table().exact(name).unwrap()).collect()
    }

    #[test]
    fn often_picked_words_move_up() {
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "几\tji\t900\n机\tji\t800\n鸡\tji\t700\n记\tji\t600\n姬\tji\t500\n",
        ]));
        assert_eq!(texts(&engine.convert("ji")), ["几", "机", "鸡", "记", "姬"]);
        let mut user = UserDict::default();
        for _ in 0..3 {
            user.learn(&engine, "姬", &syllables(&engine, "ji"), 0);
        }
        let c = engine.convert_with("ji", &user);
        assert_eq!(c.candidates[0].text, "姬", "排第 5 的选了 3 次，升到首位");
        assert_eq!(c.candidates[0].syllables, syllables(&engine, "ji"));
        assert_eq!(c.candidates.len(), 5, "换成用户词，不多出一个");
    }

    #[test]
    fn a_rare_reading_does_not_push_out_the_common_one() {
        let engine = Engine::new(Dictionary::from_rime_sources(&["的\tde\t100000\n的\tdi\t100\n到\tdao\t5000\n"]));
        let mut user = UserDict::default();
        user.learn(&engine, "的", &syllables(&engine, "di"), 0);
        assert_eq!(
            engine.convert_with("d", &user).candidates[0].text,
            "的",
            "简拼 d 时留常用的 de，不换成选过一次的 di"
        );
        assert_eq!(engine.convert_with("di", &user).candidates[0].text, "的");
    }

    #[test]
    fn learned_phrases_can_be_typed() {
        let engine = engine();
        let mut user = UserDict::default();
        user.learn(&engine, "妮鸟", &syllables(&engine, "ni niao"), 0);
        assert_eq!(engine.convert("niniao").candidates[0].text, "你鸟", "词库里没有「妮」");
        assert_eq!(engine.convert_with("niniao", &user).candidates[0].text, "妮鸟");
        assert!(texts(&engine.convert_with("nn", &user)).contains(&"妮鸟"), "简拼也能打出学到的词");
    }

    #[test]
    fn learned_words_never_alter_correct_spelling() {
        let engine = engine();
        let mut user = UserDict::default();
        for _ in 0..100 {
            user.learn(&engine, "南宁", &syllables(&engine, "nan ning"), 0);
            user.learn(&engine, "难", &syllables(&engine, "nan"), 0);
        }
        assert_eq!(engine.convert_with("naanni", &user).spelling, "na'an'ni", "纠错的读法不用用户词");
    }

    /// 换上用这份内容建的一层附加词库。
    fn with_local(engine: Engine, source: &str) -> Engine {
        engine.with_layers(vec![engine.layer(&[source])])
    }

    #[test]
    fn local_words_can_be_typed() {
        // 本地词的音节要是主词库认得的：SAMPLE 里有 su、hong
        let engine = with_local(engine(), "酥虹\tsu hong\n那安\n");
        assert_eq!(engine.convert("suhong").candidates[0].text, "酥虹");
        assert!(texts(&engine.convert("suh")).contains(&"酥虹"), "简拼也能打出来");
        let c = engine.convert("naan");
        assert_eq!(c.candidates[0].text, "那安", "没写拼音的按主词库补上");
        assert_eq!(c.candidates[0].syllables, syllables(&engine, "na an"));
    }

    #[test]
    fn added_words_never_alter_correct_spelling() {
        let engine = with_local(engine(), "南宁\tnan ning\t100000000\n难\tnan\t100000000\n");
        // 切分可以变，打的字母一个都不能少：不能读成 nan'ni「南宁」
        assert_eq!(engine.convert("naanni").spelling.replace('\'', ""), "naanni", "纠错的读法不用加进来的词");
        assert_eq!(engine.convert("nanning").candidates[0].text, "南宁");
    }

    #[test]
    fn every_added_layer_counts() {
        let engine = engine();
        let engine = engine.with_layers(vec![engine.layer(&["酥虹\tsu hong\n"]), engine.layer(&["素虹\tsu hong\n"])]);
        let c = engine.convert("suhong");
        assert!(texts(&c).contains(&"酥虹") && texts(&c).contains(&"素虹"), "两层的词都有：{:?}", texts(&c));
    }

    #[test]
    fn learned_local_words_start_from_their_local_cost() {
        let engine = Engine::new(Dictionary::from_rime_sources(&["鹏\tpeng\t5000\n"]));
        let engine = with_local(engine, "朋\tpeng\t100000\n");
        assert_eq!(engine.convert("peng").candidates[0].text, "朋");
        let mut user = UserDict::default();
        user.learn(&engine, "朋", &syllables(&engine, "peng"), 0);
        assert_eq!(engine.convert_with("peng", &user).candidates[0].text, "朋", "选过一次不该反而掉下去");
    }

    #[test]
    fn sentences_are_ranked_and_distinct() {
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "好\thao\t1000\n号\thao\t100\n难\tnan\t900\n男\tnan\t300\n和\the\t1000\n喝\the\t500\n难喝\tnan he\t20\n",
        ]));
        let user = UserDict::default();
        let sentences: Vec<String> = engine.sentences("haonanhe", &user, 5).into_iter().map(|c| c.text).collect();
        assert_eq!(sentences[0], engine.convert("haonanhe").candidates[0].text, "第一个就是首选");
        assert!(sentences.contains(&"好难喝".to_string()), "{sentences:?}");
        assert_eq!(sentences.len(), 5);
        let mut unique = sentences.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), sentences.len(), "文字各不相同：{sentences:?}");
        assert!(engine.sentences("vv", &user, 5).is_empty(), "拼不通的没有整句");
    }

    #[test]
    fn collocations_pick_among_sentences_that_read_the_same() {
        // 只看词频是「好难和」（「和」比「喝」常用）；「难喝」常见、「难和」没见过，看搭配换成「好难喝」
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "好\thao\t1000\n难\tnan\t900\n和\the\t1000\n喝\the\t300\n好难\thao nan\t100\n太难喝\ttai nan he\t50000\n",
        ]));
        let c = engine.convert("haonanhe");
        assert_eq!(texts(&c)[..2], ["好难喝", "好难和"], "原来的首选排第二");
        assert_eq!(c.candidates[0].syllables, syllables(&engine, "hao nan he"));
        assert_eq!((c.preedit.as_str(), c.spelling.as_str()), ("hao'nan'he", "hao'nan'he"), "读音不变");
        assert_eq!(engine.sentences("haonanhe", &UserDict::default(), 3)[0].text, "好难喝");
        let c = engine.convert("haonnahe");
        assert_eq!(c.spelling, "hao'nan'he", "打反了，纠回来");
        assert_eq!(texts(&c)[..2], ["好难喝", "好难和"], "纠过错的也看搭配");
        assert_eq!(c.candidates[0].consumed, "haonnahe".len());
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
    fn words_of_another_split_can_be_picked() {
        // wengan 首选按 weng'an 是「瓮安」，wen'gan 那种切法的「温」也要能选，选了剩下 gan
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "瓮安\tweng an\t500\n翁\tweng\t300\n温\twen\t500\n感\tgan\t500\n文革\twen ge\t800\n",
        ]));
        let c = engine.convert("wengan");
        assert_eq!(c.candidates[0].text, "瓮安");
        let warm = c.candidates.iter().find(|c| c.text == "温").expect("出「温」");
        assert_eq!(warm.consumed, "wen".len());
        assert!(!texts(&c).contains(&"文革"), "g 和后面的 an 拼成 gan，不当简拼：{:?}", texts(&c));
    }

    #[test]
    fn abbreviated_words_stay_as_alternatives() {
        // 首选照着打，没拼成音节的字母当简拼的词照样排在后面：shenmesh 也出「什么时候」，ali 也出「案例」
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "什么是\tshen me shi\t100000\n什么时候\tshen me shi hou\t10\n阿里\ta li\t100000\n案例\tan li\t1000\n",
        ]));
        let c = engine.convert("shenmesh");
        assert_eq!(c.candidates[0].text, "什么是");
        assert!(texts(&c).contains(&"什么时候"), "{:?}", texts(&c));
        let c = engine.convert("ali");
        assert_eq!(c.candidates[0].text, "阿里");
        assert!(texts(&c).contains(&"案例"), "{:?}", texts(&c));
    }

    #[test]
    fn typed_syllables_are_not_read_as_abbreviations() {
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "对\tdui\t1000\n说\tshuo\t1000\n对生活\tdui sheng huo\t100000\n生活\tsheng huo\t100000\n\
             活\thuo\t100\n加拿大\tjia na da\t100000\n简答\tjian da\t10\n加\tjia\t100\n你好\tni hao\t500\n",
        ]));
        let c = engine.convert("duishuo");
        assert_eq!((c.candidates[0].text.as_str(), c.spelling.as_str()), ("对说", "dui'shuo"), "不读成 dui's'huo");
        let c = engine.convert("jianda");
        assert_eq!((c.candidates[0].text.as_str(), c.spelling.as_str()), ("简答", "jian'da"), "不读成 jia'n'da");
        assert_eq!(engine.convert("nhao").candidates[0].text, "你好", "拼不成音节的字母照样按简拼读");
    }

    #[test]
    fn rare_characters_follow_common_words_of_other_readings() {
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "瓮安\tweng an\t1420\n翁\tweng\t48774\n蓊\tweng\t394\n滃\tweng\t9\n温\twen\t447521\n感\tgan\t500000\n",
        ]));
        let c = engine.convert("wengan");
        let at = |text: &str| texts(&c).iter().position(|t| *t == text).unwrap();
        assert!(at("翁") < at("温"), "常用的字还是覆盖得越长越靠前：{:?}", texts(&c));
        assert!(at("温") < at("蓊") && at("温") < at("滃"), "生僻字排在后面：{:?}", texts(&c));
    }

    #[test]
    fn many_long_words_leave_room_for_other_readings() {
        // jia 能接着打成 jian、jiang、jiao，这一长度的字远超上限，ji'a 的「机」也要留在候选里
        let mut source: String = (0..200)
            .map(|i| {
                format!(
                    "{}\tjia{}\t{}\n",
                    char::from_u32(0x4E00 + i).unwrap(),
                    ["", "n", "ng", "o"][i as usize % 4],
                    100_000 + i
                )
            })
            .collect();
        source.push_str("机\tji\t5000\n啊\ta\t5000\n");
        let engine = Engine::new(Dictionary::from_rime_sources(&[&source]));
        let c = engine.convert("jia");
        let machine = c.candidates.iter().find(|c| c.text == "机").expect("出「机」");
        assert_eq!(machine.consumed, "ji".len());
    }

    #[test]
    fn words_that_leave_untypable_letters_are_not_offered() {
        let engine = Engine::new(Dictionary::from_rime_sources(&[
            "开\tkai\t500\n会\thui\t500\n开会\tkai hui\t300\n开户\tkai hu\t300\n开花\tkai hua\t300\n",
        ]));
        let c = engine.convert("kaihui");
        assert_eq!(texts(&c), ["开会", "开"], "开户剩下 i，开花要把 h 当简拼");
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
        assert_eq!(c.preedit, "zhnog", "显示的是打的字母");
        assert_eq!(c.spelling, "zhong", "引擎理解成纠正后的拼写");
        let c = engine().convert("nihoa");
        assert_eq!(c.candidates[0].text, "你好");
        assert_eq!(c.preedit, "nihoa", "纠过错的词照原样显示，中间不加分隔符");
        assert_eq!(c.spelling, "ni'hao");
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
        assert_eq!(c.preedit, "nhiao");
        assert_eq!(c.spelling, "ni'hao");
    }

    #[test]
    fn doubled_letters_on_a_syllable_boundary_are_kept() {
        // naan 是 na'an，不是 nan 多按了一个 a；不然 naanni 会被吃掉一个 a 变成「南宁」
        let c = engine().convert("naan");
        assert_eq!(c.spelling, "na'an");
        assert_eq!(c.candidates[0].text, "那按");
        let c = engine().convert("naanni");
        assert_eq!(c.spelling, "na'an'ni");
        assert_eq!(c.candidates[0].text, "那按你");
    }

    #[test]
    fn unfinished_tail_keeps_typed_letters_and_offers_the_fix_second() {
        // suho 可能要接着打 suhong，也可能是「说」打反了：首选按照着打的读法，「说」排第二
        let c = engine().convert("suho");
        assert_eq!(c.spelling, "su'ho");
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
