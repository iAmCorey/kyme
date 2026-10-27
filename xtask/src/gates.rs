//! 发版门槛，写在 qa/gates.toml 里。

use std::fs;
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gates {
    pub engine: EngineGates,
    pub macos: MacosGates,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineGates {
    /// 评测集里单条拼音转换的最长耗时（毫秒）。
    pub max_convert_ms: f64,
    /// 词库载入耗时（毫秒）。
    pub max_load_ms: f64,
    /// 常用音节两两拼接后逐字母打，拼音被改掉的输入最多占多少（%）。
    pub max_altered_percent: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct MacosGates {
    /// 处理一个按键（会话、接入代码、更新候选窗）耗时的 p95（毫秒）。
    pub max_key_p95_ms: f64,
    /// 输入法进程（自测模式）的常驻内存（MB）。
    pub max_rss_mb: f64,
}

impl Gates {
    pub fn load(path: &Path) -> Result<Gates, String> {
        let text = fs::read_to_string(path).map_err(|err| format!("读不了 {}：{err}", path.display()))?;
        toml::from_str(&text).map_err(|err| format!("{} 写得不对：{err}", path.display()))
    }
}
