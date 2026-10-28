//! 输入法进程的全局状态。IMK 回调和 GPUI 都跑在主线程，状态放在 thread_local 里。

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_graphics::event::KeyCode;
use gpui::AsyncApp;
use kyme_core::pinyin::Engine;
use kyme_core::{Key, Mode, Modifiers, Pick, Response, Session, Settings, SettingsFile, UserDictFile, View, today};
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
/// 小键盘上的回车，core-graphics 没有这个常量。
const KEYPAD_ENTER: u16 = 0x4c;
/// 不显示内嵌拼音、又还没选定任何字时，输入框里放的占位符（全角空格）。
/// IMK 客户端靠 marked text 判断是否正在组字：没有它，iTerm2 会把按键原样回显，
/// Chromium 会先把删除键交给网页、删掉前面的字。半角占位会让中文基线跳动。
const MARKED_PLACEHOLDER: &str = "\u{3000}";

thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
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
    /// 最近一次按键所在的输入框。鼠标点候选窗时往这里上屏。
    client: Option<Retained<AnyObject>>,
    /// 客户端输入框里当前的 marked text，没变就不重复同步。
    marked: String,
    /// 每次显示中英提示加一，过期的隐藏任务据此作废。
    hint_generation: u64,
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

/// 每次切到一个输入框时调用。顺便检查设置文件和用户词库有没有改过。
pub fn activate() {
    with_state((), |state| {
        state.reload_settings();
        state.reload_user_words();
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
        state.panel.hide();
        state.save_user_words();
    });
}

/// 鼠标点了候选窗。
pub fn pick(pick: Pick) {
    with_state((), |state| state.pick(pick));
}

impl AppState {
    pub fn new(
        engine: Arc<Engine>,
        panel: PanelWindow,
        app: AsyncApp,
        settings: SettingsFile,
        userdict: Option<UserDictFile>,
    ) -> Self {
        let mut state = AppState {
            session: Session::new(engine),
            panel,
            app,
            settings,
            userdict,
            unsaved: false,
            client: None,
            marked: String::new(),
            hint_generation: 0,
            timings: Timings::default(),
        };
        state.reload_settings();
        // 有用户词库文件才挂上用户词库、开始学词；文件还不在就从空的开始
        if state.userdict.is_some() {
            state.session.set_user_words(Vec::new());
            state.reload_user_words();
        }
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

    /// 过一会儿在主线程上对全局状态做 `f`。
    fn after(&self, delay: Duration, f: impl FnOnce(&mut AppState) + 'static) {
        let timer = self.app.background_executor().timer(delay);
        self.app
            .foreground_executor()
            .spawn(async move {
                timer.await;
                with_state((), f);
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
            Err(err) => log::warn!("用户词库存不进去：{err}"),
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
            "设置已载入：每页 {} 个候选，字号 {}，内嵌拼音{}",
            settings.input.page_size,
            settings.appearance.font_size,
            if settings.input.inline_pinyin { "开" } else { "关" }
        );
        self.session.set_settings(settings);
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
                if !self.marked.is_empty() {
                    client::set_marked_text(client, "");
                    self.marked.clear();
                }
                if response.mode_changed.is_none() {
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
                let caret = client::caret_rect(client);
                let appearance = &self.session.settings().appearance;
                self.panel.show(&self.app, Content::Candidates(panel), appearance, caret);
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
            if state.hint_generation == generation && !state.session.is_composing() {
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
