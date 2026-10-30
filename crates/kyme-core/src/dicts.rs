//! 本地词库的目录 `dicts/`，和 settings.toml 放在一起：自定义词库是 `dicts/custom.dict.yaml`，导入的外部词库
//! 转成同样的写法放在 `dicts/imported/`，一个词库一个文件。都只存在本机，只有自己能读写。

use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use kyme_pinyin::local::{self, IMPORTED_WEIGHT, Row};
use kyme_pinyin::{DICT_SUFFIX, dict_files};

use crate::{disk, import};

const DIR: &str = "dicts";
/// 导入的外部词库放在 `dicts/` 下的这个目录里。
const IMPORTED: &str = "imported";
const CUSTOM: &str = "custom.dict.yaml";
const CUSTOM_TEMPLATE: &str = "\
# Kyme 自定义词库：每行一个词条，保存后切换输入框即可生效。
# 拼音可省略，省略时按常用读音补全；多音字和人名建议注明拼音。词频可省略，数值越大越靠前。
# 词条、拼音、词频之间用 Tab 或空格分隔，例如：
#
# 鲲鹏
# 曾小贤 zeng xiao xian
# 鲲鹏 kun peng 100000
";

/// 本地词库里的一个词：文字，和写的拼音（小写，音节用空格隔开，没写是空的）。
#[derive(Clone, Debug, PartialEq)]
pub struct LocalWord {
    pub text: String,
    pub pinyin: String,
}

impl From<Row<'_>> for LocalWord {
    fn from(row: Row<'_>) -> Self {
        LocalWord { text: row.text.into(), pinyin: row.pinyin.join(" ").to_ascii_lowercase() }
    }
}

/// 一个导入的外部词库。
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub name: String,
    pub words: usize,
}

/// 一个文件和它的修改时间。
type Stamped = (PathBuf, Option<SystemTime>);

pub struct LocalDicts {
    dir: PathBuf,
    /// 上次读的时候有哪些词库文件、各自的修改时间。
    seen: Vec<Stamped>,
    /// 数过的外部词库有几个词，文件没改过就不再数：设置窗口每次回到前面都要列一遍。
    counted: HashMap<PathBuf, (Option<SystemTime>, usize)>,
}

impl LocalDicts {
    /// 和设置文件放在同一个目录下。
    pub fn beside(settings: &Path) -> Self {
        LocalDicts { dir: settings.with_file_name(DIR), seen: Vec::new(), counted: HashMap::new() }
    }

    /// 有词库加了、删了、改了就返回现在的全部词库文件：`dicts/` 里的和 `dicts/imported/` 里的，各自按文件名排序。
    /// 没变返回 None，一个词库都没有也算没变。
    pub fn reload(&mut self) -> Option<Vec<PathBuf>> {
        let now: Vec<Stamped> = [self.dir.clone(), self.imported_dir()].iter().flat_map(|dir| stamped(dir)).collect();
        if now == self.seen {
            return None;
        }
        let files = now.iter().map(|(path, _)| path.clone()).collect();
        self.seen = now;
        Some(files)
    }

    fn imported_dir(&self) -> PathBuf {
        self.dir.join(IMPORTED)
    }

    /// 自定义词库的文件。还没有就先写一份带说明的。
    pub fn custom(&self) -> Result<PathBuf, String> {
        let path = self.dir.join(CUSTOM);
        if !path.exists() {
            disk::write_atomic(&path, CUSTOM_TEMPLATE, true).map_err(|err| format!("无法创建自定义词库文件：{err}"))?;
        }
        Ok(path)
    }

    /// 自定义词库里的词，按文件里的顺序。
    pub fn custom_words(&self) -> Vec<LocalWord> {
        let text = fs::read_to_string(self.dir.join(CUSTOM)).unwrap_or_default();
        local::rows(&text).map(LocalWord::from).collect()
    }

