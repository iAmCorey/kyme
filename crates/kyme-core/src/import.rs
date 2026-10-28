//! 读外部词库：搜狗细胞词库 .scel，和各种文本格式（Rime 的 .dict.yaml、搜狗和 QQ 导出的 .txt、
//! 一行一个词的词表）。文本每行怎么认见 `kyme_pinyin::local::parse_line`。外部词库的词频尺度各不相同，不要。

use std::borrow::Cow;
use std::collections::HashSet;

use kyme_pinyin::local;

use crate::dicts::LocalWord;

/// 认出格式，读出全部词，去掉重复的。没写拼音的词拼音是空的，输入法载入时按主词库补。
/// 格式认不出、一个词都没有时返回原因。
pub fn words(data: &[u8]) -> Result<Vec<LocalWord>, String> {
    let mut words = match scel(data) {
        Some(words) => words,
        None => {
            let text = decode(data).ok_or("不是搜狗细胞词库，也不是 UTF-8 或 UTF-16 的文本")?;
            local::rows(&text).map(LocalWord::from).collect()
        }
    };
    // 一样的词只留第一次出现的
    let keep: Vec<bool> = {
        let mut seen = HashSet::new();
        words.iter().map(|word| seen.insert((word.text.as_str(), word.pinyin.as_str()))).collect()
    };
    let mut keep = keep.into_iter();
    words.retain(|_| keep.next().unwrap_or(false));
    if words.is_empty() {
        return Err("里面没有认得出的词".into());
    }
    Ok(words)
}

/// 文本的编码：带 BOM 的 UTF-16（搜狗导出的就是），或者 UTF-8。
fn decode(data: &[u8]) -> Option<Cow<'_, str>> {
    match data {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes).map(Cow::Owned),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes).map(Cow::Owned),
        [0xEF, 0xBB, 0xBF, rest @ ..] => std::str::from_utf8(rest).ok().map(Cow::Borrowed),
        _ => std::str::from_utf8(data).ok().map(Cow::Borrowed),
    }
}

fn utf16(data: &[u8], unit: fn([u8; 2]) -> u16) -> Option<String> {
    let units: Vec<u16> = data.as_chunks::<2>().0.iter().map(|&pair| unit(pair)).collect();
    String::from_utf16(&units).ok()
}

/// 搜狗细胞词库开头的几个字节，第 4 个字节是版本（0x44、0x45），这里不比。
const SCEL_MAGIC: [u8; 12] = [0x40, 0x15, 0x00, 0x00, 0x00, 0x43, 0x53, 0x01, 0x01, 0x00, 0x00, 0x00];
/// 拼音表从这里开始：先是音节个数（4 个字节），再逐个是下标、字节数、UTF-16LE 的拼音。
const SCEL_SYLLABLES: usize = 0x1540;

/// 搜狗细胞词库。词表从 0x2628 开始（0x45 版从 0x26C4），一直到文件结尾，每组是一串同音词：
/// 同音词个数、拼音下标的字节数、各个音节在拼音表里的下标，再逐个是词的字节数、UTF-16LE 的词、
/// 扩展数据的字节数和扩展数据。不是 .scel 返回 None；后面坏了的话，留下坏之前读到的词。
fn scel(data: &[u8]) -> Option<Vec<LocalWord>> {
    let header = data.get(..SCEL_MAGIC.len())?;
    let magic = header.iter().zip(SCEL_MAGIC).enumerate().all(|(i, (&byte, magic))| i == 4 || byte == magic);
    let words_at = match header[4] {
        0x44 if magic => 0x2628,
        0x45 if magic => 0x26C4,
        _ => return None,
    };

    let mut reader = Reader { data, at: SCEL_SYLLABLES };
    let count = reader.u16()?;
    reader.bytes(2)?;
    let mut syllables = vec![String::new(); usize::from(count)];
    for _ in 0..count {
        let index = reader.u16()?;
        let len = reader.u16()?;
        let name = reader.utf16(len)?;
        if let Some(slot) = syllables.get_mut(usize::from(index)) {
            *slot = name;
        }
    }
    // 拼音表之外的下标是英文字母：表里的个数之后依次是 a 到 z
    let syllable = |index: u16| match syllables.get(usize::from(index)) {
        Some(name) => name.clone(),
        None => index
            .checked_sub(count)
            .and_then(|letter| char::from_u32(u32::from(letter) + 'a' as u32))
            .map_or_else(String::new, String::from),
    };

    let mut reader = Reader { data, at: words_at };
    let mut words = Vec::new();
    while reader.at < data.len()
        && let Some(group) = scel_group(&mut reader, &syllable)
    {
        words.extend(group);
    }
    Some(words)
}

