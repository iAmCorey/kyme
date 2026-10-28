//! 设置窗口：上面是候选窗的预览，下面是分成「输入」「外观」两组的各项。点一下就写进设置文件，
//! 文件里的注释照旧；输入法切换输入框时读到新设置。

use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, BoxShadow, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla, Pixels, Render,
    SharedString, Size, Stateful, Subscription, Window, WindowAppearance, div, font, point, px, rgb, rgba, size,
    transparent_black,
};
use kyme_core::pinyin::{Dictionary, Emoji, Engine};
use kyme_core::{FONT_SIZES, Key, Modifiers, PAGE_SIZES, Rgb, Session, Settings, SettingsFile, UserDictFile, View};

use crate::{CandidateView, Content, Metrics, Theme, panel_height};

const WIDTH: f32 = 520.0;
/// 标题栏那一条，红绿灯在里面。
const TITLEBAR: f32 = 32.0;
/// 窗口左右的留白。
const MARGIN: f32 = 16.0;
/// 预览区的内边距。
const STAGE_PAD: f32 = 16.0;
/// 预览里模拟的输入框那一行。
const INPUT_LINE: f32 = 20.0;
/// 模拟的输入框和候选窗之间的距离。
const STAGE_GAP: f32 = 8.0;
/// 分组的标题，连同上方的留白。
const HEADER: f32 = 32.0;
const ROW: f32 = 40.0;
/// 每组几项。render 里每组的行按它定长，加了一组或一项忘了改这里会编译不过，窗口高度就不会算错。
const SECTION_ROWS: [usize; 2] = [4, 3];
/// 每组卡片上下各一像素的边框。
const CARD_BORDER: f32 = 2.0;
const FOOTER: f32 = 48.0;

/// 预览打的拼音和用的小词库：打 nihao 时第一页会出现的词，按词频从高到低。
const PREVIEW_INPUT: &str = "nihao";
const PREVIEW_DICT: &str = "你好\tni hao\t1000\n你\tni\t900\n拟\tni\t800\n泥\tni\t700\n尼\tni\t600\n\
                            妮\tni\t500\n倪\tni\t400\n霓\tni\t300\n逆\tni\t200\n腻\tni\t100\n";
const PREVIEW_EMOJI: &str = "你好\t你好 👋\n";

/// 字体的选项：设置里写的字体名和显示的名字。只列出这台机器上装了的。
const FONTS: [(&str, &str); 6] = [
    ("", "默认"),
    ("Hiragino Sans GB", "冬青黑体"),
    ("Lantinghei SC", "兰亭黑"),
    ("Yuanti SC", "圆体"),
    ("Songti SC", "宋体"),
    ("Kaiti SC", "楷体"),
];

/// 高亮色的选项：系统的几种强调色。
const COLORS: [u32; 9] = [0x5e5ce6, 0x007aff, 0xaf52de, 0xff2d55, 0xff3b30, 0xff9500, 0xffcc00, 0x34c759, 0x8e8e93];

