//! 用户词库：常选的词，和分几次选出来的词组。
//!
//! 纯数据：不读文件也不看时钟，今天是哪天、什么时候存盘都由调用方决定。词的代价和主词库直接可比，
//! 组句时和主词库的词放在一起比。

use crate::dict::{EXACT_SLOTS, KeyIndex, TYPICAL_WEIGHT, abbr_key_of, abbr_prefix, exact_key, exact_prefix};
use crate::engine::Engine;
use crate::syllable::SylId;
/// 加分的上限：选得再多，也不该压过所有常用词。
const MAX_BONUS: f64 = 6.0;
/// 最多记多少个词，超出时淘汰选得最少的。
pub const MAX_WORDS: usize = 10_000;
/// 一个词最多几个音节：全拼索引放得下的长度。
const MAX_SYLLABLES: usize = EXACT_SLOTS;
/// 选过的次数每过这么多天减半，很久不用的词慢慢让位。
const HALF_LIFE_DAYS: f64 = 60.0;

#[derive(Clone, Debug, PartialEq)]
pub struct UserWord {
    pub text: String,
    pub syllables: Vec<SylId>,
    /// 选过几次，是 `day` 那天的数：之后每过 60 天减半。
    pub count: f64,
    /// 从 1970-01-01 起的天数。
    pub day: u32,
}

impl UserWord {
    /// 把次数折算到 `today` 那天。
    pub fn decay_to(&mut self, today: u32) {
        let days = today.saturating_sub(self.day);
        self.count *= 0.5f64.powf(f64::from(days) / HALF_LIFE_DAYS);
        self.day = self.day.max(today);
    }
}

/// 选的次数换成代价上的加分：头几次加得多，之后越来越少，有上限。
fn bonus(count: f64) -> f64 {
    (2.5 * count.ln_1p()).min(MAX_BONUS)
}

pub struct UserDict {
    words: Vec<UserWord>,
    /// 每个词的基础代价：主词库、本地词库里有的词用它在词库里的代价，新词按中位数词频算。
    /// 学词时只改次数，不用再查词库；词库换了要整个重建。
    bases: Vec<f64>,
    /// 每个词的代价：基础代价减去加分。
    costs: Vec<f64>,
    exact: KeyIndex,
    abbr: KeyIndex,
    /// 所有词的次数都折算到了这一天。
    day: u32,
}

impl UserDict {
    pub const fn empty() -> Self {
        UserDict {
            words: Vec::new(),
            bases: Vec::new(),
            costs: Vec::new(),
            exact: KeyIndex::new(),
            abbr: KeyIndex::new(),
            day: 0,
        }
    }

    /// 从读出来的词建，代价按 `engine` 的词库算。超过上限时留下选得多的。
    pub fn new(engine: &Engine, mut words: Vec<UserWord>) -> Self {
        words.retain(|word| (1..=MAX_SYLLABLES).contains(&word.syllables.len()));
        if words.len() > MAX_WORDS {
            words.sort_by(|a, b| b.count.total_cmp(&a.count));
            words.truncate(MAX_WORDS);
        }
        let bases: Vec<f64> = words.iter().map(|word| base_cost(engine, word)).collect();
        let costs = words.iter().zip(&bases).map(|(word, base)| base - bonus(word.count)).collect();
        let keys = |key_of: &dyn Fn(&UserWord) -> Option<u64>| {
            KeyIndex::build(words.iter().enumerate().filter_map(|(i, word)| Some((key_of(word)?, i as u32))).collect())
        };
        let exact = keys(&|word| exact_key(&word.syllables));
        let abbr = keys(&|word| abbr_key_of(&initials(engine, &word.syllables)));
        let day = words.iter().map(|word| word.day).max().unwrap_or(0);
        UserDict { words, bases, costs, exact, abbr, day }
    }

    pub fn words(&self) -> &[UserWord] {
        &self.words
    }

