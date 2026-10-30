//! 设置窗口：左边是导航栏，右边是一页设置。「输入」「外观」两页上面是候选窗的预览；「自定义词库」加词、删词
//! （custom.rs）；「外部词库」下载、载入扩展词库，导入别家的词库；「AI」选服务商、填 Key。点一下就写进设置文件，文件里的注释照旧；
//! AI 的 Key、模型和地址写进旁边只有自己能读写的 ai.toml。输入法切换输入框时读到新设置。

mod custom;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, Deferred, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla,
    MouseButton, MouseDownEvent, PathPromptOptions, Pixels, Render, SharedString, Size, Stateful, Subscription, Window,
    WindowAppearance, anchored, deferred, div, font, point, px, rgb, rgba, size, transparent_black,
};
use kyme_core::pinyin::{Dictionary, Emoji, Engine};
use kyme_core::{
    AI_PAUSES, AiFile, AiProvider, AiService, AiServices, EXTRA_DICTS, ExtraDir, FONT_SIZES, Imported, Key, LocalDicts,
    Modifiers, PAGE_SIZES, Rgb, Session, Settings, SettingsFile, UserDictFile, View,
};

use self::custom::CustomWordsView;
use crate::input::{Edited, FieldColors, HEIGHT as FIELD_HEIGHT, TextField};
use crate::{CandidateView, Content, Metrics, Theme};

/// 窗口大小固定，右边那一页放不下时滚动。
const WIDTH: f32 = 720.0;
const HEIGHT: f32 = 540.0;
const SIDEBAR: f32 = 180.0;
/// 标题栏那一条，红绿灯在里面，导航栏从它下面开始。
const TITLEBAR: f32 = 32.0;
/// 右边一页的左右留白。
const MARGIN: f32 = 20.0;
/// 预览区的内边距。
const STAGE_PAD: f32 = 16.0;
/// 预览里模拟的输入框那一行。
const INPUT_LINE: f32 = 20.0;
/// 模拟的输入框和候选窗之间的距离。
const STAGE_GAP: f32 = 8.0;
const ROW: f32 = 40.0;
/// 行里输入框、下拉框的宽度。
const FIELD_WIDTH: f32 = 280.0;
/// 展开的选项和下拉框之间的空隙。
const DROPDOWN_GAP: f32 = 4.0;
/// 模型下拉框最后一项：选了它，下面多一行自己填模型名。
const OTHER_MODEL: &str = "其他…";

/// 预览打的拼音和用的小词库：打 nihao 时第一页会出现的词，按词频从高到低。
const PREVIEW_INPUT: &str = "nihao";
const PREVIEW_DICT: &str = "你好\tni hao\t1000\n你\tni\t900\n拟\tni\t800\n泥\tni\t700\n尼\tni\t600\n\
                            妮\tni\t500\n倪\tni\t400\n霓\tni\t300\n逆\tni\t200\n腻\tni\t100\n";
const PREVIEW_EMOJI: &str = "你好\t你好 👋\n";

/// 「测试连接」问的例子：看前文是「美食」，引擎的首选是「没事」。例子是定死的，不发打过的字。
const CHECK_CONTEXT: &str = "周末想去夜市吃点";
const CHECK_PINYIN: &str = "meishi";
const CHECK_CANDIDATES: [&str; 3] = ["没事", "美食", "美式"];

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

/// 导航栏里的一页。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Input,
    Appearance,
    Custom,
    Imported,
    Ai,
}

impl Page {
    const ALL: [Page; 5] = [Page::Input, Page::Appearance, Page::Custom, Page::Imported, Page::Ai];

    fn title(self) -> &'static str {
        match self {
            Page::Input => "输入",
            Page::Appearance => "外观",
            Page::Custom => "自定义词库",
            Page::Imported => "外部词库",
            Page::Ai => "AI",
        }
    }
}

/// 「AI」页的三个输入框。
#[derive(Clone, Copy, PartialEq, Eq)]
enum AiField {
    Model,
    Endpoint,
    Key,
}

impl AiField {
    const ALL: [AiField; 3] = [AiField::Model, AiField::Endpoint, AiField::Key];

    fn of(self, service: &mut AiService) -> &mut String {
        match self {
            AiField::Model => &mut service.model,
            AiField::Endpoint => &mut service.endpoint,
            AiField::Key => &mut service.key,
        }
    }
}

/// 「AI」页的两个下拉框。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Provider,
    Model,
}

