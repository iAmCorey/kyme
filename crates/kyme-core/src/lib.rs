//! 输入会话：把按键变成「上屏文字 + 候选窗状态」，与平台无关。
//!
//! 平台层（macOS 的 IMK，之后的 TSF / fcitx5）只负责把系统按键翻译成 [`Key`]，
//! 再把 [`Response`] 交给系统和候选窗。

mod punct;
mod session;
mod settings;

pub use kyme_pinyin as pinyin;
pub use session::{Key, Mode, Modifiers, Panel, Pick, Response, SELECT_KEYS, Session, View};
pub use settings::{AppearanceSettings, InputSettings, Rgb, SETTINGS_TEMPLATE, Settings, SettingsFile};
