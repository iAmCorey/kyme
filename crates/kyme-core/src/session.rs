use std::sync::Arc;
use std::time::{Duration, Instant};

use kyme_pinyin::{Conversion, Engine};

use crate::punct::Punctuation;
use crate::settings::Settings;

/// 轻点 Shift 切换中英：按下到松开不能超过这么久，中间也不能按别的键。
const SHIFT_TAP: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Chinese,
    English,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Space,
    Enter,
    Backspace,
    Escape,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    /// Shift 单独按下 / 松开。
    ShiftDown,
    ShiftUp,
    Other,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

impl Modifiers {
    fn any(self) -> bool {
        self.control || self.option || self.command
    }
}

/// 候选窗要显示的内容（当前页）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Panel {
    pub preedit: String,
    pub candidates: Vec<String>,
    pub highlighted: usize,
    pub has_prev: bool,
    pub has_next: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Idle,
    Composing {
        /// 输入框里显示的部分（marked text）。默认只有已选定、还没上屏的汉字，可能为空；
        /// 打开 `inline_pinyin` 时再加上拼音。
        marked: String,
        panel: Panel,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// false 表示这个键交还给 App 自己处理。
    pub handled: bool,
    /// 要上屏的文字。
    pub commit: Option<String>,
    pub view: View,
    /// 刚切换到的中英状态，平台层据此显示提示。
    pub mode_changed: Option<Mode>,
}

/// 已经选定、但整串拼音还没打完的部分。
struct Piece {
    text: String,
    raw: String,
}

pub struct Session {
    engine: Arc<Engine>,
    settings: Settings,
    mode: Mode,
    input: String,
    pieces: Vec<Piece>,
    conversion: Conversion,
    highlight: usize,
    punct: Punctuation,
    after_digit: bool,
    shift_down_at: Option<Instant>,
}

