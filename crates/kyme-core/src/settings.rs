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

use crate::disk;
use crate::session::SELECT_KEYS;

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub input: InputSettings,
    pub appearance: AppearanceSettings,
    pub dicts: DictSettings,
    pub ai: AiSettings,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct InputSettings {
    /// 每页候选个数，5–10。第 10 个用 0 键选。
    pub page_size: usize,
    /// 输入框内显示拼音（默认），候选窗只有一行候选。关掉的话拼音显示在候选窗第一行，
    /// 输入框里只有选定的字（还没选时是一个占位）。
    pub inline_pinyin: bool,
    /// 候选第一页最后一格放一个 emoji，取自第一页里和整串拼音对得上的词。
    pub emoji: bool,
    /// 选词记忆：常用候选往前排，分几次选出来的词组记成新词。存在本机的 userdict.tsv。
    pub learn: bool,
}

impl Default for InputSettings {
    fn default() -> Self {
        InputSettings { page_size: 7, inline_pinyin: true, emoji: true, learn: true }
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

/// 载入哪些扩展词库（见 [`EXTRA_DICTS`](crate::EXTRA_DICTS)）。要先在设置窗口里下载；词多、占内存，默认都不载入。
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct DictSettings {
    /// 雾凇拼音的扩展词库 ext.dict.yaml，约 34 万词。
    pub ext: bool,
    /// 雾凇拼音收录的腾讯词库 tencent.dict.yaml，约 98 万词。没写拼音，载入时按主词库补读音。
    pub tencent: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct AiSettings {
    /// AI 预测选词：打字停下来时，把光标前的文字和几个候选整句发给 `provider` 这家的模型，让它挑接得最顺的一句，
    /// 放在第一个、标 ✨。默认关。
    pub enabled: bool,
    /// 用哪家的模型。各家的 Key 和模型存在同目录的 ai.toml 里，见 [`AiFile`](crate::AiFile)。
    pub provider: AiProvider,
    /// 预测延迟（毫秒）：停止输入多久后发送请求。越短 ✨ 出现越快，但打字中途也会发请求，回来时输入已经变了的多。
    pub pause_ms: u64,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings { enabled: false, provider: AiProvider::default(), pause_ms: 100 }
    }
}

/// AI 预测选词用哪家的模型。接口地址和能选的模型在 kyme-ai 里，自定义的自己填地址。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AiProvider {
    /// TypeSafe 的 Jev：只在给的选项里挑，还给出把握有多大。
    #[default]
    TypeSafe,
    Anthropic,
    OpenAi,
    DeepSeek,
    /// 阿里云百炼的通义千问。
    Qwen,
    Kimi,
    /// 智谱的 GLM。
    Zhipu,
    OpenRouter,
    /// 别的 OpenAI 兼容的接口，自己填地址：Ollama、LM Studio、自己搭的转发等。
    Custom,
}

impl AiProvider {
    pub const ALL: [AiProvider; 9] = [
        AiProvider::TypeSafe,
        AiProvider::Anthropic,
        AiProvider::OpenAi,
        AiProvider::DeepSeek,
        AiProvider::Qwen,
        AiProvider::Kimi,
        AiProvider::Zhipu,
        AiProvider::OpenRouter,
        AiProvider::Custom,
    ];

    /// 设置文件里的写法。
    pub fn id(self) -> &'static str {
        match self {
            AiProvider::TypeSafe => "typesafe",
            AiProvider::Anthropic => "anthropic",
            AiProvider::OpenAi => "openai",
            AiProvider::DeepSeek => "deepseek",
            AiProvider::Qwen => "qwen",
            AiProvider::Kimi => "kimi",
            AiProvider::Zhipu => "zhipu",
            AiProvider::OpenRouter => "openrouter",
            AiProvider::Custom => "custom",
        }
    }

    /// 界面上显示的名字。
    pub fn name(self) -> &'static str {
        match self {
            AiProvider::TypeSafe => "TypeSafe",
            AiProvider::Anthropic => "Anthropic",
            AiProvider::OpenAi => "OpenAI",
            AiProvider::DeepSeek => "DeepSeek",
            AiProvider::Qwen => "通义千问",
            AiProvider::Kimi => "Kimi",
            AiProvider::Zhipu => "智谱",
            AiProvider::OpenRouter => "OpenRouter",
            AiProvider::Custom => "自定义",
        }
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
        parsed.map(Rgb).ok_or_else(|| de::Error::custom(format!("颜色格式应为 \"#RRGGBB\"，实际为 \"{text}\"")))
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
        self.ai.pause_ms = self.ai.pause_ms.clamp(AI_PAUSES[0], AI_PAUSES[AI_PAUSES.len() - 1]);
        self
    }
}

/// 每页候选个数的范围。
pub const PAGE_SIZES: RangeInclusive<usize> = 5..=SELECT_KEYS.len();
/// 候选字号的范围（pt）。
pub const FONT_SIZES: RangeInclusive<f32> = 12.0..=36.0;
/// AI 预测选词的预测延迟（毫秒），设置窗口里能选的几档。设置文件里写别的数，收拢到最短和最长这两档之间。
pub const AI_PAUSES: [u64; 4] = [100, 200, 300, 500];

/// 设置文件不存在时写出的模板，内容就是默认值。
pub const SETTINGS_TEMPLATE: &str = r##"# Kyme 设置。也可在输入法菜单的「Kyme 设置…」中修改；保存后切换输入框即生效。

