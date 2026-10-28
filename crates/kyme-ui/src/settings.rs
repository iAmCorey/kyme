//! 设置窗口：上面是候选窗的预览，下面是分成「输入」「外观」「词库」三组的各项。点一下就写进设置文件，
//! 文件里的注释照旧；输入法切换输入框时读到新设置。「自定义词库」点进去是单独的一页（custom.rs）。

mod custom;

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{
    App, BoxShadow, ClickEvent, Context, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla, PathPromptOptions,
    Pixels, Render, SharedString, Size, Stateful, Subscription, Window, WindowAppearance, div, font, point, px, rgb,
    rgba, size, transparent_black,
};
use kyme_core::pinyin::{Dictionary, Emoji, Engine};
use kyme_core::{
    FONT_SIZES, Imported, Key, LocalDicts, Modifiers, PAGE_SIZES, Rgb, Session, Settings, SettingsFile, UserDictFile,
    View,
};

use self::custom::{Back, CustomWordsView};
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
/// 「输入」「外观」「词库」每组固定几项。render 里每组的行按它定长，加了一组或一项忘了改这里会编译不过，
/// 窗口高度就不会算错。「词库」组下面每个导入过的外部词库再一行，多于 MAX_LISTED 个就在卡片里滚动。
const SECTION_ROWS: [usize; 3] = [4, 3, 2];
const MAX_LISTED: usize = 4;
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

/// 要点两次才做的事，点过第一次的是哪一个。
#[derive(PartialEq)]
enum Confirm {
    /// 清除学到的词。
    Clear,
    /// 删掉这个导入的外部词库。
    Remove(String),
}

pub struct SettingsView {
    file: SettingsFile,
    /// 本地词库的目录。外部词库的词数在这里记着，没改过的不再数。
    dicts: LocalDicts,
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
    /// 点过一次、等着再点一次的操作。
    confirm: Option<Confirm>,
    /// 自定义词库里有几个词。
    custom_words: usize,
    /// 安装包里的词库目录，自定义词库页定读音用。
    dict_dir: Option<PathBuf>,
    /// 「自定义词库」页，第一次点进去时建，之后留着：主词库不用再读一遍。
    custom: Option<Entity<CustomWordsView>>,
    /// 正显示着「自定义词库」页。
    in_custom: bool,
    /// 导入过的词库。
    imported: Vec<Imported>,
    /// 窗口最高多高：屏幕扣掉菜单栏和 Dock。内容比这高时，标题栏和底部不动，中间那部分滚动。
    max_height: Pixels,
    /// 正在导入，先不让再点。
    importing: bool,
    focus: FocusHandle,
    _subscriptions: [Subscription; 2],
}

