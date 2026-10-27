//! GPUI 界面：候选窗。三个桌面端共用，平台层只决定窗口放在哪、什么时候显示。

use gpui::prelude::*;
use gpui::{
    Context, Div, Hsla, Pixels, Render, SharedString, Size, TextRun, Window, WindowAppearance, div, font, px, rgb,
    rgba, size,
};
use kyme_core::{AppearanceSettings, Mode, Panel, Rgb};

/// 其余尺寸都按 18pt 的候选字号设计，换字号时等比缩放。
const BASE_FONT_SIZE: f32 = 18.0;
/// 测量误差和边框留的余量，避免文字被挤到换行。
const SLACK: f32 = 4.0;

/// 候选窗里显示什么。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Candidates(Panel),
    /// 中英切换后短暂显示的提示。
    Mode(Mode),
}

pub struct CandidateView {
    content: Content,
    appearance: AppearanceSettings,
    default_font: SharedString,
}

impl CandidateView {
    /// `default_font`：设置里没指定字体时用的平台默认中文字体。
    pub fn new(default_font: impl Into<SharedString>) -> Self {
        CandidateView {
            content: Content::Mode(Mode::Chinese),
            appearance: AppearanceSettings::default(),
            default_font: default_font.into(),
        }
    }

    pub fn set(&mut self, content: Content, appearance: AppearanceSettings) {
        self.content = content;
        self.appearance = appearance;
    }

    fn font_family(&self) -> SharedString {
        if self.appearance.font_family.is_empty() {
            self.default_font.clone()
        } else {
            self.appearance.font_family.clone().into()
        }
    }

    /// 当前内容需要的窗口大小。平台层先测量，再把窗口挪到光标旁边。
    pub fn measure(&self, window: &Window) -> Size<Pixels> {
        let m = Metrics::new(self.appearance.font_size);
        let family = self.font_family();
        let width_of = |text: &str, font_size: f32| text_width(window, &family, text, font_size);
        match &self.content {
            Content::Mode(mode) => {
                let width = width_of(mode_label(*mode), m.hint) + m.pad_x * 2.0 + SLACK;
                size(px(width.max(m.hint_height).ceil()), px(m.hint_height.ceil()))
            }
            Content::Candidates(panel) => {
                let mut row = 0.0;
                for (i, text) in panel.candidates.iter().enumerate() {
                    if i > 0 {
                        row += m.cell_gap;
                    }
                    row += m.cell_pad_x * 2.0
                        + width_of(&(i + 1).to_string(), m.index)
                        + m.index_gap
                        + width_of(text, m.candidate);
                }
                if panel.has_prev || panel.has_next {
                    row += m.pager;
                }
                let preedit = width_of(&panel.preedit, m.preedit);
                let width = row.max(preedit).max(m.min_width) + m.pad_x * 2.0 + SLACK;
                // 拼音显示在输入框里时，候选窗没有拼音那一行
                let preedit_row = if panel.preedit.is_empty() { 0.0 } else { m.preedit_line + m.row_gap };
                let height = m.pad_y * 2.0 + preedit_row + m.row;
                size(px(width.ceil()), px(height.ceil()))
            }
        }
    }
}

fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Chinese => "中",
        Mode::English => "英",
    }
}

/// 按字号算出的各种尺寸。
struct Metrics {
    candidate: f32,
    index: f32,
    preedit: f32,
    preedit_line: f32,
    row: f32,
    pad_x: f32,
    pad_y: f32,
    row_gap: f32,
    cell_pad_x: f32,
    cell_gap: f32,
    index_gap: f32,
    pager: f32,
    radius: f32,
    cell_radius: f32,
    min_width: f32,
    hint: f32,
    hint_height: f32,
}

impl Metrics {
    fn new(font_size: f32) -> Self {
        let s = font_size / BASE_FONT_SIZE;
        Metrics {
            candidate: font_size,
            index: 12.0 * s,
            preedit: 13.0 * s,
            preedit_line: 18.0 * s,
            row: 30.0 * s,
            pad_x: 10.0 * s,
            pad_y: 7.0 * s,
            row_gap: 4.0 * s,
            cell_pad_x: 7.0 * s,
            cell_gap: 2.0 * s,
            index_gap: 4.0 * s,
            pager: 30.0 * s,
            radius: 10.0 * s,
            cell_radius: 6.0 * s,
            min_width: 120.0 * s,
            hint: 16.0 * s,
            hint_height: 36.0 * s,
        }
    }
}