pub struct SettingsView {
    file: SettingsFile,
    settings: Settings,
    /// 设置文件读不了、写不进去时的说明。
    error: Option<String>,
    default_font: SharedString,
    /// 这台机器上有的字体选项。
    fonts: Vec<(&'static str, &'static str)>,
    /// 预览用的引擎，只有 PREVIEW_DICT 里的几个词。
    engine: Arc<Engine>,
    /// 预览的输入框里显示的内容。
    marked: String,
    preview: Entity<CandidateView>,
    /// 用户词库里有几个词。
    learned_words: usize,
    /// 已经点过一次「清除」，再点一次才真的清。
    confirm_clear: bool,
    focus: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl SettingsView {
    /// `default_font`：设置里没指定字体时用的平台默认中文字体，界面文字也用它。
    pub fn new(
        file: SettingsFile,
        default_font: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_font = default_font.into();
        // 只查这几个字体，不把系统字体全列一遍：没装的字体会落到后备字体上，名字就对不上
        let text = cx.text_system();
        let fonts = FONTS
            .into_iter()
            .filter(|(family, _)| {
                family.is_empty()
                    || text
                        .get_font_for_id(text.resolve_font(&font(*family)))
                        .is_some_and(|found| found.family == *family)
            })
            .collect();
        let engine =
            Engine::new(Dictionary::from_rime_sources(&[PREVIEW_DICT])).with_emoji(Emoji::parse(PREVIEW_EMOJI));
        let preview = cx.new(|_| CandidateView::new(default_font.clone(), |_| {}));
        let subscriptions = [
            // 回到这个窗口时重读一次：可能刚在编辑器里改过设置文件
            cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.reload(window, cx);
                }
            }),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        // 拿着焦点，⌘W、⌘Q 这些快捷键才有地方派
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut view = SettingsView {
            file,
            settings: Settings::default(),
            error: None,
            default_font,
            fonts,
            engine: Arc::new(engine),
            marked: String::new(),
            preview,
            learned_words: 0,
            confirm_clear: false,
            focus,
            _subscriptions: subscriptions,
        };
        view.reload(window, cx);
        view
    }

    /// 按这份设置，窗口内容该多大。预览跟着字号变高，窗口也跟着变；打开窗口前用它定大小。
    pub fn size_for(settings: &Settings) -> Size<Pixels> {
        let sections: f32 = SECTION_ROWS.iter().map(|&rows| HEADER + rows as f32 * ROW + CARD_BORDER).sum();
        size(px(WIDTH), px(TITLEBAR + stage_height(settings) + sections + FOOTER))
    }

    /// 设置文件改过（或者还没读过）就重读。读不了时和输入法一样用默认设置，并显示原因。
    /// 用户词库是输入法写的，每次都重数一遍有几个词。
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.learned_words = self.userdict().count();
        let (settings, error) = match self.file.reload() {
            None => return,
            Some(Ok(settings)) => (settings, None),
            Some(Err(err)) => (Settings::default(), Some(err)),
        };
        self.settings = settings;
        self.error = error;
        self.refresh(window, cx);
    }

    /// 改一项设置并写进文件。写不进去就保持原样，显示原因。
    fn change(&mut self, edit: &dyn Fn(&mut Settings), window: &mut Window, cx: &mut Context<Self>) {
        // 文件可能刚在别处改过，以文件里的为准再改
        self.reload(window, cx);
        self.confirm_clear = false;
        let mut next = self.settings.clone();
        edit(&mut next);
        let next = next.normalized();
        if next == self.settings {
            return;
        }
        match self.file.save(&next) {
            Ok(()) => {
                self.settings = next;
                self.error = None;
            }
            Err(err) => self.error = Some(err),
        }
        self.refresh(window, cx);
    }

    /// 预览用真的输入会话打一遍 nihao，和打字时看到的一样；窗口跟着预览变高变矮，顶边不动。
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut session = Session::new(self.engine.clone());
        session.set_settings(self.settings.clone());
        for c in PREVIEW_INPUT.chars() {
            session.handle_key(Key::Char(c), Modifiers::default());
        }
        if let View::Composing { marked, panel } = session.view() {
            let appearance = self.settings.appearance.clone();
            self.preview.update(cx, |preview, _| preview.set(Content::Candidates(panel), appearance));
            self.marked = marked;
        }
        window.resize(Self::size_for(&self.settings));
        cx.notify();
    }

    /// 预览：模拟的输入框，下面是和设置里一样的候选窗。字号大到放不下时右边被裁掉。
    fn stage(&self, p: &Palette, window: &Window, cx: &App) -> Div {
        let panel = self.preview.read(cx).measure(window);
        let radius = Metrics::new(self.settings.appearance.font_size).radius;
        // 拼音不显示在输入框里、又还没选字时，输入框里只有光标
        let marked = (!self.marked.is_empty()).then(|| div().underline().child(self.marked.clone()));
        div()
            .mx(px(MARGIN))
            .h(px(stage_height(&self.settings)))
            .p(px(STAGE_PAD))
            .flex()
            .flex_col()
            .gap(px(STAGE_GAP))
            .rounded(px(10.0))
            .bg(p.stage)
            .overflow_hidden()
            .child(
                div()
                    .h(px(INPUT_LINE))
                    .flex()
                    .items_center()
                    .text_size(px(15.0))
                    .children(marked)
                    .child(div().ml(px(1.0)).w(px(2.0)).h(px(17.0)).rounded(px(1.0)).bg(p.theme.accent)),
            )
            .child(
                div()
                    .flex_none()
                    .w(panel.width)
                    .h(panel.height)
                    .rounded(px(radius))
                    .shadow(shadow(p.shadow, 2.0, 10.0))
                    .child(self.preview.clone()),
            )
    }

    fn userdict(&self) -> UserDictFile {
        UserDictFile::beside(self.file.path())
    }

    /// 「记住选过的词」一行的右边：学了几个词、清除，和开关。清除要点两次，免得手滑。
    fn learn_control(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let clear = (self.learned_words > 0).then(|| {
            let (label, color) = if self.confirm_clear {
                ("再点一次清除".to_string(), p.error)
            } else {
                (format!("{} 个词 · 清除", self.learned_words), p.theme.secondary)
            };
            div()
                .id("clear-learned")
                .text_size(px(12.0))
                .text_color(color)
                .cursor_pointer()
                .hover(|style| style.underline())
                .child(label)
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.clear_learned(cx)))
        });
        div().flex().items_center().gap(px(12.0)).children(clear).child(
            switch(p, "learn", self.settings.input.learn).on_click(on_change(cx, |s| s.input.learn = !s.input.learn)),
        )
    }

    /// 清除学到的词：删掉用户词库的文件，输入法下次切换输入框时发现文件没了，就清空。
    fn clear_learned(&mut self, cx: &mut Context<Self>) {
        if self.confirm_clear {
            match self.userdict().clear() {
                Ok(()) => self.learned_words = 0,
                Err(err) => self.error = Some(format!("清除不了学到的词：{err}")),
            }
        }
        self.confirm_clear = !self.confirm_clear;
        cx.notify();
    }

    fn page_picker(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let current = self.settings.input.page_size;
        segmented(p).children(PAGE_SIZES.map(|n| {
            let selected = n == current;
            segment(p, ("page", n), selected)
                .child(n.to_string())
                .when(!selected, |segment| segment.on_click(on_change(cx, move |s| s.input.page_size = n)))
        }))
    }

    fn stepper(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let font_size = self.settings.appearance.font_size;
        let mut button = |id: &'static str, label: &'static str, step: f32| {
            let enabled = FONT_SIZES.contains(&(font_size + step));
            let hover = p.hover;
            div()
                .id(id)
                .w(px(28.0))
                .h(px(22.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.0))
                .text_size(px(15.0))
                .text_color(if enabled { p.theme.primary } else { p.theme.disabled })
                .child(label)
                .when(enabled, |button| {
                    button
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(on_change(cx, move |s| s.appearance.font_size += step))
                })
        };
        let smaller = button("smaller", "−", -1.0);
        let bigger = button("bigger", "+", 1.0);
        div()
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(div().text_size(px(12.0)).text_color(p.theme.secondary).child(format!("{font_size} pt")))
            .child(segmented(p).child(smaller).child(bigger))
    }

    fn font_picker(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let current = self.settings.appearance.font_family.clone();
        let mut options: Vec<(String, String)> =
            self.fonts.iter().map(|(family, label)| (family.to_string(), label.to_string())).collect();
        // 设置文件里手写了别的字体：也列出来
        if !options.iter().any(|(family, _)| *family == current) {
            options.push((current.clone(), current.clone()));
        }
        segmented(p).children(options.into_iter().enumerate().map(|(i, (family, label))| {
            let selected = family == current;
            let font = if family.is_empty() { self.default_font.clone() } else { family.clone().into() };
            segment(p, ("font", i), selected).font_family(font).child(label).when(!selected, |segment| {
                segment.on_click(on_change(cx, move |s| s.appearance.font_family = family.clone()))
            })
        }))
    }

    fn color_picker(&self, cx: &mut Context<Self>) -> Div {
        let current = self.settings.appearance.highlight_color;
        let mut colors: Vec<Rgb> = COLORS.into_iter().map(Rgb).collect();
        if !colors.contains(&current) {
            colors.push(current);
        }
        div().flex().items_center().gap(px(4.0)).children(colors.into_iter().enumerate().map(|(i, color)| {
            let selected = color == current;
            let fill: Hsla = rgb(color.0).into();
            // 选中的那个外面套一圈同色的环，中间空出一点
            div()
                .id(("color", i))
                .size(px(22.0))
                .p(px(2.0))
                .rounded_full()
                .border_2()
                .border_color(if selected { fill } else { transparent_black() })
                .child(div().size_full().rounded_full().bg(fill))
                .when(!selected, |swatch| {
                    swatch
                        .cursor_pointer()
                        .hover(move |style| style.border_color(fill.opacity(0.4)))
                        .on_click(on_change(cx, move |s| s.appearance.highlight_color = color))
                })
        }))
    }

    /// 底部：打开设置文件；右边是版本号，出错时换成错误说明。
    fn footer(&self, p: &Palette) -> Div {
        let path = self.file.path().to_path_buf();
        let note = match &self.error {
            Some(err) => div().text_color(p.error).child(err.clone()),
            None => div().text_color(p.theme.secondary).child(concat!("Kyme ", env!("CARGO_PKG_VERSION"))),
        };
        div()
            .h(px(FOOTER))
            .mx(px(MARGIN))
            .px(px(4.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.0))
            .text_size(px(12.0))
            .child(
                div()
                    .id("open-file")
                    .flex_none()
                    .text_color(p.theme.accent)
                    .cursor_pointer()
                    .hover(|style| style.underline())
                    .child("打开设置文件")
                    .on_click(move |_, _, cx| cx.open_with_system(&path)),
            )
            .child(note.min_w(px(0.0)).truncate())
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::new(window.appearance(), self.settings.appearance.highlight_color);
        let input = &self.settings.input;
        let input_rows: [Div; SECTION_ROWS[0]] = [
            row(&p, true, "每页候选", self.page_picker(&p, cx)),
            row(
                &p,
                false,
                "拼音显示在输入框里",
                switch(&p, "inline", input.inline_pinyin)
                    .on_click(on_change(cx, |s| s.input.inline_pinyin = !s.input.inline_pinyin)),
            ),
            row(
                &p,
                false,
                "候选带 emoji",
                switch(&p, "emoji", input.emoji).on_click(on_change(cx, |s| s.input.emoji = !s.input.emoji)),
            ),
            row(&p, false, "记住选过的词", self.learn_control(&p, cx)),
        ];
        let appearance_rows: [Div; SECTION_ROWS[1]] = [
            row(&p, true, "字号", self.stepper(&p, cx)),
            row(&p, false, "字体", self.font_picker(&p, cx)),
            row(&p, false, "高亮色", self.color_picker(cx)),
        ];
        let sections: [Div; SECTION_ROWS.len()] =
            [section(&p, "输入", input_rows), section(&p, "外观", appearance_rows)];
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(p.window)
            .font_family(self.default_font.clone())
            .text_color(p.theme.primary)
            .child(
                div()
                    .h(px(TITLEBAR))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Kyme 设置"),
            )
            .child(self.stage(&p, window, cx))
            .children(sections)
            .child(self.footer(&p))
    }
}

