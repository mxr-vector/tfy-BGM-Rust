//! 应用运行期状态：偏好、曲库根目录与最近一次扫描结果。
//!
//! 曲库根目录解析优先级：`TFY_BGM_DIR` 环境变量 → 用户自选目录 → 应用资源目录 → 开发仓库目录。
//! 所有写操作先持久化再更新内存，并通过独立的命令修改曲库目录，避免前端偏好覆盖目录选择。

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bgm_core::{LibrarySnapshot, LibrarySource, Prefs, Song, scan_library};
use parking_lot::RwLock;
use serde::Serialize;
use tauri::{AppHandle, Manager};

pub struct AppState {
    prefs_path: PathBuf,
    prefs: RwLock<Prefs>,
    runtime: RwLock<Runtime>,
}

struct Runtime {
    root: PathBuf,
    source: LibrarySource,
    library: LibrarySnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub prefs: Prefs,
    pub library: LibrarySnapshot,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self> {
        let config_dir = app
            .path()
            .app_config_dir()
            .context("无法定位应用配置目录")?;
        let prefs_path = config_dir.join("prefs.json");
        let (prefs, prefs_warning) = Prefs::load(&prefs_path);

        let (root, source, mut warnings) = resolve_root(app, &prefs);
        if let Some(warning) = prefs_warning {
            warnings.push(warning);
        }

        let mut library = scan_library(&root, source);
        warnings.append(&mut library.warnings);
        library.warnings = warnings;
        allow_audio_scope(app, &root)?;

        Ok(Self {
            prefs_path,
            prefs: RwLock::new(prefs),
            runtime: RwLock::new(Runtime {
                root,
                source,
                library,
            }),
        })
    }

    pub fn bootstrap(&self) -> Bootstrap {
        Bootstrap {
            prefs: self.prefs.read().clone(),
            library: self.runtime.read().library.clone(),
        }
    }

    pub fn save_prefs(&self, incoming: Prefs) -> Result<Prefs> {
        let mut current = self.prefs.write();
        let mut next = incoming.normalize();
        // 曲库目录只允许通过 choose/reset 命令修改，避免偏好保存把目录改坏。
        next.library_dir = current.library_dir.clone();
        next.save(&self.prefs_path).context("保存偏好设置失败")?;
        *current = next.clone();
        Ok(next)
    }

    pub fn rescan(&self, app: &AppHandle) -> Result<LibrarySnapshot> {
        let (root, source) = {
            let runtime = self.runtime.read();
            (runtime.root.clone(), runtime.source)
        };
        self.install_root(app, root, source, Vec::new())
    }

    pub fn choose_library_dir(
        &self,
        app: &AppHandle,
        dir: Option<PathBuf>,
    ) -> Result<Option<LibrarySnapshot>> {
        let Some(dir) = dir else {
            return Ok(None);
        };
        if !dir.is_dir() {
            bail!("所选曲库目录不存在或不是文件夹：{}", dir.display());
        }
        let root = fs::canonicalize(&dir).unwrap_or(dir);
        self.persist_library_dir(Some(root.to_string_lossy().into_owned()))?;
        let snapshot = self.install_root(app, root, LibrarySource::Custom, Vec::new())?;
        Ok(Some(snapshot))
    }

    pub fn reset_library_dir(&self, app: &AppHandle) -> Result<LibrarySnapshot> {
        self.persist_library_dir(None)?;
        let prefs = self.prefs.read().clone();
        let (root, source, warnings) = resolve_root(app, &prefs);
        self.install_root(app, root, source, warnings)
    }

    pub fn song(&self, key: &str) -> Option<Song> {
        self.runtime.read().library.song(key).cloned()
    }

    pub fn root(&self) -> PathBuf {
        self.runtime.read().root.clone()
    }

    fn persist_library_dir(&self, library_dir: Option<String>) -> Result<()> {
        let mut prefs = self.prefs.write();
        let mut next = prefs.clone();
        next.library_dir = library_dir;
        next.save(&self.prefs_path).context("保存曲库目录失败")?;
        *prefs = next;
        Ok(())
    }

    fn install_root(
        &self,
        app: &AppHandle,
        root: PathBuf,
        source: LibrarySource,
        mut warnings: Vec<String>,
    ) -> Result<LibrarySnapshot> {
        allow_audio_scope(app, &root)?;
        let mut library = scan_library(&root, source);
        warnings.append(&mut library.warnings);
        library.warnings = warnings;
        *self.runtime.write() = Runtime {
            root,
            source,
            library: library.clone(),
        };
        Ok(library)
    }
}

fn resolve_root(app: &AppHandle, prefs: &Prefs) -> (PathBuf, LibrarySource, Vec<String>) {
    let mut warnings = Vec::new();

    if let Some(dir) = env::var_os("TFY_BGM_DIR") {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return (absolute(path), LibrarySource::Env, warnings);
        }
        warnings.push(format!(
            "环境变量 TFY_BGM_DIR 指向的目录不存在，已忽略：{}",
            path.display()
        ));
    }

    if let Some(dir) = &prefs.library_dir {
        let path = PathBuf::from(dir);
        if path.is_dir() {
            return (absolute(path), LibrarySource::Custom, warnings);
        }
        warnings.push(format!(
            "自定义曲库目录不存在，已回退到内置曲库：{}",
            path.display()
        ));
    }

    resolve_bundled_root(app, warnings)
}

fn resolve_bundled_root(
    app: &AppHandle,
    mut warnings: Vec<String>,
) -> (PathBuf, LibrarySource, Vec<String>) {
    let resource_root = app.path().resource_dir().ok().map(|dir| dir.join("bgm"));
    if let Some(root) = resource_root.as_ref().filter(|root| root.is_dir()) {
        return (root.clone(), LibrarySource::Bundled, warnings);
    }

    #[cfg(debug_assertions)]
    {
        let dev_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bgm");
        if dev_root.is_dir() {
            return (dev_root, LibrarySource::Bundled, warnings);
        }
    }

    let fallback = resource_root.unwrap_or_else(|| PathBuf::from("bgm"));
    warnings.push(format!(
        "未找到内置曲库目录，请通过「更换目录」选择文件夹：{}",
        fallback.display()
    ));
    (fallback, LibrarySource::Bundled, warnings)
}

fn absolute(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

fn allow_audio_scope(app: &AppHandle, root: &Path) -> Result<()> {
    if root.is_dir() {
        app.asset_protocol_scope()
            .allow_directory(root, true)
            .with_context(|| format!("无法授权音频目录：{}", root.display()))?;
    }
    Ok(())
}
