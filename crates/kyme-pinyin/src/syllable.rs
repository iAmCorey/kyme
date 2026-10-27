//! 音节表：从词库的编码列里收集全部音节，再算出每个前缀能匹配哪些音节。
//!
//! 代码里不硬写音节清单，词库用到哪些音节，引擎就认哪些。

use std::collections::{HashMap, HashSet};

pub type SylId = u16;

/// 拼音里的元音字母（ü 写作 v），声母就是音节开头、第一个元音之前的部分。
const VOWELS: &[u8] = b"aeiouv";

/// 音节集合的容量。普通话音节约 410 个，留足余量；词库索引里每个 id 占 10 bit。
const CAPACITY: usize = 512;

/// 一个音节最多几个字母。
const MAX_SPELLING_LEN: usize = 6;

/// ü 的两种写法，词库和用户可能各用一种。
const ALIASES: [(&str, &str); 2] = [("lve", "lue"), ("nve", "nue")];

/// 音节集合（位图），表示「以某个前缀开头的所有音节」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SylSet([u64; CAPACITY / 64]);

impl SylSet {
    fn insert(&mut self, id: SylId) {
        self.0[id as usize / 64] |= 1 << (id % 64);
    }

    pub fn contains(&self, id: SylId) -> bool {
        self.0[id as usize / 64] & (1 << (id % 64)) != 0
    }

    pub fn len(&self) -> u32 {
        self.0.iter().map(|word| word.count_ones()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Default)]
pub struct SyllableTable {
    names: Vec<String>,
    exact: HashMap<String, SylId>,
    prefixes: HashMap<String, SylSet>,
    /// 漏打一个字母后的写法 → 原来的音节，例如 zhng → zhang / zheng / zhong。
    missing: HashMap<String, Vec<SylId>>,
    /// 声母，例如 b、zh。
    initials: HashSet<String>,
    max_len: usize,
}

impl SyllableTable {
    /// 从一组写法建表。只收纯小写字母、不超过 6 个字母的写法，其余忽略。
    pub fn from_spellings<'a>(spellings: impl IntoIterator<Item = &'a str>) -> Self {
        let mut table = SyllableTable::default();
        for spelling in spellings {
            table.intern(spelling);
        }
        table.finish();
        table
    }

    /// 读词库时逐个登记写法：第一次见到就分配新 id，不像音节的写法返回 None。
    pub(crate) fn intern(&mut self, spelling: &str) -> Option<SylId> {
        if let Some(&id) = self.exact.get(spelling) {
            return Some(id);
        }
        let valid = !spelling.is_empty()
            && spelling.len() <= MAX_SPELLING_LEN
            && spelling.bytes().all(|b| b.is_ascii_lowercase());
        if !valid || self.names.len() >= CAPACITY {
            return None;
        }
        let id = self.names.len() as SylId;
        self.names.push(spelling.to_string());
        self.exact.insert(spelling.to_string(), id);
        Some(id)
    }

    /// 登记完全部写法后调用：补上 ü 的另一种写法，算出每个前缀对应的音节集合。
    pub(crate) fn finish(&mut self) {
        for (a, b) in ALIASES {
            match (self.exact.get(a).copied(), self.exact.get(b).copied()) {
                (Some(id), None) => {
                    self.exact.insert(b.to_string(), id);
                }
                (None, Some(id)) => {
                    self.exact.insert(a.to_string(), id);
                }
                _ => {}
            }
        }
        for (spelling, &id) in &self.exact {
            for end in 1..=spelling.len() {
                self.prefixes.entry(spelling[..end].to_string()).or_default().insert(id);
            }
        }
        for name in &self.names {
            let len = name.bytes().take_while(|b| !VOWELS.contains(b)).count();
            if len > 0 {
                self.initials.insert(name[..len].to_string());
            }
        }
        // 只收至少两个字母、而且本身不是音节的写法，免得和简拼、正常输入混在一起
        for (id, name) in self.names.iter().enumerate() {
            for skip in 0..name.len() {
                let variant = format!("{}{}", &name[..skip], &name[skip + 1..]);
                if variant.len() >= 2 && !self.exact.contains_key(&variant) {
                    let ids = self.missing.entry(variant).or_default();
                    if !ids.contains(&(id as SylId)) {
                        ids.push(id as SylId);
                    }
                }
            }
        }
        self.max_len = self.exact.keys().map(String::len).max().unwrap_or(0);
    }

    /// 完整音节的 id。
    pub fn exact(&self, spelling: &str) -> Option<SylId> {
        self.exact.get(spelling).copied()
    }

    /// 以 `prefix` 开头的全部音节（含 `prefix` 本身是完整音节的情况）。
    pub fn prefix(&self, prefix: &str) -> Option<&SylSet> {
        self.prefixes.get(prefix)
    }

    /// 是不是声母，例如 zh。
    pub fn is_initial(&self, letters: &str) -> bool {
        self.initials.contains(letters)
    }

    /// 补上漏打的一个字母就能变成的音节。
    pub fn missing_letter(&self, spelling: &str) -> &[SylId] {
        self.missing.get(spelling).map_or(&[], Vec::as_slice)
    }

    pub fn name(&self, id: SylId) -> &str {
        &self.names[id as usize]
    }

    /// 音节的首字母，简拼索引用它做 key。
    pub fn initial(&self, id: SylId) -> u8 {
        self.names[id as usize].as_bytes()[0]
    }

    pub fn max_len(&self) -> usize {
        self.max_len
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_cover_longer_syllables() {
        let table = SyllableTable::from_spellings(["xi", "xian", "xiang", "an"]);
        let set = table.prefix("xia").unwrap();
        assert!(set.contains(table.exact("xian").unwrap()));
        assert!(set.contains(table.exact("xiang").unwrap()));
        assert!(!set.contains(table.exact("xi").unwrap()));
        assert_eq!(table.max_len(), 5);
    }

    #[test]
    fn umlaut_spellings_are_aliases() {
        let table = SyllableTable::from_spellings(["lve"]);
        assert_eq!(table.exact("lue"), table.exact("lve"));
    }

    #[test]
    fn missing_letter_variants() {
        let table = SyllableTable::from_spellings(["zhong", "zheng", "hong", "men"]);
        let ids: Vec<&str> = table.missing_letter("zhng").iter().map(|&id| table.name(id)).collect();
        assert_eq!(ids, ["zhong", "zheng"]);
        assert!(table.missing_letter("hong").is_empty(), "本身是音节的写法不算漏字母");
        assert_eq!(table.missing_letter("mn").len(), 1);
    }

    #[test]
    fn initials_come_from_syllables() {
        let table = SyllableTable::from_spellings(["zhong", "zong", "an", "ba"]);
        assert!(table.is_initial("zh") && table.is_initial("z") && table.is_initial("b"));
        assert!(!table.is_initial("zo") && !table.is_initial("a"));
    }

    #[test]
    fn rejects_non_syllables() {
        let table = SyllableTable::from_spellings(["Ni", "n1", "toolongword", ""]);
        assert!(table.is_empty());
    }
}
