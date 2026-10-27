//! 用户设置。没写的项用默认值，超出范围的值收拢到合法范围。
//! [`SettingsFile`] 负责磁盘上的文件：不存在时写出模板，改过才重新读，设置窗口写回时只改值、注释照旧；
//! 平台层只需给出路径。

use std::fs;
use std::io;
use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use toml_edit::{DocumentMut, Item, Value};

use crate::session::SELECT_KEYS;

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub input: InputSettings,
    pub appearance: AppearanceSettings,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct InputSettings {
    /// 每页候选个数，5–10。第 10 个用 0 键选。
    pub page_size: usize,
    /// 输入时拼音显示在输入框里（默认），候选窗只有一行候选。关掉的话拼音显示在候选窗第一行，
    /// 输入框里只有选定的字（还没选时是一个占位）。
    pub inline_pinyin: bool,
    /// 候选第一页最后一格放一个 emoji，取自第一页里和整串拼音对得上的词。
    pub emoji: bool,
}

impl Default for InputSettings {
    fn default() -> Self {
        InputSettings { page_size: 7, inline_pinyin: true, emoji: true }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
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

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("#{:06X}", self.0))
    }
}

impl Settings {
    pub fn from_toml(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str::<Settings>(text).map(Settings::normalized)
    }

    /// 把超出范围的值收拢到合法范围。
    pub fn normalized(mut self) -> Self {
        self.input.page_size = self.input.page_size.clamp(*PAGE_SIZES.start(), *PAGE_SIZES.end());
        let font_size = self.appearance.font_size;
        self.appearance.font_size = if font_size.is_finite() {
            font_size.clamp(*FONT_SIZES.start(), *FONT_SIZES.end())
        } else {
            AppearanceSettings::default().font_size
        };
        self
    }
}

/// 每页候选个数的范围。
pub const PAGE_SIZES: RangeInclusive<usize> = 5..=SELECT_KEYS.len();
/// 候选字号的范围（pt）。
pub const FONT_SIZES: RangeInclusive<f32> = 12.0..=36.0;

/// 设置文件不存在时写出的模板，内容就是默认值。
pub const SETTINGS_TEMPLATE: &str = r##"# Kyme 设置，也可以在输入法菜单的「Kyme 设置…」里改。改完保存，切换一下输入框就会生效。

[input]
# 每页候选个数（5–10），第 10 个用 0 键选
page_size = 7
# 输入时拼音显示在输入框里，候选窗只有一行候选。false：拼音显示在候选窗第一行
inline_pinyin = true
# 候选里带 emoji：第一页最后一格放一个和打的拼音对得上的 emoji
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

    /// 第一次调用、或文件改过之后，返回读到的设置；文件写错时返回错误说明（一行，不带路径）。没变返回 None。
    /// 文件不存在时先写出带注释的模板。
    pub fn reload(&mut self) -> Option<Result<Settings, String>> {
        if !self.path.exists()
            && let Err(err) = self.write_template()
        {
            return Some(Err(format!("写不了设置文件：{err}")));
        }
        let modified = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if modified.is_some() && modified == self.modified {
            return None;
        }
        self.modified = modified;
        let result = match fs::read_to_string(&self.path) {
            Ok(text) => Settings::from_toml(&text).map_err(|err| invalid(&text, err.span(), err.message())),
            Err(err) => Err(format!("读不了设置文件：{err}")),
        };
        Some(result)
    }

    /// 把设置写回文件。只改各项的值，文件里的注释和排版照旧；文件不存在时从模板开始。
    /// 文件写得不对（语法错，或者哪一项的值不对）时不写，等用户先在文件里改好。
    pub fn save(&mut self, settings: &Settings) -> Result<(), String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => SETTINGS_TEMPLATE.to_string(),
            Err(err) => return Err(format!("读不了设置文件：{err}")),
        };
        // 有一项写错时输入法用的是整套默认设置，这时写回去会把其余写对的项也盖成默认值
        Settings::from_toml(&text).map_err(|err| invalid(&text, err.span(), err.message()))?;
        let mut doc: DocumentMut =
            text.parse().map_err(|err: toml_edit::TomlError| invalid(&text, err.span(), err.message()))?;
        // 每一节的每一项都写回去，以后加了设置项这里不用改
        let fresh = toml_edit::ser::to_document(settings).map_err(|err| format!("设置转不成 TOML：{err}"))?;
        for (section, table) in fresh.iter() {
            for (key, item) in table.as_table_like().into_iter().flat_map(|table| table.iter()) {
                if let Some(value) = item.as_value() {
                    set(&mut doc, section, key, integral(value.clone()))?;
                }
            }
        }

        // 先写到旁边的临时文件再换上，输入法进程不会读到写了一半的文件
        let tmp = self.path.with_extension("toml.tmp");
        self.create_dir()
            .and_then(|()| fs::write(&tmp, doc.to_string()))
            .and_then(|()| fs::rename(&tmp, &self.path))
            .map_err(|err| format!("写不了设置文件：{err}"))?;
        // 自己写的不算改过，下次 reload 不重读
        self.modified = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        Ok(())
    }

    fn write_template(&self) -> io::Result<()> {
        self.create_dir()?;
        fs::write(&self.path, SETTINGS_TEMPLATE)
    }

    fn create_dir(&self) -> io::Result<()> {
        match self.path.parent() {
            Some(dir) => fs::create_dir_all(dir),
            None => Ok(()),
        }
    }
}

