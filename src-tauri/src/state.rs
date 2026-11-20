//! 应用运行期状态：偏好、曲库根目录与最近一次扫描结果。
//!
//! 曲库根目录解析优先级：`TFY_BGM_DIR` 环境变量 → 用户自选目录 → 当前工作目录下的
//! `bgm/`（内置默认）→ 应用资源目录 → 开发仓库目录。
//! 所有写操作先持久化再更新内存，并通过独立的命令修改曲库目录，避免前端偏好覆盖目录选择。

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bgm_core::ops::{self, BatchOutcome, BatchReport, KeyChange};
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

/// 曲库写操作后的统一结果：新快照 + key 迁移 + 操作统计。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMutation {
    pub library: LibrarySnapshot,
    pub changes: Vec<KeyChange>,
    /// 被删除曲目的旧 key，前端据此清理收藏与选择。
    pub removed: Vec<String>,
    pub report: BatchReport,
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

    /// 导入音频到指定分类，完成后重新扫描曲库。
    pub fn import_tracks(
        &self,
        app: &AppHandle,
        category: &str,
        sources: Vec<PathBuf>,
    ) -> Result<LibraryMutation> {
        if sources.is_empty() {
            bail!("未选择任何音频文件");
        }
        if bgm_core::categories::by_id(category).is_none() {
            bail!("未注册的分类：{category}");
        }
        let report = ops::import_files(&self.root(), category, &sources);
        let library = self.refresh_library(app)?;
        Ok(LibraryMutation {
            library,
            changes: Vec::new(),
            removed: Vec::new(),
            report,
        })
    }

    /// 修改曲名与分类；文件名变化会同步迁移收藏 key。
    pub fn update_track(
        &self,
        app: &AppHandle,
        key: &str,
        title: &str,
        category: &str,
    ) -> Result<LibraryMutation> {
        let song = self
            .song(key)
            .ok_or_else(|| anyhow::anyhow!("曲目不存在：{key}"))?;
        let change = ops::update_song(&self.root(), &song, Some(title), Some(category))
            .map_err(|error| anyhow::anyhow!("{error}"))?;

        let mut report = BatchReport {
            total: 1,
            succeeded: 1,
            ..BatchReport::default()
        };
        if change.from == change.to {
            report.messages.push("曲目未发生变化".to_owned());
        }

        let favorite_warning = self.sync_favorites(std::slice::from_ref(&change), &[]);
        let mut library = self.refresh_library(app)?;
        if let Some(warning) = favorite_warning {
            library.warnings.push(warning);
        }
        Ok(LibraryMutation {
            library,
            changes: vec![change],
            removed: Vec::new(),
            report,
        })
    }

    pub fn move_tracks(
        &self,
        app: &AppHandle,
        keys: &[String],
        category: &str,
    ) -> Result<LibraryMutation> {
        if bgm_core::categories::by_id(category).is_none() {
            bail!("未注册的分类：{category}");
        }
        let songs = self.songs_for_keys(keys)?;
        let refs: Vec<&Song> = songs.iter().collect();
        let outcome = ops::move_songs(&self.root(), &refs, category);

        let favorite_warning = self.sync_favorites(&outcome.changes, &[]);
        let mut library = self.refresh_library(app)?;
        if let Some(warning) = favorite_warning {
            library.warnings.push(warning);
        }
        Ok(mutation_from(library, outcome))
    }

    /// 软删除：移入曲库根目录下的 `.tfy-trash`，并清理对应收藏。
    pub fn delete_tracks(&self, app: &AppHandle, keys: &[String]) -> Result<LibraryMutation> {
        let songs = self.songs_for_keys(keys)?;
        let refs: Vec<&Song> = songs.iter().collect();
        let outcome = ops::trash_songs(&self.root(), &refs);

        let favorite_warning = self.sync_favorites(&[], &outcome.removed);
        let mut library = self.refresh_library(app)?;
        if let Some(warning) = favorite_warning {
            library.warnings.push(warning);
        }
        Ok(mutation_from(library, outcome))
    }

    /// 批量导出到指定文件夹，不改变曲库内容。
    pub fn export_tracks(&self, keys: &[String], dest_dir: &Path) -> Result<BatchOutcome> {
        let songs = self.songs_for_keys(keys)?;
        let refs: Vec<&Song> = songs.iter().collect();
        Ok(ops::export_songs(&self.root(), dest_dir, &refs))
    }

    fn songs_for_keys(&self, keys: &[String]) -> Result<Vec<Song>> {
        if keys.is_empty() {
            bail!("未选择任何曲目");
        }
        let runtime = self.runtime.read();
        let songs: Vec<Song> = keys
            .iter()
            .filter_map(|key| runtime.library.song(key).cloned())
            .collect();
        if songs.is_empty() {
            bail!("选中的曲目均已不在曲库中");
        }
        Ok(songs)
    }

    /// 把重命名/移动产生的 key 变化同步到收藏，删除的 key 直接移除。
    fn sync_favorites(&self, changes: &[KeyChange], removed: &[String]) -> Option<String> {
        let remap: HashMap<&str, &str> = changes
            .iter()
            .filter(|change| change.from != change.to)
            .map(|change| (change.from.as_str(), change.to.as_str()))
            .collect();
        let removed: HashSet<&str> = removed.iter().map(String::as_str).collect();
        if remap.is_empty() && removed.is_empty() {
            return None;
        }

        let mut prefs = self.prefs.write();
        let mut next = prefs.clone();
        next.favorites = next
            .favorites
            .iter()
            .map(|key| remap.get(key.as_str()).copied().unwrap_or(key))
            .filter(|key| !removed.contains(key))
            .map(str::to_owned)
            .collect();
        next = next.normalize();
        match next.save(&self.prefs_path) {
            Ok(()) => {
                *prefs = next;
                None
            }
            Err(error) => Some(format!("收藏列表同步失败：{error}")),
        }
    }

    fn refresh_library(&self, app: &AppHandle) -> Result<LibrarySnapshot> {
        let (root, source) = {
            let runtime = self.runtime.read();
            (runtime.root.clone(), runtime.source)
        };
        self.install_root(app, root, source, Vec::new())
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

fn mutation_from(library: LibrarySnapshot, outcome: BatchOutcome) -> LibraryMutation {
    LibraryMutation {
        library,
        changes: outcome.changes,
        removed: outcome.removed,
        report: outcome.report,
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
    // 内置曲库默认取当前工作目录下的 bgm/，便于 portable 分发与开发调试。
    let current_root = env::current_dir().ok().map(|dir| dir.join("bgm"));
    if let Some(root) = current_root.as_ref().filter(|root| root.is_dir()) {
        return (absolute(root.clone()), LibrarySource::Bundled, warnings);
    }

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

    let fallback = current_root
        .or(resource_root)
        .unwrap_or_else(|| PathBuf::from("bgm"));
    warnings.push(format!(
        "未找到内置曲库目录（当前路径下的 bgm/），请通过「更换目录」选择文件夹：{}",
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
