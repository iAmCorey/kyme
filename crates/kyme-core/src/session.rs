use std::sync::Arc;
use std::time::{Duration, Instant};

use kyme_pinyin::{Candidate, Conversion, Engine, SylId, UserDict, UserWord};
use serde::{Deserialize, Serialize};

use crate::punct::Punctuation;
use crate::settings::Settings;
use crate::userdict::today;

/// 轻点 Shift 切换中英：按下到松开不能超过这么久，中间也不能按别的键。
const SHIFT_TAP: Duration = Duration::from_millis(400);

/// 选候选的键，按顺序对应本页第 1、2…… 个候选。每页最多也就这么多个候选。
pub const SELECT_KEYS: &str = "1234567890";

/// 鼠标点了候选窗的哪里。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pick {
    /// 本页第几个候选，从 0 数。
    Candidate(usize),
    PrevPage,
    NextPage,
}

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
    /// 这一页里 emoji 是第几个候选（从 0 数），没有就是 None。候选窗在它前面画一条分隔线。
    pub emoji: Option<usize>,
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
    /// 这一下学到了新词，平台层该安排存盘。
    pub learned: bool,
}

/// 已经选定、但整串拼音还没打完的部分。
struct Piece {
    text: String,
    raw: String,
    /// 读音，学词用。emoji 和原样的字母没有读音。
    syllables: Vec<SylId>,
    /// 用户主动选的，不是直接要了首选。
    chosen: bool,
}

pub struct Session {
    engine: Arc<Engine>,
    settings: Settings,
    mode: Mode,
    input: String,
    pieces: Vec<Piece>,
    conversion: Conversion,
    /// `conversion.candidates` 里 emoji 那一个的下标。
    emoji_at: Option<usize>,
    /// 用户词库。平台层有用户词库文件时才挂上（输入法里有，按键场景和自测都没有），没挂就不学。
    user: Option<UserDict>,
    /// 这一下学到了新词，放进下一个 Response。
    learned: bool,
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
            emoji_at: None,
            user: None,
            learned: false,
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

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// 换一个引擎，比如本地词库改了、在后台重建好了。学到的词按新的词库重算代价。
    /// 正在打的这段照旧，下一个键起用新的。
    pub fn set_engine(&mut self, engine: Arc<Engine>) {
        self.engine = engine;
        if let Some(user) = self.user.take() {
            self.set_user_words(user.into_words());
        }
    }

    /// 挂上用户词库，比如刚从文件读出来的词。挂上以后才学词。
    pub fn set_user_words(&mut self, words: Vec<UserWord>) {
        self.user = Some(UserDict::new(&self.engine, words));
    }

    /// 学到的词，存盘用。
    pub fn user_words(&self) -> &[UserWord] {
        self.user.as_ref().map_or(&[], UserDict::words)
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
        if composing
            && let Key::Char(c) = key
            && let Some(slot) = SELECT_KEYS.find(c)
            && slot < self.page_size()
        {
            return self.pick(Pick::Candidate(slot));
        }
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
            // 本页没有这么多候选：1–9 什么都不做，0 往下按普通字符处理（首选上屏，再接上 0）
            Key::Char('1'..='9') if composing => self.handled(None),
            Key::Char('-') | Key::Up | Key::PageUp if composing => self.pick(Pick::PrevPage),
            Key::Char('=') | Key::Down | Key::PageDown if composing => self.pick(Pick::NextPage),
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

    /// 选本页的某个候选，或者翻页。鼠标点候选窗时直接调它，选字键和翻页键也走这里，两边的效果一样。
    /// 不在输入中时什么都不做。
    pub fn pick(&mut self, pick: Pick) -> Response {
        if !self.is_composing() {
            return self.pass();
        }
        match pick {
            Pick::Candidate(slot) if slot < self.page_size() => self.select(self.page() * self.page_size() + slot),
            Pick::Candidate(_) => self.handled(None),
            Pick::PrevPage | Pick::NextPage => {
                self.turn_page(pick == Pick::NextPage);
                self.handled(None)
            }
        }
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
                emoji: self.emoji_at.filter(|at| (start..start + page_size).contains(at)).map(|at| at - start),
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
        Response { handled: false, commit, view: View::Idle, mode_changed: Some(self.mode), learned: false }
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
        let raw = self.input[..consumed].to_string();
        self.pieces.push(Piece { text: candidate.text, raw, syllables: candidate.syllables, chosen: index != 0 });
        self.input = self.input[consumed..].trim_start_matches('\'').to_string();
        if self.input.is_empty() {
            self.learn();
            let text = self.take_raw();
            self.handled(Some(text))
        } else {
            self.refresh();
            self.handled(None)
        }
    }

    /// 已选部分 + 当前首选；首选没覆盖全部拼音时，剩下的原样保留。
    fn take_best(&mut self) -> String {
        match self.conversion.candidates.first().cloned() {
            Some(first) if first.consumed >= self.input.len() => {
                let raw = std::mem::take(&mut self.input);
                self.pieces.push(Piece { text: first.text, raw, syllables: first.syllables, chosen: false });
                self.learn();
                let text = self.selected();
                self.clear();
                text
            }
            _ => self.take_raw(),
        }
    }

    /// 上屏前记下这次选了什么：主动选的词加分；分几段选出来、其中有主动选的，整个记成一个新词
    /// （先选「你」再选「好」）。emoji 和原样的字母没有读音，不记；回车、Shift 上屏的也不走这里。
    fn learn(&mut self) {
        if !self.settings.input.learn {
            return;
        }
        let Some(user) = &mut self.user else { return };
        let day = today();
        let engine = &self.engine;
        for piece in self.pieces.iter().filter(|piece| piece.chosen) {
            self.learned |= user.learn(engine, &piece.text, &piece.syllables, day);
        }
        let phrase = self.pieces.len() >= 2
            && self.pieces.iter().any(|piece| piece.chosen)
            && self.pieces.iter().all(|piece| !piece.syllables.is_empty());
        if phrase {
            let text: String = self.pieces.iter().map(|piece| piece.text.as_str()).collect();
            let syllables: Vec<SylId> = self.pieces.iter().flat_map(|piece| piece.syllables.iter().copied()).collect();
            self.learned |= user.learn(engine, &text, &syllables, day);
        }
    }

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
        self.conversion = match &self.user {
            Some(user) if self.settings.input.learn => self.engine.convert_with(&self.input, user),
            _ => self.engine.convert(&self.input),
        };
        self.emoji_at = if self.settings.input.emoji { self.add_emoji() } else { None };
        self.highlight = 0;
    }

