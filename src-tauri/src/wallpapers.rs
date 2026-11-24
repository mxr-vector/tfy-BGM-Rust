//! 界面壁纸：内置资源定位、自定义文件校验与 asset 协议授权。
//!
//! 打包后壁纸位于 `$RESOURCE/assets/bg/`，开发模式由 `tauri-build` 复制到
//! `target/<profile>/assets/bg/`，两者都通过 `resource_dir` 命中；仓库路径作为
//! 最后兜底，方便直接 `cargo run` 调试。
//! 图片使用 asset 协议；Linux 视频改用本机 HTTP Range 流，以规避 WebKitGTK
//! 无法通过 `asset://` 打开 `<video>` 的限制。媒体服务本身是平台专有实现，
//! 位于 `platform/linux/media.rs`，其余平台由 `platform/asset_media.rs` 兜底，
//! 本文件只保留跨平台的资源定位与授权。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bgm_core::prefs::is_wallpaper_path;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::platform::WallpaperMediaServer;

/// (id, 名称, 类型, 壁纸文件, 缩略图文件)
pub const BUILTIN_WALLPAPERS: [(&str, &str, &str, &str, &str); 2] = [
    ("builtin-1", "幻彩流光", "video", "1.mp4", "1.thumb.jpg"),
    ("builtin-2", "雪山云海", "image", "2.jpg", "2.jpg"),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperAsset {
    pub id: String,
    pub label: String,
    pub kind: String,
    /// 传给前端 `convertFileSrc` 的本地路径。
    pub path: String,
    pub thumb: String,
}

/// 自定义壁纸选择结果；`label` 只用于界面提示，不参与持久化。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperPick {
    pub path: String,
    pub kind: String,
    pub label: String,
}

/// 按扩展名区分静态图片与视频；判定规则由 `bgm_core` 统一维护。
pub fn kind_of(path: &Path) -> &'static str {
    bgm_core::prefs::wallpaper_kind(path)
}

/// 自定义壁纸只接受 jpg / jpeg / png / webp / mp4。
pub fn is_supported(path: &Path) -> bool {
    path.to_str().is_some_and(is_wallpaper_path)
}

pub fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub fn allow_wallpaper_file(app: &AppHandle, path: &Path) -> Result<()> {
    app.asset_protocol_scope()
        .allow_file(path)
        .with_context(|| format!("无法授权壁纸文件：{}", path.display()))?;
    if let Some(media) = app.try_state::<WallpaperMediaServer>() {
        media.register(path)?;
    }
    Ok(())
}

fn candidates(app: &AppHandle, file_name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        paths.push(resource_dir.join("assets").join("bg").join(file_name));
    }
    // 开发模式兜底：资源尚未复制进 target 时直接读仓库文件。
    paths.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("assets")
            .join("bg")
            .join(file_name),
    );
    paths
}

pub fn resolve_asset(app: &AppHandle, file_name: &str) -> Option<PathBuf> {
    candidates(app, file_name)
        .into_iter()
        .find(|path| path.is_file())
}

/// 返回仓库内置的图片 / 视频壁纸，并顺带授权资源文件访问。
pub fn builtin_catalog(app: &AppHandle) -> Vec<WallpaperAsset> {
    BUILTIN_WALLPAPERS
        .iter()
        .filter_map(|(id, label, kind, file, thumb_file)| {
            let path = resolve_asset(app, file)?;
            // 缩略图缺失时留空，前端会退回类型图标而不是显示破图。
            let thumb = resolve_asset(app, thumb_file);
            allow_wallpaper_file(app, &path).ok()?;
            if let Some(thumb) = &thumb {
                allow_wallpaper_file(app, thumb).ok()?;
            }
            Some(WallpaperAsset {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                kind: (*kind).to_owned(),
                path: path.to_string_lossy().into_owned(),
                thumb: thumb
                    .map(|thumb| thumb.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// 启动时授权上次保存的自定义壁纸，避免首屏加载被 asset 协议拦下。
/// 文件缺失或格式不再受支持时静默跳过，由前端提示用户重新选择。
pub fn prime_custom(app: &AppHandle, path: Option<&str>) {
    let Some(path) = path else { return };
    let path = Path::new(path);
    if path.is_file() && is_supported(path) {
        let _ = allow_wallpaper_file(app, path);
    }
}
