//! 设置窗口里的「自定义词库」页：上面加词，下面是加过的词和输入法会用的读音，可以删。读音由输入法定，
//! 读错的可以在词后面跟上拼音（`曾小贤 zengxiaoxian`）。词存在本地词库目录的 custom.dict.yaml 里，
//! 输入法切换输入框时读到。

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::*;
use gpui::{App, ClickEvent, Context, Div, Entity, Hsla, Render, Stateful, Window, div, px};
use kyme_core::pinyin::Engine;
use kyme_core::{LocalDicts, LocalWord, Rgb};

use super::{MARGIN, Palette, card, field_colors, link, note, open_text, row};
use crate::input::{Submit, TextField};

/// 词那一列的宽度。
const WORD_WIDTH: f32 = 150.0;

pub(super) struct CustomWordsView {
    dicts: LocalDicts,
    words: Vec<LocalWord>,
    /// 每个词输入法会用的读音，和 `words` 一一对应，定不出来的是 None。主词库还没读好时是空的。
    readings: Vec<Option<String>>,
    /// 主词库，定读音用。打开这一页时在后台读；读不了（比如不在安装包里跑）就一直是 None。
    engine: Option<Arc<Engine>>,
    word: Entity<TextField>,
    /// 加词、删词出错时的说明。
    error: Option<String>,
    /// 设置里的高亮色。
    pub(super) accent: Rgb,
}

impl CustomWordsView {
    /// 词由调用方接着 `reload` 读进来。
    pub(super) fn new(dicts: LocalDicts, dict_dir: Option<PathBuf>, accent: Rgb, cx: &mut Context<Self>) -> Self {
        let word = cx.new(|cx| TextField::new("加个词，比如：曾小贤 zengxiaoxian", cx));
        if let Some(dir) = dict_dir {
            cx.spawn(async move |view, cx| {
                let engine = cx.background_executor().spawn(async move { Engine::load_dir(&dir).ok() }).await;
                let _ = view.update(cx, |view, cx| {
                    view.engine = engine.map(Arc::new);
                    view.update_readings(cx);
                });
            })
            .detach();
        }
        CustomWordsView { dicts, words: Vec::new(), readings: Vec::new(), engine: None, word, error: None, accent }
    }

    /// 重读自定义词库：可能刚在编辑器里改过。
    pub(super) fn reload(&mut self, cx: &mut Context<Self>) {
        self.words = self.dicts.custom_words();
        self.update_readings(cx);
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &App) {
        self.word.read(cx).focus(window);
    }

    /// 在后台定每个词的读音，定好了再显示。补读音要给主词库建一次索引，几十毫秒。
    fn update_readings(&mut self, cx: &mut Context<Self>) {
        self.readings.clear();
        cx.notify();
        let Some(engine) = self.engine.clone() else { return };
        let words = self.words.clone();
        cx.spawn(async move |view, cx| {
            let (words, readings) = cx
                .background_executor()
                .spawn(async move {
                    let pairs: Vec<(&str, &str)> =
                        words.iter().map(|word| (word.text.as_str(), word.pinyin.as_str())).collect();
                    let readings = engine.local_readings(&pairs);
                    (words, readings)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                // 这期间又加了、删了词的话，等下一次的结果
                if view.words == words {
                    view.readings = readings;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.word.read(cx).text().to_string();
        match self.dicts.add_custom(&input) {
            Ok(_) => {
                self.error = None;
                self.word.update(cx, |field, cx| field.clear(cx));
                self.focus(window, cx);
                self.reload(cx);
            }
            Err(err) => {
                self.error = Some(err);
                cx.notify();
            }
        }
    }

    fn remove(&mut self, word: &LocalWord, cx: &mut Context<Self>) {
        self.error = self.dicts.remove_custom(word).err().map(|err| format!("删不了「{}」：{err}", word.text));
        self.reload(cx);
    }

    /// 列表里的一行：词、输入法会用的读音、删除。
    fn word_row(&self, p: &Palette, index: usize, first: bool, cx: &mut Context<Self>) -> Div {
        let word = &self.words[index];
        let (reading, color): (String, Hsla) = match self.readings.get(index) {
            Some(Some(reading)) => (reading.clone(), p.theme.secondary),
            Some(None) => ("读不出来".into(), p.error),
            // 主词库还没读好：写了拼音的先照写的显示
            None => (word.pinyin.clone(), p.theme.secondary),
        };
        let removed = word.clone();
        let remove = link(("remove-word", index), "删除", p.theme.secondary)
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| view.remove(&removed, cx)));
        let label = div()
            .flex()
            .gap(px(12.0))
            .child(div().w(px(WORD_WIDTH)).flex_none().truncate().child(word.text.clone()))
            .child(div().flex_1().min_w(px(0.0)).truncate().text_size(px(12.0)).text_color(color).child(reading));
        row(p, first, label, remove)
    }
}

impl Render for CustomWordsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::new(window.appearance(), self.accent);
        let colors = field_colors(&p);
        self.word.update(cx, |field, _| field.colors = colors);
        let add =
            button(&p, "add", "添加").on_click(cx.listener(|view, _: &ClickEvent, window, cx| view.add(window, cx)));
        let error = self.error.clone().map(|err| note(&p, err).text_color(p.error));
        // 新加的在最上面
        let rows: Vec<Div> = (0..self.words.len())
            .rev()
            .map(|index| self.word_row(&p, index, index + 1 == self.words.len(), cx))
            .collect();
        let list = if rows.is_empty() {
            card(&p).flex_1().items_center().justify_center().child(note(&p, "还没有加过词"))
        } else {
            card(&p).flex_1().min_h(px(0.0)).child(div().id("words").size_full().overflow_y_scroll().children(rows))
        };
        // 文件还不在就先建一份带说明的
        let open = link("open-custom", "用文本编辑器打开", p.theme.accent).on_click(cx.listener(
            |view, _: &ClickEvent, _, cx| match view.dicts.custom() {
                Ok(path) => open_text(path, cx),
                Err(err) => {
                    view.error = Some(format!("建不了自定义词库的文件：{err}"));
                    cx.notify();
                }
            },
        ));
        div()
            // 在输入框里按回车也是加词：输入框不处理回车，交到这里
            .on_action(cx.listener(|view, _: &Submit, window, cx| view.add(window, cx)))
            .size_full()
            .px(px(MARGIN))
            .pb(px(16.0))
            .flex()
            .flex_col()
            .child(div().flex().items_center().gap(px(8.0)).child(div().flex_1().child(self.word.clone())).child(add))
            .child(div().min_h(px(16.0)).py(px(8.0)).px(px(4.0)).children(error))
            .child(list)
            .child(
                div()
                    .h(px(32.0))
                    .px(px(4.0))
                    .flex()
                    .items_end()
                    .justify_between()
                    .child(open)
                    .child(note(&p, format!("{} 个词", self.words.len()))),
            )
    }
}

/// 实心的按钮。
fn button(p: &Palette, id: &'static str, label: &'static str) -> Stateful<Div> {
    let hover = p.theme.accent.opacity(0.85);
    div()
        .id(id)
        .h(px(28.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .rounded(px(6.0))
        .bg(p.theme.accent)
        .text_color(p.theme.on_accent)
        .text_size(px(13.0))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .child(label)
}
