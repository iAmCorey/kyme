//! 扩展词库：不随安装包分发。用户在设置窗口里点了下载，才从雾凇拼音的仓库取锁定版本的文件，
//! 核对 SHA-256 后存在设置文件旁边的 `extra/`；设置 `[dicts]` 里打开才载入。下载本身由 kyme-ai 做，
//! 这里只管有哪几个、从哪下、存在哪、下对了没有。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::DictSettings;
use crate::disk;

/// 雾凇拼音锁定的版本。和 `scripts/fetch-dict.sh` 读的是同一个文件，改版本只改那里，扩展词库的校验值跟着改。
pub const RIME_ICE_COMMIT: &str = include_str!("../../../data/rime-ice.commit").trim_ascii_end();

const DIR: &str = "extra";

/// 一个可以下载的扩展词库。
pub struct ExtraDict {
    /// 设置窗口里显示的名字。
    pub name: &'static str,
    /// 设置窗口里名字下面的说明。
    pub detail: &'static str,
    /// 在雾凇仓库里的路径，下载后存成同名文件。
    path: &'static str,
    /// 锁定版本的 SHA-256，下载的内容对不上就不存。
    sha256: &'static str,
    /// 设置里对应的开关。
    pub enabled: fn(&DictSettings) -> bool,
    pub set: fn(&mut DictSettings, bool),
}

pub static EXTRA_DICTS: [ExtraDict; 2] = [
    ExtraDict {
        name: "雾凇扩展词库",
        detail: "约 34 万词，下载约 12MB，内存占用增加约 25MB",
        path: "cn_dicts/ext.dict.yaml",
        sha256: "8e49a0294c0139e63c815cc33a2d438cc144eb7889297d30cec899a368697c06",
        enabled: |dicts| dicts.ext,
        set: |dicts, on| dicts.ext = on,
    },
    ExtraDict {
        name: "腾讯词库",
        detail: "约 98 万词，下载约 17MB，内存占用增加约 65MB",
        path: "cn_dicts/tencent.dict.yaml",
        sha256: "352239dc875343cd49a04d7e79a9b73c5c95ad54e7fe1f8608b30c855a942904",
        enabled: |dicts| dicts.tencent,
        set: |dicts, on| dicts.tencent = on,
    },
];

impl ExtraDict {
    /// 存下来的文件名，比如 `tencent.dict.yaml`。
    pub fn file(&self) -> &'static str {
        self.path.rsplit('/').next().unwrap_or(self.path)
    }

    /// 下载地址，按顺序试：GitHub，和国内也能访问的 jsDelivr 镜像。都锁定在同一个版本。
    pub fn urls(&self) -> [String; 2] {
        [
            format!("https://raw.githubusercontent.com/iDvel/rime-ice/{RIME_ICE_COMMIT}/{}", self.path),
            format!("https://cdn.jsdelivr.net/gh/iDvel/rime-ice@{RIME_ICE_COMMIT}/{}", self.path),
        ]
    }

    /// 内容是不是锁定的那一份。
    pub fn verify(&self, bytes: &[u8]) -> bool {
        format!("{:x}", Sha256::digest(bytes)) == self.sha256
    }
}

/// 设置文件旁边存扩展词库的目录 `extra/`。
pub struct ExtraDir {
    dir: PathBuf,
    /// 上次看时载入着的扩展词库，和各自文件的修改时间。
    loaded: HashMap<&'static str, SystemTime>,
}

impl ExtraDir {
    pub fn beside(settings: &Path) -> Self {
        ExtraDir { dir: settings.with_file_name(DIR), loaded: HashMap::new() }
    }

    /// 这个文件存在哪。
    pub fn path(&self, file: &str) -> PathBuf {
        self.dir.join(file)
    }

    /// 下载过了没有。
    pub fn has(&self, dict: &ExtraDict) -> bool {
        self.path(dict.file()).is_file()
    }

