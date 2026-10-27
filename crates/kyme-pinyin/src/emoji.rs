//! 词语到 emoji 的对照表，格式是雾凇拼音的 `opencc/emoji.txt`：
//! 每行 `词<Tab>词 emoji emoji…`，Tab 后面第一个是词本身（OpenCC 转换时保留原词用的），之后才是 emoji。

use std::collections::HashMap;

#[derive(Default)]
pub struct Emoji {
    table: HashMap<String, Vec<String>>,
}

impl Emoji {
    pub fn parse(text: &str) -> Self {
        let mut table = HashMap::new();
        for line in text.lines() {
            let Some((word, values)) = line.split_once('\t') else { continue };
            let emoji: Vec<String> =
                values.split_whitespace().filter(|value| *value != word).map(str::to_owned).collect();
            if !emoji.is_empty() {
                table.insert(word.to_owned(), emoji);
            }
        }
        Emoji { table }
    }

    /// 这个词对应的 emoji，按表里的顺序；没有就是空的。
    pub fn get(&self, word: &str) -> &[String] {
        self.table.get(word).map_or(&[], Vec::as_slice)
    }

    /// 有 emoji 的词的个数。
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rime_ice_format() {
        let emoji = Emoji::parse("笑\t笑 😄 😊\n你好\t你好 👋\n没有 Tab 的行\n空\t空\n");
        assert_eq!(emoji.get("笑"), ["😄", "😊"]);
        assert_eq!(emoji.get("你好"), ["👋"]);
        assert!(emoji.get("空").is_empty(), "只有原词、没有 emoji 的不算");
        assert!(emoji.get("哭").is_empty());
        assert_eq!(emoji.len(), 2);
    }
}
