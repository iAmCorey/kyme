//! 词库：读取 Rime 格式的 .dict.yaml，建全拼、简拼两套索引。
//!
//! 两套索引的 key 都把第一个音节放在最高位，所以「以某几个音节开头的词」在排序后是连续的一段，
//! 一次二分查找就能判断有没有，切分时用它剪枝。

use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::{fmt, fs, io};

use crate::syllable::{SylId, SyllableTable};

/// 全拼索引最多支持 6 个音节，每个音节 id + 1 占 10 bit。
pub(crate) const EXACT_SLOTS: usize = 6;
const EXACT_BITS: u32 = 10;
/// 简拼索引最多支持 12 个音节，每个首字母（a–z 记作 1–26）占 5 bit。
const ABBR_SLOTS: usize = 12;
const ABBR_BITS: u32 = 5;
/// 自带词库词频的中位数，用来给不知道词频的词定个中等的代价：学到的新词、导入的外部词库。
pub(crate) const TYPICAL_WEIGHT: u32 = 500;
/// 词库文件的扩展名，Rime 的写法。
pub const DICT_SUFFIX: &str = ".dict.yaml";

#[derive(Clone, Copy)]
struct Entry {
    text_off: u32,
    syl_off: u32,
    weight: u32,
    text_len: u16,
    syl_len: u8,
}

pub struct Dictionary {
    table: SyllableTable,
    texts: String,
    syls: Vec<SylId>,
    /// 按全拼 key 排序，同一个 key 内按词频从高到低。
    entries: Vec<Entry>,
    exact_keys: Vec<u64>,
    /// 简拼索引，同一个 key 内按词频从高到低。
    abbr: KeyIndex,
    /// 超过 6 个音节的词，前 6 个音节的全拼 key（有序、去重）。全拼索引放不下这些词，
    /// 判断「有没有以这几个音节开头的词」时要一起查，不然它们会在切分时被提前剪掉。
    long_prefixes: Vec<u64>,
    ln_total: f64,
    /// 按文字查词条：全部词条按文字排序，同一个写法只留最常用的读音。给没写拼音的本地词补读音用，
    /// 第一次用到时才建。
    by_text: OnceLock<Vec<u32>>,
}

#[derive(Debug)]
pub struct LoadError {
    path: String,
    source: io::Error,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "读取词库 {} 失败：{}", self.path, self.source)
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

impl LoadError {
    pub(crate) fn new(path: &Path, source: io::Error) -> Self {
        LoadError { path: path.display().to_string(), source }
    }
}

/// 目录里的词库文件，按文件名排序。
pub fn dict_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.to_string_lossy().ends_with(DICT_SUFFIX))
        .collect();
    paths.sort();
    Ok(paths)
}

/// 读整个文件，出错时带上路径。
pub(crate) fn read_file(path: &Path) -> Result<String, LoadError> {
    fs::read_to_string(path).map_err(|source| LoadError::new(path, source))
}

struct Row<'a> {
    text: &'a str,
    code: &'a str,
    weight: u32,
}

/// 去掉 YAML 头，剩下的是词条。头以单独一行 `...` 结束，没有头就是整个文件。
pub(crate) fn body(content: &str) -> &str {
    content.split_once("\n...\n").map_or(content, |(_, body)| body)
}

/// 逐行读出词条。
fn rows(content: &str) -> impl Iterator<Item = Row<'_>> {
    body(content).lines().filter_map(|line| {
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut cols = line.split('\t');
        let text = cols.next()?;
        let code = cols.next()?;
        let weight = cols.next().and_then(weight).unwrap_or(0);
        Some(Row { text, code, weight })
    })
}

/// 词频那一列：一串数字，可以带小数点，或者像 Rime 那样带百分号。`nan` 这种既是音节又能当数字读的不算。
pub(crate) fn weight(token: &str) -> Option<u32> {
    let digits = token.trim().trim_end_matches('%');
    let numeric = !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    numeric.then(|| digits.parse::<f64>().ok()).flatten().map(|weight| weight.min(f64::from(u32::MAX)) as u32)
}

/// 把若干值从最高位起依次放进 `slots` 个 `bits` 位宽的格子，后面空着的格子是 0。
fn pack(values: impl Iterator<Item = u64>, bits: u32, slots: usize) -> u64 {
    let mut key = 0;
    let mut shift = bits * slots as u32;
    for value in values {
        shift -= bits;
        key |= value << shift;
    }
    key
}

