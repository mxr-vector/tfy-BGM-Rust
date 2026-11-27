//! 曲库根目录解析与访问授权。
//!
//! 解析优先级：`TFY_BGM_DIR` → 用户自选目录 → 开发仓库 `bgm/`（仅 debug）
//! → Windows 绿色版已有的可写同级 `bgm/` → 用户数据目录（兼容旧配置目录）。
//! 默认曲库不使用当前工作目录或安装包资源，也不迁移或复制音频。

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bgm_core::paths::{LIBRARY_DIR_NAME, LIBRARY_ENV_VAR};
use bgm_core::{Category, LibrarySource, Prefs};
#[cfg(any(windows, test))]
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Manager};

/// 启动和恢复默认曲库共用此入口；仅为外置默认曲库补齐分类。
pub fn resolve(
    app: &AppHandle,
    prefs: &Prefs,
    categories: &[Category],
) -> Result<(PathBuf, LibrarySource, Vec<String>)> {
    let mut warnings = Vec::new();
    let dev_root = cfg!(debug_assertions).then(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(LIBRARY_DIR_NAME)
    });
    if let Some((root, source)) = resolve_override(
        env::var_os(LIBRARY_ENV_VAR).map(PathBuf::from),
        prefs.library_dir.as_deref().map(PathBuf::from),
        dev_root,
        &mut warnings,
    ) {
        return Ok((root, source, warnings));
    }

    let root = resolve_external_root(app, &mut warnings)?;
    warnings.extend(ensure_category_dirs(&root, categories));
    Ok((root, LibrarySource::External, warnings))
}

fn resolve_override(
    env_root: Option<PathBuf>,
    custom_root: Option<PathBuf>,
    dev_root: Option<PathBuf>,
    warnings: &mut Vec<String>,
) -> Option<(PathBuf, LibrarySource)> {
    if let Some(path) = env_root {
        if path.is_dir() {
            return Some((absolute(path), LibrarySource::Env));
        }
        warnings.push(format!(
            "环境变量 {LIBRARY_ENV_VAR} 指向的目录不存在，已忽略：{}",
            path.display()
        ));
    }

    if let Some(path) = custom_root {
        if path.is_dir() {
            return Some((absolute(path), LibrarySource::Custom));
        }
        warnings.push(format!(
            "自定义曲库目录不存在，已回退到默认曲库：{}",
            path.display()
        ));
    }

    dev_root
        .filter(|path| path.is_dir())
        .map(|path| (absolute(path), LibrarySource::Bundled))
}

fn resolve_external_root(app: &AppHandle, warnings: &mut Vec<String>) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let exe = env::current_exe();
        if let Err(error) = &exe {
            warnings.push(format!("无法定位程序同级曲库，将使用用户曲库：{error}"));
        }
        if let Some(root) = resolve_portable_root(
            exe.as_deref().ok(),
            tauri::utils::platform::bundle_type(),
            check_writable,
            warnings,
        ) {
            return Ok(root);
        }
    }

    resolve_user_root(
        app.path().app_data_dir().ok(),
        app.path().app_config_dir().ok(),
        warnings,
    )
}

#[cfg(any(windows, test))]
fn resolve_portable_root(
    exe: Option<&Path>,
    bundle_type: Option<BundleType>,
    check_write: impl FnOnce(&Path) -> io::Result<()>,
    warnings: &mut Vec<String>,
) -> Option<PathBuf> {
    // Windows 安装包的资源可能也和 exe 同级，升级后残留的 bgm/ 不能当作绿色版曲库。
    if matches!(bundle_type, Some(BundleType::Nsis | BundleType::Msi)) {
        return None;
    }
    let root = exe?.parent()?.join(LIBRARY_DIR_NAME);
    // 绿色版自带分类脚手架；没有同级 bgm/ 时不在程序目录新建。
    let writable = match fs::symlink_metadata(&root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => Err(error),
        Ok(_) => check_write(&root),
    };
    match writable {
        Ok(()) => Some(absolute(root)),
        Err(error) => {
            warnings.push(format!(
                "程序同级曲库 {} 不可写，将使用用户曲库：{error}",
                root.display()
            ));
            None
        }
    }
}

