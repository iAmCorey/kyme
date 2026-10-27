//! 按键场景：在模拟的输入框里按一串键，看最后输入框里是什么字。
//!
//! qa/scenarios.tsv 每行一个场景，写法见文件开头。场景按顺序在同一个会话里跑，和真实输入法一样
//! 会延续状态（比如引号的左右）；每个场景开始前清空输入框，结束后按一次 Esc。
//! macOS 接入层（macos.rs）在 `Kyme --qa` 里跑同一批场景，用这里的会话逐键对照。

use std::fs;
use std::path::Path;
use std::time::Duration;

use kyme_core::{Key, Mode, Modifiers, Response, Session};

use crate::Context;
use crate::report::Outcome;

pub struct Scenario {
    pub name: String,
    pub strokes: Vec<Stroke>,
    pub expected: String,
}

pub enum Stroke {
    Press(Press),
    Wait(Duration),
}

/// 一个按键事件：会话看到的键，以及在 macOS 上怎么合成它。
#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    pub key: Key,
    pub mods: Modifiers,
    /// 写进报告的名字，比如 `n`、`{space}`。
    pub label: String,
    /// 修饰键本身按下或松开：macOS 上是 FlagsChanged 事件。
    pub flags_changed: bool,
    /// macOS 的虚拟键码（美式键盘）。
    pub code: u16,
    pub chars: String,
    pub shift: bool,
    pub command: bool,
}

/// 美式键盘上每个键码打出的字符，下标就是键码；`\0` 是不打字符的键。
const KEYBOARD: &str = "asdfhgzxcv\0bqweryt123465=97-80]ou[ip\0lj'k;\\,/nm.\0\0`";
/// 按住 Shift 打出的字符，和 UNSHIFTED 里同一位置的字符在同一个键上。
const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";
const UNSHIFTED: &str = "`1234567890-=[]\\;',./";
const SHIFT_CODE: u16 = 56;
const COMMAND_CODE: u16 = 55;

impl Press {
    fn key(key: Key, label: &str, code: u16, chars: &str) -> Press {
        Press {
            key,
            mods: Modifiers::default(),
            label: label.into(),
            flags_changed: false,
            code,
            chars: chars.into(),
            shift: false,
            command: false,
        }
    }

    /// 修饰键按下时，事件本身就带着它。
    fn modifier(key: Key, label: &str, code: u16, shift: bool, command: bool) -> Press {
        Press {
            key,
            mods: Modifiers { command, ..Modifiers::default() },
            label: label.into(),
            flags_changed: true,
            code,
            chars: String::new(),
            shift,
            command,
        }
    }
}

pub fn escape() -> Press {
    Press::key(Key::Escape, "{esc}", 53, "\u{1b}")
}

fn key_code(c: char) -> Option<u16> {
    KEYBOARD.chars().position(|k| k == c && k != '\0').map(|code| code as u16)
}

/// 打一个字符要按的键。要按 Shift 的字符前后带上 Shift 的按下和松开，和真的打字一样。
pub fn char_presses(c: char) -> Result<Vec<Press>, String> {
    let (base, shift) = match SHIFTED.chars().position(|s| s == c) {
        Some(index) => (UNSHIFTED.chars().nth(index).unwrap_or(c), true),
        None if c.is_ascii_uppercase() => (c.to_ascii_lowercase(), true),
        None => (c, false),
    };
    let code = key_code(base).ok_or_else(|| format!("不支持的字符「{c}」"))?;
    let mut press = Press::key(Key::Char(c), &c.to_string(), code, &c.to_string());
    if !shift {
        return Ok(vec![press]);
    }
    press.shift = true;
    Ok(vec![
        Press::modifier(Key::ShiftDown, "{shift-down}", SHIFT_CODE, true, false),
        press,
        Press::modifier(Key::ShiftUp, "{shift-up}", SHIFT_CODE, false, false),
    ])
}