impl SettingsView {
    /// `dicts`：和设置文件放在一起的本地词库目录，打开窗口时数过一遍词数的话就接着用。
    /// `default_font`：设置里没指定字体时用的平台默认中文字体，界面文字也用它。
    /// `dict_dir`：安装包里的词库目录，自定义词库页用它给没写拼音的词定读音。
    /// `max_height`：窗口最高多高，见 `size_for`。
    pub fn new(
        file: SettingsFile,
        dicts: LocalDicts,
        default_font: impl Into<SharedString>,
        dict_dir: Option<PathBuf>,
        max_height: Pixels,
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
            dicts,
            settings: Settings::default(),
            error: None,
            default_font,
            fonts,
            engine: Arc::new(engine),
            marked: String::new(),
            preview,
            learned_words: 0,
            confirm: None,
            custom_words: 0,
            dict_dir,
            custom: None,
            in_custom: false,
            imported: Vec::new(),
            max_height,
            importing: false,
            focus,
            _subscriptions: subscriptions,
        };
        view.reload(window, cx);
        view
    }

    /// 按这份设置和导入过几个词库，窗口内容该多大。预览跟着字号变高，窗口也跟着变；打开窗口前用它定大小。
    /// 不超过 `max_height`（屏幕扣掉菜单栏和 Dock 的高度），放不下时中间那部分滚动。
    pub fn size_for(settings: &Settings, imported: usize, max_height: Pixels) -> Size<Pixels> {
        let mut rows = SECTION_ROWS;
        rows[2] += imported.min(MAX_LISTED);
        let sections: f32 = rows.iter().map(|&rows| HEADER + rows as f32 * ROW + CARD_BORDER).sum();
        size(px(WIDTH), px(TITLEBAR + stage_height(settings) + sections + FOOTER).min(max_height))
    }

    /// 设置文件改过（或者还没读过）就重读。读不了时和输入法一样用默认设置，并显示原因。
    /// 用户词库是输入法写的、本地词库可能刚在编辑器里改过，每次都重数一遍。
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.learned_words = self.userdict().count();
        self.custom_words = self.dicts.custom_words().len();
        self.imported = self.dicts.imported();
        if let Some(result) = self.file.reload() {
            (self.settings, self.error) = match result {
                Ok(settings) => (settings, None),
                Err(err) => (Settings::default(), Some(err)),
            };
        }
        self.refresh(window, cx);
    }

    /// 改一项设置并写进文件。写不进去就保持原样，显示原因。
    fn change(&mut self, edit: &dyn Fn(&mut Settings), window: &mut Window, cx: &mut Context<Self>) {
        // 文件可能刚在别处改过，以文件里的为准再改
        self.reload(window, cx);
        self.confirm = None;
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
        window.resize(Self::size_for(&self.settings, self.imported.len(), self.max_height));
        // 正开着自定义词库页时重读：可能刚在编辑器里改过。没开着的话，点进去时会重读
        if let Some(custom) = &self.custom {
            let (accent, showing) = (self.settings.appearance.highlight_color, self.in_custom);
            custom.update(cx, |custom, cx| {
                custom.accent = accent;
                if showing {
                    custom.reload(cx);
                }
            });
        }
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
            let label = format!("{} 个词 · 清除", self.learned_words);
            confirm_link(p, "clear-learned", label, "再点一次清除", self.confirm == Some(Confirm::Clear))
                .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.clear_learned(cx)))
        });
        div().flex().items_center().gap(px(12.0)).children(clear).child(
            switch(p, "learn", self.settings.input.learn).on_click(on_change(cx, |s| s.input.learn = !s.input.learn)),
        )
    }

    /// 清除学到的词：删掉用户词库的文件，输入法下次切换输入框时发现文件没了，就清空。
    fn clear_learned(&mut self, cx: &mut Context<Self>) {
        if self.confirmed(Confirm::Clear) {
            match self.userdict().clear() {
                Ok(()) => self.learned_words = 0,
                Err(err) => self.error = Some(format!("清除不了学到的词：{err}")),
            }
        }
        cx.notify();
    }

    /// 要点两次的操作：第一次点只记下来，返回 false；紧接着再点同一个才返回 true。
    fn confirmed(&mut self, what: Confirm) -> bool {
        if self.confirm.as_ref() == Some(&what) {
            self.confirm = None;
            true
        } else {
            self.confirm = Some(what);
            false
        }
    }

    /// 「自定义词库」一行的右边：有几个词，「管理」点进去加词、删词。
    fn custom_control(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let count = (self.custom_words > 0).then(|| note(p, format!("{} 个词", self.custom_words)));
        let manage = link("manage-custom", "管理", p.theme.accent)
            .on_click(cx.listener(|view, _: &ClickEvent, window, cx| view.open_custom(window, cx)));
        div().flex().items_center().gap(px(12.0)).children(count).child(manage)
    }

    /// 换到「自定义词库」页，光标放进「词」的输入框。
    fn open_custom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let custom = match &self.custom {
            Some(custom) => custom.clone(),
            None => {
                let (dicts, dict_dir, accent) = (
                    LocalDicts::beside(self.file.path()),
                    self.dict_dir.clone(),
                    self.settings.appearance.highlight_color,
                );
                let custom = cx.new(|cx| CustomWordsView::new(dicts, dict_dir, accent, cx));
                cx.subscribe_in(&custom, window, |view, _, _: &Back, window, cx| {
                    view.in_custom = false;
                    // 回来时窗口拿回焦点，⌘W 才有地方派；词数跟着重数
                    view.focus.focus(window);
                    view.reload(window, cx);
                })
                .detach();
                self.custom = Some(custom.clone());
                custom
            }
        };
        custom.update(cx, |custom, cx| {
            custom.reload(cx);
            custom.focus(window, cx);
        });
        self.in_custom = true;
        cx.notify();
    }

    /// 「外部词库」一行的右边：选几个文件，在后台转好存进本地词库。
    fn import_control(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        if self.importing {
            return div().id("import").child(note(p, "正在导入…"));
        }
        link("import", "导入…", p.theme.accent)
            .on_click(cx.listener(|view, _: &ClickEvent, window, cx| view.import(window, cx)))
    }

    fn import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("导入".into()),
        });
        let dicts = LocalDicts::beside(self.file.path());
        cx.spawn_in(window, async move |view, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = view.update(cx, |view, cx| {
                view.importing = true;
                cx.notify();
            });
            let results = cx
                .background_executor()
                .spawn(async move { paths.iter().map(|path| dicts.import(path)).collect::<Vec<_>>() })
                .await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.importing = false;
                view.error = results.into_iter().find_map(Result::err);
                view.reload(window, cx);
            });
        })
        .detach();
    }

    /// 一个导入的词库那一行的右边：有几个词、删除。删除要点两次，免得手滑。
    fn remove_control(&self, dict: &Imported, p: &Palette, cx: &mut Context<Self>) -> Div {
        let name = dict.name.clone();
        let confirming = self.confirm == Some(Confirm::Remove(name.clone()));
        let id = ElementId::Name(format!("remove-{name}").into());
        let remove = confirm_link(p, id, "删除".into(), "再点一次删除", confirming).on_click(cx.listener(
            move |view, _: &ClickEvent, window, cx| {
                if view.confirmed(Confirm::Remove(name.clone())) {
                    if let Err(err) = view.dicts.remove(&name) {
                        view.error = Some(format!("删不了「{name}」：{err}"));
                    }
                    view.reload(window, cx);
                } else {
                    cx.notify();
                }
            },
        ));
        div().flex().items_center().gap(px(12.0)).child(note(p, format!("{} 个词", dict.words))).child(remove)
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
                link("open-file", "打开设置文件", p.theme.accent)
                    .flex_none()
                    .on_click(move |_, _, cx| open_text(path.clone(), cx)),
            )
            .child(note.min_w(px(0.0)).truncate())
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::new(window.appearance(), self.settings.appearance.highlight_color);
        if self.in_custom
            && let Some(custom) = &self.custom
        {
            return self.frame(&p, "自定义词库").child(custom.clone());
        }
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
        let dict_rows: [Div; SECTION_ROWS[2]] = [
            row(&p, true, "自定义词库", self.custom_control(&p, cx)),
            row(
                &p,
                false,
                described(&p, "外部词库", "搜狗细胞词库 .scel、Rime .dict.yaml、文本词表"),
                self.import_control(&p, cx),
            ),
        ];
        let listed: Vec<Div> = self
            .imported
            .iter()
            .map(|dict| row(&p, false, dict.name.clone(), self.remove_control(dict, &p, cx)))
            .collect();
        let listed = (!listed.is_empty())
            .then(|| div().id("imported").max_h(px(MAX_LISTED as f32 * ROW)).overflow_y_scroll().children(listed));
        let sections: [Div; SECTION_ROWS.len()] = [
            section(&p, "输入", card(&p).children(input_rows)),
            section(&p, "外观", card(&p).children(appearance_rows)),
            section(&p, "词库", card(&p).children(dict_rows).children(listed)),
        ];
        // 屏幕不够高时，标题栏和底部不动，中间这部分滚动
        let body = div()
            .id("body")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .child(div().flex().flex_col().child(self.stage(&p, window, cx)).children(sections));
        self.frame(&p, "Kyme 设置").child(body).child(self.footer(&p))
    }
}