    /// 第一页最后一格放一个 emoji，返回它在候选列表里的下标。取第一页里第一个和整串拼音对得上、又有 emoji 的词，
    /// 用它的第一个 emoji，选中时一样消耗整串拼音。只放这一个、只放在这一格，前面几格永远是字，数字键选字的位置不受影响。
    fn add_emoji(&mut self) -> Option<usize> {
        let slot = self.page_size() - 1;
        let whole = self.input.len();
        let emoji = self.engine.emoji();
        let candidates = &self.conversion.candidates;
        let text =
            candidates.iter().take(slot).filter(|c| c.consumed == whole).find_map(|c| emoji.get(&c.text).first())?;
        let candidate = Candidate { text: text.clone(), consumed: whole, syllables: Vec::new() };
        let at = slot.min(candidates.len());
        self.conversion.candidates.insert(at, candidate);
        Some(at)
    }

    fn clear(&mut self) {
        self.input.clear();
        self.pieces.clear();
        self.conversion = Conversion::default();
        self.emoji_at = None;
        self.highlight = 0;
    }

    fn handled(&mut self, commit: Option<String>) -> Response {
        let learned = std::mem::take(&mut self.learned);
        Response { handled: true, commit, view: self.view(), mode_changed: None, learned }
    }

    fn pass(&self) -> Response {
        Response { handled: false, commit: None, view: self.view(), mode_changed: None, learned: false }
    }
}

#[cfg(test)]
mod tests {
    use kyme_pinyin::{Dictionary, Emoji};

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

    fn candidates(s: &Session) -> Vec<String> {
        match s.view() {
            View::Composing { panel, .. } => panel.candidates,
            View::Idle => Vec::new(),
        }
    }

    /// 按数字键选第一页里的 `text`。
    fn pick_text(s: &mut Session, text: &str) -> Response {
        let at = candidates(s).iter().position(|c| c == text).expect("第一页里有这个候选");
        press(s, Key::Char(char::from(SELECT_KEYS.as_bytes()[at])))
    }

    /// 挂上一个空的用户词库，开始学词。
    fn learning_session() -> Session {
        let mut s = session();
        s.set_user_words(Vec::new());
        s
    }

    #[test]
    fn often_picked_words_move_up() {
        let mut s = learning_session();
        type_str(&mut s, "ni");
        assert_eq!(candidates(&s)[..3], ["你", "拟", "泥"]);
        press(&mut s, Key::Escape);
        // 「泥」的词频只有「你」的 1/20，主动选 3 次后排到首位
        for _ in 0..3 {
            type_str(&mut s, "ni");
            let r = pick_text(&mut s, "泥");
            assert_eq!(r.commit.as_deref(), Some("泥"));
            assert!(r.learned, "学到了就告诉平台层，好安排存盘");
        }
        type_str(&mut s, "ni");
        assert_eq!(candidates(&s)[0], "泥");
    }

    #[test]
    fn pieces_picked_one_by_one_become_a_word() {
        let mut s = learning_session();
        type_str(&mut s, "nihao");
        pick_text(&mut s, "拟");
        assert_eq!(press(&mut s, Key::Space).commit.as_deref(), Some("拟好"));
        type_str(&mut s, "nihao");
        assert!(candidates(&s).iter().any(|c| c == "拟好"));
    }