[input]
# 每页候选数（5–10），第 10 个候选用 0 键选择
page_size = 7
# 输入框内显示拼音，候选窗仅显示候选。false：拼音显示在候选窗第一行
inline_pinyin = true
# 显示 Emoji 候选：第一页末位显示一个与拼音匹配的 emoji
emoji = true
# 选词记忆：常用候选自动前移，分次选出的词组记为新词。数据仅保存在本机的 userdict.tsv
learn = true

[appearance]
# 候选字号（pt，12–36），其余文字和间距按比例缩放
font_size = 15
# 候选窗字体，留空则使用系统默认中文字体
font_family = ""
# 选中候选的高亮颜色（#RRGGBB）
highlight_color = "#5E5CE6"

[dicts]
# 雾凇扩展词库：约 34 万词，内存占用增加约 25MB。需先在设置窗口中下载
ext = false
# 腾讯词库：约 98 万词，内存占用增加约 65MB。需先在设置窗口中下载
tencent = false

[ai]
# AI 预测选词：停止输入时，将光标前的文字和候选整句发送给所选服务商的模型，由模型选出最通顺的一句，
# 置于首位并以 ✨ 标记
enabled = false
# 服务商：typesafe、anthropic、openai、deepseek、qwen（通义千问）、kimi、zhipu（智谱）、openrouter，
# 或 custom（自定义的 OpenAI 兼容接口）。Key 和模型在设置窗口中填写，保存在同目录的 ai.toml
provider = "typesafe"
# 预测延迟（毫秒，100–500）：停止输入后经过该时长再发送请求。越短 ✨ 出现越快，请求次数也越多
pause_ms = 100
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
            return Some(Err(format!("无法写入设置文件：{err}")));
        }
        let modified = disk::modified(&self.path);
        if modified.is_some() && modified == self.modified {
            return None;
        }
        self.modified = modified;
        let result = match fs::read_to_string(&self.path) {
            Ok(text) => Settings::from_toml(&text).map_err(|err| invalid("设置文件", &text, err.span(), err.message())),
            Err(err) => Err(format!("无法读取设置文件：{err}")),
        };
        Some(result)
    }

    /// 把设置写回文件。只改各项的值，文件里的注释和排版照旧；文件不存在时从模板开始。
    /// 文件写得不对（语法错，或者哪一项的值不对）时不写，等用户先在文件里改好。
    pub fn save(&mut self, settings: &Settings) -> Result<(), String> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(err) if err.kind() == io::ErrorKind::NotFound => SETTINGS_TEMPLATE.to_string(),
            Err(err) => return Err(format!("无法读取设置文件：{err}")),
        };
        // 有一项写错时输入法用的是整套默认设置，这时写回去会把其余写对的项也盖成默认值
        Settings::from_toml(&text).map_err(|err| invalid("设置文件", &text, err.span(), err.message()))?;
        let mut doc: DocumentMut =
            text.parse().map_err(|err: toml_edit::TomlError| invalid("设置文件", &text, err.span(), err.message()))?;
        // 每一节的每一项都写回去，以后加了设置项这里不用改
        let fresh = toml_edit::ser::to_document(settings).map_err(|err| format!("设置无法转换为 TOML：{err}"))?;
        for (section, table) in fresh.iter() {
            for (key, item) in table.as_table_like().into_iter().flat_map(|table| table.iter()) {
                if let Some(value) = item.as_value() {
                    set(&mut doc, section, key, integral(value.clone()))?;
                }
            }
        }

        disk::write_atomic(&self.path, doc.to_string(), false).map_err(|err| format!("无法写入设置文件：{err}"))?;
        // 自己写的不算改过，下次 reload 不重读
        self.modified = disk::modified(&self.path);
        Ok(())
    }

    fn write_template(&self) -> io::Result<()> {
        disk::write_atomic(&self.path, SETTINGS_TEMPLATE, false)
    }
}

/// TOML 的报错带着原文和箭头，有好几行。界面和日志只要一行：第几行，什么问题。
pub(crate) fn invalid(file: &str, text: &str, span: Option<Range<usize>>, message: &str) -> String {
    let line = span.and_then(|span| text.get(..span.start)).map_or(1, |before| before.matches('\n').count() + 1);
    let reason: Vec<&str> = message.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    format!("{file}第 {line} 行有误：{}", reason.join("，"))
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
        return Err(format!("设置文件中的 {section} 须为 [{section}] 节"));
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
        let pause = |ms: u64| Settings::from_toml(&format!("[ai]\npause_ms = {ms}")).unwrap().ai.pause_ms;
        assert_eq!((pause(20), pause(250), pause(5000)), (100, 250, 500), "停顿收拢到最短和最长两档之间");
    }

    #[test]
    fn ai_providers() {
        let parse = |s: &str| Settings::from_toml(&format!("[ai]\nprovider = \"{s}\""));
        assert_eq!(parse("deepseek").unwrap().ai.provider, AiProvider::DeepSeek);
        assert_eq!(parse("custom").unwrap().ai.provider, AiProvider::Custom);
        assert!(parse("gemini").is_err());
        for provider in AiProvider::ALL {
            assert_eq!(parse(provider.id()).unwrap().ai.provider, provider, "id 和设置文件里的写法一样");
        }
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
        assert!(err.starts_with("设置文件第 2 行有误：") && !err.contains('\n'), "{err}");

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
        settings.dicts.tencent = true;

        // 文件还不存在：从模板开始
        file.save(&settings).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 每页候选数"), "模板里的注释还在：{text}");
        assert!(text.contains("page_size = 9\n") && text.contains("font_size = 18\n"), "整数字号写成整数：{text}");
        assert!(text.contains("highlight_color = \"#34C759\""));
        assert!(text.contains("# 腾讯词库") && text.contains("tencent = true\n"), "{text}");
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