/// 以这串 key 开头的 key 所在的区间 [lo, hi)。
fn prefix_range(prefix: u64, len: usize, bits: u32, slots: usize) -> (u64, u64) {
    (prefix, prefix + (1 << (bits * (slots - len) as u32)))
}

pub(crate) fn exact_key(ids: &[SylId]) -> Option<u64> {
    (ids.len() <= EXACT_SLOTS).then(|| pack(ids.iter().map(|&id| u64::from(id) + 1), EXACT_BITS, EXACT_SLOTS))
}

fn abbr_key(initials: impl ExactSizeIterator<Item = u8>) -> Option<u64> {
    (initials.len() <= ABBR_SLOTS).then(|| pack(initials.map(|c| u64::from(c - b'a' + 1)), ABBR_BITS, ABBR_SLOTS))
}

/// 有序数组里是否存在落在 [lo, hi) 的值。
fn any_in(sorted: &[u64], (lo, hi): (u64, u64)) -> bool {
    sorted.get(sorted.partition_point(|&k| k < lo)).is_some_and(|&k| k < hi)
}

/// 有序数组里等于 `key` 的那一段。
fn equal_range(sorted: &[u64], key: u64) -> std::ops::Range<usize> {
    sorted.partition_point(|&k| k < key)..sorted.partition_point(|&k| k <= key)
}

/// 以这几个音节开头的全拼 key 所在的区间。音节太多、放不下时是 None。
pub(crate) fn exact_prefix(ids: &[SylId]) -> Option<(u64, u64)> {
    exact_key(ids).map(|key| prefix_range(key, ids.len(), EXACT_BITS, EXACT_SLOTS))
}

/// 首字母依次为 `initials` 的简拼 key。
pub(crate) fn abbr_key_of(initials: &[u8]) -> Option<u64> {
    abbr_key(initials.iter().copied())
}

/// 首字母以 `initials` 开头的简拼 key 所在的区间。
pub(crate) fn abbr_prefix(initials: &[u8]) -> Option<(u64, u64)> {
    abbr_key_of(initials).map(|key| prefix_range(key, initials.len(), ABBR_BITS, ABBR_SLOTS))
}

/// 一套有序索引：key 和词的下标两列平行，按 key 排序。查某个 key 下的词、判断有没有以某串开头的 key，
/// 都是二分查找。主词库的简拼索引和用户词库都用它。
pub(crate) struct KeyIndex {
    keys: Vec<u64>,
    ids: Vec<u32>,
}

impl KeyIndex {
    pub(crate) const fn new() -> Self {
        KeyIndex { keys: Vec::new(), ids: Vec::new() }
    }

    /// 从已经按 key 排好序的 (key, 下标) 建。
    fn from_sorted(pairs: impl Iterator<Item = (u64, u32)>) -> Self {
        let (keys, ids) = pairs.unzip();
        KeyIndex { keys, ids }
    }

    pub(crate) fn build(mut pairs: Vec<(u64, u32)>) -> Self {
        pairs.sort_unstable();
        Self::from_sorted(pairs.into_iter())
    }

    /// 插一个词，保持有序。key 是 None（放不下）就不插。
    pub(crate) fn insert(&mut self, key: Option<u64>, id: u32) {
        if let Some(key) = key {
            let at = self.keys.partition_point(|&k| k <= key);
            self.keys.insert(at, key);
            self.ids.insert(at, id);
        }
    }

    pub(crate) fn get(&self, key: Option<u64>) -> &[u32] {
        key.map_or(&[], |key| &self.ids[equal_range(&self.keys, key)])
    }

    pub(crate) fn has_prefix(&self, range: Option<(u64, u64)>) -> bool {
        range.is_some_and(|range| any_in(&self.keys, range))
    }
}

/// 建词库时先把词条攒在这里，最后一起排序、建索引。主词库和本地词库共用。
#[derive(Default)]
pub(crate) struct Builder {
    texts: String,
    syls: Vec<SylId>,
    entries: Vec<Entry>,
}

impl Builder {
    /// 加一个词。没有读音、读音或文字太长的不要。
    pub(crate) fn push(&mut self, text: &str, ids: &[SylId], weight: u32) {
        if ids.is_empty() || ids.len() > usize::from(u8::MAX) || text.len() > usize::from(u16::MAX) {
            return;
        }
        self.entries.push(Entry {
            text_off: self.texts.len() as u32,
            syl_off: self.syls.len() as u32,
            weight,
            text_len: text.len() as u16,
            syl_len: ids.len() as u8,
        });
        self.texts.push_str(text);
        self.syls.extend_from_slice(ids);
    }

