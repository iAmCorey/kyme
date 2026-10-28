//! 用户词库的文件 `userdict.tsv`，和 settings.toml 放在同一个目录：每行是词、读音（音节用空格隔开）、
//! 选过几次、这个次数是哪天的数。只存在本机，只有自己能读写；日志里从不写词的内容。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kyme_pinyin::{SyllableTable, UserWord};

use crate::disk;

const USER_DICT_FILE: &str = "userdict.tsv";
/// 次数衰减到这么少就算忘了，不再读进来。
const FORGOTTEN: f64 = 0.1;

/// 今天是 1970-01-01 起的第几天。
pub fn today() -> u32 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| (since.as_secs() / 86_400) as u32)
}

pub struct UserDictFile {
    path: PathBuf,
    /// 上次读或写之后文件的修改时间。没读过、文件不在都是 None：反正都是空词库。
    seen: Option<SystemTime>,
}

impl UserDictFile {
    /// 和设置文件放在同一个目录。
    pub fn beside(settings: &Path) -> Self {
        UserDictFile { path: settings.with_file_name(USER_DICT_FILE), seen: None }
    }

    /// 文件在上次读或写之后被别人改过，比如设置窗口清除了词库。
    pub fn changed(&self) -> bool {
        disk::modified(&self.path) != self.seen
    }

    /// 文件被改过（或者还没读过）就读出里面的词，次数折算到今天。文件不在就是空的。没变返回 None。
    pub fn reload(&mut self, table: &SyllableTable, today: u32) -> Option<Vec<UserWord>> {
        if !self.changed() {
            return None;
        }
        self.seen = disk::modified(&self.path);
        let text = fs::read_to_string(&self.path).unwrap_or_default();
        Some(text.lines().filter_map(|line| parse(line, table, today)).collect())
    }

    /// 写回文件，只有自己能读写。
    pub fn save(&mut self, table: &SyllableTable, words: &[UserWord]) -> io::Result<()> {
        let mut text = String::new();
        for word in words {
            let pinyin: Vec<&str> = word.syllables.iter().map(|&id| table.name(id)).collect();
            text.push_str(&format!("{}\t{}\t{:.2}\t{}\n", word.text, pinyin.join(" "), word.count, word.day));
        }
        disk::write_atomic(&self.path, &text, true)?;
        self.seen = disk::modified(&self.path);
        Ok(())
    }

    /// 有几个词。
    pub fn count(&self) -> usize {
        fs::read_to_string(&self.path).map_or(0, |text| text.lines().count())
    }

    /// 清除：删掉文件。输入法下次切换输入框时发现文件没了，就清空。
    pub fn clear(&self) -> io::Result<()> {
        match fs::remove_file(&self.path) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

/// 读一行。读音里有认不出的音节、格式不对的行跳过。
fn parse(line: &str, table: &SyllableTable, today: u32) -> Option<UserWord> {
    let mut cols = line.split('\t');
    let (text, pinyin, count, day) = (cols.next()?, cols.next()?, cols.next()?, cols.next()?);
    let syllables = pinyin.split(' ').map(|name| table.exact(name)).collect::<Option<Vec<_>>>()?;
    let mut word = UserWord { text: text.into(), syllables, count: count.parse().ok()?, day: day.trim().parse().ok()? };
    word.decay_to(today);
    (!word.text.is_empty() && word.count >= FORGOTTEN).then_some(word)
}

#[cfg(test)]
mod tests {
    use kyme_pinyin::Dictionary;

    use super::*;

    #[test]
    fn saves_reloads_decays_and_clears() {
        let dir = std::env::temp_dir().join(format!("kyme-userdict-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let settings = dir.join("settings.toml");
        let dict = Dictionary::from_rime_sources(&["泥\tni\t1\n号\thao\t1\n"]);
        let table = dict.table();
        let read = |pinyin: &str| pinyin.split(' ').map(|name| table.exact(name).unwrap()).collect();
        let words = vec![
            UserWord { text: "泥".into(), syllables: read("ni"), count: 3.0, day: 100 },
            UserWord { text: "拟号".into(), syllables: read("ni hao"), count: 1.0, day: 100 },
        ];

        let mut file = UserDictFile::beside(&settings);
        assert_eq!(file.reload(table, 100), None, "还没有文件，就是空的，不用读");
        file.save(table, &words).unwrap();
        assert!(!file.changed(), "自己写的不算别人改过");
        assert_eq!(file.count(), 2);
        assert_eq!(UserDictFile::beside(&settings).reload(table, 100), Some(words.clone()));

        // 过了两个半衰期，次数剩四分之一
        let later = UserDictFile::beside(&settings).reload(table, 220).unwrap();
        assert!((later[0].count - 0.75).abs() < 1e-9, "{}", later[0].count);

        file.clear().unwrap();
        assert_eq!(file.reload(table, 100), Some(Vec::new()), "清除以后读到的是空的");
        assert_eq!(file.count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}