impl Session {
    pub fn new(engine: Arc<Engine>) -> Self {
        Session {
            engine,
            settings: Settings::default(),
            mode: Mode::Chinese,
            input: String::new(),
            pieces: Vec::new(),
            conversion: Conversion::default(),
            highlight: 0,
            punct: Punctuation::default(),
            after_digit: false,
            shift_down_at: None,
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) {
        self.settings = settings.normalized();
    }

    pub fn is_composing(&self) -> bool {
        !self.input.is_empty() || !self.pieces.is_empty()
    }

    pub fn handle_key(&mut self, key: Key, mods: Modifiers) -> Response {
        // 任何别的键都会打断「轻点 Shift」
        let shift_down_at = self.shift_down_at.take();
        match key {
            Key::ShiftDown => {
                if !mods.any() {
                    self.shift_down_at = Some(Instant::now());
                }
                return self.pass();
            }
            Key::ShiftUp => {
                let tapped = shift_down_at.is_some_and(|at| at.elapsed() < SHIFT_TAP);
                return if tapped { self.toggle_mode() } else { self.pass() };
            }
            _ => {}
        }
        if self.mode == Mode::English || mods.any() {
            return self.pass();
        }

        let composing = self.is_composing();
        let after_digit = std::mem::replace(&mut self.after_digit, false);
        match key {
            Key::Char(c) if c.is_ascii_lowercase() || (composing && c.is_ascii_uppercase()) => {
                self.input.push(c.to_ascii_lowercase());
                self.refresh();
                self.handled(None)
            }
            Key::Char('\'') if composing => {
                if !self.input.is_empty() && !self.input.ends_with('\'') {
                    self.input.push('\'');
                    self.refresh();
                }
                self.handled(None)
            }
            Key::Char(c @ '1'..='9') if composing => {
                let offset = c as usize - '1' as usize;
                if offset < self.page_size() {
                    self.select(self.page() * self.page_size() + offset)
                } else {
                    self.handled(None)
                }
            }
            Key::Char('-') | Key::Up | Key::PageUp if composing => {
                self.turn_page(false);
                self.handled(None)
            }
            Key::Char('=') | Key::Down | Key::PageDown if composing => {
                self.turn_page(true);
                self.handled(None)
            }
            // 输入中按别的字符：首选上屏，再接上它。有中文标点的换成中文标点，
            // 没有的（/ @ + 0 等）原样接上，不能吞掉
            Key::Char(c) if composing => {
                let mut text = self.take_best();
                match self.punct.map(c) {
                    Some(p) => text.push_str(p),
                    None => text.push(c),
                }
                self.handled(Some(text))
            }
            Key::Char(c) if c.is_ascii_digit() => {
                self.after_digit = true;
                self.pass()
            }
            // 数字后面的 . , : 保持半角，方便打小数和时间
            Key::Char('.' | ',' | ':') if after_digit => self.pass(),
            Key::Char(c) => match self.punct.map(c) {
                Some(p) => self.handled(Some(p.to_string())),
                None => self.pass(),
            },
            Key::Space if composing => self.select(self.highlight),
            Key::Enter if composing => {
                let text = self.take_raw();
                self.handled(Some(text))
            }
            Key::Escape if composing => {
                self.clear();
                self.handled(None)
            }
            Key::Backspace if composing => {
                self.backspace();
                self.handled(None)
            }
            Key::Left if composing => {
                self.highlight = self.highlight.saturating_sub(1);
                self.handled(None)
            }
            Key::Right if composing => {
                if self.highlight + 1 < self.conversion.candidates.len() {
                    self.highlight += 1;
                }
                self.handled(None)
            }
            _ if composing => self.handled(None),
            _ => self.pass(),
        }
    }

    /// 系统要求结束输入（失焦、切换输入法等）时，把当前内容原样交出去。
    pub fn commit_all(&mut self) -> Option<String> {
        self.is_composing().then(|| self.take_raw())
    }

    pub fn view(&self) -> View {
        if !self.is_composing() {
            return View::Idle;
        }
        let selected = self.selected();
        let pinyin = &self.conversion.preedit;
        // 拼音只显示在一处：要么输入框里，要么候选窗第一行
        let (marked, preedit) = if self.settings.input.inline_pinyin {
            (format!("{selected}{pinyin}"), String::new())
        } else {
            let preedit = format!("{selected}{pinyin}");
            (selected, preedit)
        };
        let page = self.page();
        let page_size = self.page_size();
        let start = page * page_size;
        let total = self.conversion.candidates.len();
        let candidates =
            self.conversion.candidates[start.min(total)..].iter().take(page_size).map(|c| c.text.clone()).collect();
        View::Composing {
            marked,
            panel: Panel {
                preedit,
                candidates,
                highlighted: self.highlight - start,
                has_prev: page > 0,
                has_next: start + page_size < total,
            },
        }
    }

    /// 中英切换。输入到一半时把打出的拼音原样上屏。
    fn toggle_mode(&mut self) -> Response {
        let commit = self.is_composing().then(|| self.take_raw());
        self.mode = match self.mode {
            Mode::Chinese => Mode::English,
            Mode::English => Mode::Chinese,
        };
        Response { handled: false, commit, view: View::Idle, mode_changed: Some(self.mode) }
    }

    fn page_size(&self) -> usize {
        self.settings.input.page_size
    }

    fn page(&self) -> usize {
        self.highlight / self.page_size()
    }

    /// 已经选定、还没上屏的汉字。
    fn selected(&self) -> String {
        self.pieces.iter().map(|p| p.text.as_str()).collect()
    }

    fn select(&mut self, index: usize) -> Response {
        let Some(candidate) = self.conversion.candidates.get(index).cloned() else {
            return self.handled(None);
        };
        let consumed = candidate.consumed.min(self.input.len());
        self.pieces.push(Piece { text: candidate.text, raw: self.input[..consumed].to_string() });
        self.input = self.input[consumed..].trim_start_matches('\'').to_string();
        if self.input.is_empty() {
            let text = self.take_raw();
            self.handled(Some(text))
        } else {
            self.refresh();
            self.handled(None)
        }
    }

    /// 已选部分 + 当前首选；首选没覆盖全部拼音时，剩下的原样保留。
    fn take_best(&mut self) -> String {
        match self.conversion.candidates.first() {
            Some(first) if first.consumed >= self.input.len() => {
                let text = self.selected() + &first.text;
                self.clear();
                text
            }
            _ => self.take_raw(),
        }
    }

    /// 已选部分 + 原始拼音（去掉分隔符）。
    fn take_raw(&mut self) -> String {
        let text = self.selected() + &self.input.replace('\'', "");
        self.clear();
        text
    }

    fn backspace(&mut self) {
        self.input.pop();
        // 拼音删光了就退回上一次的选择，方便改选
        if self.input.is_empty()
            && let Some(piece) = self.pieces.pop()
        {
            self.input = piece.raw;
        }
        if self.input.is_empty() {
            self.clear();
        } else {
            self.refresh();
        }
    }

    fn turn_page(&mut self, forward: bool) {
        let page = self.page();
        if forward {
            let next = (page + 1) * self.page_size();
            if next < self.conversion.candidates.len() {
                self.highlight = next;
            }
        } else if page > 0 {
            self.highlight = (page - 1) * self.page_size();
        }
    }

    fn refresh(&mut self) {
        self.conversion = self.engine.convert(&self.input);
        self.highlight = 0;
    }

    fn clear(&mut self) {
        self.input.clear();
        self.pieces.clear();
        self.conversion = Conversion::default();
        self.highlight = 0;
    }

    fn handled(&self, commit: Option<String>) -> Response {
        Response { handled: true, commit, view: self.view(), mode_changed: None }
    }

    fn pass(&self) -> Response {
        Response { handled: false, commit: None, view: self.view(), mode_changed: None }
    }
}

#[cfg(test)]
mod tests {
    use kyme_pinyin::Dictionary;