    /// 排序、建索引。`ln_total` 是 None 时按这些词自己的总词频算代价。
    fn build(self, table: SyllableTable, ln_total: Option<f64>) -> Dictionary {
        let Builder { texts, syls, mut entries } = self;
        let ids_of = |e: &Entry| &syls[e.syl_off as usize..][..usize::from(e.syl_len)];
        let exact_key_of = |e: &Entry| exact_key(ids_of(e)).unwrap_or(u64::MAX);
        // 稳定排序：同 key 同词频时保持读入顺序
        entries.sort_by_cached_key(|e| (exact_key_of(e), Reverse(e.weight)));
        let exact_keys: Vec<u64> = entries.iter().map(exact_key_of).collect();

        let mut abbr: Vec<(u64, Reverse<u32>, u32)> = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let key = abbr_key(ids_of(e).iter().map(|&id| table.initial(id)))?;
                Some((key, Reverse(e.weight), i as u32))
            })
            .collect();
        abbr.sort_unstable();
        let abbr = KeyIndex::from_sorted(abbr.into_iter().map(|(key, _, i)| (key, i)));
        let mut long_prefixes: Vec<u64> = entries
            .iter()
            .map(ids_of)
            .filter(|ids| ids.len() > EXACT_SLOTS)
            .filter_map(|ids| exact_key(&ids[..EXACT_SLOTS]))
            .collect();
        long_prefixes.sort_unstable();
        long_prefixes.dedup();
        let ln_total = ln_total.unwrap_or_else(|| {
            let total: f64 = entries.iter().map(|e| f64::from(e.weight) + 1.0).sum();
            total.max(1.0).ln()
        });

        Dictionary { table, texts, syls, entries, exact_keys, abbr, long_prefixes, ln_total, by_text: OnceLock::new() }
    }
}

impl Dictionary {
    pub fn load_rime<P: AsRef<Path>>(paths: &[P]) -> Result<Self, LoadError> {
        let contents = paths.iter().map(|path| read_file(path.as_ref())).collect::<Result<Vec<_>, _>>()?;
        Ok(Self::from_rime_sources(&contents.iter().map(String::as_str).collect::<Vec<_>>()))
    }