fn resolve_user_root(
    data_dir: Option<PathBuf>,
    config_dir: Option<PathBuf>,
    warnings: &mut Vec<String>,
) -> Result<PathBuf> {
    let to_root = |dir: PathBuf| dir.is_absolute().then(|| dir.join(LIBRARY_DIR_NAME));
    let data = data_dir.and_then(to_root);
    let config = config_dir
        .and_then(to_root)
        .filter(|root| Some(root) != data.as_ref());

    // 已有数据目录优先；尚未建立时保留旧配置曲库，不移动任何文件。
    let (first, second) = match (&data, &config) {
        (Some(data), Some(config)) if !data.is_dir() && config.is_dir() => {
            (Some(config), Some(data))
        }
        _ => (data.as_ref(), config.as_ref()),
    };
    for root in [first, second].into_iter().flatten() {
        match fs::create_dir_all(root).and_then(|()| check_writable(root)) {
            Ok(()) => return Ok(absolute(root.clone())),
            Err(error) => warnings.push(format!("用户曲库目录不可用 {}：{error}", root.display())),
        }
    }

    // 即使目录暂时不可写，仍允许启动并让用户手动切换；不退到程序目录或 cwd。
    if let Some(root) = first.or(second) {
        warnings.push("未找到可写的用户曲库，请通过「切换曲库目录」选择文件夹".to_owned());
        return Ok(absolute(root.clone()));
    }
    bail!("无法定位用户曲库目录")
}

fn check_writable(root: &Path) -> io::Result<()> {
    // 用自动清理的临时文件实际探测写权限，不能仅凭目录存在或只读属性判断。
    tempfile::tempfile_in(root).map(drop)
}

