//! 桌面集成：托盘图标与最小化行为。
//!
//! 开机自动启动与 GPU 渲染后端属于平台专有逻辑，分别由
//! `platform::set_autostart` 与 `platform::apply_gpu_acceleration` 提供，
//! 本模块只负责托管托盘状态与窗口事件：
//! - 最小化托盘：启用后窗口最小化或系统关闭（Alt+F4 / 任务栏）时隐藏到托盘，
//!   托盘图标左键恢复窗口；
//! - 窗口销毁：主窗口真正关闭时一并关闭歌词浮窗，让进程正常退出。

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};

const TRAY_ID: &str = "tfy-bgm-tray";

/// 桌面集成的运行时开关，由偏好写入，窗口事件与托盘回调读取。
pub struct DesktopState {
    minimize_to_tray: AtomicBool,
    /// 应用正在退出：退出过程会关闭所有窗口，此时关闭事件不代表用户意图。
    quitting: AtomicBool,
    tray: Mutex<Option<TrayIcon>>,
}

impl DesktopState {
    pub fn new(minimize_to_tray: bool) -> Self {
        Self {
            minimize_to_tray: AtomicBool::new(minimize_to_tray),
            quitting: AtomicBool::new(false),
            tray: Mutex::new(None),
        }
    }

    pub fn minimize_to_tray(&self) -> bool {
        self.minimize_to_tray.load(Ordering::Relaxed)
    }

    /// 标记进入退出流程；之后的窗口关闭事件不应再回写界面偏好。
    pub fn begin_quit(&self) {
        self.quitting.store(true, Ordering::Relaxed);
    }

    pub fn quitting(&self) -> bool {
        self.quitting.load(Ordering::Relaxed)
    }
}

/// 应用启动时应用一次桌面集成配置：注册窗口事件、按需创建托盘图标，
/// 并把开机启动入口与偏好对齐（可执行文件路径变化后自动更新）。
pub fn install(app: &AppHandle, autostart: bool, minimize_to_tray: bool) -> Result<()> {
    attach_window_events(app);
    let tray = sync_tray(app, minimize_to_tray);
    if tray.is_err() {
        // 托盘不可用时不能继续「隐藏窗口」，否则用户没有任何入口把它恢复出来。
        app.state::<DesktopState>()
            .minimize_to_tray
            .store(false, Ordering::Relaxed);
    }
    tray.and(crate::platform::set_autostart(autostart))
}

/// 界面保存「常规」设置后调用；任一步失败都会回滚托盘状态，避免界面显示与实际行为不一致。
///
/// 开机启动写注册表 / autostart 文件失败时，前端会撤销刚切换的开关；
/// 因此这里必须把托盘与内存开关一并恢复，否则会出现「界面已关闭、系统仍在托盘运行」。
pub fn apply(
    app: &AppHandle,
    state: &DesktopState,
    autostart: bool,
    minimize_to_tray: bool,
) -> Result<()> {
    let previous = state.minimize_to_tray();
    state
        .minimize_to_tray
        .store(minimize_to_tray, Ordering::Relaxed);
    if let Err(error) = sync_tray(app, minimize_to_tray) {
        rollback_tray(app, state, previous);
        return Err(error);
    }
    if let Err(error) = crate::platform::set_autostart(autostart) {
        rollback_tray(app, state, previous);
        return Err(error);
    }
    Ok(())
}

fn rollback_tray(app: &AppHandle, state: &DesktopState, previous: bool) {
    state.minimize_to_tray.store(previous, Ordering::Relaxed);
    let _ = sync_tray(app, previous);
}

/// 托盘图标与「最小化托盘」开关保持同步：关闭时不留下无用的托盘图标。
fn sync_tray(app: &AppHandle, enabled: bool) -> Result<()> {
    let state = app.state::<DesktopState>();
    let mut tray = state.tray.lock();
    if enabled {
        if tray.is_none() {
            *tray = Some(build_tray(app)?);
        }
    } else {
        *tray = None;
    }
    Ok(())
}

fn build_tray(app: &AppHandle) -> Result<TrayIcon> {
    let show = MenuItem::with_id(app, "show", "显示主界面", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let icon = app
        .default_window_icon()
        .cloned()
        .context("缺少应用窗口图标，无法创建托盘图标")?;

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("听风吟 · BGM")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => {
                app.state::<DesktopState>().begin_quit();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(tray)
}

/// 窗口事件：最小化时按设置隐藏到托盘；开启「最小化托盘」时，
/// Alt+F4 / 任务栏关闭等系统关闭也隐藏到托盘，保证后台继续播放。
fn attach_window_events(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let handle = app.clone();
    let target = window.clone();
    window.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            if handle.state::<DesktopState>().minimize_to_tray() {
                api.prevent_close();
                let _ = target.hide();
            }
        }
        // 主窗口真正销毁（未隐藏到托盘）意味着应用要退出：Tauri 只在所有窗口
        // 都销毁时才退出，桌面上遗留的歌词浮窗会让进程继续存活，且默认无托盘
        // 图标可恢复主界面。这里把歌词浮窗一并关闭，让退出流程正常收尾。
        WindowEvent::Destroyed => {
            let Some(state) = handle.try_state::<DesktopState>() else {
                return;
            };
            if state.quitting() {
                return;
            }
            state.begin_quit();
            if let Some(lyrics) = handle.get_webview_window(crate::lyrics::LYRICS_WINDOW_LABEL) {
                let _ = lyrics.close();
            }
        }
        WindowEvent::Resized(_) => {
            let state = handle.state::<DesktopState>();
            if state.minimize_to_tray() && target.is_minimized().unwrap_or(false) {
                let _ = target.hide();
            }
        }
        _ => {}
    });
}

/// 「关闭窗口」提示中选择最小化托盘：先确保托盘图标存在，再隐藏主窗口。
///
/// 与「最小化托盘」设置无关：只要用户主动选择最小化托盘，就必须留下恢复窗口的入口。
pub fn hide_to_tray(app: &AppHandle) -> Result<()> {
    sync_tray(app, true)?;
    // 与「常规 · 最小化托盘」联动：选择关闭到托盘即视为开启该设置，
    // 恢复窗口后最小化按钮也走托盘，重启后仍保留托盘图标。
    app.state::<DesktopState>()
        .minimize_to_tray
        .store(true, Ordering::Relaxed);
    let Some(window) = app.get_webview_window("main") else {
        bail!("主窗口不存在");
    };
    window.hide().context("隐藏主窗口失败")
}

fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let _ = window.show();
    let _ = window.unminimize();
    let _ = window.set_focus();
}
