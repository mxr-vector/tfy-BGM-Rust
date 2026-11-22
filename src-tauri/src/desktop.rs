//! 桌面集成：开机自动启动、最小化到托盘与 GPU 加速开关。
//!
//! 这三项都属于「基本系统配置 · 常规」，偏好保存在 `prefs.json`：
//! - 开机自动启动：Windows 写注册表 Run 项，Linux 写 `~/.config/autostart`，
//!   macOS 写 LaunchAgent；开关立即生效，不需要重启应用；
//! - 最小化托盘：启用后窗口最小化或系统关闭（Alt+F4 / 任务栏）时隐藏到托盘，
//!   托盘图标左键恢复窗口；
//! - GPU 加速：WebView 的渲染后端只能在创建前确定，因此开关写盘后需要重启应用，
//!   启动阶段由 [`apply_gpu_acceleration`] 读取并注入对应的环境变量。

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WindowEvent};

const TRAY_ID: &str = "tfy-bgm-tray";
const AUTOSTART_KEY: &str = "tfy-bgm";

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

/// 当前操作系统标识，随 `bootstrap` 下发，界面据此显示平台特有配置。
pub fn platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

/// 应用启动时应用一次桌面集成配置：注册窗口事件、按需创建托盘图标，
/// 并把注册表 / autostart 文件里的入口与偏好对齐（可执行文件路径变化后自动更新）。
pub fn install(app: &AppHandle, autostart: bool, minimize_to_tray: bool) -> Result<()> {
    attach_window_events(app);
    let tray = sync_tray(app, minimize_to_tray);
    if tray.is_err() {
        // 托盘不可用时不能继续「隐藏窗口」，否则用户没有任何入口把它恢复出来。
        app.state::<DesktopState>()
            .minimize_to_tray
            .store(false, Ordering::Relaxed);
    }
    tray.and(set_autostart(autostart))
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
    if let Err(error) = set_autostart(autostart) {
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

// ---------------- 开机自动启动 ----------------

fn set_autostart(enabled: bool) -> Result<()> {
    let exe = std::env::current_exe().context("无法定位应用可执行文件")?;
    platform_autostart(enabled, &exe)
}

#[cfg(windows)]
fn platform_autostart(enabled: bool, exe: &std::path::Path) -> Result<()> {
    use windows_registry::CURRENT_USER;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    let key = CURRENT_USER
        .create(RUN_KEY)
        .map_err(|error| anyhow::anyhow!("无法打开开机启动注册表项：{error}"))?;
    if enabled {
        let command = format!("\"{}\"", exe.display());
        key.set_string(AUTOSTART_KEY, command)
            .map_err(|error| anyhow::anyhow!("写入开机启动注册表项失败：{error}"))?;
    } else {
        // 值不存在时删除会返回错误，直接忽略。
        let _ = key.remove_value(AUTOSTART_KEY);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_autostart(enabled: bool, exe: &std::path::Path) -> Result<()> {
    let path = autostart_dir()
        .context("无法定位 autostart 目录：缺少 XDG_CONFIG_HOME 与 HOME")?
        .join(format!("{AUTOSTART_KEY}.desktop"));
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => anyhow::bail!("关闭开机自动启动失败：{error}"),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("创建 autostart 目录失败")?;
    }
    let content = format!(
        "[Desktop Entry]\nType=Application\nName=听风吟 · BGM\nExec=\"{}\"\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::write(&path, content).context("写入 autostart 桌面项失败")
}

#[cfg(target_os = "macos")]
fn platform_autostart(enabled: bool, exe: &std::path::Path) -> Result<()> {
    let path = launch_agent_path().context("无法定位 LaunchAgents 目录")?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => anyhow::bail!("关闭开机自动启动失败：{error}"),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("创建 LaunchAgents 目录失败")?;
    }
    let content = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>com.yuanjie.tfybgm</string>\n  <key>ProgramArguments</key>\n  <array>\n    <string>{}</string>\n  </array>\n  <key>RunAtLoad</key>\n  <true/>\n</dict>\n</plist>\n",
        exe.display()
    );
    std::fs::write(&path, content).context("写入 LaunchAgent 文件失败")
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn platform_autostart(_enabled: bool, _exe: &std::path::Path) -> Result<()> {
    anyhow::bail!("当前平台不支持开机自动启动")
}

#[cfg(target_os = "linux")]
fn autostart_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|base| base.join("autostart"))
}

#[cfg(target_os = "macos")]
fn launch_agent_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/LaunchAgents"))
        .map(|dir| dir.join("com.yuanjie.tfybgm.plist"))
}

// ---------------- GPU 加速 ----------------

/// 在创建 WebView 之前设置渲染后端开关；关闭加速时向窗口配置追加对应参数。
///
/// wry 创建 WebView2 环境时总会显式写入 additional browser arguments，环境变量会被忽略，
/// 因此这里改的是 `Context` 里的窗口配置（必须发生在 `Builder::run` 之前）。
pub fn apply_gpu_acceleration<R: tauri::Runtime>(context: &mut tauri::Context<R>, enabled: bool) {
    #[cfg(windows)]
    {
        if enabled {
            return;
        }
        // wry 的默认参数；显式写入 additionalBrowserArgs 后需要一并保留。
        const DEFAULT_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";
        for window in context.config_mut().app.windows.iter_mut() {
            let args = window
                .additional_browser_args
                .get_or_insert_with(|| DEFAULT_ARGS.to_owned());
            if args.contains("--disable-gpu") {
                continue;
            }
            if !args.is_empty() {
                args.push(' ');
            }
            args.push_str("--disable-gpu");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = context;
        if !enabled {
            unsafe { std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1") };
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = context;
        let _ = enabled;
    }
}
