//! 输入法进程的全局状态。IMK 回调和 GPUI 都跑在主线程，状态放在 thread_local 里。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_graphics::event::KeyCode;
use gpui::{AsyncApp, Task};
use kyme_core::pinyin::{Dictionary, Engine};
use kyme_core::{
    AiFile, AiProvider, AiQuery, AiService, AiServices, ExtraDir, Key, LocalDicts, Mode, Modifiers, Panel, Pick,
    Response, Session, Settings, SettingsFile, UserDictFile, View, read_sources, today,
};
use kyme_ui::Content;
use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::NSRect;

use crate::client;
use crate::panel::PanelWindow;

/// 中英提示显示多久。
const HINT_DURATION: Duration = Duration::from_millis(1500);
/// 一个键处理超过这么久就写进日志。
const SLOW_KEY: Duration = Duration::from_millis(100);
/// 学到新词后停这么久再存盘，打字时不反复写文件。
const SAVE_DELAY: Duration = Duration::from_secs(10);
/// 光标前最多读这么多字当前文。
const CONTEXT_CHARS: usize = 64;
/// 小键盘上的回车，core-graphics 没有这个常量。
const KEYPAD_ENTER: u16 = 0x4c;
/// 不显示内嵌拼音、又还没选定任何字时，输入框里放的占位符（全角空格）。
/// IMK 客户端靠 marked text 判断是否正在组字：没有它，iTerm2 会把按键原样回显，
/// Chromium 会先把删除键交给网页、删掉前面的字。半角占位会让中文基线跳动。
const MARKED_PLACEHOLDER: &str = "\u{3000}";

thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// 主词库旁边附加的一层词库从哪来：本地词库（自定义的和导入的合成一层），或者一个扩展词库文件。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Source {
    Local,
    Extra(&'static str),
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Source::Local => f.write_str("本地词库"),
            Source::Extra(file) => write!(f, "扩展词库 {file} "),
        }
    }
}

pub struct AppState {
    session: Session,
    panel: PanelWindow,
    app: AsyncApp,
    settings: SettingsFile,
    /// 用户词库的文件。自测模式不读也不写。
    userdict: Option<UserDictFile>,
    /// 学到了新词，还没存盘。从无到有时安排一次存盘，这段时间里再学到的一起存。
    unsaved: bool,
    /// 本地词库的目录。自测模式不读。
    dicts: Option<LocalDicts>,
    /// 下载的扩展词库存在哪。自测模式不读。
    extra: Option<ExtraDir>,
    /// 主词库旁边附加的几层词库，按来源存着。哪层建好了、卸掉了，就照这里重新拼到引擎上。
    layers: BTreeMap<Source, Arc<Dictionary>>,
    /// 每层最近一次开始建的编号。建好时对不上，说明这期间又改过、又开始了一次，这次的作废。
    building: HashMap<Source, u64>,
    /// 开始建的次数，给每次编号。
    builds: u64,
    /// 最近一次按键所在的输入框。鼠标点候选窗时往这里上屏。
    client: Option<Retained<AnyObject>>,
    /// 客户端输入框里当前的 marked text，没变就不重复同步。
    marked: String,
    /// 最近一次按键时光标的位置。AI 的回答到了要重画候选窗时用它，不再去问 App。
    caret: NSRect,
    /// 每次显示中英提示加一，过期的隐藏任务据此作废。
    hint_generation: u64,
    /// AI 预测选词：设置里开着、ai.toml 里这一家的 Key 填好了才有。
    ai: Option<Arc<kyme_ai::Client>>,
    /// ai.toml：各家服务商的 Key、模型和接口地址。
    ai_file: AiFile,
    ai_services: AiServices,
    /// 现在的 AI 是照着哪一家的哪份设置建的，没建成（比如还没填 Key）也记着：设置没变就不重建，
    /// 不每次切输入框都试一遍。
    ai_for: Option<(AiProvider, AiService)>,
    /// 等着问 AI 的计时器。换上新的，旧的就取消了。
    ai_timer: Option<Task<()>>,
    /// 已经问过 AI 的那一次输入，同一次不重复问。
    ai_asked: u64,
    /// 这一段输入里读到的光标前的文字，读不到是空的。组字时它不会变，只读一次；输入结束就作废。
    ai_before: Option<String>,
    timings: Timings,
}

