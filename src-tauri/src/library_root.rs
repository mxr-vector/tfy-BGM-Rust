//! 曲库根目录解析与访问授权。
//!
//! 解析优先级：`TFY_BGM_DIR` 环境变量 → 用户自选目录 → 开发仓库 `bgm/`
//! （仅 debug）→ 发布包资源目录 `bgm/` → exe 同级 `bgm/` → 当前工作目录
//! `bgm/` → 应用配置目录 `bgm/`。
//! 外置挂载点缺失时自动创建（优先 exe 同级，无写入权限时退到配置目录），
//! 分类子目录由调用方按需补全。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bgm_core::paths::{LIBRARY_DIR_NAME, LIBRARY_ENV_VAR};
use bgm_core::{Category, LibrarySource, Prefs};
use tauri::{AppHandle, Manager};

pub fn resolve(app: &AppHandle, prefs: &Prefs) -> (PathBuf, LibrarySource, Vec<String>) {
    let mut warnings = Vec::new();

    if let Some(dir) = env::var_os(LIBRARY_ENV_VAR) {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return (absolute(path), LibrarySource::Env, warnings);
        }
        warnings.push(format!(
            "环境变量 {LIBRARY_ENV_VAR} 指向的目录不存在，已忽略：{}",
            path.display()
        ));
    }

    if let Some(dir) = &prefs.library_dir {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return (absolute(path), LibrarySource::Custom, warnings);
        }
        warnings.push(format!(
            "自定义曲库目录不存在，已回退到默认曲库：{}",
            path.display()
        ));
    }

    // 开发模式优先使用仓库根目录的 bgm/，避免在 target/debug 下生成空曲库。
    #[cfg(debug_assertions)]
    {
        let dev_root = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../{LIBRARY_DIR_NAME}"));
        if dev_root.is_dir() {
            return (absolute(dev_root), LibrarySource::Bundled, warnings);
        }
    }

    // 安装版 / Linux 包把 bgm/ 作为资源分发；绿色版只有空分类目录，
    // 必须含实际音频才命中，避免把空曲库误标成内置曲库。
    #[cfg(not(debug_assertions))]
    if let Some(root) = app
        .path()
        .resource_dir()
        .ok()
        .and_then(|dir| bundled_root_at(&dir))
    {
        return (root, LibrarySource::Bundled, warnings);
    }

    resolve_external_root(app, warnings)
}

/// 发布包资源目录中的内置曲库；空分类脚手架不算内置曲库。
fn bundled_root_at(resource_dir: &Path) -> Option<PathBuf> {
    let root = resource_dir.join(LIBRARY_DIR_NAME);
    (root.is_dir() && contains_audio(&root)).then(|| absolute(root))
}

fn contains_audio(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            contains_audio(&path)
        } else {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(bgm_core::library::has_audio_extension)
        }
    })
}

/// 外置曲库挂载点：优先 exe 同级，其次当前工作目录，最后应用配置目录。
/// 都不存在时尝试在 exe 同级创建，失败则落到配置目录；分类子目录由调用方补全。
fn resolve_external_root(
    app: &AppHandle,
    mut warnings: Vec<String>,
) -> (PathBuf, LibrarySource, Vec<String>) {
    let exe_root = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|dir| dir.join(LIBRARY_DIR_NAME));
    let cwd_root = env::current_dir()
        .ok()
        .map(|dir| dir.join(LIBRARY_DIR_NAME));
    let config_root = app
        .path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(LIBRARY_DIR_NAME));

    if let Some(root) = [&exe_root, &cwd_root, &config_root]
        .into_iter()
        .flatten()
        .find(|root| root.is_dir())
    {
        return (absolute(root.clone()), LibrarySource::External, warnings);
    }

    for root in [&exe_root, &config_root].into_iter().flatten() {
        match fs::create_dir_all(root) {
            Ok(()) => return (absolute(root.clone()), LibrarySource::External, warnings),
            Err(error) => {
                warnings.push(format!("无法创建外置曲库目录 {}：{error}", root.display()))
            }
        }
    }

    let fallback = exe_root
        .or(cwd_root)
        .or(config_root)
        .unwrap_or_else(|| PathBuf::from(LIBRARY_DIR_NAME));
    warnings.push(format!(
        "未找到可用的外置曲库目录，请在程序同级创建 {LIBRARY_DIR_NAME}/，或通过「切换曲库目录」选择文件夹"
    ));
    (fallback, LibrarySource::External, warnings)
}

/// 在外置曲库中补齐分类子目录，首次启动即可直接往对应目录放音频。
pub fn ensure_category_dirs(root: &Path, categories: &[Category]) -> Vec<String> {
    let mut warnings = Vec::new();
    for category in categories {
        let dir = root.join(&category.id);
        if dir.is_dir() {
            continue;
        }
        if let Err(error) = fs::create_dir_all(&dir) {
            warnings.push(format!("无法创建分类目录 {}：{error}", dir.display()));
        }
    }
    warnings
}

/// 把曲库目录授权给 asset 协议，前端才能通过 `convertFileSrc` 访问音频。
pub fn allow_audio_scope(app: &AppHandle, root: &Path) -> Result<()> {
    if root.is_dir() {
        app.asset_protocol_scope()
            .allow_directory(root, true)
            .with_context(|| format!("无法授权音频目录：{}", root.display()))?;
    }
    Ok(())
}

fn absolute(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_resource_scaffold_is_not_a_bundled_library() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(LIBRARY_DIR_NAME).join("cheerful")).unwrap();

        assert!(bundled_root_at(dir.path()).is_none());
    }

    #[test]
    fn bundled_library_requires_supported_audio() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(LIBRARY_DIR_NAME);
        fs::create_dir_all(root.join("epic")).unwrap();
        fs::write(root.join("epic/notes.txt"), b"not audio").unwrap();
        assert!(bundled_root_at(dir.path()).is_none());

        fs::write(root.join("epic/track.MP3"), b"audio").unwrap();
        assert_eq!(bundled_root_at(dir.path()), Some(absolute(root)));
    }
}
