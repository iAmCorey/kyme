//! AI 预测选词各家服务商的 Key 和模型，存在设置文件旁边的 ai.toml，只有自己能读写。
//! 和设置文件分开放：设置文件常常要打开来看、发给别人看，Key 不该在里面。

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::disk;
use crate::settings::{AiProvider, invalid};

/// 和设置文件放在同一个目录。
const FILE_NAME: &str = "ai.toml";
/// 写文件时放在最上面的说明。
const HEADER: &str = "# Kyme 的 AI 预测选词：各家服务商的 API Key 和模型，在设置窗口里填。\n\
                      # 只有你自己能读写这个文件，别发给别人。\n\n";

/// 一家服务商的 Key 和模型。模型留空用这家默认的；接口地址只有自定义的那一家要填。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AiService {
    pub key: String,
    pub model: String,
    pub endpoint: String,
}

/// 什么都没填的那一家。
static EMPTY: AiService = AiService { key: String::new(), model: String::new(), endpoint: String::new() };

/// 每家各一份，按服务商在设置文件里的写法分开：换了服务商，原来那家填的都还留着。
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct AiServices(BTreeMap<String, AiService>);

impl AiServices {
    pub fn get(&self, provider: AiProvider) -> &AiService {
        self.0.get(provider.id()).unwrap_or(&EMPTY)
    }

    pub fn get_mut(&mut self, provider: AiProvider) -> &mut AiService {
        self.0.entry(provider.id().to_string()).or_default()
    }
}

/// 磁盘上的 ai.toml。输入法只读，设置窗口读写。
pub struct AiFile {
    path: PathBuf,
    /// 上次读的时候文件的修改时间，文件不在是 `Some(None)`。还没读过是 None。
    seen: Option<Option<SystemTime>>,
}

impl AiFile {
    /// 设置文件 `settings` 旁边的那一份。
    pub fn beside(settings: &Path) -> Self {
        AiFile { path: settings.with_file_name(FILE_NAME), seen: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 第一次调用、或文件改过之后，返回读到的内容，文件不在就是全空的；写得不对返回说明（一行）。没变返回 None。
    pub fn reload(&mut self) -> Option<Result<AiServices, String>> {
        let modified = disk::modified(&self.path);
        if self.seen == Some(modified) {
            return None;
        }
        self.seen = Some(modified);
        let result = match fs::read_to_string(&self.path) {
            Ok(text) => toml::from_str(&text).map_err(|err| invalid("ai.toml ", &text, err.span(), err.message())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AiServices::default()),
            Err(err) => Err(format!("读不了 ai.toml：{err}")),
        };
        Some(result)
    }

    /// 整个写回去，只有自己能读写。什么都没填的那几家不写。原来的文件写坏了就不写：这时手里的只是读出来的那部分，
    /// 写回去会把读不出来的那几家的 Key 冲掉，等用户先在文件里改好。
    pub fn save(&mut self, services: &AiServices) -> Result<(), String> {
        match fs::read_to_string(&self.path) {
            Ok(text) => {
                toml::from_str::<AiServices>(&text)
                    .map_err(|err| invalid("ai.toml ", &text, err.span(), err.message()))?;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("读不了 ai.toml：{err}")),
        }
        let filled: BTreeMap<&String, &AiService> =
            services.0.iter().filter(|(_, service)| **service != EMPTY).collect();
        let text = toml::to_string(&filled).map_err(|err| format!("AI 的设置转不成 TOML：{err}"))?;
        disk::write_atomic(&self.path, &format!("{HEADER}{text}"), true)
            .map_err(|err| format!("写不了 ai.toml：{err}"))?;
        // 自己写的不算改过，下次 reload 不重读
        self.seen = Some(disk::modified(&self.path));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn saves_privately_and_reloads_on_change() {
        let dir = std::env::temp_dir().join(format!("kyme-ai-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut file = AiFile::beside(&dir.join("settings.toml"));
        assert_eq!(file.path(), dir.join("ai.toml"));
        assert_eq!(file.reload(), Some(Ok(AiServices::default())), "文件不在是全空的");
        assert_eq!(file.reload(), None, "还是不在，不重读");

        let mut services = AiServices::default();
        services.get_mut(AiProvider::Custom).key = "sk-test".into();
        services.get_mut(AiProvider::Custom).endpoint = "http://localhost:11434/v1".into();
        services.get_mut(AiProvider::DeepSeek);
        file.save(&services).unwrap();
        assert!(!fs::read_to_string(file.path()).unwrap().contains("[deepseek]"), "空的不写");
        services.0.remove("deepseek");
        assert_eq!(file.reload(), None, "自己写的不算改过");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(file.path()).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(AiFile::beside(&dir.join("settings.toml")).reload(), Some(Ok(services.clone())));

        // 修改时间要和上次不同才算改过
        std::thread::sleep(Duration::from_millis(20));
        fs::write(file.path(), "[anthropic]\nkey = \"sk-ant\"\n").unwrap();
        let reloaded = file.reload().unwrap().unwrap();
        assert_eq!(reloaded.get(AiProvider::Anthropic).key, "sk-ant");
        assert_eq!(reloaded.get(AiProvider::Custom), &AiService::default());

        std::thread::sleep(Duration::from_millis(20));
        fs::write(file.path(), "[anthropic\n").unwrap();
        let err = file.reload().unwrap().unwrap_err();
        assert!(err.starts_with("ai.toml 第 1 行写得不对") && !err.contains('\n'), "{err}");
        assert!(file.save(&services).is_err(), "写坏了就不写");
        assert_eq!(fs::read_to_string(file.path()).unwrap(), "[anthropic\n", "原样留着");

        fs::remove_dir_all(&dir).unwrap();
    }
}
