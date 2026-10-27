//! 输入法进程的全局状态。IMK 回调和 GPUI 都跑在主线程，状态放在 thread_local 里。

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use core_graphics::event::KeyCode;
use gpui::AsyncApp;
use kyme_core::pinyin::Engine;
use kyme_core::{Key, Mode, Modifiers, Response, Session, Settings, SettingsFile, View};
use kyme_ui::Content;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::NSRect;

use crate::client;
use crate::panel::PanelWindow;

/// 中英提示显示多久。
const HINT_DURATION: Duration = Duration::from_millis(700);
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
            None => fallback,
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

/// 每次切到一个输入框时调用。顺便检查设置文件有没有改过。
pub fn activate() {
    with_state((), AppState::reload_settings);
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
        state.panel.hide();
    });
}

impl AppState {
    pub fn new(engine: Arc<Engine>, panel: PanelWindow, app: AsyncApp, settings: SettingsFile) -> Self {
        let mut state = AppState {
            session: Session::new(engine),
            panel,
            app,
            settings,
            marked: String::new(),
            hint_generation: 0,
            timings: Timings::default(),
        };
        state.reload_settings();
        state
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
        let handled = match translate(event) {
            Some((key, mods)) => {
                let response = self.session.handle_key(key, mods);
                // 修饰键的变化总是交还给 App
                self.apply(response, client) && event.r#type() == NSEventType::KeyDown
            }
            None => false,
        };
        self.timings.record(started.elapsed());
        handled
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
        response.handled
    }

    fn show_mode(&mut self, mode: Mode, client: &AnyObject) {
        self.hint_generation += 1;
        let generation = self.hint_generation;
        let caret = client::caret_rect(client);
        let appearance = &self.session.settings().appearance;
        self.panel.show(&self.app, Content::Mode(mode), appearance, caret);
        let timer = self.app.background_executor().timer(HINT_DURATION);
        self.app
            .foreground_executor()
            .spawn(async move {
                timer.await;
                with_state((), |state| {
                    if state.hint_generation == generation && !state.session.is_composing() {
                        state.panel.hide();
                    }
                });
            })
            .detach();
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