impl SettingsView {
    /// 整个窗口：底色、字体，最上面是标题栏。下面接着放这一页的内容。
    fn frame(&self, p: &Palette, title: &'static str) -> Div {
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
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
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
fn section(p: &Palette, title: &'static str, card: Div) -> Div {
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
        .child(card)
}

/// 放一组设置的卡片，一行一行往里加。
fn card(p: &Palette) -> Div {
    div().flex().flex_col().rounded(px(10.0)).border_1().border_color(p.line).bg(p.card)
}

/// 卡片里的一行：左边是名字（太长就截断），右边是控件。除了第一行，上面有一条从文字处开始的分隔线。
fn row(p: &Palette, first: bool, label: impl IntoElement, control: impl IntoElement) -> Div {
    div().h(px(ROW)).pl(px(14.0)).child(
        div()
            .size_full()
            .pr(px(14.0))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.0))
            .when(!first, |row| row.border_t_1().border_color(p.line))
            .child(div().min_w(px(0.0)).truncate().text_size(px(13.0)).child(label))
            .child(div().flex_none().child(control)),
    )
}

/// 名字下面再带一行小字说明。
fn described(p: &Palette, label: &'static str, detail: &'static str) -> Div {
    div().flex().flex_col().child(label).child(div().text_size(px(11.0)).text_color(p.theme.secondary).child(detail))
}

/// 行里的说明文字，比如有几个词。
fn note(p: &Palette, text: impl Into<SharedString>) -> Div {
    div().text_size(px(12.0)).text_color(p.theme.secondary).child(text.into())
}

/// 要点两次的链接：点过一次后变成红字的「再点一次……」。
fn confirm_link(
    p: &Palette,
    id: impl Into<ElementId>,
    label: String,
    again: &'static str,
    confirming: bool,
) -> Stateful<Div> {
    if confirming { link(id, again, p.error) } else { link(id, label, p.theme.secondary) }
}

/// 能点的文字。
fn link(id: impl Into<ElementId>, label: impl Into<SharedString>, color: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .text_size(px(12.0))
        .text_color(color)
        .cursor_pointer()
        .hover(|style| style.underline())
        .child(label.into())
}

/// 用默认的 App 打开文本文件。macOS 上这种扩展名没设默认 App（.toml、.yaml 常常没有）时，改用文本编辑打开。
fn open_text(path: PathBuf, cx: &App) {
    #[cfg(target_os = "macos")]
    {
        let _ = cx;
        std::thread::spawn(move || {
            let open = |args: &[&std::ffi::OsStr]| {
                std::process::Command::new("open").args(args).status().is_ok_and(|status| status.success())
            };
            if !open(&[path.as_os_str()]) {
                open(&["-t".as_ref(), path.as_os_str()]);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    cx.open_with_system(&path);
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
