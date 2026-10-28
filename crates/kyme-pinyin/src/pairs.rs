//! 字和字的搭配：词库里哪两个字常常前后挨着出现。组句时用它看相邻两个词接得顺不顺：
//! 「难」后面接「喝」很常见（难喝），接「和」几乎没有。只看每个词各自的词频的话，「和」比「喝」常用得多，
//! haonanhe 就会组出「好难和」。

use crate::dict::Dictionary;

/// 一对字一起出现的次数（含这一对字的词，词频加起来）到这么多就不罚。
const COMMON: u64 = 100_000;
/// 罚 ln(COMMON / 次数) 再乘这个数：按次数连续地罚，次数越少罚得越重，一次都没有的罚约 2.3。
/// 不能「出现过就不罚」：冷门词「点九图」会让「点九」算常见，youdianjiujie 就成了「有点九节」。
const SCALE: f64 = 0.2;
/// 一个字占的位数：Unicode 最大到 0x10FFFF。
const CHAR_BITS: u32 = 21;
/// 次数占的位数，再多的按最大值算：够分出常不常见就行。
const COUNT_BITS: u32 = u64::BITS - 2 * CHAR_BITS;
const COUNT_MAX: u64 = (1 << COUNT_BITS) - 1;

/// 每一对前后挨着的字和它们一起出现的次数，各压成一个 u64（从高位起：前一个字、后一个字、次数），排好序。
pub(crate) struct Pairs(Vec<u64>);

impl Pairs {
    /// 数主词库里每个词中前后挨着的两个字。主词库约 136 万对、去重后 44 万对，建一次约几十毫秒、占 3.5 MB。
    pub(crate) fn new(dict: &Dictionary) -> Self {
        // 先数好有多少对，一次分配够：按倍数长大的话，释放掉的旧数组不会还给系统，常驻内存多出十来 MB
        let total = (0..dict.len() as u32).map(|entry| dict.text(entry).chars().count().saturating_sub(1)).sum();
        let mut pairs = Vec::with_capacity(total);
        for entry in 0..dict.len() as u32 {
            let count = u64::from(dict.weight(entry)).min(COUNT_MAX);
            let text = dict.text(entry);
            for (a, b) in text.chars().zip(text.chars().skip(1)) {
                pairs.push((pair(a, b) << COUNT_BITS) | count);
            }
        }
        pairs.sort_unstable();
        // 同一对字合成一个，次数相加
        pairs.dedup_by(|later, kept| {
            let same = *later >> COUNT_BITS == *kept >> COUNT_BITS;
            if same {
                let count = ((*kept & COUNT_MAX) + (*later & COUNT_MAX)).min(COUNT_MAX);
                *kept = (*kept & !COUNT_MAX) | count;
            }
            same
        });
        // 挪到一块正好大小的新数组里，大的那块整个释放
        Pairs(pairs.as_slice().to_vec())
    }

    /// `a` 后面紧接着 `b` 的代价：搭配越少见越高。词库里没有多字词、看不出搭配的话都是 0。
    pub(crate) fn cost(&self, a: char, b: char) -> f64 {
        if self.0.is_empty() {
            return 0.0;
        }
        // 合并以后每对字只有一个
        let key = pair(a, b);
        let count =
            self.0.binary_search_by_key(&key, |&packed| packed >> COUNT_BITS).map_or(0, |at| self.0[at] & COUNT_MAX);
        if count >= COMMON { 0.0 } else { SCALE * ((COMMON + 1) as f64 / (count + 1) as f64).ln() }
    }
}

fn pair(a: char, b: char) -> u64 {
    (u64::from(a) << CHAR_BITS) | u64::from(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rarer_pairs_cost_more() {
        let dict = Dictionary::from_rime_sources(&[
            "难喝\tnan he\t3000\n好难\thao nan\t60\n很好难\then hao nan\t60\n点九图\tdian jiu tu\t150000\n",
        ]);
        let pairs = Pairs::new(&dict);
        assert_eq!(pairs.cost('点', '九'), 0.0, "十万次以上不罚");
        assert!(pairs.cost('难', '喝') < pairs.cost('好', '难'), "次数越多罚得越轻");
        assert!(pairs.cost('好', '难') < pairs.cost('很', '好'), "两个词里的次数加起来");
        assert!(pairs.cost('很', '好') < pairs.cost('难', '和'), "没见过的罚得最重");
        assert_eq!(pairs.cost('喝', '难'), pairs.cost('难', '和'), "要按顺序挨着");
        let single = Dictionary::from_rime_sources(&["好\thao\t100\n"]);
        assert_eq!(Pairs::new(&single).cost('难', '和'), 0.0, "看不出搭配就不罚");
    }
}