pub fn install(state: AppState) {
    STATE.with_borrow_mut(|slot| *slot = Some(state));
}

/// 取出全局状态执行 `f`。万一 IMK 回调重入（状态已被借用），就放弃处理、把按键交还给 App，
/// 而不是让输入法进程崩掉。
fn with_state<R>(fallback: R, f: impl FnOnce(&mut AppState) -> R) -> R {
    STATE.with(|cell| match cell.try_borrow_mut() {
        Ok(mut slot) => match slot.as_mut() {
            Some(state) => f(state),
            None => {
                log::warn!("输入法还没准备好，本次按键交还给 App");
                fallback
            }
        },
        Err(_) => {
            log::warn!("IMK 回调重入，本次按键交还给 App");
            fallback
        }
    })
}

pub fn handle_event(event: &NSEvent, client: &AnyObject) -> bool {
    with_state(false, |state| state.handle_event(event, client))
}

/// 每次切到一个输入框时调用。顺便检查设置文件、用户词库和本地词库有没有改过。
pub fn activate() {
    with_state((), |state| {
        state.reload_settings();
        state.reload_extra_dicts();
        state.reload_user_words();
        state.reload_local_dicts();
        state.load_ai();
        // 切到输入框，又是中文状态，多半是要打字了
        if state.session.mode() == Mode::Chinese {
            state.warm_ai();
        }
    });
}

/// 候选窗正显示着的话，它的位置（屏幕坐标，原点在左下）。自测模式用。
pub fn panel_frame() -> Option<NSRect> {
    with_state(None, |state| state.panel.visible_frame())
}

/// 失焦、切走输入法等情况：把还没上屏的内容原样交出去。
pub fn finish_composition(client: &AnyObject) {
    with_state((), |state| {
        if let Some(text) = state.session.commit_all() {
            client::insert_text(client, &text);
        }
        state.marked.clear();
        state.client = None;
        state.ai_before = None;
        state.panel.hide();
        state.save_user_words();
    });
}

/// 鼠标点了候选窗。
pub fn pick(pick: Pick) {
    with_state((), |state| state.pick(pick));
}

impl AppState {
    /// `user_data`：读写用户词库和本地词库（和设置文件放在一起）。自测模式不读也不写。
    pub fn new(
        engine: Arc<Engine>,
        panel: PanelWindow,
        app: AsyncApp,
        settings: SettingsFile,
        user_data: bool,
    ) -> Self {
        let userdict = user_data.then(|| UserDictFile::beside(settings.path()));
        let ai_file = AiFile::beside(settings.path());
        let dicts = user_data.then(|| LocalDicts::beside(settings.path()));
        let extra = user_data.then(|| ExtraDir::beside(settings.path()));
        let mut state = AppState {
            session: Session::new(engine),
            panel,
            app,
            settings,
            userdict,
            unsaved: false,
            dicts,
            extra,
            layers: BTreeMap::new(),
            building: HashMap::new(),
            builds: 0,
            client: None,
            marked: String::new(),
            caret: NSRect::ZERO,
            hint_generation: 0,
            ai: None,
            ai_file,
            ai_services: AiServices::default(),
            ai_for: None,
            ai_timer: None,
            ai_asked: 0,
            ai_before: None,
            timings: Timings::default(),
        };
        state.reload_settings();
        state.reload_extra_dicts();
        // 有用户词库文件才挂上用户词库、开始学词；文件还不在就从空的开始
        if state.userdict.is_some() {
            state.session.set_user_words(Vec::new());
            state.reload_user_words();
        }
        state.reload_local_dicts();
        state.load_ai();
        state
    }

    /// 用户词库的文件被别人改过（比如设置窗口清除了）就重读。
    fn reload_user_words(&mut self) {
        let Some(file) = &mut self.userdict else { return };
        if let Some(words) = file.reload(self.session.engine().dictionary().table(), today()) {
            log::info!("用户词库载入：{} 个词", words.len());
            self.session.set_user_words(words);
            self.unsaved = false;
        }
    }

