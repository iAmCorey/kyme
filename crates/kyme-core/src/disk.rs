//! 设置文件和用户词库共用的文件操作：看修改时间、先写临时文件再换上。

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 文件的修改时间。文件不在是 None。
pub(crate) fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

/// 先写到旁边的临时文件再换上，别的进程不会读到写了一半的文件。`private` 时只有自己能读写。
pub(crate) fn write_atomic(path: &Path, content: impl AsRef<[u8]>, private: bool) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp = OsString::from(path);
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&tmp)?.write_all(content.as_ref())?;
    fs::rename(&tmp, path)
}

/// 删掉文件。本来就没有也算删掉了。
pub(crate) fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
