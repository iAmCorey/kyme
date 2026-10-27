//! 写到 ~/Library/Logs/Kyme/kyme.log 的简单日志。只记事件和耗时，不记输入内容。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

struct FileLogger {
    file: Mutex<File>,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        if let Ok(mut file) = self.file.lock() {
            let _ =
                writeln!(file, "{}.{:03} {:<5} {}", now.as_secs(), now.subsec_millis(), record.level(), record.args());
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

pub fn init() {
    let Some(dir) = crate::library_dir().map(|library| library.join("Logs/Kyme")) else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(dir.join("kyme.log")) else {
        return;
    };
    let logger = Box::leak(Box::new(FileLogger { file: Mutex::new(file) }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