    /// 本地词库有改动就在后台重建。
    fn reload_local_dicts(&mut self) {
        if let Some(paths) = self.dicts.as_mut().and_then(LocalDicts::reload) {
            self.rebuild(vec![(Source::Local, paths)]);
        }
    }

    /// 扩展词库打开了、下载了或重新下载了的，在后台重建；关了、删了的直接卸掉，内存就还回去了。
    fn reload_extra_dicts(&mut self) {
        let Some(extra) = self.extra.as_mut() else { return };
        let mut builds = Vec::new();
        let mut dropped = false;
        for (file, path) in extra.reload(&self.session.settings().dicts) {
            let source = Source::Extra(file);
            match path {
                Some(path) => builds.push((source, vec![path])),
                None => {
                    // 正在建的也作废
                    self.building.remove(&source);
                    dropped |= self.layers.remove(&source).is_some();
                }
            }
        }
        if dropped {
            self.compose();
        }
        self.rebuild(builds);
    }

    /// 在后台把这几层词库一层一层建好，建好后拼到引擎上，打字不用等。一个接一个地建：同时建几个大词库，
    /// 内存的最高点会叠在一起，释放了也不还给系统。读不了的文件跳过。
    fn rebuild(&mut self, layers: Vec<(Source, Vec<PathBuf>)>) {
        if layers.is_empty() {
            return;
        }
        let jobs: Vec<(Source, u64, Vec<PathBuf>)> = layers
            .into_iter()
            .map(|(source, paths)| {
                self.builds += 1;
                self.building.insert(source, self.builds);
                (source, self.builds, paths)
            })
            .collect();
        let engine = self.session.engine().clone();
        let building = self.app.background_executor().spawn(async move {
            jobs.into_iter()
                .map(|(source, build, paths)| {
                    let started = Instant::now();
                    let sources = read_sources(&paths);
                    let layer = engine.layer(&sources.iter().map(String::as_str).collect::<Vec<_>>());
                    (source, build, layer, started.elapsed())
                })
                .collect::<Vec<_>>()
        });
        self.when(building, |state, built| {
            for (source, build, layer, elapsed) in built {
                // 这期间又改过、又开始建了的话，等那一次的
                if state.building.get(&source) == Some(&build) {
                    log::info!("{source}载入：{} 个词，耗时 {elapsed:?}", layer.len());
                    state.building.remove(&source);
                    state.layers.insert(source, layer);
                }
            }
            state.compose();
            crate::release_memory();
        });
    }

    /// 把现在的几层词库拼到引擎上。
    fn compose(&mut self) {
        let layers = self.layers.values().cloned().collect();
        self.session.set_engine(Arc::new(self.session.engine().with_layers(layers)));
    }

    /// 过一会儿在主线程上对全局状态做 `f`。
    fn after(&self, delay: Duration, f: impl FnOnce(&mut AppState) + 'static) {
        self.when(self.app.background_executor().timer(delay), move |state, _| f(state));
    }

