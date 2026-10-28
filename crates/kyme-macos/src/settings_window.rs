//! 设置窗口：单独的进程（`Kyme --settings`），崩了也不影响打字。
//!
//! 输入法菜单里的「Kyme 设置…」和再打开一次 Kyme.app 都会打开它。已经开着的话，只叫它到最前面，
//! 不再拉起新进程。改动直接写进设置文件，输入法切换输入框时读到。

use std::cell::RefCell;
use std::fs::{self, File, TryLockError};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::Command;
use std::thread;

use dispatch2::DispatchQueue;
use gpui::{
    App, AppContext, Application, AsyncApp, Bounds, KeyBinding, Menu, MenuItem, Pixels, TitlebarOptions, WindowBounds,
    WindowHandle, WindowOptions, actions, point, px,
};
use kyme_core::{LocalDicts, SettingsFile};
use kyme_ui::SettingsView;
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;

use crate::panel::DEFAULT_FONT;

actions!(kyme, [Quit]);

thread_local! {
    /// 开着的设置窗口。别的进程叫它到最前面时用。
    static WINDOW: RefCell<Option<(AsyncApp, WindowHandle<SettingsView>)>> = const { RefCell::new(None) };
}

/// 在输入法进程里调用：打开设置窗口。
pub fn open() {
    if show_existing() {
        return;
    }
    match std::env::current_exe().and_then(|exe| Command::new(exe).arg("--settings").spawn()) {
        // 等它退出，免得留下僵尸进程
        Ok(mut child) => {
            thread::spawn(move || child.wait());
        }
        Err(err) => log::error!("打不开设置窗口：{err}"),
    }
}

/// `Kyme --settings`：显示设置窗口，关掉就退出。
pub fn run(path: PathBuf) {
    crate::logger::init();
    // 抢设置窗口的锁：抢到的开窗口；别的设置窗口拿着锁，就叫它到最前面，自己退出。
    // 进程退出（包括崩溃）时锁自动放开。锁文件打不开就照样开窗口，只是不保证只有一个
    let lock = File::create(std::env::temp_dir().join("kyme-settings.lock"));
    if let Ok(lock) = &lock
        && let Err(TryLockError::WouldBlock) = lock.try_lock()
    {
        show_existing();
        return;
    }
    // 拿到了锁，就没有别的设置窗口在听，留下的文件是上一个没清理的
    let socket = socket();
    let _ = fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).inspect_err(|err| log::warn!("设置窗口没法单开：{err}")).ok();

    Application::new().run(move |cx| {
        cx.on_action(|_: &Quit, cx| cx.quit());
        // 只有一个窗口，关窗口就是退出
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None), KeyBinding::new("cmd-w", Quit, None)]);
        kyme_ui::bind_text_field_keys(cx);
        cx.set_menus(vec![Menu { name: "Kyme".into(), items: vec![MenuItem::action("退出 Kyme 设置", Quit)] }]);
        let Some(handle) = open_window(cx, path) else {
            cx.quit();
            return;
        };
        WINDOW.set(Some((cx.to_async(), handle)));
        if let Some(listener) = listener {
            thread::spawn(move || {
                for _ in listener.incoming() {
                    DispatchQueue::main().exec_async(show);
                }
            });
        }
        cx.on_window_closed(|cx| cx.quit()).detach();
        cx.activate(true);
        log::info!("设置窗口打开");
    });
}

/// 开着的设置窗口在这里等别的进程叫它。
fn socket() -> PathBuf {
    std::env::temp_dir().join("kyme-settings.sock")
}

/// 设置窗口已经开着的话，叫它到最前面。连上就够了：那边一收到连接就把窗口提到前面，不用再写什么。
fn show_existing() -> bool {
    UnixStream::connect(socket()).is_ok()
}

fn open_window(cx: &mut App, path: PathBuf) -> Option<WindowHandle<SettingsView>> {
    // 先读一遍设置和导入过几个词库，按它定窗口大小：预览的高度跟着字号变，每个词库一行
    let settings = SettingsFile::new(path.clone()).reload().and_then(Result::ok).unwrap_or_default();
    let mut dicts = LocalDicts::beside(&path);
    let max_height = max_height();
    let size = SettingsView::size_for(&settings, dicts.imported().len(), max_height);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size, cx))),
        titlebar: Some(TitlebarOptions {
            title: Some("Kyme 设置".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.0), px(10.0))),
        }),
        is_resizable: false,
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        // 不在安装包里跑（target 下的可执行文件）时读不到词库，自定义词库页就只显示写的拼音
        let file = SettingsFile::new(path);
        cx.new(|cx| SettingsView::new(file, dicts, DEFAULT_FONT, crate::bundled_dict_dir(), max_height, window, cx))
    })
    .inspect_err(|err| log::error!("打不开设置窗口：{err}"))
    .ok()
}

/// 设置窗口最高多高：主屏幕扣掉菜单栏和 Dock 的高度。窗口开在主屏幕中间。
fn max_height() -> Pixels {
    let mtm = MainThreadMarker::new().expect("设置窗口在主线程上开");
    px(NSScreen::mainScreen(mtm).map_or(f32::MAX, |screen| screen.visibleFrame().size.height as f32))
}

/// 别的进程叫这个窗口到最前面。
fn show() {
    WINDOW.with_borrow(|window| {
        if let Some((app, handle)) = window {
            let _ = app.update(|cx| {
                cx.activate(true);
                let _ = handle.update(cx, |_, window, _| window.activate_window());
            });
        }
    });
}