/// 词表里的一组同音词。读过头（文件坏了）返回 None。
fn scel_group(reader: &mut Reader, syllable: &impl Fn(u16) -> String) -> Option<Vec<LocalWord>> {
    let same = reader.u16()?;
    let indices = reader.u16()? / 2;
    let pinyin = (0..indices).map(|_| reader.u16().map(syllable)).collect::<Option<Vec<_>>>()?.join(" ");
    (0..same)
        .map(|_| {
            let len = reader.u16()?;
            let text = reader.utf16(len)?;
            let extra = reader.u16()?;
            reader.bytes(extra)?;
            Some(LocalWord { text, pinyin: pinyin.clone() })
        })
        .collect()
}

/// 按小端顺序读二进制文件，读过头返回 None。
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, len: u16) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.at..self.at + usize::from(len))?;
        self.at += usize::from(len);
        Some(bytes)
    }

    fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn utf16(&mut self, len: u16) -> Option<String> {
        let text = utf16(self.bytes(len)?, u16::from_le_bytes)?;
        Some(text.trim_end_matches('\0').to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    /// 按格式拼一个小的细胞词库：拼音表，和若干组同音词（音节下标、词）。
    fn scel_file(syllables: &[&str], groups: &[(&[u16], &[&str])]) -> Vec<u8> {
        let mut data = vec![0u8; 0x2628];
        data[..12].copy_from_slice(&[0x40, 0x15, 0x00, 0x00, 0x44, 0x43, 0x53, 0x01, 0x01, 0x00, 0x00, 0x00]);
        let mut table = (syllables.len() as u32).to_le_bytes().to_vec();
        for (index, syllable) in syllables.iter().enumerate() {
            let name = utf16le(syllable);
            table.extend((index as u16).to_le_bytes());
            table.extend((name.len() as u16).to_le_bytes());
            table.extend(name);
        }
        data[SCEL_SYLLABLES..SCEL_SYLLABLES + table.len()].copy_from_slice(&table);
        for (indices, texts) in groups {
            data.extend((texts.len() as u16).to_le_bytes());
            data.extend((indices.len() as u16 * 2).to_le_bytes());
            data.extend(indices.iter().flat_map(|index| index.to_le_bytes()));
            for text in *texts {
                let text = utf16le(text);
                data.extend((text.len() as u16).to_le_bytes());
                data.extend(text);
                data.extend(10u16.to_le_bytes());
                data.extend([0u8; 10]);
            }
        }
        data
    }

    fn word(text: &str, pinyin: &str) -> LocalWord {
        LocalWord { text: text.into(), pinyin: pinyin.into() }
    }

    #[test]
    fn reads_sogou_cell_dictionaries() {
        let data = scel_file(&["hao", "kun", "ni", "peng"], &[(&[1, 3], &["鲲鹏", "坤朋"]), (&[2, 0], &["你好"])]);
        assert_eq!(words(&data).unwrap(), [word("鲲鹏", "kun peng"), word("坤朋", "kun peng"), word("你好", "ni hao")]);
        // 坏在半截：留下坏之前的
        assert_eq!(words(&data[..data.len() - 5]).unwrap(), [word("鲲鹏", "kun peng"), word("坤朋", "kun peng")]);
    }

    #[test]
    fn reads_text_exports() {
        let sogou: Vec<u8> = [0xFF, 0xFE].into_iter().chain(utf16le("'kun'peng 鲲鹏\r\n'ni'hao 你好\r\n")).collect();
        assert_eq!(words(&sogou).unwrap(), [word("鲲鹏", "kun peng"), word("你好", "ni hao")], "搜狗导出的 UTF-16");
        let rime = "---\nname: x\n...\n鲲鹏\tkun peng\t100\n鲲鹏\tkun peng\t5\n";
        assert_eq!(words(rime.as_bytes()).unwrap(), [word("鲲鹏", "kun peng")], "重复的只留一个");
        assert_eq!(words("鲲鹏\n你好\n".as_bytes()).unwrap(), [word("鲲鹏", ""), word("你好", "")], "只有词");
        assert!(words(&[0x80, 0x81, 0x82]).is_err(), "不认识的编码");
        assert!(words("hello\nworld\n".as_bytes()).is_err(), "没有汉字");
    }
}
