//! 用户设置。没写的项用默认值，超出范围的值收拢到合法范围。
//! [`SettingsFile`] 负责磁盘上的文件：不存在时写出模板，改过才重新读；平台层只需给出路径。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, de};

use crate::session::SELECT_KEYS;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub input: InputSettings,
    pub appearance: AppearanceSettings,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct InputSettings {
    /// 每页候选个数，5–10。第 10 个用 0 键选。
    pub page_size: usize,
    /// 输入时拼音显示在输入框里（默认），候选窗只有一行候选。关掉的话拼音显示在候选窗第一行，
    /// 输入框里只有选定的字（还没选时是一个占位）。
    pub inline_pinyin: bool,
    /// 和整串拼音对得上的词后面，候选里跟上它的 emoji。
    pub emoji: bool,
}

impl Default for InputSettings {
    fn default() -> Self {
        InputSettings { page_size: 7, inline_pinyin: true, emoji: true }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct AppearanceSettings {
    /// 候选字号（pt），12–36。其余文字和间距按比例缩放。
    pub font_size: f32,
    /// 候选窗字体。留空用平台默认的中文字体。
    pub font_family: String,
    /// 选中候选的高亮颜色。
    pub highlight_color: Rgb,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        AppearanceSettings { font_size: 15.0, font_family: String::new(), highlight_color: Rgb(0x5e5ce6) }
    }
}

/// 24 位 RGB 颜色，TOML 里写作 `"#RRGGBB"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u32);

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let hex = text.strip_prefix('#').unwrap_or(&text);
        let parsed = (hex.len() == 6).then(|| u32::from_str_radix(hex, 16).ok()).flatten();
        parsed.map(Rgb).ok_or_else(|| de::Error::custom(format!("颜色要写成 \"#RRGGBB\"，收到的是 \"{text}\"")))
    }
}

impl Settings {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str::<Settings>(text).map(Settings::normalized)
    }

    /// 把超出范围的值收拢到合法范围。
    pub fn normalized(mut self) -> Self {
        self.input.page_size = self.input.page_size.clamp(5, SELECT_KEYS.len());
        let font_size = self.appearance.font_size;
        self.appearance.font_size =
            if font_size.is_finite() { font_size.clamp(12.0, 36.0) } else { AppearanceSettings::default().font_size };
        self
    }
}

/// 设置文件不存在时写出的模板，内容就是默认值。
pub const SETTINGS_TEMPLATE: &str = r##"# Kyme 设置。改完保存，切换一下输入框就会生效。

[input]
# 每页候选个数（5–10），第 10 个用 0 键选
page_size = 7
# 输入时拼音显示在输入框里，候选窗只有一行候选。false：拼音显示在候选窗第一行
inline_pinyin = true
# 候选里带 emoji：和打的拼音对得上的词后面跟着它的 emoji
emoji = true

[appearance]
# 候选字号（pt，12–36），其余文字和间距按比例缩放
font_size = 15
# 候选窗字体，留空用系统默认的中文字体
font_family = ""
# 选中候选的高亮颜色（#RRGGBB）
highlight_color = "#5E5CE6"
"##;

/// 磁盘上的设置文件。
pub struct SettingsFile {
    path: PathBuf,
    modified: Option<SystemTime>,
}

impl SettingsFile {
    pub fn new(path: PathBuf) -> Self {
        SettingsFile { path, modified: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 第一次调用、或文件改过之后，返回读到的设置；文件写错时返回错误说明。没变返回 None。
    /// 文件不存在时先写出带注释的模板。
    pub fn reload(&mut self) -> Option<Result<Settings, String>> {
        if !self.path.exists()
            && let Err(err) = self.write_template()
        {
            return Some(Err(format!("写不了设置模板 {}：{err}", self.path.display())));
        }
        let modified = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if modified.is_some() && modified == self.modified {
            return None;
        }
        self.modified = modified;
        let result = match fs::read_to_string(&self.path) {
            Ok(text) => Settings::from_toml(&text).map_err(|err| format!("{} 写得不对：{err}", self.path.display())),
            Err(err) => Err(format!("读不了 {}：{err}", self.path.display())),
        };
        Some(result)
    }

    fn write_template(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&self.path, SETTINGS_TEMPLATE)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn template_is_the_default() {
        assert_eq!(Settings::from_toml(SETTINGS_TEMPLATE).unwrap(), Settings::default());
    }

    #[test]
    fn missing_keys_use_defaults() {
        assert_eq!(Settings::from_toml("").unwrap(), Settings::default());
        let settings = Settings::from_toml("[input]\ninline_pinyin = false").unwrap();
        assert_eq!(settings.input, InputSettings { inline_pinyin: false, ..InputSettings::default() });
    }

    #[test]
    fn values_are_clamped() {
        let settings = Settings::from_toml("[input]\npage_size = 20\n[appearance]\nfont_size = 4").unwrap();
        assert_eq!(settings.input.page_size, 10);
        assert_eq!(settings.appearance.font_size, 12.0);
        assert_eq!(Settings::from_toml("[input]\npage_size = 3").unwrap().input.page_size, 5);
    }

    #[test]
    fn colors() {
        let parse = |s: &str| Settings::from_toml(&format!("[appearance]\nhighlight_color = \"{s}\""));
        assert_eq!(parse("#07C160").unwrap().appearance.highlight_color, Rgb(0x07c160));
        assert_eq!(parse("ff9500").unwrap().appearance.highlight_color, Rgb(0xff9500));
        assert!(parse("green").is_err());
        assert!(parse("#12345").is_err());
    }

    #[test]
    fn settings_file_writes_template_and_reloads_on_change() {
        let dir = std::env::temp_dir().join(format!("kyme-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("settings.toml");
        let mut file = SettingsFile::new(path.clone());

        assert_eq!(file.reload(), Some(Ok(Settings::default())));
        assert_eq!(fs::read_to_string(&path).unwrap(), SETTINGS_TEMPLATE);
        assert_eq!(file.reload(), None, "没改过就不重读");

        // 修改时间要和上次不同才算改过
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, "[input]\npage_size = 6\n").unwrap();
        assert_eq!(file.reload().unwrap().unwrap().input.page_size, 6);

        std::thread::sleep(Duration::from_millis(20));
        fs::write(&path, "[input]\npage_size = \"many\"\n").unwrap();
        assert!(file.reload().unwrap().is_err());

        fs::remove_dir_all(&dir).unwrap();
    }
}