    #[test]
    fn nothing_is_learned_without_a_user_dict_or_when_turned_off() {
        let mut s = session();
        type_str(&mut s, "ni");
        assert!(!pick_text(&mut s, "泥").learned, "没挂用户词库就不学");

        let mut s = learning_session();
        let mut settings = Settings::default();
        settings.input.learn = false;
        s.set_settings(settings);
        type_str(&mut s, "ni");
        assert!(!pick_text(&mut s, "泥").learned);
        assert!(s.user_words().is_empty());
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

    /// ni 有 11 个候选，每页最多 10 个也翻得了页。词频从高到低：你拟泥尼妮倪霓逆腻溺匿。
    /// `emoji` 是 emoji 对照表的内容，不要就传空串。
    fn session_with_many_candidates(page_size: usize, emoji: &str) -> Session {
        let sample: String = "你拟泥尼妮倪霓逆腻溺匿"
            .chars()
            .enumerate()
            .map(|(i, c)| format!("{c}\tni\t{}\n", 1000 - i * 10))
            .collect();
        let engine = Engine::new(Dictionary::from_rime_sources(&[&sample])).with_emoji(Emoji::parse(emoji));
        let mut s = Session::new(Arc::new(engine));
        let mut settings = Settings::default();
        settings.input.page_size = page_size;
        s.set_settings(settings);
        s
    }

    #[test]
    fn page_size_option() {
        let mut s = session_with_many_candidates(5, "");
        type_str(&mut s, "ni");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert_eq!(panel.candidates.len(), 5);
        assert!(panel.has_next);
        let r = press(&mut s, Key::Char('6'));
        assert_eq!(r.commit, None, "超出本页的数字键不选词");
        press(&mut s, Key::Char('='));
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert!(panel.has_prev);
    }

    #[test]
    fn zero_selects_the_tenth_candidate() {
        let mut s = session_with_many_candidates(10, "");
        type_str(&mut s, "ni");
        assert_eq!(press(&mut s, Key::Char('0')).commit.as_deref(), Some("溺"));

        // 每页不到 10 个时 0 不选字：首选上屏，再接上 0
        let mut s = session();
        type_str(&mut s, "ni");
        assert_eq!(press(&mut s, Key::Char('0')).commit.as_deref(), Some("你0"));
    }

    #[test]
    fn one_emoji_in_the_last_slot_of_the_first_page() {
        // 满一页时 emoji 占第一页最后一格，原来那格的字挤到第二页；一个词有几个 emoji 也只放第一个
        let mut s = session_with_many_candidates(5, "你\t你 👉 👈\n妮\t妮 👧\n");
        type_str(&mut s, "ni");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert_eq!(panel.candidates, ["你", "拟", "泥", "尼", "👉"]);
        assert_eq!(panel.emoji, Some(4), "候选窗据此在 emoji 前面画分隔线");
        press(&mut s, Key::Char('='));
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert_eq!(panel.candidates, ["妮", "倪", "霓", "逆", "腻"], "整个列表只放一个 emoji");
        assert_eq!(panel.emoji, None);
        press(&mut s, Key::Char('-'));
        assert_eq!(press(&mut s, Key::Char('5')).commit.as_deref(), Some("👉"));
    }

    #[test]
    fn emoji_only_for_words_matching_the_whole_input() {
        let emoji = Emoji::parse("你好\t你好 👋\n你\t你 👉\n");
        let engine = Engine::new(Dictionary::from_rime_sources(&[SAMPLE])).with_emoji(emoji);
        let mut s = Session::new(Arc::new(engine));
        type_str(&mut s, "nihao");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert_eq!(panel.candidates.last().map(String::as_str), Some("👋"), "不满一页时放在最后");
        assert_eq!(panel.emoji, Some(panel.candidates.len() - 1));
        assert!(!panel.candidates.iter().any(|c| c == "👉"), "只消耗了 ni 的「你」不带 emoji");
        let key = SELECT_KEYS.chars().nth(panel.candidates.len() - 1).unwrap();
        assert_eq!(press(&mut s, Key::Char(key)).commit.as_deref(), Some("👋"), "选 emoji 消耗整串拼音");

        let mut settings = Settings::default();
        settings.input.emoji = false;
        s.set_settings(settings);
        type_str(&mut s, "nihao");
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert!(!panel.candidates.iter().any(|c| c == "👋"), "设置里关掉就不带");
    }

    #[test]
    fn clicks_pick_candidates_and_turn_pages() {
        let mut s = session_with_many_candidates(5, "");
        type_str(&mut s, "ni");
        s.pick(Pick::NextPage);
        let View::Composing { panel, .. } = s.view() else { panic!() };
        assert!(panel.has_prev, "点 › 翻到了第二页");
        assert_eq!(s.pick(Pick::Candidate(1)).commit.as_deref(), Some("霓"), "第二页的第 2 个");
        assert_eq!(s.pick(Pick::Candidate(0)).commit, None, "不在输入中，点了也不算");
    }

    #[test]
    fn marked_text_is_what_was_typed() {
        // nihoa 会纠成「你好」，但输入框里显示的、回车上屏的都是打的字母
        let mut s = session();
        type_str(&mut s, "nihoa");
        let View::Composing { marked, panel } = s.view() else { panic!() };
        assert_eq!(marked, "nihoa");
        assert_eq!(panel.candidates[0], "你好");
        assert_eq!(press(&mut s, Key::Enter).commit.as_deref(), Some("nihoa"));
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