    use super::*;

    const SAMPLE: &str = "\
你\tni\t1000
拟\tni\t100
泥\tni\t50
好\thao\t1000
你好\tni hao\t500
是\tshi\t2000
世界\tshi jie\t300
界\tjie\t100
";

    fn session() -> Session {
        Session::new(Arc::new(Engine::new(Dictionary::from_rime_sources(&[SAMPLE]))))
    }

    fn type_str(s: &mut Session, text: &str) {
        for c in text.chars() {
            s.handle_key(Key::Char(c), Modifiers::default());
        }
    }

    fn press(s: &mut Session, key: Key) -> Response {
        s.handle_key(key, Modifiers::default())
    }

    fn tap_shift(s: &mut Session) -> Response {
        press(s, Key::ShiftDown);
        press(s, Key::ShiftUp)
    }

    #[test]
    fn space_commits_sentence() {
        let mut s = session();
        type_str(&mut s, "nihaoshijie");
        let r = press(&mut s, Key::Space);
        assert_eq!(r.commit.as_deref(), Some("你好世界"));
        assert_eq!(r.view, View::Idle);
    }

    #[test]
    fn partial_selection_keeps_composing() {
        let mut s = session();
        type_str(&mut s, "nihaoshijie");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        let index = panel.candidates.iter().position(|c| c == "你好").unwrap();
        let r = s.handle_key(Key::Char(char::from(b'1' + index as u8)), Modifiers::default());
        assert_eq!(r.commit, None);
        let View::Composing { marked, panel } = r.view else { panic!() };
        assert_eq!(marked, "你好shi'jie", "输入框里是选定的字加上剩下的拼音");
        assert_eq!(panel.preedit, "", "拼音已经在输入框里，候选窗只有一行候选");
        let r = press(&mut s, Key::Space);
        assert_eq!(r.commit.as_deref(), Some("你好世界"));
    }

    #[test]
    fn backspace_undoes_selection() {
        let mut s = session();
        type_str(&mut s, "nihaoshi");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        let index = panel.candidates.iter().position(|c| c == "你好").unwrap();
        s.handle_key(Key::Char(char::from(b'1' + index as u8)), Modifiers::default());
        for _ in 0..3 {
            press(&mut s, Key::Backspace);
        }
        let View::Composing { marked, panel } = s.view() else { panic!() };
        assert_eq!(marked, "ni'hao");
        assert_eq!(panel.preedit, "");
    }