/// 花括号里的特殊键。
fn special(name: &str) -> Result<Vec<Stroke>, String> {
    let label = format!("{{{name}}}");
    let press = |key, code, chars: &str| Ok(vec![Stroke::Press(Press::key(key, &label, code, chars))]);
    let shift_down = || Stroke::Press(Press::modifier(Key::ShiftDown, &label, SHIFT_CODE, true, false));
    let shift_up = || Stroke::Press(Press::modifier(Key::ShiftUp, &label, SHIFT_CODE, false, false));
    match name {
        "space" => press(Key::Space, 49, " "),
        "enter" => press(Key::Enter, 36, "\r"),
        "bs" => press(Key::Backspace, 51, "\u{7f}"),
        "esc" => Ok(vec![Stroke::Press(escape())]),
        // 方向键和翻页键的 characters 是 AppKit 功能键的私用区字符
        "left" => press(Key::Left, 123, "\u{f702}"),
        "right" => press(Key::Right, 124, "\u{f703}"),
        "down" => press(Key::Down, 125, "\u{f701}"),
        "up" => press(Key::Up, 126, "\u{f700}"),
        "pgup" => press(Key::PageUp, 116, "\u{f72c}"),
        "pgdn" => press(Key::PageDown, 121, "\u{f72d}"),
        "shift" => Ok(vec![shift_down(), shift_up()]),
        "shift-down" => Ok(vec![shift_down()]),
        "shift-up" => Ok(vec![shift_up()]),
        _ => {
            if let Some(ms) = name.strip_prefix("wait:") {
                let ms = ms.parse().map_err(|_| format!("{label} 里要写毫秒数"))?;
                return Ok(vec![Stroke::Wait(Duration::from_millis(ms))]);
            }
            let Some(c) =
                name.strip_prefix("cmd+").filter(|key| key.chars().count() == 1).and_then(|k| k.chars().next())
            else {
                return Err(format!("不认识的键 {label}"));
            };
            let code = key_code(c).ok_or_else(|| format!("{label}：不支持的字符「{c}」"))?;
            let mut press = Press::key(Key::Char(c), &label, code, &c.to_string());
            press.mods.command = true;
            press.command = true;
            Ok(vec![
                Stroke::Press(Press::modifier(Key::Other, &label, COMMAND_CODE, false, true)),
                Stroke::Press(press),
                Stroke::Press(Press::modifier(Key::Other, &label, COMMAND_CODE, false, false)),
            ])
        }
    }
}

