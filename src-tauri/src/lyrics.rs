//! 「桌面歌词」：独立透明悬浮窗口 + 主窗口播放状态同步。
//!
//! Rust 侧持有权威状态（开关与最新快照），负责创建 / 关闭歌词窗口；主窗口把
//! 当前曲目、播放状态、进度与歌词行推给 Rust，Rust 广播给歌词窗口渲染。
//! 歌词窗口在启动时会主动拉取一次快照，避免错过创建前的最后一次广播。
//!
//! 置顶不是一次性的 builder 标志：全屏应用抢焦点、虚拟桌面切换都可能把 topmost
//! 窗口降级。因此创建后二次确认、窗口失焦时重新声明，并暴露 `enforce_top_most`
//! 供歌词窗口定时巡检。
//!
//! Wayland 下不再使用 gtk-layer-shell：实测 WebKitGTK 在 layer surface 上不会提交
//! 任何 buffer（窗口完全空白）。歌词窗与设置窗因此都是普通 xdg 窗口：内容能正常
//! 渲染，但合成器不允许客户端定位或强制置顶，窗口位置与 Z 序由桌面环境决定。

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Size, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent, webview::PageLoadEvent,
};

use crate::desktop::DesktopState;
use crate::state::AppState;

/// 歌词窗口 label；需与 `capabilities/default.json` 中的 windows 列表一致。
pub const LYRICS_WINDOW_LABEL: &str = "desktop-lyrics";
/// 开关状态广播（窗口被手动关闭时也会推送 `enabled: false`）。
pub const LYRICS_CONTROL_EVENT: &str = "desktop-lyrics-control";
/// 歌词数据与播放进度广播。
pub const LYRICS_DATA_EVENT: &str = "desktop-lyrics-data";
/// 窗口逻辑尺寸变化（设置面板调整或用户拖拽边缘）广播，前端据此记忆尺寸。
pub const LYRICS_SIZE_EVENT: &str = "desktop-lyrics-size";
/// 锁定态下鼠标是否进入歌词热区；前端据此显示 / 隐藏「解锁」入口。
pub const LYRICS_HOVER_EVENT: &str = "desktop-lyrics-hover";
/// 显示设置悬浮窗 label；需与 `capabilities/default.json` 中的 windows 列表一致。
pub const LYRICS_SETTINGS_LABEL: &str = "desktop-lyrics-settings";
/// 显示设置悬浮窗开关状态广播；Aa 按钮据此高亮。
pub const LYRICS_SETTINGS_EVENT: &str = "desktop-lyrics-settings-state";
/// 设置悬浮窗的逻辑像素尺寸（内容完整显示，不滚动）；高度需容纳全部控件。
const LYRICS_SETTINGS_WIDTH: f64 = 380.0;
/// 高度需容纳全部控件（含「已唱标记提前」档位），避免设置窗内出现滚动条。
const LYRICS_SETTINGS_HEIGHT: f64 = 460.0;
/// 设置悬浮窗与歌词条之间的间距。
const LYRICS_SETTINGS_GAP: f64 = 8.0;

/// 是否运行在原生 Wayland 后端；XWayland 仍走普通窗口逻辑。
#[cfg(target_os = "linux")]
fn is_native_wayland() -> bool {
    let has_wayland_display =
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let backend = std::env::var("GDK_BACKEND").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    has_wayland_display && backend != "x11" && (backend == "wayland" || session == "wayland")
}

#[cfg(not(target_os = "linux"))]
fn is_native_wayland() -> bool {
    false
}

/// 窗口默认 / 允许的**逻辑像素**尺寸；与前端 `LYRICS_SIZE_LIMITS` 保持一致。
pub const LYRICS_DEFAULT_WIDTH: f64 = 760.0;
pub const LYRICS_DEFAULT_HEIGHT: f64 = 120.0;
pub const LYRICS_MIN_WIDTH: f64 = 420.0;
pub const LYRICS_MIN_HEIGHT: f64 = 120.0;
pub const LYRICS_MAX_WIDTH: f64 = 1600.0;
pub const LYRICS_MAX_HEIGHT: f64 = 900.0;
/// 默认位置距屏幕工作区底边的间距（逻辑像素）。
pub const LYRICS_BOTTOM_MARGIN: f64 = 32.0;

