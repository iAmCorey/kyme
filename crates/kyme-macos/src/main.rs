//! Kyme 的 macOS 输入法进程：objc2 接 InputMethodKit，GPUI 画候选窗。
//!
//! 同一个可执行文件也兼任安装工具：
//! - `Kyme --install [--select]`：向系统注册并启用输入法，可选地直接切换过去
//! - `Kyme --current-source` / `Kyme --select-source <id>`：查看、切换当前输入源
//! - `Kyme --qa`：自测模式，换上假的输入框跑输入会话，给 `cargo xtask qa` 用（见 qa.rs）

mod client;
mod controller;
mod install;
mod logger;
mod panel;
mod qa;
mod state;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use gpui::{App, Application};
use kyme_core::SettingsFile;
use kyme_core::pinyin::{Dictionary, Engine};
use objc2::rc::Retained;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::{NSBundle, NSString, ns_string};
use objc2_input_method_kit::IMKServer;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--install") => {
            let select = args.iter().any(|arg| arg == "--select");
            std::process::exit(install::install(select));
        }
        Some("--current-source") => std::process::exit(install::print_current()),
        Some("--select-source") => match args.get(1) {
            Some(id) => std::process::exit(install::select_source(id)),
            None => {
                eprintln!("用法：Kyme --select-source <输入源 ID>");
                std::process::exit(2);
            }
        },
        Some("--qa") => return qa::run(),
        _ => {}
    }

    logger::init();
    log::info!("Kyme 启动，pid {}", std::process::id());
    let loading = load_engine_in_background();
    Application::new().run(move |cx| {
        let settings = library_dir().unwrap_or_default().join("Application Support/Kyme/settings.toml");
        boot(cx, loading, SettingsFile::new(settings));
        match start_server() {
            // IMKServer 要在整个进程生命周期里一直存在
            Some(server) => {
                std::mem::forget(server);
                log::info!("IMKServer 就绪");
            }
            None => log::error!("IMKServer 创建失败"),
        }
    });
}

/// 词库在后台线程里读，同时初始化 GPUI 和候选窗。
fn load_engine_in_background() -> JoinHandle<Engine> {
    let dict_dir =
        NSBundle::mainBundle().resourcePath().map(|resources| PathBuf::from(resources.to_string()).join("dict"));
    std::thread::spawn(move || load_engine(dict_dir.as_deref()))
}

/// 输入法和自测模式共用的启动：设成后台 App，建候选窗，等词库读完，装好全局状态。
fn boot(cx: &mut App, loading: JoinHandle<Engine>, settings: SettingsFile) {
    let mtm = MainThreadMarker::new().expect("GPUI 的启动回调在主线程");
    // GPUI 启动时把激活策略设成 Regular（会出现 Dock 图标），输入法要改回来
    let _ = NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let panel = panel::PanelWindow::open(cx);
    let engine = Arc::new(loading.join().expect("读词库的线程崩溃了"));
    // 构建词库时的临时内存是在另一个线程分配的，释放后 malloc 不会主动还给系统
    // SAFETY: 传 NULL 表示处理全部 malloc zone，0 表示尽量多还
    unsafe { malloc_zone_pressure_relief(std::ptr::null_mut(), 0) };
    state::install(state::AppState::new(engine, panel, cx.to_async(), settings));
}

unsafe extern "C" {
    /// libmalloc：把空闲的脏页还给系统，返回还了多少字节。
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// `~/Library`
fn library_dir() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join("Library"))
}

/// 读 `dir` 下全部 `.dict.yaml`，按文件名排序。
fn load_engine(dir: Option<&Path>) -> Engine {
    let started = Instant::now();
    let mut paths: Vec<PathBuf> = dir
        .and_then(|dir| std::fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.to_string_lossy().ends_with(".dict.yaml"))
        .collect();
    paths.sort();
    let dict = Dictionary::load_rime(&paths).unwrap_or_else(|err| {
        log::error!("{err}");
        Dictionary::from_rime_sources(&[])
    });
    log::info!("词库 {} 条（{} 个文件），载入耗时 {:?}", dict.len(), paths.len(), started.elapsed());
    Engine::new(dict)
}

fn start_server() -> Option<Retained<IMKServer>> {
    controller::KymeInputController::register();
    let bundle = NSBundle::mainBundle();
    // 连接名必须和 Info.plist 里的 InputMethodConnectionName 一致，所以直接从那里读
    let name =
        bundle.objectForInfoDictionaryKey(ns_string!("InputMethodConnectionName"))?.downcast::<NSString>().ok()?;
    let bundle_id = bundle.bundleIdentifier()?;
    // SAFETY: 两个参数都是有效的 NSString，在主线程调用
    unsafe { IMKServer::initWithName_bundleIdentifier(IMKServer::alloc(), Some(&name), Some(&bundle_id)) }
}