/// 「测试连接」的进度。在查的那一次记着编号，回来的结果对得上才算数。
enum Check {
    Running(u64),
    Done(Result<(), String>),
}

/// 要点两次才做的事，点过第一次的是哪一个。
#[derive(PartialEq)]
enum Confirm {
    /// 清除学到的词。
    Clear,
    /// 删掉这个导入的外部词库。
    Remove(String),
    /// 删掉下载的这个扩展词库（`EXTRA_DICTS` 里的下标）。
    RemoveExtra(usize),
}

/// 正在下载的扩展词库和进度：后台线程边下边写，界面每隔一会儿读一次。
struct Download {
    /// `EXTRA_DICTS` 里的下标。
    dict: usize,
    done: AtomicU64,
    /// 总共多少字节，服务器没说是 0。
    total: AtomicU64,
}

impl Download {
    /// 「正在下载 45%」，不知道总共多少时显示下了几 MB。
    fn label(&self) -> String {
        let (done, total) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        match total {
            0 => format!("正在下载 {:.1}MB", done as f64 / 1e6),
            total => format!("正在下载 {}%", done * 100 / total),
        }
    }
}

pub struct SettingsView {
    file: SettingsFile,
    /// 本地词库的目录。外部词库的词数在这里记着，没改过的不再数。
    dicts: LocalDicts,
    settings: Settings,
    /// 设置文件、ai.toml 读不了、写不进去时的说明。
    error: Option<String>,
    default_font: SharedString,
    /// 这台机器上有的字体选项。
    fonts: Vec<(&'static str, &'static str)>,
    /// 预览用的引擎，只有 PREVIEW_DICT 里的几个词。
    engine: Arc<Engine>,
    /// 预览的输入框里显示的内容。
    marked: String,
    preview: Entity<CandidateView>,
    /// 正显示着哪一页。
    page: Page,
    /// 用户词库里有几个词。
    learned_words: usize,
    /// 点过一次、等着确认的操作。
    confirm: Option<Confirm>,
    /// 安装包里的词库目录，自定义词库页定读音用。
    dict_dir: Option<PathBuf>,
    /// 「自定义词库」页，第一次点进去时建，之后留着：主词库不用再读一遍。
    custom: Option<Entity<CustomWordsView>>,
    /// 导入过的词库。
    imported: Vec<Imported>,
    /// 正在导入，先不让再点。
    importing: bool,
    /// 正在下载的扩展词库，一次只下一个。
    download: Option<Arc<Download>>,
    ai_file: AiFile,
    ai_services: AiServices,
    /// 「AI」页的输入框里正显示着哪一家的设置。
    ai_shown: Option<AiProvider>,
    /// 模型、接口地址、Key 三个输入框，顺序同 `AiField::ALL`。
    ai_fields: [Entity<TextField>; 3],
    /// 正展开着的下拉框。
    menu: Option<Menu>,
    /// 模型选了「其他…」，自己填模型名。
    other_model: bool,
    /// 「测试连接」的进度和结果。
    ai_check: Option<Check>,
    /// 点了几次「测试连接」，给每一次编号。
    checks: u64,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// `default_font`：设置里没指定字体时用的平台默认中文字体，界面文字也用它。
    /// `dict_dir`：安装包里的词库目录，自定义词库页用它给没写拼音的词定读音。
    pub fn new(
        file: SettingsFile,
        default_font: impl Into<SharedString>,
        dict_dir: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let default_font = default_font.into();
        let dicts = LocalDicts::beside(file.path());
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
        let ai_fields = AiField::ALL.map(|field| {
            cx.new(|cx| {
                let mut text = TextField::new("", cx);
                text.secret = field == AiField::Key;
                text
            })
        });
        let mut subscriptions = vec![
            // 回到这个窗口时重读一次：可能刚在编辑器里改过设置文件
            cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.reload(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, _, cx| cx.notify()),
        ];
        for (field, entity) in AiField::ALL.into_iter().zip(&ai_fields) {
            subscriptions.push(cx.subscribe(entity, move |view, _, _: &Edited, cx| view.ai_edited(field, cx)));
        }
        // 拿着焦点，⌘W、⌘Q 这些快捷键才有地方派
        let focus = cx.focus_handle();
        focus.focus(window);
        let mut view = SettingsView {
            ai_file: AiFile::beside(file.path()),
            file,
            dicts,
            settings: Settings::default(),
            error: None,
            default_font,
            fonts,
            engine: Arc::new(engine),
            marked: String::new(),
            preview,
            page: Page::Input,
            learned_words: 0,
            confirm: None,
            dict_dir,
            custom: None,
            imported: Vec::new(),
            importing: false,
            download: None,
            ai_services: AiServices::default(),
            ai_shown: None,
            ai_fields,
            menu: None,
            other_model: false,
            ai_check: None,
            checks: 0,
            focus,
            _subscriptions: subscriptions,
        };
        view.reload(cx);
        view
    }