/// 解析一串按键：普通字符直接写，特殊键写在花括号里，`{{` 是字面的 `{`。
pub fn parse_keys(text: &str) -> Result<Vec<Stroke>, String> {
    let mut strokes = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("{{") {
            strokes.extend(char_presses('{')?.into_iter().map(Stroke::Press));
            rest = after;
        } else if c == '{' {
            let end = rest.find('}').ok_or("花括号没有配对")?;
            strokes.extend(special(&rest[1..end])?);
            rest = &rest[end + 1..];
        } else {
            match c {
                ' ' => strokes.extend(special("space")?),
                _ => strokes.extend(char_presses(c)?.into_iter().map(Stroke::Press)),
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    Ok(strokes)
}

pub fn load(root: &Path) -> Result<Vec<Scenario>, String> {
    let path = root.join("qa/scenarios.tsv");
    let text = fs::read_to_string(&path).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|(index, line)| {
            let mut cols = line.split('\t');
            let (Some(name), Some(keys)) = (cols.next(), cols.next()) else {
                return Err(format!("{} 第 {} 行至少要有名称和按键两列", path.display(), index + 1));
            };
            let strokes =
                parse_keys(keys).map_err(|err| format!("{} 第 {} 行（{name}）：{err}", path.display(), index + 1))?;
            // 期望为空时可以不写第三列
            Ok(Scenario { name: name.into(), strokes, expected: cols.next().unwrap_or("").into() })
        })
        .collect()
}

/// 模拟的输入框：上屏的文字接在末尾，输入法没接的键按普通文本框的方式处理。
#[derive(Default)]
pub struct Document {
    pub text: String,
}

impl Document {
    pub fn apply(&mut self, press: &Press, response: &Response) -> Result<(), String> {
        if let Some(text) = &response.commit {
            self.text.push_str(text);
        }
        // 输入法吃掉的键、修饰键本身、快捷键，都不会让文本框自己改文字
        if response.handled || press.flags_changed || press.mods != Modifiers::default() {
            return Ok(());
        }
        match press.key {
            Key::Char(c) => self.text.push(c),
            Key::Space => self.text.push(' '),
            Key::Enter => self.text.push('\n'),
            Key::Backspace => {
                self.text.pop();
            }
            Key::Left | Key::Right | Key::Up | Key::Down => {
                return Err(format!("{} 只能在输入中用：模拟的输入框不移动光标", press.label));
            }
            Key::Escape | Key::PageUp | Key::PageDown | Key::ShiftDown | Key::ShiftUp | Key::Other => {}
        }
        Ok(())
    }
}

/// 在会话里按完一个场景的键，返回输入框里的文字。
fn play(session: &mut Session, scenario: &Scenario) -> Result<String, String> {
    let mut document = Document::default();
    for stroke in &scenario.strokes {
        match stroke {
            Stroke::Wait(duration) => std::thread::sleep(*duration),
            Stroke::Press(press) => {
                let response = session.handle_key(press.key, press.mods);
                document.apply(press, &response)?;
            }
        }
    }
    finish(session)?;
    Ok(document.text)
}

/// 场景之间按一次 Esc：清掉没打完的输入，「数字后面的标点」这类状态也归零。
pub fn finish(session: &mut Session) -> Result<(), String> {
    let esc = escape();
    session.handle_key(esc.key, esc.mods);
    match session.mode() {
        Mode::Chinese => Ok(()),
        Mode::English => Err("场景结束时还在英文状态，最后要切回中文".into()),
    }
}

pub fn run(ctx: &mut Context) -> Outcome {
    let loaded = match ctx.engine() {
        Ok(loaded) => loaded,
        Err(err) => return Outcome::fail(err),
    };
    let scenarios = match load(&ctx.root) {
        Ok(scenarios) => scenarios,
        Err(err) => return Outcome::fail(err),
    };
    // 用默认设置跑：期望不能依赖设置
    let mut session = Session::new(loaded.engine);
    let mut failures = Vec::new();
    for scenario in &scenarios {
        match play(&mut session, scenario) {
            Ok(text) if text == scenario.expected => {}
            Ok(text) => failures.push(format!("{}：得到「{text}」，期望「{}」", scenario.name, scenario.expected)),
            Err(err) => failures.push(format!("{}：{err}", scenario.name)),
        }
    }
    let presses: usize =
        scenarios.iter().flat_map(|s| &s.strokes).filter(|stroke| matches!(stroke, Stroke::Press(_))).count();
    if failures.is_empty() {
        Outcome::pass(format!("{} 个场景（{presses} 次按键）都对", scenarios.len()))
    } else {
        Outcome::fail(format!("{}/{} 个场景不对", failures.len(), scenarios.len())).with_details(failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presses(text: &str) -> Vec<Press> {
        parse_keys(text)
            .unwrap()
            .into_iter()
            .filter_map(|stroke| match stroke {
                Stroke::Press(press) => Some(press),
                Stroke::Wait(_) => None,
            })
            .collect()
    }

    #[test]
    fn letters_use_us_key_codes() {
        let keys = presses("ni");
        assert_eq!(
            keys.iter().map(|p| (p.key, p.code)).collect::<Vec<_>>(),
            [(Key::Char('n'), 45), (Key::Char('i'), 34)]
        );
    }

    #[test]
    fn shifted_characters_are_wrapped_in_shift() {
        let keys = presses("A\"");
        let kinds: Vec<Key> = keys.iter().map(|p| p.key).collect();
        assert_eq!(kinds, [Key::ShiftDown, Key::Char('A'), Key::ShiftUp, Key::ShiftDown, Key::Char('"'), Key::ShiftUp]);
        assert_eq!((keys[1].code, keys[1].shift), (0, true));
        assert_eq!(keys[4].code, 39, "双引号在单引号那个键上");
    }

    #[test]
    fn special_keys() {
        let keys = presses("{space}{bs}{cmd+c}{{");
        assert_eq!(keys[0].key, Key::Space);
        assert_eq!(keys[1].key, Key::Backspace);
        assert!(keys[2].flags_changed && keys[2].command, "先按下 Command");
        assert!(keys[3].mods.command && keys[3].key == Key::Char('c'));
        assert!(keys[4].flags_changed && !keys[4].command, "再松开 Command");
        assert_eq!(keys[6].key, Key::Char('{'));
        assert!(matches!(parse_keys("{wait:500}").unwrap()[..], [Stroke::Wait(d)] if d == Duration::from_millis(500)));
    }

    #[test]
    fn bad_keys_are_reported() {
        assert!(parse_keys("{nope}").is_err());
        assert!(parse_keys("{space").is_err());
        assert!(parse_keys("你").is_err());
    }
}
