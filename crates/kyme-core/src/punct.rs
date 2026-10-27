//! 中文模式下的标点。引号成对交替出现。

#[derive(Default)]
pub(crate) struct Punctuation {
    double_open: bool,
    single_open: bool,
}

impl Punctuation {
    pub fn map(&mut self, c: char) -> Option<&'static str> {
        Some(match c {
            ',' => "，",
            '.' => "。",
            '?' => "？",
            '!' => "！",
            ':' => "：",
            ';' => "；",
            '\\' => "、",
            '(' => "（",
            ')' => "）",
            '[' => "【",
            ']' => "】",
            '{' => "「",
            '}' => "」",
            '<' => "《",
            '>' => "》",
            '^' => "……",
            '_' => "——",
            '$' => "￥",
            '~' => "～",
            '`' => "·",
            '"' => {
                self.double_open = !self.double_open;
                if self.double_open { "“" } else { "”" }
            }
            '\'' => {
                self.single_open = !self.single_open;
                if self.single_open { "‘" } else { "’" }
            }
            _ => return None,
        })
    }
}