    #[test]
    fn pinyin_is_shown_inline_by_default() {
        let mut s = session();
        type_str(&mut s, "nihaoshijie");
        let View::Composing { marked, panel } = s.view() else { panic!() };
        assert_eq!(marked, "ni'hao'shi'jie");
        assert_eq!(panel.preedit, "", "拼音已经在输入框里，候选窗只有一行候选");
    }

    #[test]
    fn pinyin_in_panel_option() {
        let mut s = session();
        let mut settings = Settings::default();
        settings.input.inline_pinyin = false;
        s.set_settings(settings);
        type_str(&mut s, "nihao");
        let View::Composing { marked, panel } = s.view() else { panic!() };
        assert_eq!(marked, "", "输入框里只有选定的字，还没选");
        assert_eq!(panel.preedit, "ni'hao");
    }

    #[test]
    fn page_size_option() {
        let mut s = session();
        let mut settings = Settings::default();
        settings.input.page_size = 2;
        s.set_settings(settings);
        type_str(&mut s, "ni");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert_eq!(panel.candidates.len(), 2);
        assert!(panel.has_next);
        let r = press(&mut s, Key::Char('3'));
        assert_eq!(r.commit, None, "超出本页的数字键不选词");
        press(&mut s, Key::Char('='));
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert!(panel.has_prev);
    }

    #[test]
    fn enter_commits_letters() {
        let mut s = session();
        type_str(&mut s, "nihao");
        assert_eq!(press(&mut s, Key::Enter).commit.as_deref(), Some("nihao"));
    }

    #[test]
    fn escape_cancels() {
        let mut s = session();
        type_str(&mut s, "nihao");
        let r = press(&mut s, Key::Escape);
        assert!(r.handled);
        assert_eq!(r.commit, None);
        assert_eq!(r.view, View::Idle);
    }

    #[test]
    fn punctuation() {
        let mut s = session();
        assert_eq!(press(&mut s, Key::Char(',')).commit.as_deref(), Some("，"));
        type_str(&mut s, "nihao");
        assert_eq!(press(&mut s, Key::Char('.')).commit.as_deref(), Some("你好。"));
        assert!(!press(&mut s, Key::Char('3')).handled);
        assert!(!press(&mut s, Key::Char('.')).handled, "数字后的点保持半角");
    }

    #[test]
    fn other_characters_end_composition_instead_of_vanishing() {
        let mut s = session();
        type_str(&mut s, "nihao");
        let r = press(&mut s, Key::Char('/'));
        assert_eq!(r.commit.as_deref(), Some("你好/"));
        assert_eq!(r.view, View::Idle);
        type_str(&mut s, "shijie");
        assert_eq!(press(&mut s, Key::Char('@')).commit.as_deref(), Some("世界@"));
    }

    #[test]
    fn shortcuts_and_english_mode_pass_through() {
        let mut s = session();
        let cmd = Modifiers { command: true, ..Modifiers::default() };
        assert!(!s.handle_key(Key::Char('c'), cmd).handled);
        assert_eq!(tap_shift(&mut s).mode_changed, Some(Mode::English));
        assert!(!press(&mut s, Key::Char('a')).handled);
        assert_eq!(tap_shift(&mut s).mode_changed, Some(Mode::Chinese));
    }

    #[test]
    fn shift_tap_commits_raw_input() {
        let mut s = session();
        type_str(&mut s, "nihao");
        assert_eq!(tap_shift(&mut s).commit.as_deref(), Some("nihao"));
    }

    #[test]
    fn shift_with_another_key_does_not_toggle() {
        let mut s = session();
        press(&mut s, Key::ShiftDown);
        press(&mut s, Key::Char('A'));
        assert_eq!(press(&mut s, Key::ShiftUp).mode_changed, None);
        assert_eq!(s.mode(), Mode::Chinese);
    }
}