/// 一行歌词（起止秒 + 文本），与字幕分段一一对应。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LyricLine {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 主窗口推送的播放 / 歌词快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsSnapshot {
    pub track_key: Option<String>,
    pub title: String,
    pub playing: bool,
    pub position: f64,
    /// 播放倍速；歌词窗口在两次快照之间按同一速率本地推进，倍速播放时高亮才不会漂移。
    #[serde(default = "default_rate")]
    pub rate: f64,
    pub lines: Vec<LyricLine>,
    /// 当前曲目是否已有字幕；为 false 时歌词窗口显示引导提示。
    pub has_lyrics: bool,
}

fn default_rate() -> f64 {
    1.0
}

impl Default for LyricsSnapshot {
    fn default() -> Self {
        Self {
            track_key: None,
            title: String::new(),
            playing: false,
            position: 0.0,
            rate: 1.0,
            lines: Vec::new(),
            has_lyrics: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsControl {
    pub enabled: bool,
}

/// 歌词窗口启动时的初始数据。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsBootstrap {
    pub enabled: bool,
    pub snapshot: LyricsSnapshot,
}

/// 窗口当前逻辑尺寸；广播给前端用于记住用户手动拖拽后的大小。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsWindowSize {
    pub width: f64,
    pub height: f64,
}

/// 锁定态歌词热区（窗口内逻辑像素）；由歌词窗口在布局变化后上报。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsHotZone {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl LyricsHotZone {
    fn normalized(self) -> Option<Self> {
        if self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
        {
            Some(self)
        } else {
            None
        }
    }
}

/// 锁定态悬停事件负载。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsHover {
    pub hovered: bool,
}

/// 显示设置悬浮窗开关事件负载。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LyricsSettingsState {
    pub open: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct LockState {
    locked: bool,
    hot_zone: Option<LyricsHotZone>,
}

pub struct LyricsState {
    enabled: Mutex<bool>,
    snapshot: Mutex<LyricsSnapshot>,
    lock: Mutex<LockState>,
    /// 递增代次：每次锁 / 解锁都会让旧的鼠标轮询线程退出，避免重复线程。
    monitor_generation: AtomicU64,
    /// 当前是否已有轮询线程在运行；仅更新热区时复用线程，避免反复切换穿透。
    monitor_active: AtomicBool,
    hovered: AtomicBool,
}

impl Default for LyricsState {
    fn default() -> Self {
        Self {
            enabled: Mutex::new(false),
            snapshot: Mutex::new(LyricsSnapshot::default()),
            lock: Mutex::new(LockState::default()),
            monitor_generation: AtomicU64::new(0),
            monitor_active: AtomicBool::new(false),
            hovered: AtomicBool::new(false),
        }
    }
}

impl LyricsState {
    pub fn enabled(&self) -> bool {
        *self.enabled.lock()
    }

    pub fn set_enabled(&self, value: bool) {
        *self.enabled.lock() = value;
    }

    pub fn snapshot(&self) -> LyricsSnapshot {
        self.snapshot.lock().clone()
    }

    pub fn set_snapshot(&self, value: LyricsSnapshot) {
        *self.snapshot.lock() = value;
    }

    fn lock_state(&self) -> LockState {
        *self.lock.lock()
    }

    fn set_lock_state(&self, value: LockState) {
        *self.lock.lock() = value;
    }

