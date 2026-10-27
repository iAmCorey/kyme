//! 词库：读取 Rime 格式的 .dict.yaml，建全拼、简拼两套索引。
//!
//! 两套索引的 key 都把第一个音节放在最高位，所以「以某几个音节开头的词」在排序后是连续的一段，
//! 一次二分查找就能判断有没有，切分时用它剪枝。

use std::cmp::Reverse;
use std::path::Path;
use std::{fmt, fs, io};

use crate::syllable::{SylId, SyllableTable};

/// 全拼索引最多支持 6 个音节，每个音节 id + 1 占 10 bit。
const EXACT_SLOTS: usize = 6;
const EXACT_BITS: u32 = 10;
/// 简拼索引最多支持 12 个音节，每个首字母（a–z 记作 1–26）占 5 bit。
const ABBR_SLOTS: usize = 12;
const ABBR_BITS: u32 = 5;

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
    /// 简拼索引：两列平行数组，按 key 排序，同一个 key 内按词频从高到低。
    abbr_keys: Vec<u64>,
    abbr_entries: Vec<u32>,
    /// 超过 6 个音节的词，前 6 个音节的全拼 key（有序、去重）。全拼索引放不下这些词，
    /// 判断「有没有以这几个音节开头的词」时要一起查，不然它们会在切分时被提前剪掉。
    long_prefixes: Vec<u64>,
    ln_total: f64,
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

struct Row<'a> {
    text: &'a str,
    code: &'a str,
    weight: u32,
}

/// 逐行读出词条。YAML 头以单独一行 `...` 结束，没有头就从第一行读。
fn rows(content: &str) -> impl Iterator<Item = Row<'_>> {
    let body = content.split_once("\n...\n").map_or(content, |(_, body)| body);
    body.lines().filter_map(|line| {
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut cols = line.split('\t');
        let text = cols.next()?;
        let code = cols.next()?;
        let weight = cols.next().and_then(|w| w.trim().trim_end_matches('%').parse::<f64>().ok()).unwrap_or(0.0) as u32;
        Some(Row { text, code, weight })
    })
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

fn exact_key(ids: &[SylId]) -> Option<u64> {
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

impl Dictionary {
    pub fn load_rime<P: AsRef<Path>>(paths: &[P]) -> Result<Self, LoadError> {
        let mut contents = Vec::with_capacity(paths.len());
        for path in paths {
            let path = path.as_ref();
            let content =
                fs::read_to_string(path).map_err(|source| LoadError { path: path.display().to_string(), source })?;
            contents.push(content);
        }
        Ok(Self::from_rime_sources(&contents.iter().map(String::as_str).collect::<Vec<_>>()))
    }

    pub fn from_rime_sources(sources: &[&str]) -> Self {
        let mut table = SyllableTable::default();
        let mut texts = String::new();
        let mut syls = Vec::new();
        let mut parsed = Vec::new();
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
                if ids.is_empty() || ids.len() > usize::from(u8::MAX) || row.text.len() > usize::from(u16::MAX) {
                    continue;
                }
                parsed.push(Entry {
                    text_off: texts.len() as u32,
                    syl_off: syls.len() as u32,
                    weight: row.weight,
                    text_len: row.text.len() as u16,
                    syl_len: ids.len() as u8,
                });
                texts.push_str(row.text);
                syls.extend_from_slice(&ids);
            }
        }
        table.finish();

        let ids_of = |e: &Entry| &syls[e.syl_off as usize..][..usize::from(e.syl_len)];
        let exact_key_of = |e: &Entry| exact_key(ids_of(e)).unwrap_or(u64::MAX);
        let mut entries = parsed;
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
        let (abbr_keys, abbr_entries) = abbr.into_iter().map(|(key, _, i)| (key, i)).unzip();
        let mut long_prefixes: Vec<u64> = entries
            .iter()
            .map(ids_of)
            .filter(|ids| ids.len() > EXACT_SLOTS)
            .filter_map(|ids| exact_key(&ids[..EXACT_SLOTS]))
            .collect();
        long_prefixes.sort_unstable();
        long_prefixes.dedup();
        let total: f64 = entries.iter().map(|e| f64::from(e.weight) + 1.0).sum();

        Dictionary {
            table,
            texts,
            syls,
            entries,
            exact_keys,
            abbr_keys,
            abbr_entries,
            long_prefixes,
            ln_total: total.max(1.0).ln(),
        }
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
        self.ln_total - (f64::from(self.entries[entry as usize].weight) + 1.0).ln()
    }

    /// 全拼完全一致的词，按词频从高到低。音节太多、全拼索引放不下时返回 None。
    pub fn exact(&self, ids: &[SylId]) -> Option<impl Iterator<Item = u32> + '_> {
        let key = exact_key(ids)?;
        Some(equal_range(&self.exact_keys, key).map(|i| i as u32))
    }

    /// 每个音节首字母依次为 `initials` 的词，按词频从高到低。调用方再按完整约束过滤。
    pub fn abbr(&self, initials: &[u8]) -> impl Iterator<Item = u32> + '_ {
        let range = abbr_key(initials.iter().copied()).map_or(0..0, |key| equal_range(&self.abbr_keys, key));
        self.abbr_entries[range].iter().copied()
    }

    /// 有没有以这几个音节开头的词。音节太多、判断不了时返回 true。
    pub fn has_exact_prefix(&self, ids: &[SylId]) -> bool {
        exact_key(ids).is_none_or(|key| {
            let range = prefix_range(key, ids.len(), EXACT_BITS, EXACT_SLOTS);
            any_in(&self.exact_keys, range) || any_in(&self.long_prefixes, range)
        })
    }

    /// 有没有首字母以 `initials` 开头的词。
    pub fn has_abbr_prefix(&self, initials: &[u8]) -> bool {
        abbr_key(initials.iter().copied())
            .is_some_and(|key| any_in(&self.abbr_keys, prefix_range(key, initials.len(), ABBR_BITS, ABBR_SLOTS)))
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
