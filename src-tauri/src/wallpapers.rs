//! 界面壁纸：内置资源定位、自定义文件校验与 asset 协议授权。
//!
//! 打包后壁纸位于 `$RESOURCE/assets/bg/`，开发模式由 `tauri-build` 复制到
//! `target/<profile>/assets/bg/`，两者都通过 `resource_dir` 命中；仓库路径作为
//! 最后兜底，方便直接 `cargo run` 调试。内置壁纸不维护固定清单，而是扫描目录
//! 自动发现支持的图片 / 视频，新增文件无需改代码。
//! 图片使用 asset 协议；Linux 视频改用本机 HTTP Range 流，以规避 WebKitGTK
//! 无法通过 `asset://` 打开 `<video>` 的限制。媒体服务本身是平台专有实现，
//! 位于 `platform/linux/media.rs`，其余平台由 `platform/asset_media.rs` 兜底，
//! 本文件只保留跨平台的资源定位与授权。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bgm_core::prefs::is_wallpaper_path;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::platform::WallpaperMediaServer;

/// 旧版缩略图文件名形如 `1.thumb.jpg`，扫描壁纸时需要跳过。
const THUMBNAIL_MARKER: &str = ".thumb.";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperAsset {
    pub id: String,
    pub label: String,
    pub kind: String,
    /// 传给前端 `convertFileSrc` 的本地路径。
    pub path: String,
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

/// 内置壁纸目录候选：发布包读资源目录，调试构建直接读仓库目录。
/// 调试时 `target/<profile>/assets/bg` 会累积历次构建的旧副本，
/// 文件改名或删除后仍会被扫描到，因此 debug 构建跳过该目录。
///
/// 仓库目录用 `parent()` 而不是 `join("..")`：asset 协议会拒绝含 `..`
/// 组件的路径，带 `..` 的壁纸地址会一律 403。
fn bg_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if !cfg!(debug_assertions) {
        if let Ok(resource_dir) = app.path().resource_dir() {
            dirs.push(resource_dir.join("assets").join("bg"));
        }
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap_or(manifest_dir);
    dirs.push(repo_root.join("assets").join("bg"));
    dirs
}

/// 文件名主干转成可持久化的壁纸 id：`3.jpg` → `builtin-3`。
/// 纯中文等无法转写的名称退化为名称哈希，保证重启后 id 稳定。
fn builtin_id(stem: &str) -> String {
    let mut suffix = String::new();
    for ch in stem.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            suffix.push(ch);
        } else if !suffix.ends_with('-') {
            suffix.push('-');
        }
    }
    let suffix = suffix.trim_matches('-');
    if suffix.is_empty() {
        format!("builtin-{:016x}", fnv1a(stem.as_bytes()))
    } else {
        format!("builtin-{suffix}")
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn is_thumbnail(name: &str) -> bool {
    name.to_ascii_lowercase().contains(THUMBNAIL_MARKER)
}

/// 扫描一个目录顶层的壁纸文件；子目录、旧版缩略图与不支持的格式会被跳过。
/// 返回顺序按文件名排序，保证界面列表稳定。
fn discover(dir: &Path) -> Vec<(String, String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    let mut found = Vec::new();
    let mut seen_ids = HashSet::new();
    // 同名不同后缀（如 2.jpg 与 2.mp4）会撞 id，标签改用完整文件名以便区分。
    let mut stem_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for path in &files {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || is_thumbnail(name) || !is_supported(path) {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            *stem_counts.entry(stem).or_insert(0) += 1;
        }
    }

    for path in &files {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || is_thumbnail(name) || !is_supported(path) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let base = builtin_id(stem);
        let mut id = base.clone();
        let mut suffix = 2;
        while seen_ids.contains(&id) {
            id = format!("{base}-{suffix}");
            suffix += 1;
        }
        seen_ids.insert(id.clone());

        let label = if stem_counts.get(stem).copied().unwrap_or(0) > 1 {
            name.to_owned()
        } else {
            stem.to_owned()
        };
        found.push((id, label, path.clone()));
    }
    found
}

/// 返回自动发现的内置图片 / 视频壁纸，并顺带授权资源文件访问。
/// 资源目录优先，开发目录仅补齐未命中的 id，避免同一壁纸重复出现。
pub fn builtin_catalog(app: &AppHandle) -> Vec<WallpaperAsset> {
    let mut assets: Vec<WallpaperAsset> = Vec::new();
    let mut seen_ids = HashSet::new();
    for dir in bg_dirs(app) {
        for (id, label, path) in discover(&dir) {
            if !seen_ids.insert(id.clone()) {
                continue;
            }
            if allow_wallpaper_file(app, &path).is_err() {
                continue;
            }
            assets.push(WallpaperAsset {
                id,
                label,
                kind: kind_of(&path).to_owned(),
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    assets
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_bg_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("assets")
            .join("bg")
    }

    /// id 必须与 `prefs.json` 中已持久化的取值保持一致，否则升级后壁纸会静默回退默认样式。
    #[test]
    fn builtin_id_is_stable_for_chinese_names() {
        assert_eq!(builtin_id("幻彩流光"), "builtin-da6fc012554a8ed0");
        assert_eq!(builtin_id("雪山云海"), "builtin-fc1961bdcbd769d7");
        assert_eq!(builtin_id("3"), "builtin-3");
        assert_eq!(builtin_id("bgm-01"), "builtin-bgm-01");
    }

    /// 仓库内置资源应被完整发现：跳过旧版缩略图，四种壁纸各一条。
    #[test]
    fn discover_skips_thumbnails() {
        let found = discover(&repo_bg_dir());
        let ids: Vec<&str> = found.iter().map(|(id, _, _)| id.as_str()).collect();
        assert!(ids.contains(&"builtin-da6fc012554a8ed0"), "ids = {ids:?}");
        assert!(!found.iter().any(|(_, label, _)| label.contains(".thumb.")));
        assert!(found.iter().all(|(_, _, path)| is_supported(path)));
    }
}
