//! 听风吟 BGM 的 GPUI 桌面端与后台推理进程入口。

mod actions;
mod api;
mod assets;
mod audio;
mod design;
mod desktop;
mod desktop_lyrics;
mod diagnostics;
mod icons;
mod library;
mod lyrics;
mod playback_queue;
mod scheduler;
mod settings;
mod text_input;
mod view;
mod wallpaper;
mod watch;

use gpui::{
    App, Application, Bounds, Focusable, KeyBinding, TitlebarOptions, WindowBounds, WindowOptions,
    point, prelude::*, px, size,
};

use crate::actions::{
    Backspace, Confirm, Copy, Cut, Delete, Dismiss, End, FocusSearch, Home, Left, Mute,
    OpenSettings, Paste, PlayPause, Quit, Right, SeekBack, SeekForward, SelectAll, SelectLeft,
    SelectRight, VolumeDown, VolumeUp,
};
use crate::view::Workspace;

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(library::WORKER_FLAG)) {
        if let Err(error) = library::run_asr_worker() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(library::separation::WORKER_FLAG))
    {
        let result = std::env::args_os()
            .nth(2)
            .ok_or_else(|| anyhow::anyhow!("缺少分离任务文件"))
            .and_then(|path| library::separation::run_worker(std::path::Path::new(&path)));
        if let Err(error) = result {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    Application::new()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            #[cfg(target_os = "macos")]
            desktop::set_unbundled_icon();
            bind_keys(cx);
            cx.on_action(|_: &Quit, cx| cx.quit());

            let bounds = Bounds::centered(None, size(px(1240.), px(820.)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("听风吟 · TFY BGM".into()),
                            appears_transparent: true,
                            traffic_light_position: Some(point(px(18.), px(27.))),
                        }),
                        window_min_size: Some(size(px(1020.), px(680.))),
                        ..Default::default()
                    },
                    |window, cx| {
                        let workspace = cx.new(|cx| Workspace::new(cx));
                        let title = workspace.read(cx).window_title();
                        window.set_window_title(&title);
                        window.focus(&workspace.focus_handle(cx));
                        workspace
                    },
                )
                .unwrap();
            window
                .update(cx, |workspace, window, cx| {
                    workspace.install_desktop(window, cx)
                })
                .ok();
            cx.activate(true);
        });
}

fn bind_keys(cx: &mut App) {
    let library = Some("Library && !TextInput && !Dialog");
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-enter", crate::actions::TrackPage, library),
        KeyBinding::new("cmd-shift-l", crate::actions::ToggleDesktopLyrics, library),
        KeyBinding::new("space", PlayPause, library),
        KeyBinding::new("left", SeekBack, library),
        KeyBinding::new("right", SeekForward, library),
        KeyBinding::new("up", VolumeUp, library),
        KeyBinding::new("down", VolumeDown, library),
        KeyBinding::new("m", Mute, library),
        KeyBinding::new("/", FocusSearch, Some("Library && !TextInput")),
        KeyBinding::new("cmd-,", OpenSettings, Some("Library && !Dialog")),
        KeyBinding::new("escape", Dismiss, Some("Library")),
        KeyBinding::new("enter", Confirm, Some("Dialog")),
        KeyBinding::new("enter", Confirm, Some("Library && TextInput && !Dialog")),
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("cmd-v", Paste, Some("TextInput")),
        KeyBinding::new("cmd-c", Copy, Some("TextInput")),
        KeyBinding::new("cmd-x", Cut, Some("TextInput")),
    ]);
}