/// TOML 的报错带着原文和箭头，有好几行。界面和日志只要一行：第几行，什么问题。
fn invalid(text: &str, span: Option<Range<usize>>, message: &str) -> String {
    let line = span.and_then(|span| text.get(..span.start)).map_or(1, |before| before.matches('\n').count() + 1);
    let reason: Vec<&str> = message.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    format!("设置文件第 {line} 行写得不对：{}", reason.join("，"))
}

/// 没有小数的数写成整数：字号写 15，和模板一样，不写成 15.0。
fn integral(value: Value) -> Value {
    match value.as_float() {
        Some(number) if number.fract() == 0.0 => Value::from(number as i64),
        _ => value,
    }
}

/// 改文件里的一项，保留这一行的注释和空白；表或项不存在就加上。
fn set(doc: &mut DocumentMut, section: &str, key: &str, mut value: Value) -> Result<(), String> {
    let Some(table) = doc.entry(section).or_insert(toml_edit::table()).as_table_like_mut() else {
        return Err(format!("设置文件里的 {section} 应该是 [{section}] 这样的一节"));
    };
    match table.get_mut(key) {
        Some(Item::Value(old)) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        Some(item) => *item = Item::Value(value),
        None => {
            table.insert(key, Item::Value(value));
        }
    }
    Ok(())
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
        let err = file.reload().unwrap().unwrap_err();
        assert!(err.starts_with("设置文件第 2 行写得不对：") && !err.contains('\n'), "{err}");

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_changes_values_and_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("kyme-settings-save-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("settings.toml");
        let mut file = SettingsFile::new(path.clone());
        let mut settings = Settings::default();
        settings.input.page_size = 9;
        settings.appearance.font_size = 18.0;
        settings.appearance.highlight_color = Rgb(0x34c759);

        // 文件还不存在：从模板开始
        file.save(&settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 每页候选个数"), "模板里的注释还在：{text}");
        assert!(text.contains("page_size = 9\n") && text.contains("font_size = 18\n"), "整数字号写成整数：{text}");
        assert!(text.contains("highlight_color = \"#34C759\""));
        assert_eq!(Settings::from_toml(&text).unwrap(), settings);
        assert_eq!(file.reload(), None, "自己写的不算改过");

        // 用户自己的注释和行尾注释都留着，缺的项和表补上
        fs::write(&path, "# 我的设置\n[input]\npage_size = 6 # 少一点\n").unwrap();
        file.save(&settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# 我的设置\n[input]\npage_size = 9 # 少一点\n"), "{text}");
        assert_eq!(Settings::from_toml(&text).unwrap(), settings);

        // 写得不对：不写。值写错的那种也一样，不然写对的字号会被盖成默认值
        for broken in ["[input\n", "[input]\npage_size = \"many\"\n[appearance]\nfont_size = 20\n"] {
            fs::write(&path, broken).unwrap();
            assert!(file.save(&settings).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), broken);
        }

        fs::remove_dir_all(&dir).unwrap();
    }
}