    /// 等 `task` 完成后，在主线程上拿它的结果对全局状态做 `f`。
    fn when<T: 'static>(&self, task: impl Future<Output = T> + 'static, f: impl FnOnce(&mut AppState, T) + 'static) {
        self.app
            .foreground_executor()
            .spawn(async move {
                let output = task.await;
                with_state((), |state| f(state, output));
            })
            .detach();
    }

    fn save_user_words(&mut self) {
        if !self.unsaved {
            return;
        }
        // 这期间文件被清除了：以文件为准，清掉的词不再写回去
        if self.userdict.as_ref().is_some_and(UserDictFile::changed) {
            self.reload_user_words();
            return;
        }
        let Some(file) = &mut self.userdict else { return };
        match file.save(self.session.engine().dictionary().table(), self.session.user_words()) {
            Ok(()) => self.unsaved = false,
            Err(err) => log::warn!("用户词库保存失败：{err}"),
        }
    }

    fn reload_settings(&mut self) {
        let settings = match self.settings.reload() {
            None => return,
            Some(Ok(settings)) => settings,
            Some(Err(err)) => {
                log::warn!("{err}；先用默认设置");
                Settings::default()
            }
        };
        log::info!(
            "设置已载入：每页 {} 个候选，字号 {}，内嵌拼音{}，AI 预测选词{}",
            settings.input.page_size,
            settings.appearance.font_size,
            if settings.input.inline_pinyin { "开" } else { "关" },
            if settings.ai.enabled { "开" } else { "关" }
        );
        self.session.set_settings(settings);
    }

    /// AI 预测选词开着的话，照设置和 ai.toml 在后台建好；选的那一家的设置改过（设置窗口里刚填了 Key）就重建。
    fn load_ai(&mut self) {
        match self.ai_file.reload() {
            Some(Ok(services)) => self.ai_services = services,
            Some(Err(err)) => log::warn!("{err}"),
            None => {}
        }
        let ai = &self.session.settings().ai;
        let wanted = ai.enabled.then(|| (ai.provider, self.ai_services.get(ai.provider).clone()));
        if wanted == self.ai_for {
            return;
        }
        self.ai = None;
        self.ai_for = wanted.clone();
        let Some((provider, service)) = wanted else { return };
        let config = (provider, service.clone());
        // 建 HTTP 客户端要读系统证书，放到后台
        let building = self.app.background_executor().spawn(async move { kyme_ai::Client::new(provider, &service) });
        self.when(building, move |state, built| {
            // 这期间设置又变了的话，等那一次的
            if state.ai_for.as_ref() != Some(&config) {
                return;
            }
            match built {
                Ok(client) => {
                    log::info!("AI 预测选词已就绪：{}，{}", client.provider().name(), client.model());
                    state.ai = Some(Arc::new(client));
                }
                Err(err) => log::info!("AI 预测选词已开启，但{err}"),
            }
        });
    }

    /// 停下来设置里的那么久（`pause_ms`）还没再动，就问 AI：一直在打的时候不问，免得白发请求；问出去以后又打了字，
    /// 回来的答案就扔掉。每打一个键换一个计时器，前一个就取消了。
    fn schedule_ai(&mut self) {
        if self.ai.is_none() {
            return;
        }
        self.warm_ai();
        let generation = self.session.generation();
        let pause = Duration::from_millis(self.session.settings().ai.pause_ms);
        let pause = self.app.background_executor().timer(pause);
        self.ai_timer = Some(self.app.foreground_executor().spawn(async move {
            pause.await;
            let Some((ai, client)) = with_state(None, |state| state.ai_request(generation)) else { return };
            // 读光标前的文字是一次跨进程的同步调用，等 App 回话时 IMK 会把接着按的键送进来。
            // 这时不能占着全局状态：占着的话那个键进不了输入法，只能原样交给 App（打 lingwai 上屏一个 l）
            let before = client.map(|client| client::text_before_cursor(&client, CONTEXT_CHARS).unwrap_or_default());
            with_state((), |state| state.ask_ai(ai, generation, before));
        }));
    }

    /// 好一阵没和 AI 服务商连过的话，在后台先连上。重新握手要大半秒（走代理更久），等停下来要问时再连就晚了。
    fn warm_ai(&self) {
        if let Some(ai) = self.ai.as_ref().filter(|ai| ai.cold()) {
            let ai = ai.clone();
            self.app.background_executor().spawn(async move { ai.warm() }).detach();
        }
    }

    /// 停顿到了、输入还是那一次的话，取出问 AI 要用的 AI，和要去读光标前文字的输入框（这一段输入里读过了就不用），
    /// 记下这一次问过了。
    fn ai_request(&mut self, generation: u64) -> Option<(Arc<kyme_ai::Client>, Option<Retained<AnyObject>>)> {
        if self.session.generation() != generation || self.ai_asked == generation {
            return None;
        }
        let client = match self.ai_before {
            Some(_) => None,
            None => Some(self.client.clone()?),
        };
        let ai = self.ai.clone()?;
        self.ai_asked = generation;
        Some((ai, client))
    }

    /// 输入还是那一次的话，在后台问 AI。`before` 是刚读到的光标前的文字，没去读是 None。
    fn ask_ai(&mut self, ai: Arc<kyme_ai::Client>, generation: u64, before: Option<String>) {
        // 读的时候又打了字，这一次就不问了
        if self.session.generation() != generation {
            return;
        }
        if before.is_some() {
            self.ai_before = before;
        }
        let query = match self.session.ai_query() {
            Ok(query) => query,
            Err(reason) => {
                log::info!("AI 预测选词：未发送请求，{reason}");
                return;
            }
        };
        // 前文是光标前的文字，加上这一段里已经选定的字。没有前文（空的输入框，或者 App 不给读）也问，
        // 挑的就是首选时标上 ✨，看得出 AI 看过了；只是要更有把握才换（见 `kyme_ai::Pick::trusted`）
        let context = self.ai_before.clone().unwrap_or_default() + &query.selected;
        let asking = self.app.background_executor().spawn(async move {
            let sentences: Vec<&str> = query.sentences.iter().map(|sentence| sentence.text.as_str()).collect();
            let answer = ai.pick(&context, &query.pinyin, &sentences);
            (query, answer, ai)
        });
        self.when(asking, |state, (query, answer, ai)| state.take_ai_answer(&query, answer, &ai));
    }

    /// `asked` 回答了：信得过、输入还没变，就把它挑的那句放到第一个、重画候选窗；挑的就是首选，也标上 ✨。
    /// 日志只记耗时和结果，不记内容。
    fn take_ai_answer(
        &mut self,
        query: &AiQuery,
        answer: Result<(kyme_ai::Pick, Duration), kyme_ai::Error>,
        asked: &Arc<kyme_ai::Client>,
    ) {
        match answer {
            Ok((pick, elapsed)) => {
                let agreed = pick.index == 0;
                let outcome = if !agreed && !pick.trusted() {
                    "置信度不足，未替换"
                } else if self.session.offer_ai(query, pick.index, Instant::now()) {
                    self.redraw();
                    if agreed { "与首选一致，已标 ✨" } else { "已替换首选" }
                } else {
                    "输入已变化，已丢弃"
                };
                log::info!(
                    "AI 预测选词 {elapsed:?}{}，选择第 {} 句，置信度 {}，{outcome}",
                    if pick.alone { "（无前文）" } else { "" },
                    pick.index + 1,
                    pick.confidence.map_or("未提供".into(), |confidence| format!("{confidence:.2}"))
                );
            }
            // Key 不对：停掉问的那个，等设置窗口里换了 Key 再重建。这期间已经换上新的了就不动新的
            Err(kyme_ai::Error::Key) => {
                log::warn!("AI 预测选词：API Key 无效");
                if self.ai.as_ref().is_some_and(|ai| Arc::ptr_eq(ai, asked)) {
                    self.ai = None;
                }
            }
            Err(err) => log::warn!("AI 预测选词请求失败：{}", err.log_line()),
        }
    }

    /// 候选变了、但不是因为按键（AI 的回答到了）：照现在的样子重画候选窗。光标位置用最近一次按键时的，
    /// 不再去问 App：这不是 IMK 的回调，跨进程问的时候接着按的键会进不来（见 `schedule_ai`）。
    fn redraw(&mut self) {
        if let View::Composing { panel, .. } = self.session.view() {
            self.show_candidates(panel);
        }
    }

    /// 在最近一次问到的光标位置显示候选窗。
    fn show_candidates(&mut self, panel: Panel) {
        let appearance = &self.session.settings().appearance;
        self.panel.show(&self.app, Content::Candidates(panel), appearance, self.caret);
    }

    fn handle_event(&mut self, event: &NSEvent, client: &AnyObject) -> bool {
        let started = Instant::now();
        self.client = Some(client.retain());
        let handled = match translate(event) {
            Some((key, mods)) => {
                let response = self.session.handle_key(key, mods);
                // 修饰键的变化总是交还给 App
                self.apply(response, client) && event.r#type() == NSEventType::KeyDown
            }
            None => false,
        };
        let elapsed = started.elapsed();
        if elapsed > SLOW_KEY {
            log::warn!("一个键处理了 {elapsed:?}");
        }
        self.timings.record(elapsed);
        handled
    }

    fn pick(&mut self, pick: Pick) {
        let Some(client) = self.client.clone() else { return };
        let response = self.session.pick(pick);
        self.apply(response, &client);
    }

    /// 把会话的结果交给客户端和候选窗。返回这个键是否被输入法吃掉。
    fn apply(&mut self, response: Response, client: &AnyObject) -> bool {
        if let Some(text) = &response.commit {
            client::insert_text(client, text);
            self.marked.clear();
        }
        match response.view {
            View::Idle => {
                self.ai_before = None;
                if !self.marked.is_empty() {
                    client::set_marked_text(client, "");
                    self.marked.clear();
                }
                // 中英提示只在到时间、开始组字、切走输入框时收起，切到英文后接着打字不收：不然一打字它就没了
                if response.mode_changed.is_none() && !self.panel.showing_hint() {
                    self.panel.hide();
                }
            }
            View::Composing { marked, panel } => {
                let marked = if marked.is_empty() { MARKED_PLACEHOLDER.to_string() } else { marked };
                if marked != self.marked {
                    client::set_marked_text(client, &marked);
                    self.marked = marked;
                }
                // 每个键都重新问：组字期间窗口可能被拖动、页面可能滚动，marked text 不变位置也会变
                self.caret = client::caret_rect(client);
                self.show_candidates(panel);
                self.schedule_ai();
            }
        }
        if let Some(mode) = response.mode_changed {
            self.show_mode(mode, client);
        }
        if response.learned && self.userdict.is_some() && !std::mem::replace(&mut self.unsaved, true) {
            self.after(SAVE_DELAY, AppState::save_user_words);
        }
        response.handled
    }

    fn show_mode(&mut self, mode: Mode, client: &AnyObject) {
        self.hint_generation += 1;
        let generation = self.hint_generation;
        let caret = client::caret_rect(client);
        let appearance = &self.session.settings().appearance;
        self.panel.show(&self.app, Content::Mode(mode), appearance, caret);
        self.after(HINT_DURATION, move |state| {
            if state.hint_generation == generation && state.panel.showing_hint() {
                state.panel.hide();
            }
        });
    }
}