/// 预览区的高度：模拟的输入框，加上和设置里一样大的候选窗。
fn stage_height(settings: &Settings) -> f32 {
    let m = Metrics::new(settings.appearance.font_size);
    STAGE_PAD * 2.0 + INPUT_LINE + STAGE_GAP + panel_height(&m, !settings.input.inline_pinyin)
}

/// 点一下改一项设置。
fn on_change(
    cx: &mut Context<SettingsView>,
    edit: impl Fn(&mut Settings) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |view, _: &ClickEvent, window, cx| view.change(&edit, window, cx))
}

/// 一组设置：小标题，下面一张卡片。
fn section<const ROWS: usize>(p: &Palette, title: &'static str, rows: [Div; ROWS]) -> Div {
    div()
        .mx(px(MARGIN))
        .flex()
        .flex_col()
        .child(
            div()
                .h(px(HEADER))
                .flex()
                .items_end()
                .pb(px(6.0))
                .pl(px(4.0))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.theme.secondary)
                .child(title),
        )
        .child(div().flex().flex_col().rounded(px(10.0)).border_1().border_color(p.line).bg(p.card).children(rows))
}

/// 卡片里的一行：左边是名字，右边是控件。除了第一行，上面有一条从文字处开始的分隔线。
fn row(p: &Palette, first: bool, label: &'static str, control: impl IntoElement) -> Div {
    div().h(px(ROW)).pl(px(14.0)).child(
        div()
            .size_full()
            .pr(px(14.0))
            .flex()
            .items_center()
            .justify_between()
            .when(!first, |row| row.border_t_1().border_color(p.line))
            .child(div().text_size(px(13.0)).child(label))
            .child(control),
    )
}