    /// 窗口的大小，固定不变。不超过 `max_height`（屏幕扣掉菜单栏和 Dock 的高度），放不下时右边那一页滚动。
    pub fn window_size(max_height: Pixels) -> Size<Pixels> {
        size(px(WIDTH), px(HEIGHT).min(max_height))
    }

    /// 设置文件、ai.toml 改过（或者还没读过）就重读。读不了时和输入法一样用默认设置，并显示原因。
    /// 用户词库是输入法写的、本地词库可能刚在编辑器里改过，每次都重数一遍。
    fn reload(&mut self, cx: &mut Context<Self>) {
        self.learned_words = self.userdict().count();
        self.imported = self.dicts.imported();
        if let Some(result) = self.file.reload() {
            (self.settings, self.error) = match result {
                Ok(settings) => (settings, None),
                Err(err) => (Settings::default(), Some(err)),
            };
        }
        match self.ai_file.reload() {
            Some(Ok(services)) => {
                self.ai_services = services;
                // 输入框照着重读的内容重填
                self.ai_shown = None;
            }
            Some(Err(err)) => self.error = Some(err),
            None => {}
        }
        self.refresh(cx);
    }

    /// 改一项设置并写进文件。写不进去就保持原样，显示原因。
    fn change(&mut self, edit: &dyn Fn(&mut Settings), cx: &mut Context<Self>) {
        // 文件可能刚在别处改过，以文件里的为准再改
        self.reload(cx);
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
        self.refresh(cx);
    }

    /// 预览用真的输入会话打一遍 nihao，和打字时看到的一样。换了服务商的话，AI 的输入框换成那一家的。
    fn refresh(&mut self, cx: &mut Context<Self>) {
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
        // 正开着自定义词库页时重读：可能刚在编辑器里改过。没开着的话，点进去时会重读
        if let Some(custom) = &self.custom {
            let (accent, showing) = (self.settings.appearance.highlight_color, self.page == Page::Custom);
            custom.update(cx, |custom, cx| {
                custom.accent = accent;
                if showing {
                    custom.reload(cx);
                }
            });
        }
        let provider = self.settings.ai.provider;
        if self.ai_shown != Some(provider) {
            self.show_ai_fields(provider, cx);
        }
        cx.notify();
    }

