//! 应用运行期状态：偏好、分类、配置与最近一次扫描结果。
//!
//! 曲库根目录由 [`crate::library_root`] 解析，启动配置由 [`crate::settings`] 加载；
//! 本模块只负责持有多份运行时状态、串联核心写操作，并保证「先持久化再更新内存」。
//! 所有写操作先落盘再更新内存，并通过独立的命令修改曲库目录，避免前端偏好覆盖目录选择。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use bgm_core::categories::Categories;
use bgm_core::ops::{self, BatchOutcome, BatchReport, KeyChange};
use bgm_core::{AppConfig, Category, LibrarySnapshot, LibrarySource, Prefs, Song, scan_library};
use parking_lot::RwLock;
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::library_root;
use crate::settings::Settings;

pub struct AppState {
    prefs_path: PathBuf,
    categories_path: PathBuf,
    config_path: PathBuf,
    prefs: RwLock<Prefs>,
    categories: RwLock<Categories>,
    config: RwLock<AppConfig>,
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
    pub config: AppConfig,
}

/// 重新读取 `config.json` 的结果；`warnings` 用于提示损坏或自动修正。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReload {
    pub config: AppConfig,
    pub warnings: Vec<String>,
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

/// 分类新增 / 重命名后的结果；`category_id` 便于前端直接选中新分类。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryOutcome {
    pub library: LibrarySnapshot,
    pub category_id: String,
}

impl AppState {
    pub fn initialize(app: &AppHandle) -> Result<Self> {
        let Settings {
            config_path,
            prefs_path,
            categories_path,
            config,
            prefs,
            categories,
            mut warnings,
        } = Settings::load(app)?;

        // 首屏就要显示自定义壁纸，启动阶段先把文件加进 asset 白名单。
        crate::wallpapers::prime_custom(app, prefs.custom_wallpaper.as_deref());

        let (root, source, mut root_warnings) = library_root::resolve(app, &prefs);
        if source == LibrarySource::External {
            root_warnings.append(&mut library_root::ensure_category_dirs(
                &root,
                &categories.items,
            ));
        }
        warnings.append(&mut root_warnings);

        let mut library = scan_library(&root, source, &categories.items);
        warnings.append(&mut library.warnings);
        library.warnings = warnings;
        library_root::allow_audio_scope(app, &root)?;

        Ok(Self {
            prefs_path,
            categories_path,
            config_path,
            prefs: RwLock::new(prefs),
            categories: RwLock::new(categories),
            config: RwLock::new(config),
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
            config: self.config.read().clone(),
        }
    }

    pub fn save_prefs(&self, incoming: Prefs) -> Result<Prefs> {
        let rates = self.config.read().player.rates.clone();
        let mut current = self.prefs.write();
        let mut next = incoming.normalize_with_rates(&rates);
        // 曲库目录只允许通过 choose/reset 命令修改，避免偏好保存把目录改坏。
        next.library_dir = current.library_dir.clone();
        next.save(&self.prefs_path).context("保存偏好设置失败")?;
        *current = next.clone();
        Ok(next)
    }

    /// 重新读取 `config.json` 并同步窗口标题；供「重新扫描」时热加载。
    pub fn reload_config(&self, app: &AppHandle) -> ConfigReload {
        let (config, warning) = AppConfig::load(&self.config_path);
        let mut warnings: Vec<String> = warning.into_iter().collect();
        *self.config.write() = config.clone();
        warnings.append(&mut self.apply_window_title(app, &config));
        ConfigReload { config, warnings }
    }

    /// 把配置里的窗口标题应用到系统窗口。
    pub fn apply_window_title(&self, app: &AppHandle, config: &AppConfig) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(window) = app.get_webview_window("main")
            && let Err(error) = window.set_title(&config.brand.window_title)
        {
            warnings.push(format!("无法更新窗口标题：{error}"));
        }
        warnings
    }

    pub fn rescan(&self, app: &AppHandle) -> Result<LibrarySnapshot> {
        let warnings = self.reload_categories();
        let (root, source) = {
            let runtime = self.runtime.read();
            (runtime.root.clone(), runtime.source)
        };
        self.install_root(app, root, source, warnings)
    }

    /// 新增分类：更新 categories.json，并尽量在曲库目录下创建同名文件夹。
    pub fn create_category(
        &self,
        app: &AppHandle,
        name: &str,
        id: Option<&str>,
    ) -> Result<CategoryOutcome> {
        let (category_id, mut warnings) = self.save_categories(|next| {
            let category = next
                .add(name, id)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(category.id)
        })?;

        let dir = self.root().join(&category_id);
        if let Err(error) = fs::create_dir_all(&dir) {
            warnings.push(format!(
                "分类已添加，但无法创建曲库目录 {}：{error}",
                dir.display()
            ));
        }

        self.finish_category_change(app, category_id, warnings)
    }