/// 分段控件的底。
fn segmented(p: &Palette) -> Div {
    div().flex().items_center().gap(px(2.0)).p(px(2.0)).rounded(px(7.0)).bg(p.track)
}

fn segment(p: &Palette, id: impl Into<ElementId>, selected: bool) -> Stateful<Div> {
    let hover = p.hover;
    div()
        .id(id)
        .h(px(22.0))
        .min_w(px(26.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.0))
        .text_size(px(12.0))
        .map(|segment| {
            if selected {
                segment.bg(p.thumb).shadow(shadow(p.shadow, 1.0, 2.0))
            } else {
                segment.cursor_pointer().hover(move |style| style.bg(hover))
            }
        })
}

fn switch(p: &Palette, id: &'static str, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(32.0))
        .h(px(19.0))
        .p(px(2.0))
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .bg(if on { p.theme.accent } else { p.switch_off })
        .when(on, |switch| switch.justify_end())
        .child(div().size(px(15.0)).rounded_full().bg(rgb(0xffffff)).shadow(shadow(rgba(0x00000040).into(), 1.0, 2.0)))
}

fn shadow(color: Hsla, y: f32, blur: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color, offset: point(px(0.0), px(y)), blur_radius: px(blur), spread_radius: px(0.0) }]
}

/// 设置窗口的配色：文字色和强调色（设置里的高亮色）用候选窗那一套，这里只补设置窗口自己的。
struct Palette {
    theme: Theme,
    window: Hsla,
    /// 预览区的底，比窗口暗一点，像一块桌面。
    stage: Hsla,
    card: Hsla,
    /// 卡片的边框和行间的分隔线，比候选窗的边框淡。
    line: Hsla,
    /// 分段控件、步进器的底。
    track: Hsla,
    /// 分段控件里选中的那一格。
    thumb: Hsla,
    hover: Hsla,
    switch_off: Hsla,
    shadow: Hsla,
    error: Hsla,
}

impl Palette {
    fn new(appearance: WindowAppearance, accent: Rgb) -> Self {
        let theme = Theme::new(appearance, accent);
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Palette {
                theme,
                window: rgb(0x1e1e1e).into(),
                stage: rgb(0x111111).into(),
                card: rgb(0x2a2a2c).into(),
                line: rgba(0xffffff14).into(),
                track: rgba(0x7676803d).into(),
                thumb: rgb(0x636366).into(),
                hover: rgba(0xffffff0f).into(),
                switch_off: rgba(0x78788052).into(),
                shadow: rgba(0x00000066).into(),
                error: rgb(0xff453a).into(),
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Palette {
                theme,
                window: rgb(0xf5f5f7).into(),
                stage: rgb(0xe8e8ed).into(),
                card: rgb(0xffffff).into(),
                line: rgba(0x00000014).into(),
                track: rgba(0x7676801f).into(),
                thumb: rgb(0xffffff).into(),
                hover: rgba(0x0000000a).into(),
                switch_off: rgba(0x78788033).into(),
                shadow: rgba(0x00000026).into(),
                error: rgb(0xff3b30).into(),
            },
        }
    }
}