    pub fn into_words(self) -> Vec<UserWord> {
        self.words
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// 记一次「选了这个词」，返回记没记。空的、音节太多的不记。
    pub fn learn(&mut self, engine: &Engine, text: &str, syllables: &[SylId], day: u32) -> bool {
        if !(1..=MAX_SYLLABLES).contains(&syllables.len()) {
            return false;
        }
        self.age_to(day);
        let index = match self.words.iter().position(|word| word.text == text && word.syllables == syllables) {
            Some(index) => {
                self.words[index].count += 1.0;
                index
            }
            None => {
                let word = UserWord { text: text.into(), syllables: syllables.to_vec(), count: 1.0, day };
                // 满了：先淘汰一个选得最少的老词，再放新词进来，整个重建。不然新词一进来就被挤掉，永远学不会
                if self.words.len() >= MAX_WORDS {
                    let mut words = std::mem::take(&mut self.words);
                    if let Some(fewest) = (0..words.len()).min_by(|&a, &b| words[a].count.total_cmp(&words[b].count)) {
                        words.swap_remove(fewest);
                    }
                    words.push(word);
                    *self = UserDict::new(engine, words);
                    return true;
                }
                let index = self.words.len();
                self.exact.insert(exact_key(syllables), index as u32);
                self.abbr.insert(abbr_key_of(&initials(engine, syllables)), index as u32);
                self.bases.push(base_cost(engine, &word));
                self.costs.push(0.0);
                self.words.push(word);
                index
            }
        };
        self.costs[index] = self.bases[index] - bonus(self.words[index].count);
        true
    }

    /// 日子变了，把所有词的次数统一折算到 `day`，代价跟着重算。不然只有刚选的那个词在衰减，
    /// 很久没用的词再选一次反而排得更低。一天最多做一次。
    fn age_to(&mut self, day: u32) {
        if day <= self.day {
            return;
        }
        for ((word, base), cost) in self.words.iter_mut().zip(&self.bases).zip(&mut self.costs) {
            word.decay_to(day);
            *cost = base - bonus(word.count);
        }
        self.day = day;
    }

    pub(crate) fn text(&self, word: u32) -> &str {
        &self.words[word as usize].text
    }

    pub(crate) fn syllables(&self, word: u32) -> &[SylId] {
        &self.words[word as usize].syllables
    }

    pub(crate) fn cost(&self, word: u32) -> f64 {
        self.costs[word as usize]
    }

    /// 全拼完全一致的词。
    pub(crate) fn exact(&self, ids: &[SylId]) -> &[u32] {
        self.exact.get(exact_key(ids))
    }

    /// 每个音节首字母依次为 `initials` 的词。调用方再按完整约束过滤。
    pub(crate) fn abbr(&self, initials: &[u8]) -> &[u32] {
        self.abbr.get(abbr_key_of(initials))
    }

    /// 有没有以这几个音节开头的词。
    pub(crate) fn has_exact_prefix(&self, ids: &[SylId]) -> bool {
        self.exact.has_prefix(exact_prefix(ids))
    }

    /// 有没有首字母以 `initials` 开头的词。
    pub(crate) fn has_abbr_prefix(&self, initials: &[u8]) -> bool {
        self.abbr.has_prefix(abbr_prefix(initials))
    }
}

impl Default for UserDict {
    fn default() -> Self {
        Self::empty()
    }
}

fn base_cost(engine: &Engine, word: &UserWord) -> f64 {
    engine
        .word_cost(&word.text, &word.syllables)
        .unwrap_or_else(|| engine.dictionary().cost_of_weight(f64::from(TYPICAL_WEIGHT)))
}

fn initials(engine: &Engine, syllables: &[SylId]) -> Vec<u8> {
    syllables.iter().map(|&id| engine.dictionary().table().initial(id)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::dict::Dictionary;

    fn engine() -> Engine {
        Engine::new(Dictionary::from_rime_sources(&["你\tni\t1000\n泥\tni\t50\n"]))
    }

    fn ni(engine: &Engine) -> Vec<SylId> {
        vec![engine.dictionary().table().exact("ni").unwrap()]
    }

    #[test]
    fn every_word_ages_when_the_day_changes() {
        let engine = engine();
        let mut user = UserDict::default();
        user.learn(&engine, "你", &ni(&engine), 0);
        user.learn(&engine, "泥", &ni(&engine), 0);
        // 120 天后再选「泥」：「你」也一起折算，剩四分之一
        user.learn(&engine, "泥", &ni(&engine), 120);
        let count = |text: &str| user.words().iter().find(|word| word.text == text).unwrap().count;
        assert!((count("你") - 0.25).abs() < 1e-9);
        assert!((count("泥") - 1.25).abs() < 1e-9);
    }

    #[test]
    fn a_full_dictionary_still_takes_new_words() {
        let engine = engine();
        let words = (0..MAX_WORDS)
            .map(|i| UserWord { text: format!("词{i}"), syllables: ni(&engine), count: 2.0, day: 0 })
            .collect();
        let mut user = UserDict::new(&engine, words);
        user.learn(&engine, "泥", &ni(&engine), 0);
        assert_eq!(user.words().len(), MAX_WORDS);
        user.learn(&engine, "泥", &ni(&engine), 0);
        let ni = user.words().iter().find(|word| word.text == "泥").expect("新词没被挤掉");
        assert_eq!(ni.count, 2.0, "再选一次接着累计");
    }
}