    pub fn from_rime_sources(sources: &[&str]) -> Self {
        let mut table = SyllableTable::default();
        let mut words = Builder::default();
        let mut ids = Vec::new();
        for source in sources {
            'rows: for row in rows(source) {
                ids.clear();
                for spelling in row.code.split(' ') {
                    match table.intern(spelling) {
                        Some(id) => ids.push(id),
                        None => continue 'rows,
                    }
                }
                words.push(row.text, &ids, row.weight);
            }
        }
        table.finish();
        words.build(table, None)
    }

    /// 和这个词库共用音节表和总词频的另一套词（本地词库）：两边的代价是同一个尺度，可以放在一起比，
    /// 加了词也不会改变这个词库里每个词的代价。
    pub(crate) fn sibling(&self, words: Builder) -> Dictionary {
        words.build(self.table.clone(), Some(self.ln_total))
    }

    pub fn table(&self) -> &SyllableTable {
        &self.table
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn text(&self, entry: u32) -> &str {
        let e = self.entries[entry as usize];
        &self.texts[e.text_off as usize..][..usize::from(e.text_len)]
    }

    pub fn syllables(&self, entry: u32) -> &[SylId] {
        let e = self.entries[entry as usize];
        &self.syls[e.syl_off as usize..][..usize::from(e.syl_len)]
    }

    /// 一元模型下的代价：-ln(词频 / 总词频)。
    pub fn cost(&self, entry: u32) -> f64 {
        self.cost_of_weight(f64::from(self.entries[entry as usize].weight))
    }

    /// 词频为 `weight` 的词的代价。
    pub(crate) fn cost_of_weight(&self, weight: f64) -> f64 {
        self.ln_total - (weight + 1.0).ln()
    }

    /// 文字是 `text` 的词条，同一个写法有几个读音的（「行」xing、hang）取最常用的。
    /// 第一次调用时建索引（主词库约 0.1 秒、2 MB），之后一直留着。
    pub(crate) fn find_text(&self, text: &str) -> Option<u32> {
        let index = self.by_text.get_or_init(|| {
            let weight = |entry: u32| self.entries[entry as usize].weight;
            let mut index: Vec<u32> = (0..self.entries.len() as u32).collect();
            index.sort_unstable_by(|&a, &b| self.text(a).cmp(self.text(b)).then(weight(b).cmp(&weight(a))));
            // 同一个写法只留排在最前、也就是最常用的那个
            index.dedup_by(|later, kept| self.text(*later) == self.text(*kept));
            index
        });
        index.binary_search_by(|&entry| self.text(entry).cmp(text)).ok().map(|at| index[at])
    }

    /// 读音为 `ids`、文字为 `text` 的词条。
    pub fn find(&self, text: &str, ids: &[SylId]) -> Option<u32> {
        self.exact(ids)?.find(|&entry| self.text(entry) == text)
    }

    /// 全拼完全一致的词，按词频从高到低。音节太多、全拼索引放不下时返回 None。
    pub fn exact(&self, ids: &[SylId]) -> Option<impl Iterator<Item = u32> + '_> {
        let key = exact_key(ids)?;
        Some(equal_range(&self.exact_keys, key).map(|i| i as u32))
    }

    /// 每个音节首字母依次为 `initials` 的词，按词频从高到低。调用方再按完整约束过滤。
    pub fn abbr(&self, initials: &[u8]) -> impl Iterator<Item = u32> + '_ {
        self.abbr.get(abbr_key_of(initials)).iter().copied()
    }

    /// 有没有以这几个音节开头的词。音节太多、判断不了时返回 true。
    pub fn has_exact_prefix(&self, ids: &[SylId]) -> bool {
        exact_prefix(ids).is_none_or(|range| any_in(&self.exact_keys, range) || any_in(&self.long_prefixes, range))
    }

    /// 有没有首字母以 `initials` 开头的词。
    pub fn has_abbr_prefix(&self, initials: &[u8]) -> bool {
        self.abbr.has_prefix(abbr_prefix(initials))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nname: sample\n...\n\
        你\tni\t100\n\
        你好\tni hao\t50\n\
        拟好\tni hao\t5\n\
        # 注释行\n\
        好\thao\t80\n";

    #[test]
    fn skips_header_and_comments() {
        let dict = Dictionary::from_rime_sources(&[SAMPLE]);
        assert_eq!(dict.len(), 4);
    }

    #[test]
    fn exact_lookup_is_sorted_by_weight() {
        let dict = Dictionary::from_rime_sources(&[SAMPLE]);
        let table = dict.table();
        let ids = [table.exact("ni").unwrap(), table.exact("hao").unwrap()];
        let words: Vec<&str> = dict.exact(&ids).unwrap().map(|e| dict.text(e)).collect();
        assert_eq!(words, ["你好", "拟好"]);
    }

    #[test]
    fn abbr_lookup_uses_initials() {
        let dict = Dictionary::from_rime_sources(&[SAMPLE]);
        let words: Vec<&str> = dict.abbr(b"nh").map(|e| dict.text(e)).collect();
        assert_eq!(words, ["你好", "拟好"]);
    }

    #[test]
    fn prefix_checks() {
        let dict = Dictionary::from_rime_sources(&[SAMPLE]);
        let table = dict.table();
        let (ni, hao) = (table.exact("ni").unwrap(), table.exact("hao").unwrap());
        assert!(dict.has_exact_prefix(&[ni]));
        assert!(dict.has_exact_prefix(&[ni, hao]));
        assert!(!dict.has_exact_prefix(&[hao, ni]));
        assert!(dict.has_abbr_prefix(b"n"));
        assert!(dict.has_abbr_prefix(b"nh"));
        assert!(!dict.has_abbr_prefix(b"hn"));
    }

    #[test]
    fn prefix_checks_see_words_longer_than_the_exact_index() {
        let dict = Dictionary::from_rime_sources(&["横眉冷对千夫指\theng mei leng dui qian fu zhi\t1\n"]);
        let table = dict.table();
        let ids: Vec<SylId> = ["heng", "mei", "leng", "dui", "qian"].iter().map(|s| table.exact(s).unwrap()).collect();
        assert!(dict.has_exact_prefix(&ids[..1]));
        assert!(dict.has_exact_prefix(&ids));
    }
}