/// 在外置曲库中补齐分类子目录，首次启动即可直接往对应目录放音频。
fn ensure_category_dirs(root: &Path, categories: &[Category]) -> Vec<String> {
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
    fn overrides_keep_env_custom_dev_priority_and_warn_on_invalid_paths() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("env");
        let custom = dir.path().join("custom");
        let dev = dir.path().join("dev");
        for path in [&env, &custom, &dev] {
            fs::create_dir(path).unwrap();
        }
        let missing = dir.path().join("missing");
        let mut warnings = Vec::new();

        assert_eq!(
            resolve_override(
                Some(env.clone()),
                Some(custom.clone()),
                Some(dev.clone()),
                &mut warnings
            ),
            Some((absolute(env), LibrarySource::Env))
        );
        assert!(warnings.is_empty());
        assert_eq!(
            resolve_override(
                Some(missing.clone()),
                Some(custom.clone()),
                Some(dev.clone()),
                &mut warnings
            ),
            Some((absolute(custom), LibrarySource::Custom))
        );
        assert_eq!(warnings.len(), 1);
        warnings.clear();
        assert_eq!(
            resolve_override(
                Some(missing.clone()),
                Some(missing),
                Some(dev.clone()),
                &mut warnings
            ),
            Some((absolute(dev), LibrarySource::Bundled))
        );
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn fresh_default_creates_only_user_data_library_and_category_scaffold() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let config = dir.path().join("config");
        let resources = dir.path().join("resources/bgm/epic");
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("track.mp3"), b"bundled").unwrap();
        let mut warnings = Vec::new();
        let root =
            resolve_user_root(Some(data.clone()), Some(config.clone()), &mut warnings).unwrap();
        let categories = bgm_core::Categories::default();

        assert_eq!(root, absolute(data.join(LIBRARY_DIR_NAME)));
        assert!(warnings.is_empty());
        assert!(ensure_category_dirs(&root, &categories.items).is_empty());
        assert!(
            categories
                .items
                .iter()
                .all(|category| root.join(&category.id).is_dir())
        );
        assert!(fs::read_dir(root.join("epic")).unwrap().next().is_none());
        assert!(!config.exists());
        assert_eq!(fs::read(resources.join("track.mp3")).unwrap(), b"bundled");
    }

    #[test]
    fn existing_legacy_library_is_preserved_until_data_library_exists() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let config = dir.path().join("config");
        let legacy = config.join(LIBRARY_DIR_NAME);
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("track.mp3"), b"user track").unwrap();
        let mut warnings = Vec::new();
        let selected =
            resolve_user_root(Some(data.clone()), Some(config.clone()), &mut warnings).unwrap();
        assert_eq!(selected, absolute(legacy.clone()));
        assert!(!data.exists());

        let preferred = data.join(LIBRARY_DIR_NAME);
        fs::create_dir_all(&preferred).unwrap();
        let selected = resolve_user_root(Some(data), Some(config), &mut warnings).unwrap();
        assert_eq!(selected, absolute(preferred));
        assert_eq!(fs::read(legacy.join("track.mp3")).unwrap(), b"user track");
        assert!(warnings.is_empty());
    }

    #[test]
    fn unusable_data_path_warns_and_falls_back_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let config = dir.path().join("config");
        fs::write(&data, b"not a directory").unwrap();
        let mut warnings = Vec::new();
        let selected = resolve_user_root(Some(data), Some(config.clone()), &mut warnings).unwrap();

        assert_eq!(selected, absolute(config.join(LIBRARY_DIR_NAME)));
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn missing_or_relative_user_paths_never_fall_back_to_cwd() {
        assert!(resolve_user_root(None, Some(PathBuf::from("relative")), &mut Vec::new()).is_err());
    }

    #[test]
    fn portable_requires_existing_sibling_and_accepts_scaffold_or_user_music() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tfy-bgm.exe");
        let root = dir.path().join(LIBRARY_DIR_NAME);
        let mut warnings = Vec::new();
        assert_eq!(
            resolve_portable_root(Some(&exe), None, check_writable, &mut warnings),
            None
        );
        assert!(!root.exists());

        fs::create_dir_all(root.join("epic")).unwrap();
        for populated in [false, true] {
            if populated {
                fs::write(root.join("epic/track.mp3"), b"user track").unwrap();
            }
            assert_eq!(
                resolve_portable_root(Some(&exe), None, check_writable, &mut warnings),
                Some(absolute(root.clone()))
            );
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert!(warnings.is_empty());
    }

    #[test]
    fn installer_never_uses_sibling_even_with_leftover_bundled_tracks() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tfy-bgm.exe");
        let root = dir.path().join(LIBRARY_DIR_NAME);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("leftover.mp3"), b"bundled").unwrap();
        for installer in [BundleType::Nsis, BundleType::Msi] {
            assert_eq!(
                resolve_portable_root(
                    Some(&exe),
                    Some(installer),
                    |_| panic!("must not probe bundle resources"),
                    &mut Vec::new()
                ),
                None
            );
        }
        assert_eq!(fs::read(root.join("leftover.mp3")).unwrap(), b"bundled");
    }

    #[test]
    fn unwritable_portable_sibling_warns_and_allows_user_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tfy-bgm.exe");
        let root = dir.path().join(LIBRARY_DIR_NAME);
        fs::create_dir(&root).unwrap();
        let mut warnings = Vec::new();
        assert_eq!(
            resolve_portable_root(
                Some(&exe),
                None,
                |path| {
                    assert_eq!(path, root);
                    Err(io::ErrorKind::PermissionDenied.into())
                },
                &mut warnings,
            ),
            None
        );
        assert_eq!(warnings.len(), 1);
        let data = dir.path().join("data");
        assert_eq!(
            resolve_user_root(Some(data.clone()), None, &mut warnings).unwrap(),
            absolute(data.join(LIBRARY_DIR_NAME))
        );
    }
}