    fn next_generation(&self) -> u64 {
        self.monitor_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn generation(&self) -> u64 {
        self.monitor_generation.load(Ordering::SeqCst)
    }

    fn monitor_active(&self) -> bool {
        self.monitor_active.load(Ordering::SeqCst)
    }

    fn set_monitor_active(&self, value: bool) {
        self.monitor_active.store(value, Ordering::SeqCst);
    }

    fn set_hovered(&self, value: bool) -> bool {
        self.hovered.swap(value, Ordering::Relaxed) != value
    }
}

/// 开启 / 关闭桌面歌词窗口，并广播开关状态。
pub fn set_enabled(app: &AppHandle, state: &LyricsState, enabled: bool) -> Result<()> {
    // 关闭时先翻转状态再关窗口：窗口的 Destroyed 回调只有在「仍为开启」时才会翻转
    // 开关，这样程序主动关闭不会和回调互相覆盖。
    if enabled {
        // 先置位，保证页面加载完成回调能显示窗口；创建失败再回滚。
        state.set_enabled(true);
        if let Err(error) = ensure_window(app) {
            state.set_enabled(false);
            return Err(error);
        }
    } else {
        state.set_enabled(false);
        // 停止锁定态鼠标轮询；窗口关闭后不再需要穿透 / 悬停检测。
        state.next_generation();
        state.set_monitor_active(false);
        if state.set_hovered(false) {
            emit_hover(app, false);
        }
        if let Some(window) = app.get_webview_window(LYRICS_WINDOW_LABEL) {
            let _ = window.close();
        }
        close_settings(app);
    }
    emit_control(app, enabled);
    Ok(())
}

/// 关闭显示设置悬浮窗（歌词窗口关闭 / 歌词开关关闭时调用）。
pub fn close_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LYRICS_SETTINGS_LABEL) {
        let _ = window.close();
    }
}

/// 打开 / 关闭贴在歌词条上方的显示设置悬浮窗。
pub fn toggle_settings(app: &AppHandle) -> Result<()> {
    if let Some(window) = app.get_webview_window(LYRICS_SETTINGS_LABEL) {
        let _ = window.close();
        return Ok(());
    }
    app.get_webview_window(LYRICS_WINDOW_LABEL)
        .context("桌面歌词窗口尚未创建")?;

    let window = WebviewWindowBuilder::new(
        app,
        LYRICS_SETTINGS_LABEL,
        WebviewUrl::App("lyrics-settings.html".into()),
    )
    .title("桌面歌词设置")
    .inner_size(LYRICS_SETTINGS_WIDTH, LYRICS_SETTINGS_HEIGHT)
    .resizable(false)
    .maximizable(false)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .shadow(false)
    .focused(true)
    .visible(false)
    .on_page_load(|window, payload| {
        if payload.event() == PageLoadEvent::Finished {
            let _ = window.show();
            let _ = window.set_focus();
        }
    })
    .build()
    .context("创建桌面歌词设置窗口失败")?;

    place_settings(app, &window)?;
    window
        .set_always_on_top(true)
        .context("设置桌面歌词设置窗口置顶失败")?;

    let handle = app.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            let _ = handle.emit(LYRICS_SETTINGS_EVENT, LyricsSettingsState { open: false });
        }
    });

    let _ = app.emit(LYRICS_SETTINGS_EVENT, LyricsSettingsState { open: true });
    Ok(())
}

/// 设置悬浮窗贴在歌词条右上方；上方空间不足时改贴下方，并钳制到当前屏幕工作区内。
fn place_settings(app: &AppHandle, settings: &WebviewWindow) -> Result<()> {
    let lyrics = app
        .get_webview_window(LYRICS_WINDOW_LABEL)
        .context("桌面歌词窗口尚未创建")?;
    // Wayland 不允许客户端定位顶层窗口，位置交给合成器决定。
    if is_native_wayland() {
        return Ok(());
    }
    let monitor = match lyrics.current_monitor()? {
        Some(monitor) => monitor,
        None => app
            .primary_monitor()?
            .context("无法定位当前屏幕，无法放置桌面歌词设置窗口")?,
    };
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let lyrics_position = lyrics.outer_position().context("读取歌词窗口位置失败")?;
    let lyrics_size = lyrics.outer_size().context("读取歌词窗口尺寸失败")?;
    let width = (LYRICS_SETTINGS_WIDTH * scale).round() as i32;
    let height = (LYRICS_SETTINGS_HEIGHT * scale).round() as i32;
    let gap = (LYRICS_SETTINGS_GAP * scale).round() as i32;

    // 右对齐歌词条；优先贴正上方，上方放不下时改贴下方，避免遮住歌词。
    let x = clamp_axis(
        lyrics_position.x + lyrics_size.width as i32 - width,
        work.position.x,
        work.size.width,
        width,
    );
    let above = lyrics_position.y - height - gap;
    let below = lyrics_position.y + lyrics_size.height as i32 + gap;
    let y = if above >= work.position.y {
        clamp_axis(above, work.position.y, work.size.height, height)
    } else {
        clamp_axis(below, work.position.y, work.size.height, height)
    };

    settings
        .set_position(PhysicalPosition::new(x, y))
        .context("设置桌面歌词设置窗口位置失败")?;
    Ok(())
}