    /// 往自定义词库里加一个词，加在最后。一般只写词，读音由输入法定；读错的可以在词后面跟上拼音
    /// （`曾小贤 zengxiaoxian`），还可以再跟词频，和文件里一行的写法一样。不对的话返回原因。
    pub fn add_custom(&self, input: &str) -> Result<(), String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("请输入词条".into());
        }
        if input.starts_with('#') || input.contains('\n') {
            return Err("词条不能以 # 开头".into());
        }
        let row = local::parse_line(input).ok_or("词条须包含汉字")?;
        if !row.pinyin.iter().all(|syl| syl.bytes().all(|b| b.is_ascii_alphabetic())) {
            return Err("拼音只能包含字母".into());
        }
        let weight = row.weight;
        let word = LocalWord::from(row);
        if self.custom_words().contains(&word) {
            return Err(format!("「{}」已在自定义词库中", word.text));
        }
        let path = self.custom()?;
        let mut content = fs::read_to_string(&path).map_err(|err| format!("无法读取自定义词库：{err}"))?;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        // Rime 的写法：词、拼音、词频用 Tab 隔开，后面没写的列省掉
        let _ = match (word.pinyin.as_str(), weight) {
            ("", None) => writeln!(content, "{}", word.text),
            (pinyin, None) => writeln!(content, "{}\t{pinyin}", word.text),
            (pinyin, Some(weight)) => writeln!(content, "{}\t{pinyin}\t{weight}", word.text),
        };
        disk::write_atomic(&path, &content, true).map_err(|err| format!("无法保存：{err}"))
    }

    /// 从自定义词库里删掉这个词。别的行，包括注释，都照旧。
    pub fn remove_custom(&self, word: &LocalWord) -> io::Result<()> {
        let path = self.dir.join(CUSTOM);
        let content = fs::read_to_string(&path)?;
        let same = |line: &str| local::parse_line(line).map(LocalWord::from).as_ref() == Some(word);
        let kept: String = content.lines().filter(|line| !same(line)).flat_map(|line| [line, "\n"]).collect();
        disk::write_atomic(&path, &kept, true)
    }

    /// 导入过的外部词库，按名字排序。词数按文件的修改时间记着，没改过的不再数。
    pub fn imported(&mut self) -> Vec<Imported> {
        let files = stamped(&self.imported_dir());
        self.counted.retain(|path, _| files.iter().any(|(file, _)| file == path));
        files
            .into_iter()
            .filter_map(|(path, modified)| {
                let name = path.file_name()?.to_str()?.strip_suffix(DICT_SUFFIX)?.to_string();
                let words = match self.counted.get(&path) {
                    Some(&(counted, words)) if counted == modified => words,
                    _ => {
                        let words = count(&path);
                        self.counted.insert(path, (modified, words));
                        words
                    }
                };
                Some(Imported { name, words })
            })
            .collect()
    }

    /// 导入一个外部词库：转成本地词库的写法存进来，名字是源文件名去掉扩展名，同名的覆盖。
    /// 外部词库的词频尺度不一样，一律按 `IMPORTED_WEIGHT` 算。
    pub fn import(&self, source: &Path) -> Result<(), String> {
        let file =
            source.file_name().map_or_else(|| source.display().to_string(), |name| name.to_string_lossy().into());
        let data = fs::read(source).map_err(|err| format!("无法读取 {file}：{err}"))?;
        let words = import::words(&data).map_err(|err| format!("{file}：{err}"))?;
        let name = [DICT_SUFFIX, ".scel", ".txt"].iter().find_map(|suffix| file.strip_suffix(suffix)).unwrap_or(&file);
        let mut text = format!("# 导入自 {file}\n");
        for word in &words {
            let _ = writeln!(text, "{}\t{}\t{IMPORTED_WEIGHT}", word.text, word.pinyin);
        }
        let path = self.imported_dir().join(format!("{name}{DICT_SUFFIX}"));
        disk::write_atomic(&path, &text, true).map_err(|err| format!("无法保存：{err}"))
    }

    /// 删掉一个导入的外部词库。
    pub fn remove(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.imported_dir().join(format!("{name}{DICT_SUFFIX}")))
    }
}

/// 读出这些词库文件的内容，读不了的跳过。
pub fn read_sources(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().filter_map(|path| fs::read_to_string(path).ok()).collect()
}

/// 目录里的词库文件和各自的修改时间。目录不在就是空的。
fn stamped(dir: &Path) -> Vec<Stamped> {
    let stamp = |path: PathBuf| {
        let modified = disk::modified(&path);
        (path, modified)
    };
    dict_files(dir).unwrap_or_default().into_iter().map(stamp).collect()
}

/// 一个词库文件里有几个词。
fn count(path: &Path) -> usize {
    fs::read_to_string(path).map_or(0, |text| local::rows(&text).count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_lists_and_removes_dictionaries() {
        let dir = std::env::temp_dir().join(format!("kyme-dicts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut dicts = LocalDicts::beside(&dir.join("settings.toml"));
        assert_eq!(dicts.reload(), None, "一个词库都没有，不用建");

        let custom = dicts.custom().unwrap();
        assert!(dicts.custom_words().is_empty(), "说明里的例子是注释");
        let source = dir.join("custom.txt");
        fs::write(&source, "'kun'peng 鲲鹏\n曾小贤 zeng xiao xian 3\n").unwrap();
        dicts.import(&source).unwrap();
        let imported = dicts.imported_dir().join("custom.dict.yaml");
        let text = fs::read_to_string(&imported).unwrap();
        assert!(text.ends_with("鲲鹏\tkun peng\t500\n曾小贤\tzeng xiao xian\t500\n"), "{text}");
        assert_eq!(dicts.imported(), [Imported { name: "custom".into(), words: 2 }], "和自定义词库同名也不冲突");
        assert_eq!(dicts.reload(), Some(vec![custom.clone(), imported]));

        dicts.add_custom(" 曾小贤 Zeng'Xiao'Xian ").unwrap();
        dicts.add_custom("鲲鹏").unwrap();
        assert!(dicts.add_custom("鲲鹏").is_err(), "已经有了");
        assert!(dicts.add_custom("鲲鹏 kun1 peng2").is_err(), "拼音里有数字");
        assert!(dicts.add_custom("hello").is_err(), "没有汉字");
        dicts.add_custom("王者荣耀 30000").unwrap();
        let text = fs::read_to_string(&custom).unwrap();
        assert!(text.ends_with("王者荣耀\t\t30000\n"), "写上的词频要存下来：{text}");
        dicts.remove_custom(&LocalWord { text: "王者荣耀".into(), pinyin: String::new() }).unwrap();
        let zeng = LocalWord { text: "曾小贤".into(), pinyin: "zeng xiao xian".into() };
        assert_eq!(dicts.custom_words()[0], zeng, "读错的可以跟上拼音");
        dicts.remove_custom(&zeng).unwrap();
        assert_eq!(dicts.custom_words(), [LocalWord { text: "鲲鹏".into(), pinyin: String::new() }]);
        assert!(fs::read_to_string(&custom).unwrap().starts_with("# Kyme 自定义词库"), "说明还在");

        dicts.remove("custom").unwrap();
        assert!(dicts.imported().is_empty());
        assert!(dicts.reload().is_some(), "删了也是改动");
        fs::remove_dir_all(&dir).unwrap();
    }
}