fn text_width(window: &Window, family: &SharedString, text: &str, font_size: f32) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    let run = TextRun {
        len: text.len(),
        font: font(family.clone()),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(SharedString::from(text.to_owned()), px(font_size), &[run], None);
    f32::from(line.width)
}

struct Theme {
    background: Hsla,
    border: Hsla,
    primary: Hsla,
    secondary: Hsla,
    disabled: Hsla,
    accent: Hsla,
    on_accent: Hsla,
    on_accent_muted: Hsla,
}

impl Theme {
    fn new(appearance: WindowAppearance, accent: Rgb) -> Self {
        let (on_accent, on_accent_muted) = on_color(accent);
        let accent = rgb(accent.0).into();
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme {
                background: rgba(0x2c2c2ef8).into(),
                border: rgba(0xffffff26).into(),
                primary: rgb(0xf2f2f7).into(),
                secondary: rgb(0x98989f).into(),
                disabled: rgb(0x48484a).into(),
                accent,
                on_accent,
                on_accent_muted,
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme {
                background: rgba(0xfffffffa).into(),
                border: rgba(0x0000001f).into(),
                primary: rgb(0x1d1d1f).into(),
                secondary: rgb(0x8e8e93).into(),
                disabled: rgb(0xd1d1d6).into(),
                accent,
                on_accent,
                on_accent_muted,
            },
        }
    }
}

/// 高亮色上的文字颜色：高亮色偏亮就用深色字，否则用白字。
fn on_color(color: Rgb) -> (Hsla, Hsla) {
    let c = rgb(color.0);
    let luminance = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    if luminance > 0.6 {
        (rgb(0x1d1d1f).into(), rgba(0x1d1d1fa6).into())
    } else {
        (rgb(0xffffff).into(), rgba(0xffffffb3).into())
    }
}

impl Render for CandidateView {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::new(window.appearance(), self.appearance.highlight_color);
        let m = Metrics::new(self.appearance.font_size);
        let root = div()
            .size_full()
            .overflow_hidden()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .rounded(px(m.radius))
            .font_family(self.font_family());
        match &self.content {
            Content::Mode(mode) => root
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(m.hint))
                .text_color(theme.primary)
                .child(mode_label(*mode)),
            Content::Candidates(panel) => root
                .flex()
                .flex_col()
                .px(px(m.pad_x))
                .py(px(m.pad_y))
                .gap(px(m.row_gap))
                .when(!panel.preedit.is_empty(), |root| {
                    root.child(
                        div()
                            .h(px(m.preedit_line))
                            .flex()
                            .items_center()
                            .whitespace_nowrap()
                            .text_size(px(m.preedit))
                            .text_color(theme.secondary)
                            .child(panel.preedit.clone()),
                    )
                })
                .child(
                    div()
                        .h(px(m.row))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(m.cell_gap))
                        .children(
                            panel
                                .candidates
                                .iter()
                                .enumerate()
                                .map(|(i, text)| candidate_cell(&theme, &m, i, text, i == panel.highlighted)),
                        )
                        .when(panel.has_prev || panel.has_next, |row| {
                            row.child(pager(&theme, &m, panel.has_prev, panel.has_next))
                        }),
                ),
        }
    }
}

fn candidate_cell(theme: &Theme, m: &Metrics, index: usize, text: &str, selected: bool) -> Div {
    let (index_color, text_color) =
        if selected { (theme.on_accent_muted, theme.on_accent) } else { (theme.secondary, theme.primary) };
    div()
        .h(px(m.row - 2.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(m.index_gap))
        .px(px(m.cell_pad_x))
        .rounded(px(m.cell_radius))
        .whitespace_nowrap()
        .when(selected, |cell| cell.bg(theme.accent))
        .child(div().text_size(px(m.index)).text_color(index_color).child((index + 1).to_string()))
        .child(div().text_size(px(m.candidate)).text_color(text_color).child(text.to_string()))
}

fn pager(theme: &Theme, m: &Metrics, has_prev: bool, has_next: bool) -> Div {
    let color = |enabled: bool| if enabled { theme.secondary } else { theme.disabled };
    div()
        .w(px(m.pager))
        .flex()
        .flex_row()
        .items_center()
        .justify_end()
        .gap(px(m.index_gap))
        .text_size(px(m.preedit + 1.0))
        .child(div().text_color(color(has_prev)).child("‹"))
        .child(div().text_color(color(has_next)).child("›"))
}