/// 歌词条移动 / 改尺寸后让设置悬浮窗继续贴住它；设置窗未打开时无操作。
fn sync_settings_position(app: &AppHandle) {
    let Some(settings) = app.get_webview_window(LYRICS_SETTINGS_LABEL) else {
        return;
    };
    let _ = place_settings(app, &settings);
}

fn emit_hover(app: &AppHandle, hovered: bool) {
    let _ = app.emit(LYRICS_HOVER_EVENT, LyricsHover { hovered });
}

/// 设置锁定态：锁定后默认忽略鼠标事件，并把歌词热区交给后端轮询。
///
/// 鼠标穿透开启后 WebView 收不到 `mousemove`，因此悬停检测必须在 Rust 侧
/// 用系统光标位置完成：光标进入热区时临时取消穿透并广播 hover，前端显示
/// 「解锁」入口；光标离开窗口一小段时间后恢复穿透。
///
/// Wayland 不允许客户端读取顶层窗口的全局位置，穿透后无法判断光标是否进入歌词条，
/// 会导致「解锁」入口永远不出现。因此 Wayland 下不做鼠标穿透，锁定只隐藏背景 /
/// 控件，仍可通过页面自身的悬停显示解锁按钮。
pub fn set_lock(
    app: &AppHandle,
    state: &LyricsState,
    locked: bool,
    hot_zone: Option<LyricsHotZone>,
) -> Result<()> {
    let previous = state.lock_state();
    let next_hot = hot_zone.and_then(LyricsHotZone::normalized);
    let Some(window) = app.get_webview_window(LYRICS_WINDOW_LABEL) else {
        state.set_lock_state(LockState {
            locked,
            hot_zone: next_hot,
        });
        state.set_monitor_active(false);
        return Ok(());
    };

    // 已经锁定且轮询线程仍在运行时，只更新热区：不要重新开启穿透，
    // 否则光标正停在歌词上时会出现「刚解锁又被穿透」的闪烁。
    if locked && previous.locked && state.monitor_active() {
        state.set_lock_state(LockState {
            locked: true,
            hot_zone: next_hot,
        });
        return Ok(());
    }

    state.set_lock_state(LockState {
        locked,
        hot_zone: next_hot,
    });
    let generation = state.next_generation();
    if state.set_hovered(false) {
        emit_hover(app, false);
    }

    if locked {
        if is_native_wayland() {
            // 普通 Wayland 窗口无法可靠定位，保持可交互，悬停由页面 DOM 兜底解锁。
            state.set_monitor_active(false);
            let _ = window.set_ignore_cursor_events(false);
        } else {
            // 平台不支持穿透时保持正常交互即可，仍由轮询 + DOM 悬停兜底解锁。
            let _ = window.set_ignore_cursor_events(true);
            state.set_monitor_active(true);
            start_lock_monitor(app.clone(), generation);
        }
    } else {
        state.set_monitor_active(false);
        let _ = window.set_ignore_cursor_events(false);
    }
    Ok(())
}

