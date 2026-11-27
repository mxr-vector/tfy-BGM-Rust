//! 听风吟 BGM 的 GPUI 桌面端与后台推理进程入口。

mod actions;
mod api;
mod assets;
mod audio;
mod audio_index;
mod design;
mod desktop;
mod desktop_lyrics;
mod diagnostics;
mod icons;
mod library;
mod lyrics;
mod microphone;
mod playback_queue;
mod playlist_migration;
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
    if std::env::args_os().nth(1).as_deref()
        == Some(std::ffi::OsStr::new(library::tts::WORKER_FLAG))
    {
        if let Err(error) = library::tts::run_worker() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    let mut instance = match desktop::SingleInstance::acquire(&library::config_dir()) {
        Ok(desktop::InstanceOutcome::Primary(guard)) => Some(guard),
        Ok(desktop::InstanceOutcome::AlreadyRunning) => return,
        Err(error) => {
            eprintln!("单实例初始化失败：{error}");
            None
        }
    };
    let wake_rx = instance
        .as_mut()
        .and_then(desktop::SingleInstance::take_receiver);
    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            #[cfg(target_os = "macos")]
            desktop::set_unbundled_icon();
            bind_keys(cx);
            install_menus(cx);
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
                    workspace.install_desktop(window, cx);
                    if let Some(mut wake_rx) = wake_rx {
                        let mut context = window.to_async(cx);
                        let weak = cx.entity().downgrade();
                        cx.spawn(async move |_, _| {
                            while wake_rx.recv().await.is_some() {
                                let _ = context.update(|window, cx| {
                                    desktop::unhide_and_focus_app();
                                    let _ = weak.update(cx, |_, cx| {
                                        cx.activate(true);
                                        window.activate_window();
                                        cx.notify();
                                    });
                                });
                            }
                        })
                        .detach();
                    }
                })
                .ok();
            cx.activate(true);
        });
    drop(instance);
}

fn bind_keys(cx: &mut App) {
    let library = Some("Library && !TextInput && !Dialog && !Control");
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new(
            "cmd-enter",
            crate::actions::TrackPage,
            Some("Library && !Dialog"),
        ),
        KeyBinding::new(
            "cmd-shift-l",
            crate::actions::ToggleDesktopLyrics,
            Some("Library && !Dialog"),
        ),
        KeyBinding::new("space", PlayPause, library),
        KeyBinding::new("left", SeekBack, library),
        KeyBinding::new("right", SeekForward, library),
        KeyBinding::new("up", VolumeUp, library),
        KeyBinding::new("down", VolumeDown, library),
        KeyBinding::new("m", Mute, library),
        KeyBinding::new("/", FocusSearch, Some("Library && !TextInput && !Dialog")),
        KeyBinding::new("cmd-f", FocusSearch, Some("Library && !Dialog")),
        KeyBinding::new(
            "cmd-o",
            crate::actions::ImportMusic,
            Some("Library && !Dialog"),
        ),
        KeyBinding::new(
            "cmd-r",
            crate::actions::RescanLibrary,
            Some("Library && !Dialog"),
        ),
        KeyBinding::new(
            "alt-cmd-u",
            crate::actions::ToggleQueue,
            Some("Library && !Dialog"),
        ),
        KeyBinding::new("cmd-,", OpenSettings, Some("Library && !Dialog")),
        KeyBinding::new("escape", Dismiss, Some("Library")),
        KeyBinding::new("tab", crate::actions::FocusNext, Some("Library")),
        KeyBinding::new("shift-tab", crate::actions::FocusPrevious, Some("Library")),
        KeyBinding::new("enter", Confirm, Some("Dialog && !Control")),
        KeyBinding::new("enter", Confirm, Some("TextInput")),
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

fn install_menus(cx: &mut App) {
    use crate::actions as action;
    use gpui::{Menu, MenuItem, OsAction};

    cx.set_menus(vec![
        Menu {
            name: "听风吟".into(),
            items: vec![
                MenuItem::action("设置…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("退出听风吟", Quit),
            ],
        },
        Menu {
            name: "文件".into(),
            items: vec![
                MenuItem::action("导入音乐…", action::ImportMusic),
                MenuItem::action("管理音乐文件夹…", action::OpenFolders),
                MenuItem::action("重新扫描曲库", action::RescanLibrary),
            ],
        },
        Menu {
            name: "编辑".into(),
            items: vec![
                MenuItem::os_action("剪切", Cut, OsAction::Cut),
                MenuItem::os_action("复制", Copy, OsAction::Copy),
                MenuItem::os_action("粘贴", Paste, OsAction::Paste),
                MenuItem::os_action("全选", SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("搜索曲库", FocusSearch),
            ],
        },
        Menu {
            name: "前往".into(),
            items: vec![
                MenuItem::action("音乐库", action::OpenLibrary),
                MenuItem::action("我的收藏", action::OpenFavorites),
                MenuItem::action("节目单", action::OpenSchedule),
                MenuItem::separator(),
                MenuItem::action("字幕生成", action::OpenSubtitles),
                MenuItem::action("音频工具", action::OpenAudioTools),
                MenuItem::action("歌单迁移", action::OpenPlaylistMigration),
                MenuItem::action("文生语音", action::OpenSpeech),
            ],
        },
        Menu {
            name: "播放".into(),
            items: vec![
                MenuItem::action("播放或暂停", PlayPause),
                MenuItem::action("上一首", action::PreviousTrack),
                MenuItem::action("下一首", action::NextTrack),
                MenuItem::separator(),
                MenuItem::action("显示或收起播放页", action::TrackPage),
                MenuItem::action("显示或收起播放队列", action::ToggleQueue),
                MenuItem::action("显示或隐藏桌面歌词", action::ToggleDesktopLyrics),
            ],
        },
    ]);
}