    /// 存下载到的内容。不是锁定的那一份就不存，返回原因。
    pub fn save(&self, dict: &ExtraDict, bytes: &[u8]) -> Result<(), String> {
        if !dict.verify(bytes) {
            return Err("下载的文件校验失败".into());
        }
        disk::write_atomic(&self.path(dict.file()), bytes, false).map_err(|err| format!("无法保存词库：{err}"))
    }

    pub fn remove(&self, dict: &ExtraDict) -> io::Result<()> {
        disk::remove(&self.path(dict.file()))
    }

    /// 和上次比，哪些扩展词库要载入：打开了、下载了或重新下载了的，带上文件路径；哪些要卸掉：关了或删了的，
    /// 路径是 None。打开了但还没下载的不算载入。没变的不列出来。
    pub fn reload(&mut self, dicts: &DictSettings) -> Vec<(&'static str, Option<PathBuf>)> {
        let mut changes = Vec::new();
        for dict in &EXTRA_DICTS {
            let path = self.path(dict.file());
            let now = if (dict.enabled)(dicts) { disk::modified(&path) } else { None };
            if now == self.loaded.get(dict.file()).copied() {
                continue;
            }
            match now {
                Some(modified) => self.loaded.insert(dict.file(), modified),
                None => self.loaded.remove(dict.file()),
            };
            changes.push((dict.file(), now.map(|_| path)));
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;

    #[test]
    fn each_dict_has_its_own_switch() {
        let names: Vec<&str> = EXTRA_DICTS.iter().map(ExtraDict::file).collect();
        assert_eq!(names, ["ext.dict.yaml", "tencent.dict.yaml"]);
        for (i, dict) in EXTRA_DICTS.iter().enumerate() {
            let mut dicts = DictSettings::default();
            (dict.set)(&mut dicts, true);
            let on: Vec<bool> = EXTRA_DICTS.iter().map(|other| (other.enabled)(&dicts)).collect();
            assert_eq!(on.iter().filter(|&&on| on).count(), 1, "{} 的开关只打开它自己", dict.name);
            assert!(on[i]);
            assert!(dict.urls().iter().all(|url| url.contains(RIME_ICE_COMMIT)), "地址锁定在同一个版本");
        }
    }

    #[test]
    fn only_the_pinned_file_is_saved() {
        let dir = std::env::temp_dir().join(format!("kyme-extra-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let extra = ExtraDir::beside(&dir.join("settings.toml"));
        let dict = &EXTRA_DICTS[0];
        assert!(extra.save(dict, "改过的内容".as_bytes()).is_err());
        assert!(!extra.has(dict), "校验不过不存");
        assert!(extra.remove(dict).is_ok(), "没有也算删掉了");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reload_follows_switches_and_downloads() {
        let dir = std::env::temp_dir().join(format!("kyme-extra-reload-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut extra = ExtraDir::beside(&dir.join("settings.toml"));
        let on = DictSettings { tencent: true, ..DictSettings::default() };
        let tencent = extra.path("tencent.dict.yaml");
        assert!(extra.reload(&on).is_empty(), "打开了但还没下载，不载入");

        fs::create_dir_all(dir.join(DIR)).unwrap();
        fs::write(&tencent, "止逆阀\t100\n").unwrap();
        assert_eq!(extra.reload(&on), [("tencent.dict.yaml", Some(tencent.clone()))], "下载了，开关早就开着也载入");
        assert!(extra.reload(&on).is_empty(), "没变就不重建");

        // 修改时间要和上次不同才算重新下载了
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&tencent, "止逆阀\t100\n").unwrap();
        assert_eq!(extra.reload(&on), [("tencent.dict.yaml", Some(tencent))], "重新下载了，重建");
        assert_eq!(extra.reload(&DictSettings::default()), [("tencent.dict.yaml", None)], "关了就卸掉");
        fs::remove_dir_all(&dir).unwrap();
    }
}