/// 光标相对窗口客户区的位置（逻辑像素）与窗口逻辑尺寸。
fn cursor_metrics(window: &tauri::WebviewWindow) -> Option<(f64, f64, f64, f64)> {
    let origin = window.inner_position().ok()?;
    let size = window.inner_size().ok()?;
    let scale = window.scale_factor().ok()?;
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let cursor = window.cursor_position().ok()?;
    Some((
        (cursor.x - origin.x as f64) / scale,
        (cursor.y - origin.y as f64) / scale,
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

fn inside_zone(zone: LyricsHotZone, x: f64, y: f64) -> bool {
    x >= zone.x && x <= zone.x + zone.width && y >= zone.y && y <= zone.y + zone.height
}

/// 锁定态鼠标轮询：约 90ms 一次，只在跨入 / 离开热区时切换穿透与广播。
fn start_lock_monitor(app: AppHandle, generation: u64) {
    std::thread::spawn(move || {
        let state = app.state::<LyricsState>();
        let mut hovering = false;
        let mut outside_ticks = 0u32;
        loop {
            std::thread::sleep(Duration::from_millis(90));
            if state.generation() != generation {
                break;
            }
            let lock = state.lock_state();
            if !lock.locked {
                break;
            }
            let Some(window) = app.get_webview_window(LYRICS_WINDOW_LABEL) else {
                break;
            };
            if !window.is_visible().unwrap_or(true) {
                continue;
            }
            let Some((x, y, width, height)) = cursor_metrics(&window) else {
                continue;
            };
            let inside_window = x >= 0.0 && y >= 0.0 && x <= width && y <= height;
            let inside_hot = lock.hot_zone.is_some_and(|zone| inside_zone(zone, x, y));

            if !hovering {
                if inside_hot {
                    hovering = true;
                    outside_ticks = 0;
                    if state.generation() == generation {
                        let _ = window.set_ignore_cursor_events(false);
                    }
                    if state.set_hovered(true) {
                        emit_hover(&app, true);
                    }
                }
                continue;
            }

            // 已取消穿透：允许用户把鼠标移到窗口内任意位置（如「解锁」按钮）。
            if inside_window {
                outside_ticks = 0;
                continue;
            }
            outside_ticks += 1;
            // 约 0.7 秒宽限，避免光标短暂掠过窗口边缘时入口闪烁。
            if outside_ticks < 8 {
                continue;
            }
            hovering = false;
            outside_ticks = 0;
            if state.generation() != generation {
                break;
            }
            if state.set_hovered(false) {
                emit_hover(&app, false);
            }
            let _ = window.set_ignore_cursor_events(true);
        }

        // 仅当前代次仍有效时清理状态；新锁 / 解锁已经自行处理。
        if state.generation() == generation {
            state.set_monitor_active(false);
            if state.set_hovered(false) {
                emit_hover(&app, false);
            }
            if !state.lock_state().locked {
                if let Some(window) = app.get_webview_window(LYRICS_WINDOW_LABEL) {
                    let _ = window.set_ignore_cursor_events(false);
                }
            }
        }
    });
}

/// 保存并广播最新歌词快照。
pub fn sync_snapshot(app: &AppHandle, state: &LyricsState, snapshot: LyricsSnapshot) -> Result<()> {
    state.set_snapshot(snapshot.clone());
    app.emit(LYRICS_DATA_EVENT, snapshot)
        .context("广播桌面歌词数据失败")?;
    Ok(())
}

fn emit_control(app: &AppHandle, enabled: bool) {
    let _ = app.emit(LYRICS_CONTROL_EVENT, LyricsControl { enabled });
}

/// 重新声明歌词窗口的 Z 序置顶；返回窗口是否存在且设置成功。
///
/// 全屏应用、虚拟桌面切换等场景可能把 topmost 窗口降级，歌词窗口会定时巡检调用，
/// Rust 侧在窗口失焦时也会主动调用一次。仅调整 Z 序，不抢焦点、不改尺寸。
///
/// KDE Wayland 会忽略 keep-above 请求：窗口仍能正常显示歌词，只是不保证置顶。
pub fn enforce_top_most(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window(LYRICS_WINDOW_LABEL) else {
        return false;
    };
    window.set_always_on_top(true).is_ok()
}

/// 调整窗口大小后把窗口拉回当前屏幕的工作区内，避免滑杆把窗口推出屏幕边缘。
///
/// 只钳制位置、不改变用户的水平 / 垂直落点：窗口本来就完整可见时不移动。
fn clamp_into_work_area(window: &tauri::WebviewWindow, width: i32, height: i32) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let work = monitor.work_area();
    let x = clamp_axis(position.x, work.position.x, work.size.width, width);
    let y = clamp_axis(position.y, work.position.y, work.size.height, height);
    if x != position.x || y != position.y {
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

fn clamp_axis(value: i32, start: i32, span: u32, size: i32) -> i32 {
    let max = start + span as i32 - size;
    // 窗口比工作区还大时无处可放，贴住起始边即可。
    if max < start {
        start
    } else {
        value.clamp(start, max)
    }
}

/// 自定义歌词窗口大小（逻辑像素）。
///
/// 尺寸由前端滑块或用户拖拽边缘产生，Rust 侧统一钳制到允许范围；调整后重新确认置顶，
/// 避免某些平台在改窗口尺寸时丢掉 topmost 状态。
pub fn resize(app: &AppHandle, width: f64, height: f64) -> Result<()> {
    if !width.is_finite() || !height.is_finite() {
        bail!("窗口尺寸无效");
    }
    let window = app
        .get_webview_window(LYRICS_WINDOW_LABEL)
        .context("桌面歌词窗口尚未创建")?;
    let width = width.clamp(LYRICS_MIN_WIDTH, LYRICS_MAX_WIDTH);
    let height = height.clamp(LYRICS_MIN_HEIGHT, LYRICS_MAX_HEIGHT);
    window
        .set_size(Size::Logical(LogicalSize::new(width, height)))
        .context("调整桌面歌词窗口大小失败")?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let physical = LogicalSize::new(width, height).to_physical::<i32>(scale);
    clamp_into_work_area(&window, physical.width, physical.height);
    enforce_top_most(app);
    Ok(())
}

/// 把歌词窗口放到屏幕**底部居中**（默认位置），距任务栏所在的底边留出固定间距。
///
/// 宽高由调用方按逻辑像素传入（页面恢复已保存尺寸时直接使用偏好尺寸），
/// 避免依赖刚派发的 `set_size` 是否已在原生窗口上生效。
pub fn place_at_bottom(app: &AppHandle, width: f64, height: f64) -> Result<()> {
    if !width.is_finite() || !height.is_finite() {
        bail!("窗口尺寸无效");
    }
    let window = app
        .get_webview_window(LYRICS_WINDOW_LABEL)
        .context("桌面歌词窗口尚未创建")?;
    // Wayland 不允许客户端定位顶层窗口：位置由合成器决定，避免无效的 set_position。
    if is_native_wayland() {
        return Ok(());
    }
    let monitor = match window.current_monitor()? {
        Some(monitor) => monitor,
        None => app
            .primary_monitor()?
            .context("无法定位当前屏幕，无法放置桌面歌词窗口")?,
    };
    let work = monitor.work_area();
    let scale = monitor.scale_factor();
    let physical_width = (width.clamp(LYRICS_MIN_WIDTH, LYRICS_MAX_WIDTH) * scale).round() as i32;
    let physical_height =
        (height.clamp(LYRICS_MIN_HEIGHT, LYRICS_MAX_HEIGHT) * scale).round() as i32;
    let margin = (LYRICS_BOTTOM_MARGIN * scale).round() as i32;
    let x = work.position.x + (work.size.width as i32 - physical_width) / 2;
    let y = work.position.y + work.size.height as i32 - physical_height - margin;
    // 窗口高于工作区时贴住顶部，避免歌词被推出屏幕。
    let y = y.max(work.position.y);
    window
        .set_position(PhysicalPosition::new(x, y))
        .context("设置桌面歌词窗口位置失败")?;
    enforce_top_most(app);
    Ok(())
}

fn ensure_window(app: &AppHandle) -> Result<()> {
    if let Some(window) = app.get_webview_window(LYRICS_WINDOW_LABEL) {
        // 页面加载完成回调负责首次显示；这里只处理窗口已存在时的重复开启。
        enforce_top_most(app);
        let _ = window.show();
        return Ok(());
    }

    let window = WebviewWindowBuilder::new(
        app,
        LYRICS_WINDOW_LABEL,
        WebviewUrl::App("lyrics.html".into()),
    )
    .title("桌面歌词")
    // 高度容纳三行歌词；显示设置面板在窗口内滚动。
    .inner_size(LYRICS_DEFAULT_WIDTH, LYRICS_DEFAULT_HEIGHT)
    .min_inner_size(LYRICS_MIN_WIDTH, LYRICS_MIN_HEIGHT)
    .resizable(true)
    // 拖动区域双击会触发 Tauri 的最大化命令，歌词窗口禁止最大化避免铺满屏幕。
    .maximizable(false)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    // macOS / Linux 虚拟桌面同样可见；Windows 下为空操作。
    .visible_on_all_workspaces(true)
    .skip_taskbar(true)
    .shadow(false)
    // 不抢焦点；先隐藏创建，页面加载完成后再显示。
    .focused(false)
    .visible(false)
    .on_page_load(|window, payload| {
        if payload.event() == PageLoadEvent::Finished {
            // 页面加载完成后再显示；若期间已关闭歌词，则不再显示。
            if window.app_handle().state::<LyricsState>().enabled() {
                let _ = window.show();
            }
        }
    })
    .build()
    .context("创建桌面歌词窗口失败")?;

    // 默认位置贴屏幕底部居中；页面恢复已保存尺寸时会再次校正位置。
    // Wayland 下无法由客户端定位窗口，"贴底"一步会自动跳过。
    place_at_bottom(app, LYRICS_DEFAULT_WIDTH, LYRICS_DEFAULT_HEIGHT)?;

    // builder 的标志只是初始状态，创建后再确认一次，覆盖首次显示时的 Z 序竞争。
    window
        .set_always_on_top(true)
        .context("设置桌面歌词窗口置顶失败")?;

    // 用户在歌词窗口上手动关闭时，把主窗口的开关同步回「关闭」。
    // 失焦时重新声明置顶，对抗全屏应用抢占最顶层；尺寸变化时把逻辑尺寸广播给前端记忆。
    let handle = app.clone();
    window.on_window_event(move |event| match event {
        // 用户用 × / Alt+F4 等方式关闭歌词窗口时同步落盘，避免下次启动又自动打开。
        // 程序退出时也会关闭窗口，那种情况不能把用户的「开启」偏好清掉。
        WindowEvent::CloseRequested { .. } => {
            let lyrics = handle.state::<LyricsState>();
            if !lyrics.enabled() {
                return;
            }
            lyrics.set_enabled(false);
            emit_control(&handle, false);
            close_settings(&handle);
            if !handle.state::<DesktopState>().quitting() {
                handle.state::<AppState>().set_desktop_lyrics_enabled(false);
            }
        }
        WindowEvent::Destroyed => {
            let state = handle.state::<LyricsState>();
            if state.enabled() {
                state.set_enabled(false);
                emit_control(&handle, false);
            }
            close_settings(&handle);
        }
        WindowEvent::Focused(false) => {
            enforce_top_most(&handle);
        }
        // 设置悬浮窗锚定在歌词条上；歌词条被拖动 / 改尺寸后重新贴合。
        WindowEvent::Moved(_) => sync_settings_position(&handle),
        WindowEvent::Resized(size) => {
            if let Some(window) = handle.get_webview_window(LYRICS_WINDOW_LABEL) {
                if let Ok(scale) = window.scale_factor() {
                    if scale > 0.0 {
                        let logical = size.to_logical::<f64>(scale);
                        let _ = handle.emit(
                            LYRICS_SIZE_EVENT,
                            LyricsWindowSize {
                                width: logical.width,
                                height: logical.height,
                            },
                        );
                    }
                }
            }
            sync_settings_position(&handle);
        }
        _ => {}
    });
    Ok(())
}