    /// 换一页。回到窗口里的焦点；自定义词库页把光标放进「词」的输入框。在别处改过的，回到窗口时就重读了，这里不再读。
    fn open(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.page = page;
        self.confirm = None;
        if page == Page::Custom {
            self.open_custom(window, cx);
        } else {
            self.focus.focus(window);
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
            .flex_none()
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

    /// 下载的扩展词库存在哪。
    fn extra(&self) -> ExtraDir {
        ExtraDir::beside(self.file.path())
    }

    /// 「选词记忆」一行的右边：学了几个词、清除，和开关。清除要点两次，免得手滑。
    fn learn_control(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let clear = (self.learned_words > 0).then(|| {
            let label = format!("{} 个词 · 清除", self.learned_words);
            confirm_link(p, "clear-learned", label, "确认清除", self.confirm == Some(Confirm::Clear))
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
                Err(err) => self.error = Some(format!("无法清除选词记忆：{err}")),
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

    /// 打开「自定义词库」页：第一次建好，之后重读，光标放进「词」的输入框。
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
                self.custom = Some(custom.clone());
                custom
            }
        };
        custom.update(cx, |custom, cx| {
            custom.reload(cx);
            custom.focus(window, cx);
        });
    }

    /// 「外部词库」页「导入」一行的右边：选几个文件，在后台转好存进本地词库。
    fn import_control(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        if self.importing {
            return div().id("import").child(note(p, "正在导入…"));
        }
        link("import", "导入…", p.theme.accent).on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.import(cx)))
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("导入".into()),
        });
        let dicts = LocalDicts::beside(self.file.path());
        cx.spawn(async move |view, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let _ = view.update(cx, |view, cx| {
                view.importing = true;
                cx.notify();
            });
            let results = cx
                .background_executor()
                .spawn(async move { paths.iter().map(|path| dicts.import(path)).collect::<Vec<_>>() })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.importing = false;
                view.error = results.into_iter().find_map(Result::err);
                view.reload(cx);
            });
        })
        .detach();
    }

    /// 扩展词库那一行的右边：没下载是「下载」，下载中显示进度，下载了是删除和开关。删除要点两次，免得手滑。
    fn extra_control(&self, i: usize, p: &Palette, cx: &mut Context<Self>) -> Div {
        let dict = &EXTRA_DICTS[i];
        let control = div().flex().items_center().gap(px(12.0));
        if let Some(download) = &self.download
            && download.dict == i
        {
            return control.child(note(p, download.label()));
        }
        if !self.extra().has(dict) {
            // 一次只下一个，别的先等着
            let download = match self.download {
                Some(_) => link(("download", i), "下载", p.theme.secondary),
                None => link(("download", i), "下载", p.theme.accent)
                    .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.download(i, cx))),
            };
            return control.child(download);
        }
        let confirming = self.confirm == Some(Confirm::RemoveExtra(i));
        let remove = confirm_link(p, ("remove-extra", i), "删除".into(), "确认删除", confirming)
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.remove_extra(i, cx)));
        let on = (dict.enabled)(&self.settings.dicts);
        let toggle =
            switch(p, dict.file(), on).on_click(on_change(cx, move |s| (EXTRA_DICTS[i].set)(&mut s.dicts, !on)));
        control.child(remove).child(toggle)
    }

    /// 下载第 `i` 个扩展词库：在后台下好、核对、存起来，然后打开它的开关。下载中每 0.2 秒刷新一次进度。
    fn download(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.download.is_some() {
            return;
        }
        let download = Arc::new(Download { dict: i, done: AtomicU64::new(0), total: AtomicU64::new(0) });
        self.download = Some(download.clone());
        self.error = None;
        cx.notify();
        let extra = self.extra();
        cx.spawn(async move |view, cx| {
            // 下载期间每 0.2 秒刷新一次进度，下完把这个任务丢掉就停了
            let refresh = cx.spawn({
                let view = view.clone();
                async move |cx| {
                    while view.update(cx, |_, cx| cx.notify()).is_ok() {
                        cx.background_executor().timer(Duration::from_millis(200)).await;
                    }
                }
            });
            let dict = &EXTRA_DICTS[i];
            let result = cx
                .background_executor()
                .spawn(async move {
                    kyme_ai::download(&dict.urls(), |done, total| {
                        download.done.store(done, Ordering::Relaxed);
                        download.total.store(total.unwrap_or(0), Ordering::Relaxed);
                    })
                    .and_then(|bytes| extra.save(dict, &bytes))
                })
                .await;
            drop(refresh);
            let _ = view.update(cx, |view, cx| {
                view.download = None;
                match result {
                    Ok(()) => view.change(&|s: &mut Settings| (dict.set)(&mut s.dicts, true), cx),
                    Err(err) => {
                        view.error = Some(format!("「{}」下载失败：{err}", dict.name));
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// 删掉下载的扩展词库，同时关掉它的开关：输入法下次切换输入框时就不再载入。
    fn remove_extra(&mut self, i: usize, cx: &mut Context<Self>) {
        if self.confirmed(Confirm::RemoveExtra(i)) {
            let dict = &EXTRA_DICTS[i];
            match self.extra().remove(dict) {
                Ok(()) => return self.change(&|s: &mut Settings| (dict.set)(&mut s.dicts, false), cx),
                Err(err) => self.error = Some(format!("无法删除「{}」：{err}", dict.name)),
            }
        }
        cx.notify();
    }

    /// 一个导入的词库那一行的右边：有几个词、删除。删除要点两次，免得手滑。
    fn remove_control(&self, dict: &Imported, p: &Palette, cx: &mut Context<Self>) -> Div {
        let name = dict.name.clone();
        let confirming = self.confirm == Some(Confirm::Remove(name.clone()));
        let id = ElementId::Name(format!("remove-{name}").into());
        let remove = confirm_link(p, id, "删除".into(), "确认删除", confirming).on_click(cx.listener(
            move |view, _: &ClickEvent, _, cx| {
                if view.confirmed(Confirm::Remove(name.clone())) {
                    if let Err(err) = view.dicts.remove(&name) {
                        view.error = Some(format!("无法删除「{name}」：{err}"));
                    }
                    view.reload(cx);
                } else {
                    cx.notify();
                }
            },
        ));
        div().flex().items_center().gap(px(12.0)).child(note(p, format!("{} 个词", dict.words))).child(remove)
    }

    /// AI 的输入框换成这一家填过的内容。填过的模型不在这一家的选项里，就是选了「其他…」。
    fn show_ai_fields(&mut self, provider: AiProvider, cx: &mut Context<Self>) {
        let mut service = self.ai_services.get(provider).clone();
        let preset = kyme_ai::preset(provider);
        self.other_model = !service.model.is_empty() && !preset.models.contains(&service.model.as_str());
        let placeholder = |field| match field {
            AiField::Model => "模型名称",
            AiField::Endpoint => "http://localhost:11434/v1",
            AiField::Key if preset.key_optional() => "选填",
            AiField::Key => "",
        };
        for (field, entity) in AiField::ALL.into_iter().zip(&self.ai_fields) {
            let text = std::mem::take(field.of(&mut service));
            entity.update(cx, |entity, cx| {
                entity.set_text(&text, cx);
                entity.set_placeholder(placeholder(field), cx);
            });
        }
        self.ai_shown = Some(provider);
        self.menu = None;
        self.ai_check = None;
    }

    /// 改了模型名、地址或 Key：记到正显示着的那一家，写进 ai.toml。
    fn ai_edited(&mut self, field: AiField, cx: &mut Context<Self>) {
        let provider = self.settings.ai.provider;
        let text = self.ai_fields[field as usize].read(cx).text().to_string();
        *field.of(self.ai_services.get_mut(provider)) = text;
        self.save_ai(cx);
    }

    /// 各家的设置写进 ai.toml。填的变了，上次检查的结果不算数，还没回来的那次也作废。
    fn save_ai(&mut self, cx: &mut Context<Self>) {
        self.error = self.ai_file.save(&self.ai_services).err();
        self.ai_check = None;
        cx.notify();
    }

    /// 在下拉框里选了第 `index` 项。
    fn pick_option(&mut self, menu: Menu, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match menu {
            Menu::Provider => {
                let provider = AiProvider::ALL[index];
                self.change(&move |s| s.ai.provider = provider, cx);
            }
            Menu::Model => self.pick_model(index, window, cx),
        }
        cx.notify();
    }

    /// 选了这一家的第 `index` 个模型；过了最后一个是「其他…」，光标放进下面填模型名的那一行。
    fn pick_model(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let provider = self.settings.ai.provider;
        let models = kyme_ai::preset(provider).models;
        let field = &self.ai_fields[AiField::Model as usize];
        match models.get(index) {
            Some(model) => {
                self.other_model = false;
                field.update(cx, |field, cx| field.set_text(model, cx));
                self.ai_services.get_mut(provider).model = model.to_string();
            }
            None => {
                self.other_model = true;
                // 原来选的是列出来的模型：清空，等着填
                if models.contains(&self.ai_services.get(provider).model.as_str()) {
                    field.update(cx, |field, cx| field.clear(cx));
                    self.ai_services.get_mut(provider).model.clear();
                }
                field.read(cx).focus(window);
            }
        }
        self.save_ai(cx);
    }

    /// 用定死的例子问一次正显示着的那一家，看填的能不能用。
    fn check_ai(&mut self, cx: &mut Context<Self>) {
        let provider = self.settings.ai.provider;
        let service = self.ai_services.get(provider).clone();
        self.checks += 1;
        let id = self.checks;
        self.ai_check = Some(Check::Running(id));
        cx.notify();
        cx.spawn(async move |view, cx| {
            let result = cx.background_executor().spawn(async move { check(provider, &service) }).await;
            let _ = view.update(cx, |view, cx| {
                // 这期间改了填的、换了服务商、又点了一次，这次的结果都不算数
                if matches!(view.ai_check, Some(Check::Running(running)) if running == id) {
                    view.ai_check = Some(Check::Done(result));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn page_picker(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let current = self.settings.input.page_size;
        segmented(p).children(PAGE_SIZES.map(|n| {
            let selected = n == current;
            segment(p, ("page-size", n), selected)
                .child(n.to_string())
                .when(!selected, |segment| segment.on_click(on_change(cx, move |s| s.input.page_size = n)))
        }))
    }

    fn pause_picker(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let current = self.settings.ai.pause_ms;
        segmented(p).children(AI_PAUSES.map(|ms| {
            let selected = ms == current;
            segment(p, ("ai-pause", ms as usize), selected)
                .child(format!("{} 秒", ms as f64 / 1000.0))
                .when(!selected, |segment| segment.on_click(on_change(cx, move |s| s.ai.pause_ms = ms)))
        }))
    }

    /// 下拉框：显示选中的那一项，点开是一列选项，打勾的是选中的。框的样子和输入框一样。
    fn dropdown(&self, p: &Palette, menu: Menu, options: Vec<(SharedString, bool)>, cx: &mut Context<Self>) -> Div {
        let open = self.menu == Some(menu);
        let current =
            options.iter().find(|(_, selected)| *selected).map(|(label, _)| label.clone()).unwrap_or_default();
        let colors = field_colors(p);
        let button = div()
            .id(("dropdown", menu as usize))
            .w(px(FIELD_WIDTH))
            .h(px(FIELD_HEIGHT))
            .px(px(10.0))
            .flex()
            .items_center()
            .justify_between()
            .rounded(px(6.0))
            .border_1()
            .border_color(if open { colors.accent } else { colors.border })
            .bg(colors.background)
            .text_size(px(13.0))
            .cursor_pointer()
            .child(div().min_w(px(0.0)).truncate().child(current))
            .child(div().text_size(px(9.0)).text_color(p.theme.secondary).child("▼"))
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                view.menu = Some(menu);
                cx.notify();
            }));
        let list = open.then(|| {
            let (hover, on_hover) = (p.theme.accent, p.theme.on_accent);
            let items: Vec<Stateful<Div>> = options
                .into_iter()
                .enumerate()
                .map(|(index, (label, selected))| {
                    div()
                        .id(("option", index))
                        .h(px(26.0))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .justify_between()
                        .rounded(px(5.0))
                        .text_size(px(13.0))
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover).text_color(on_hover))
                        .child(label)
                        .when(selected, |item| item.child("✓"))
                        .on_click(cx.listener(move |view, _: &ClickEvent, window, cx| {
                            view.pick_option(menu, index, window, cx)
                        }))
                })
                .collect();
            // 画在最上面，紧贴着下拉框的底边展开（浮层的起点就是下拉框的底边），碰到窗口边就往里收
            deferred(
                anchored().offset(point(px(0.0), px(DROPDOWN_GAP))).snap_to_window_with_margin(px(8.0)).child(
                    div()
                        .id(("menu", menu as usize))
                        .occlude()
                        .w(px(FIELD_WIDTH))
                        .max_h(px(300.0))
                        .overflow_y_scroll()
                        .p(px(4.0))
                        .rounded(px(8.0))
                        .border_1()
                        .border_color(p.line)
                        .bg(p.card)
                        .shadow(shadow(p.shadow, 4.0, 16.0))
                        .children(items),
                ),
            )
            .with_priority(2)
        });
        div().flex_none().child(button).children(list)
    }

    /// 下拉框展开时盖住整个窗口的一层：点别处就收起来，点到的东西不受影响。
    fn backdrop(&self, window: &Window, cx: &mut Context<Self>) -> Deferred {
        let viewport = window.viewport_size();
        deferred(anchored().position(point(px(0.0), px(0.0))).child(
            div().id("backdrop").w(viewport.width).h(viewport.height).occlude().on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _: &MouseDownEvent, _, cx| {
                    view.menu = None;
                    cx.notify();
                }),
            ),
        ))
        .with_priority(1)
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

    /// 左边的导航栏：几页设置，最下面是打开设置文件和版本号。
    fn sidebar(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let items: Vec<Stateful<Div>> = Page::ALL
            .into_iter()
            .map(|page| {
                let hover = p.hover;
                div()
                    .id(("nav", page as usize))
                    .h(px(28.0))
                    .px(px(10.0))
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .text_size(px(13.0))
                    .map(|item| {
                        if page == self.page {
                            item.bg(p.theme.accent).text_color(p.theme.on_accent)
                        } else {
                            item.cursor_pointer().hover(move |style| style.bg(hover)).on_click(
                                cx.listener(move |view, _: &ClickEvent, window, cx| view.open(page, window, cx)),
                            )
                        }
                    })
                    .child(page.title())
            })
            .collect();
        let path = self.file.path().to_path_buf();
        div()
            .w(px(SIDEBAR))
            .flex_none()
            .h_full()
            .pt(px(TITLEBAR + 12.0))
            .px(px(10.0))
            .pb(px(16.0))
            .flex()
            .flex_col()
            .gap(px(2.0))
            .bg(p.sidebar)
            .border_r_1()
            .border_color(p.line)
            .children(items)
            .child(div().flex_1())
            .child(
                div()
                    .px(px(10.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        link("open-file", "打开设置文件", p.theme.accent)
                            .on_click(move |_, _, cx| open_text(path.clone(), cx)),
                    )
                    .child(note(p, concat!("Kyme ", env!("CARGO_PKG_VERSION")))),
            )
    }

    /// 右边一页的标题，出错时下面一行红字说明。
    fn header(&self, p: &Palette) -> Div {
        div()
            .flex_none()
            .px(px(MARGIN))
            .pt(px(16.0))
            .pb(px(12.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(div().text_size(px(17.0)).font_weight(FontWeight::SEMIBOLD).child(self.page.title()))
            .children(self.error.clone().map(|err| note(p, err).text_color(p.error)))
    }

    fn input_page(&self, p: &Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let input = &self.settings.input;
        let rows = [
            row(p, true, "每页候选数", self.page_picker(p, cx)),
            row(
                p,
                false,
                "输入框内显示拼音",
                switch(p, "inline", input.inline_pinyin)
                    .on_click(on_change(cx, |s| s.input.inline_pinyin = !s.input.inline_pinyin)),
            ),
            row(
                p,
                false,
                "显示 Emoji 候选",
                switch(p, "emoji", input.emoji).on_click(on_change(cx, |s| s.input.emoji = !s.input.emoji)),
            ),
            row(p, false, "选词记忆", self.learn_control(p, cx)),
        ];
        scrolling(Page::Input, [self.stage(p, window, cx), card(p).children(rows)])
    }

    fn appearance_page(&self, p: &Palette, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = [
            row(p, true, "字号", self.stepper(p, cx)),
            row(p, false, "字体", self.font_picker(p, cx)),
            row(p, false, "高亮色", self.color_picker(cx)),
        ];
        scrolling(Page::Appearance, [self.stage(p, window, cx), card(p).children(rows)])
    }

    fn imported_page(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let extra = card(p).children(
            EXTRA_DICTS
                .iter()
                .enumerate()
                .map(|(i, dict)| row(p, i == 0, described(p, dict.name, dict.detail), self.extra_control(i, p, cx))),
        );
        let import = card(p).child(row(
            p,
            true,
            described(p, "导入词库", ".scel、.dict.yaml、文本词表"),
            self.import_control(p, cx),
        ));
        let listed: Vec<Div> = self
            .imported
            .iter()
            .enumerate()
            .map(|(i, dict)| row(p, i == 0, dict.name.clone(), self.remove_control(dict, p, cx)))
            .collect();
        let list =
            if listed.is_empty() { note(p, "暂无导入的词库").px(px(4.0)) } else { card(p).children(listed) };
        scrolling(Page::Imported, [extra, import, list])
    }

    fn ai_page(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let ai = &self.settings.ai;
        let toggle = card(p)
            .child(row(
                p,
                true,
                "AI 预测选词",
                switch(p, "ai", ai.enabled).on_click(on_change(cx, |s| s.ai.enabled = !s.ai.enabled)),
            ))
            .child(row(p, false, "预测延迟", self.pause_picker(p, cx)));
        let provider = ai.provider;
        let providers = AiProvider::ALL.map(|option| (option.name().into(), option == provider)).to_vec();
        let field = |field: AiField| div().w(px(FIELD_WIDTH)).child(self.ai_fields[field as usize].clone());
        let mut rows = vec![row(p, true, "服务商", self.dropdown(p, Menu::Provider, providers, cx))];
        let preset = kyme_ai::preset(provider);
        match preset.endpoint {
            // 列出来的这几家：地址定死，模型从下拉框里选
            Some(endpoint) => {
                let model = preset.model(&self.ai_services.get(provider).model).unwrap_or_default();
                let mut options: Vec<(SharedString, bool)> = preset
                    .models
                    .iter()
                    .map(|option| (SharedString::from(*option), !self.other_model && *option == model))
                    .collect();
                options.push((OTHER_MODEL.into(), self.other_model));
                rows.push(row(p, false, "接口地址", note(p, endpoint)));
                rows.push(row(p, false, "模型", self.dropdown(p, Menu::Model, options, cx)));
                if self.other_model {
                    rows.push(row(p, false, "模型名称", field(AiField::Model)));
                }
            }
            // 自定义：地址和模型都自己填
            None => {
                rows.push(row(p, false, "接口地址", field(AiField::Endpoint)));
                rows.push(row(p, false, "模型", field(AiField::Model)));
            }
        }
        rows.push(row(p, false, "API Key", field(AiField::Key)));
        let result = self.ai_check.as_ref().map(|check| match check {
            Check::Running(_) => note(p, "正在测试…"),
            Check::Done(Ok(())) => note(p, "✓ 通过").text_color(p.success),
            Check::Done(Err(err)) => note(p, err.clone()).text_color(p.error),
        });
        let check = div()
            .px(px(4.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                link("check-ai", "测试连接", p.theme.accent)
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.check_ai(cx))),
            )
            .children(result);
        scrolling(Page::Ai, [toggle, card(p).children(rows), check])
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::new(window.appearance(), self.settings.appearance.highlight_color);
        let colors = field_colors(&p);
        for field in &self.ai_fields {
            field.update(cx, |field, _| field.colors = colors);
        }
        let page = match self.page {
            Page::Input => self.input_page(&p, window, cx),
            Page::Appearance => self.appearance_page(&p, window, cx),
            Page::Custom => match &self.custom {
                Some(custom) => div().flex_1().min_h(px(0.0)).child(custom.clone()).into_any_element(),
                None => div().into_any_element(),
            },
            Page::Imported => self.imported_page(&p, cx),
            Page::Ai => self.ai_page(&p, cx),
        };
        let backdrop = self.menu.is_some().then(|| self.backdrop(window, cx));
        div()
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .bg(p.window)
            .font_family(self.default_font.clone())
            .text_color(p.theme.primary)
            .child(self.sidebar(&p, cx))
            .child(div().flex_1().min_w(px(0.0)).h_full().flex().flex_col().child(self.header(&p)).child(page))
            .children(backdrop)
    }
}

/// 问一次这一家，看填的能不能用。只看请求通不通，不管它挑了哪句。
fn check(provider: AiProvider, service: &AiService) -> Result<(), String> {
    let client = kyme_ai::Client::new(provider, service)?;
    client.pick(CHECK_CONTEXT, CHECK_PINYIN, &CHECK_CANDIDATES).map(|_| ()).map_err(|err| err.to_string())
}

/// 点一下改一项设置。
fn on_change(
    cx: &mut Context<SettingsView>,
    edit: impl Fn(&mut Settings) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    cx.listener(move |view, _: &ClickEvent, _, cx| view.change(&edit, cx))
}

/// 右边的一页：从上往下排，放不下时滚动。每页各自记着滚到哪了。
fn scrolling(page: Page, children: impl IntoIterator<Item = Div>) -> AnyElement {
    div()
        .id(("page", page as usize))
        .flex_1()
        .min_h(px(0.0))
        .overflow_y_scroll()
        .child(div().px(px(MARGIN)).pb(px(20.0)).flex().flex_col().gap(px(16.0)).children(children))
        .into_any_element()
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

/// 要点两次的链接：点过一次后变成红字的「确认……」。
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

/// 输入框的颜色，跟着窗口的深浅色和高亮色。
fn field_colors(p: &Palette) -> FieldColors {
    FieldColors {
        text: p.theme.primary,
        placeholder: p.theme.secondary,
        accent: p.theme.accent,
        background: p.card,
        border: p.line,
    }
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
    /// 左边导航栏的底。
    sidebar: Hsla,
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
    success: Hsla,
    error: Hsla,
}

impl Palette {
    fn new(appearance: WindowAppearance, accent: Rgb) -> Self {
        let theme = Theme::new(appearance, accent);
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Palette {
                theme,
                window: rgb(0x1e1e1e).into(),
                sidebar: rgb(0x262628).into(),
                stage: rgb(0x111111).into(),
                card: rgb(0x2a2a2c).into(),
                line: rgba(0xffffff14).into(),
                track: rgba(0x7676803d).into(),
                thumb: rgb(0x636366).into(),
                hover: rgba(0xffffff0f).into(),
                switch_off: rgba(0x78788052).into(),
                shadow: rgba(0x00000066).into(),
                success: rgb(0x30d158).into(),
                error: rgb(0xff453a).into(),
            },
            WindowAppearance::Light | WindowAppearance::VibrantLight => Palette {
                theme,
                window: rgb(0xf5f5f7).into(),
                sidebar: rgb(0xebebef).into(),
                stage: rgb(0xe8e8ed).into(),
                card: rgb(0xffffff).into(),
                line: rgba(0x00000014).into(),
                track: rgba(0x7676801f).into(),
                thumb: rgb(0xffffff).into(),
                hover: rgba(0x0000000a).into(),
                switch_off: rgba(0x78788033).into(),
                shadow: rgba(0x00000026).into(),
                success: rgb(0x28a745).into(),
                error: rgb(0xff3b30).into(),
            },
        }
    }
}