fn translate(event: &NSEvent) -> Option<(Key, Modifiers)> {
    let flags = event.modifierFlags();
    let mods = Modifiers {
        control: flags.contains(NSEventModifierFlags::Control),
        option: flags.contains(NSEventModifierFlags::Option),
        command: flags.contains(NSEventModifierFlags::Command),
    };
    let key = match event.r#type() {
        NSEventType::FlagsChanged => match event.keyCode() {
            KeyCode::SHIFT | KeyCode::RIGHT_SHIFT if flags.contains(NSEventModifierFlags::Shift) => Key::ShiftDown,
            KeyCode::SHIFT | KeyCode::RIGHT_SHIFT => Key::ShiftUp,
            // 别的修饰键按下或松开，也会打断「轻点 Shift」
            _ => Key::Other,
        },
        NSEventType::KeyDown => match event.keyCode() {
            KeyCode::RETURN | KEYPAD_ENTER => Key::Enter,
            KeyCode::SPACE => Key::Space,
            KeyCode::DELETE => Key::Backspace,
            KeyCode::ESCAPE => Key::Escape,
            KeyCode::PAGE_UP => Key::PageUp,
            KeyCode::PAGE_DOWN => Key::PageDown,
            KeyCode::LEFT_ARROW => Key::Left,
            KeyCode::RIGHT_ARROW => Key::Right,
            KeyCode::DOWN_ARROW => Key::Down,
            KeyCode::UP_ARROW => Key::Up,
            _ => event
                .characters()
                .and_then(|chars| chars.to_string().chars().next())
                .filter(|c| !c.is_control())
                .map_or(Key::Other, Key::Char),
        },
        _ => return None,
    };
    Some((key, mods))
}

/// 统计按键处理耗时（IMK 回调从进入到返回），每 50 次写一行日志。只记耗时，不记内容。
#[derive(Default)]
struct Timings {
    samples: Vec<Duration>,
}

impl Timings {
    fn record(&mut self, elapsed: Duration) {
        self.samples.push(elapsed);
        if self.samples.len() < 50 {
            return;
        }
        self.samples.sort();
        let at = |q: f64| self.samples[((self.samples.len() - 1) as f64 * q).round() as usize];
        log::info!(
            "最近 {} 次按键处理耗时：p50 {:?}，p95 {:?}，最大 {:?}",
            self.samples.len(),
            at(0.5),
            at(0.95),
            at(1.0)
        );
        self.samples.clear();
    }
}
