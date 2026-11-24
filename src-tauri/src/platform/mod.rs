//! 按操作系统隔离专有实现；业务模块只依赖这里导出的统一接口。
//!
//! 每个目标平台在 `platform/<os>/` 下提供同一组能力：
//! - `autostart`：开机自动启动的注册方式（注册表 / autostart 项 / LaunchAgent）；
//! - `gpu`：创建 WebView 之前确定渲染后端；
//! - `fonts`：系统字体目录，Windows 额外返回注册表登记的字体文件；
//! - `config_dir` / `path_key`：配置目录约定与路径比较规则。
//!
//! 壁纸视频流服务只有 Linux 需要（WebKitGTK 无法用 `asset://` 播 `<video>`），
//! 其余平台共用 `asset_media`，直接走 Tauri 的 asset 协议。

use std::path::{Path, PathBuf};

use anyhow::Result;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;

#[cfg(not(target_os = "linux"))]
mod asset_media;

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use unsupported as imp;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(not(target_os = "linux"))]
pub use asset_media::WallpaperMediaServer;
#[cfg(target_os = "linux")]
pub use linux::media::WallpaperMediaServer;

/// 系统字体来源：可直接递归扫描的目录，以及目录之外的独立字体文件。
#[derive(Debug, Default)]
pub struct FontSources {
    pub dirs: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
}

/// 当前操作系统标识，随 `bootstrap` 下发，界面据此显示平台特有配置。
pub fn name() -> &'static str {
    imp::NAME
}

/// 把开机自动启动对齐到 `enabled`；重复调用结果一致。
pub fn set_autostart(enabled: bool) -> Result<()> {
    imp::autostart::set(enabled)
}

/// 在创建 WebView 之前设置渲染后端开关。
pub fn apply_gpu_acceleration<R: tauri::Runtime>(context: &mut tauri::Context<R>, enabled: bool) {
    imp::gpu::apply(context, enabled)
}

/// 待扫描的系统字体来源。
pub fn font_sources() -> FontSources {
    imp::fonts::sources()
}

/// 应用配置目录；`identifier` 为 Tauri 的应用标识。
pub fn config_dir(identifier: &str) -> Option<PathBuf> {
    imp::config_dir(identifier)
}

/// 路径去重键：大小写不敏感的文件系统上折叠为小写。
pub fn path_key(path: &Path) -> String {
    imp::path_key(path)
}

/// 是否运行在原生 Wayland 后端；XWayland 仍按普通窗口处理。
#[cfg(target_os = "linux")]
pub fn is_native_wayland() -> bool {
    linux::session::is_native_wayland()
}

/// 非 Linux 平台没有 Wayland 概念。
#[cfg(not(target_os = "linux"))]
pub fn is_native_wayland() -> bool {
    false
}