    /// 修改分类展示名称 / 主题色，不移动已导入的音频文件。
    pub fn update_category(
        &self,
        app: &AppHandle,
        id: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<CategoryOutcome> {
        let (category_id, warnings) = self.save_categories(|next| {
            next.update(id, name, color)
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            Ok(id.to_owned())
        })?;
        self.finish_category_change(app, category_id, warnings)
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
        let (root, source, mut warnings) = library_root::resolve(app, &prefs);
        if source == LibrarySource::External {
            warnings.append(&mut library_root::ensure_category_dirs(
                &root,
                &self.category_items(),
            ));
        }
        self.install_root(app, root, source, warnings)
    }

    pub fn song(&self, key: &str) -> Option<Song> {
        self.runtime.read().library.song(key).cloned()
    }

    pub fn root(&self) -> PathBuf {
        self.runtime.read().root.clone()
    }

    pub fn categories_path(&self) -> PathBuf {
        self.categories_path.clone()
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_path.clone()
    }

    /// 配置中的窗口标题，用于启动阶段设置系统窗口。
    pub fn window_title(&self) -> String {
        self.config.read().brand.window_title.clone()
    }

    pub fn has_category(&self, id: &str) -> bool {
        self.categories.read().contains(id)
    }

    fn category_items(&self) -> Vec<Category> {
        self.categories.read().items.clone()
    }

    /// 重新读取 categories.json；用于「重新扫描」和分类写操作前同步手工编辑。
    fn reload_categories(&self) -> Vec<String> {
        let (categories, warning) = Categories::load(&self.categories_path);
        *self.categories.write() = categories;
        warning.into_iter().collect()
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
        if !self.has_category(category) {
            bail!("未注册的分类：{category}");
        }
        let categories = self.category_items();
        let report = ops::import_files(&self.root(), &categories, category, &sources);
        self.finish_mutation(app, Vec::new(), Vec::new(), report)
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
        let categories = self.category_items();
        let change = ops::update_song(
            &self.root(),
            &categories,
            &song,
            Some(title),
            Some(category),
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;

        let mut report = BatchReport {
            total: 1,
            succeeded: 1,
            ..BatchReport::default()
        };
        if change.from == change.to {
            report.messages.push("曲目未发生变化".to_owned());
        }

        self.finish_mutation(app, vec![change], Vec::new(), report)
    }

    pub fn move_tracks(
        &self,
        app: &AppHandle,
        keys: &[String],
        category: &str,
    ) -> Result<LibraryMutation> {
        if !self.has_category(category) {
            bail!("未注册的分类：{category}");
        }
        let songs = self.songs_for_keys(keys)?;
        let refs: Vec<&Song> = songs.iter().collect();
        let categories = self.category_items();
        let BatchOutcome {
            changes,
            removed,
            report,
        } = ops::move_songs(&self.root(), &categories, &refs, category);
        self.finish_mutation(app, changes, removed, report)
    }

    /// 软删除：移入曲库根目录下的 `.tfy-trash`，并清理对应收藏。
    pub fn delete_tracks(&self, app: &AppHandle, keys: &[String]) -> Result<LibraryMutation> {
        let songs = self.songs_for_keys(keys)?;
        let refs: Vec<&Song> = songs.iter().collect();
        let categories = self.category_items();
        let BatchOutcome {
            changes,
            removed,
            report,
        } = ops::trash_songs(&self.root(), &categories, &refs);
        self.finish_mutation(app, changes, removed, report)
    }

    /// 批量导出到指定文件夹，不改变曲库内容。
    pub fn export_tracks(&self, keys: &[String], dest_dir: &Path) -> Result<BatchOutcome> {
        let songs = self.songs_for_keys(keys)?;
        let refs: Vec<&Song> = songs.iter().collect();
        let categories = self.category_items();
        Ok(ops::export_songs(
            &self.root(),
            &categories,
            dest_dir,
            &refs,
        ))
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

    /// 写操作后的统一收尾：迁移收藏 key、重新扫描并组装返回结果。
    fn finish_mutation(
        &self,
        app: &AppHandle,
        changes: Vec<KeyChange>,
        removed: Vec<String>,
        report: BatchReport,
    ) -> Result<LibraryMutation> {
        let favorite_warning = self.sync_favorites(&changes, &removed);
        let mut library = self.refresh_library(app)?;
        if let Some(warning) = favorite_warning {
            library.warnings.push(warning);
        }
        Ok(LibraryMutation {
            library,
            changes,
            removed,
            report,
        })
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

    /// 更新分类配置并返回新的分类 id；调用方负责后续目录操作与曲库刷新。
    fn save_categories(
        &self,
        mutate: impl FnOnce(&mut Categories) -> Result<String>,
    ) -> Result<(String, Vec<String>)> {
        let warnings = self.reload_categories();
        let mut next = self.categories.read().clone();
        let category_id = mutate(&mut next)?;
        next.save(&self.categories_path)
            .context("保存分类配置失败")?;
        *self.categories.write() = next;
        Ok((category_id, warnings))
    }

    fn finish_category_change(
        &self,
        app: &AppHandle,
        category_id: String,
        mut warnings: Vec<String>,
    ) -> Result<CategoryOutcome> {
        let mut library = self.refresh_library(app)?;
        warnings.append(&mut library.warnings);
        library.warnings = warnings;
        Ok(CategoryOutcome {
            library,
            category_id,
        })
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
        library_root::allow_audio_scope(app, &root)?;
        let categories = self.category_items();
        let mut library = scan_library(&root, source, &categories);
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
