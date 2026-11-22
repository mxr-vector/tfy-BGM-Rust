//! 应用运行期状态：偏好、分类、配置与最近一次扫描结果。
//!
//! 曲库根目录由 [`crate::library_root`] 解析，启动配置由 [`crate::settings`] 加载；
//! 本模块只负责持有多份运行时状态、串联核心写操作，并保证「先持久化再更新内存」。
//! 所有写操作先落盘再更新内存，并通过独立的命令修改曲库目录，避免前端偏好覆盖目录选择。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use bgm_core::categories::Categories;
use bgm_core::export::ExportSettings;
use bgm_core::ops::{self, BatchOutcome, BatchReport, KeyChange};
use bgm_core::tags::MetaCache;
use bgm_core::{
    ApiConfig, AppConfig, Category, CorsConfig, LibrarySnapshot, LibrarySource, LoopBook,
    LoopPoint, Prefs, Schedule, Song, scan_library_cached,
};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::library_root;
use crate::library_watch::LibraryWatcher;
use crate::models::{self, ModelRuntime, ModelsInfo};
use crate::settings::Settings;

/// 曲库目录发生外部变化时发给前端的事件名。
pub const LIBRARY_CHANGED_EVENT: &str = "library-changed";

pub struct AppState {
    prefs_path: PathBuf,
    categories_path: PathBuf,
    config_path: PathBuf,
    schedule_path: PathBuf,
    loops_path: PathBuf,
    prefs: RwLock<Prefs>,
    categories: RwLock<Categories>,
    config: RwLock<AppConfig>,
    schedule: RwLock<Schedule>,
    loops: RwLock<LoopBook>,
    runtime: RwLock<Runtime>,
    meta_cache: MetaCache,
    watcher: Mutex<Option<LibraryWatcher>>,
    /// ASR 模型运行期状态：已加载模型与下载任务。
    models: Arc<ModelRuntime>,
    /// 曲库写操作期间抑制 watch 扫描，避免「文件已移动、命令还没迁移 key」造成误删收藏。
    ops_lock: Mutex<()>,
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
    pub schedule: Schedule,
    pub loops: LoopBook,
    /// 当前操作系统标识：`windows` / `linux` / `macos` / `other`。
    pub platform: &'static str,
}

/// 外部文件变化后的增量结果；前端据此更新队列并提示增删。
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LibraryChange {
    pub library: LibrarySnapshot,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub moved: Vec<KeyChange>,
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
            schedule_path,
            loops_path,
            config,
            prefs,
            categories,
            schedule,
            loops,
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

        let meta_cache = MetaCache::default();
        let mut library = scan_library_cached(&root, source, &categories.items, &meta_cache);
        warnings.append(&mut library.warnings);
        library.warnings = warnings;
        library_root::allow_audio_scope(app, &root)?;

        // 模型目录只在进入「模型」分组时扫描；启动阶段仅确保目录存在，
        // 避免在大目录 / 网络盘上拖慢启动。
        let models_root = models::root_for(&prefs);
        if let Err(error) = models::ensure_root(&models_root) {
            eprintln!(
                "[models] 无法创建模型目录 {}：{error}",
                models_root.display()
            );
        }

        Ok(Self {
            prefs_path,
            categories_path,
            config_path,
            schedule_path,
            loops_path,
            prefs: RwLock::new(prefs),
            categories: RwLock::new(categories),
            config: RwLock::new(config),
            schedule: RwLock::new(schedule),
            loops: RwLock::new(loops),
            runtime: RwLock::new(Runtime {
                root,
                source,
                library,
            }),
            meta_cache,
            watcher: Mutex::new(None),
            models: Arc::new(ModelRuntime::default()),
            ops_lock: Mutex::new(()),
        })
    }

    pub fn bootstrap(&self) -> Bootstrap {
        Bootstrap {
            prefs: self.prefs.read().clone(),
            library: self.runtime.read().library.clone(),
            config: self.config.read().clone(),
            schedule: self.schedule.read().clone(),
            loops: self.loops.read().clone(),
            platform: crate::desktop::platform(),
        }
    }

    /// 「常规」里的桌面集成开关：开机自动启动、最小化托盘、GPU 加速。
    pub fn desktop_prefs(&self) -> (bool, bool, bool) {
        let prefs = self.prefs.read();
        (
            prefs.autostart,
            prefs.minimize_to_tray,
            prefs.gpu_acceleration,
        )
    }

    pub fn save_prefs(&self, incoming: Prefs) -> Result<Prefs> {
        let rates = self.config.read().player.rates.clone();
        let mut current = self.prefs.write();
        let mut next = incoming.normalize_with_rates(&rates);
        // 曲库目录与模型目录只允许通过专用命令修改，避免偏好保存把目录改坏；
        // 后两项由模型 / 歌词命令维护，前端整包保存时不得覆盖。
        next.library_dir = current.library_dir.clone();
        next.models_dir = current.models_dir.clone();
        next.last_asr_model = current.last_asr_model.clone();
        next.desktop_lyrics_enabled = current.desktop_lyrics_enabled;
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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

    /// 当前曲库快照（供开放接口读取，不占用界面）。
    pub fn library_snapshot(&self) -> LibrarySnapshot {
        self.runtime.read().library.clone()
    }

    /// 开放接口配置；修改 `config.json` 后重启应用生效。
    pub fn api_config(&self) -> bgm_core::ApiConfig {
        self.config.read().api.clone()
    }

    /// 保存 token 与跨域策略并立即写回 `config.json`；端口与启用状态保持不变。
    pub fn update_api_config(&self, token: String, cors: CorsConfig) -> Result<ApiConfig> {
        let mut config = self.config.read().clone();
        config.api.token = token;
        config.api.cors = cors;
        config.api = config.api.normalize();
        config
            .save(&self.config_path)
            .context("保存开放接口配置失败")?;
        let api = config.api.clone();
        *self.config.write() = config;
        Ok(api)
    }

    pub fn root(&self) -> PathBuf {
        self.runtime.read().root.clone()
    }

    /// 模型运行期状态（已加载模型 / 下载任务），供异步命令跨线程持有。
    pub fn model_runtime(&self) -> Arc<ModelRuntime> {
        self.models.clone()
    }

    /// 当前生效的模型目录：用户自定义优先，否则为系统「下载」目录下的 `bgm-models`。
    pub fn models_root(&self) -> PathBuf {
        models::root_for(&self.prefs.read())
    }

    /// 上次加载成功的模型 id（相对模型目录）；启动时据此自动恢复。
    pub fn last_asr_model(&self) -> Option<String> {
        self.prefs.read().last_asr_model.clone()
    }

    /// 记录 / 清除启动自动恢复目标；落盘失败只记录日志，不影响本次加载结果。
    pub fn remember_loaded_model(&self, id: Option<String>) {
        self.update_prefs(|prefs| prefs.last_asr_model = id);
    }

    /// 桌面歌词开关；启动时由前端在模型可用后恢复。
    pub fn set_desktop_lyrics_enabled(&self, enabled: bool) {
        self.update_prefs(|prefs| prefs.desktop_lyrics_enabled = enabled);
    }

    /// 修改后端维护的偏好字段并立即落盘；失败只记录日志，不阻断调用方流程。
    fn update_prefs(&self, update: impl FnOnce(&mut Prefs)) {
        let mut current = self.prefs.write();
        let mut next = current.clone();
        update(&mut next);
        if *current == next {
            return;
        }
        if let Err(error) = next.save(&self.prefs_path) {
            eprintln!("[prefs] 保存偏好设置失败：{error:#}");
            return;
        }
        *current = next;
    }

    /// 当前生效的下载地址；自定义为空时使用内置的 0.6B-int8 地址。
    pub fn asr_download_url(&self) -> String {
        let prefs = self.prefs.read();
        if prefs.asr_download_url.is_empty() {
            models::DEFAULT_DOWNLOAD_URL.to_owned()
        } else {
            prefs.asr_download_url.clone()
        }
    }

    /// 「模型」分组快照：每次进入分组都重新扫描目录下的 `.onnx`。
    pub fn models_info(&self) -> ModelsInfo {
        let (custom_dir, download_url) = {
            let prefs = self.prefs.read();
            (prefs.models_dir.clone(), prefs.asr_download_url.clone())
        };
        let root = custom_dir
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(models::default_root);

        let mut warnings = Vec::new();
        if let Err(error) = models::ensure_root(&root) {
            warnings.push(format!("无法创建模型目录 {}：{error}", root.display()));
        }
        let (list, mut scan_warnings) = models::scan(&root);
        warnings.append(&mut scan_warnings);

        ModelsInfo {
            root: root.to_string_lossy().into_owned(),
            is_default_root: custom_dir.is_none(),
            default_download_url: models::DEFAULT_DOWNLOAD_URL.to_owned(),
            download_url: if download_url.is_empty() {
                models::DEFAULT_DOWNLOAD_URL.to_owned()
            } else {
                download_url
            },
            default_model: models::default_model(&root),
            models: list,
            loaded: self.models.loaded(),
            load_status: self.models.load_status(),
            downloading: self.models.is_downloading(),
            warnings,
        }
    }

    /// 切换模型目录；`None` 表示用户取消选择，保持当前目录。
    pub fn choose_models_dir(&self, dir: Option<PathBuf>) -> Result<Option<ModelsInfo>> {
        let Some(dir) = dir else {
            return Ok(None);
        };
        if !dir.is_dir() {
            bail!("所选模型目录不存在或不是文件夹：{}", dir.display());
        }
        let root = fs::canonicalize(&dir).unwrap_or(dir);
        // Windows 上 canonicalize 会带 `\\?\` 前缀，落盘前去掉，保证与扫描路径一致。
        let stored = bgm_core::paths::strip_verbatim_prefix(&root.to_string_lossy());
        self.persist_models_dir(Some(stored))?;
        Ok(Some(self.models_info()))
    }

    /// 恢复默认模型目录（系统「下载」目录下的 `bgm-models`）。
    pub fn reset_models_dir(&self) -> Result<ModelsInfo> {
        self.persist_models_dir(None)?;
        Ok(self.models_info())
    }

    fn persist_models_dir(&self, models_dir: Option<String>) -> Result<()> {
        let mut prefs = self.prefs.write();
        let mut next = prefs.clone();
        next.models_dir = models_dir;
        next.save(&self.prefs_path).context("保存模型目录失败")?;
        *prefs = next;
        Ok(())
    }

    pub fn categories_path(&self) -> PathBuf {
        self.categories_path.clone()
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_path.clone()
    }

    /// 保存节目单；时间范围重叠会直接拒绝，避免同一时刻触发多档节目。
    pub fn save_schedule(&self, incoming: Schedule) -> Result<Schedule> {
        let schedule = incoming.normalize();
        if let Some(error) = schedule.overlap_error() {
            bail!("{error}");
        }
        schedule
            .save(&self.schedule_path)
            .context("保存节目单失败")?;
        *self.schedule.write() = schedule.clone();
        Ok(schedule)
    }

    pub fn save_loop_point(
        &self,
        key: &str,
        start: f64,
        end: f64,
        enabled: bool,
    ) -> Result<LoopBook> {
        if self.song(key).is_none() {
            bail!("曲目不存在：{key}");
        }
        let mut current = self.loops.write();
        let mut next = current.clone();
        next.set(
            key,
            LoopPoint {
                start,
                end,
                enabled,
            },
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        next.save(&self.loops_path).context("保存循环点失败")?;
        *current = next.clone();
        Ok(next)
    }

    pub fn remove_loop_point(&self, key: &str) -> Result<LoopBook> {
        let mut current = self.loops.write();
        let mut next = current.clone();
        if !next.remove(key) {
            return Ok(next);
        }
        next.save(&self.loops_path).context("保存循环点失败")?;
        *current = next.clone();
        Ok(next)
    }

    /// 启动文件监听；失败时只记录日志，前端仍可手动重新扫描。
    pub fn start_watching(&self, app: &AppHandle) -> Result<()> {
        let root = self.root();
        self.start_watching_at(app, root)
    }

    fn start_watching_at(&self, app: &AppHandle, root: PathBuf) -> Result<()> {
        let watcher = LibraryWatcher::start(app.clone(), root)?;
        *self.watcher.lock() = Some(watcher);
        Ok(())
    }

    fn restart_watcher(&self, app: &AppHandle, root: &Path) -> Result<()> {
        *self.watcher.lock() = None;
        self.start_watching_at(app, root.to_path_buf())
    }

    /// watch 事件或定期兜底触发：重新扫描并计算增删/移动，内容未变时返回 `None`。
    pub fn handle_watch_event(&self, app: &AppHandle) -> Option<LibraryChange> {
        let _guard = self.ops_lock.try_lock()?;
        let (root, source, previous) = {
            let runtime = self.runtime.read();
            (
                runtime.root.clone(),
                runtime.source,
                runtime.library.clone(),
            )
        };
        let categories = self.category_items();
        let mut next = scan_library_cached(&root, source, &categories, &self.meta_cache);
        if same_content(&previous, &next) {
            return None;
        }

        let diff = diff_songs(&previous.songs, &next.songs);
        next.warnings
            .extend(self.sync_track_metadata(&diff.moved, &diff.removed));
        // 外部重命名 / 移动音频文件时，字幕记录跟着 key 一起迁移。
        let migrations = key_migrations(&diff.moved);
        if !migrations.is_empty() {
            let warnings = crate::subtitles::migrate_keys(app, &migrations, &next.songs);
            next.warnings.extend(warnings);
        }

        *self.runtime.write() = Runtime {
            root,
            source,
            library: next.clone(),
        };
        Some(LibraryChange {
            library: next,
            added: diff.added,
            removed: diff.removed,
            moved: diff.moved,
        })
    }

    /// 当前偏好对应的导出设置（响度归一化开关 + 场景预设）。
    pub fn export_settings(&self) -> ExportSettings {
        let prefs = self.prefs.read();
        ExportSettings::from_prefs(prefs.loudness_normalization, &prefs.export_scene)
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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
        let _guard = self.ops_lock.lock();
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
            self.export_settings(),
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
        let mut warnings = self.sync_track_metadata(&changes, &removed);
        let migrations = key_migrations(&changes);
        let mut library = self.refresh_library(app)?;
        if !migrations.is_empty() {
            // 刷新曲库后再迁移：需要新曲目的标题 / 文件名 / 时长写入字幕记录。
            warnings.extend(crate::subtitles::migrate_keys(
                app,
                &migrations,
                &library.songs,
            ));
        }
        library.warnings.append(&mut warnings);
        Ok(LibraryMutation {
            library,
            changes,
            removed,
            report,
        })
    }

    /// 把重命名/移动产生的 key 变化同步到收藏与循环点，删除的 key 直接移除。
    fn sync_track_metadata(&self, changes: &[KeyChange], removed: &[String]) -> Vec<String> {
        let remap: HashMap<&str, &str> = changes
            .iter()
            .filter(|change| change.from != change.to)
            .map(|change| (change.from.as_str(), change.to.as_str()))
            .collect();
        let removed_set: HashSet<&str> = removed.iter().map(String::as_str).collect();
        if remap.is_empty() && removed_set.is_empty() {
            return Vec::new();
        }

        let mut warnings = Vec::new();
        {
            let mut prefs = self.prefs.write();
            let mut next = prefs.clone();
            next.favorites = next
                .favorites
                .iter()
                .map(|key| remap.get(key.as_str()).copied().unwrap_or(key))
                .filter(|key| !removed_set.contains(key))
                .map(str::to_owned)
                .collect();
            next = next.normalize();
            match next.save(&self.prefs_path) {
                Ok(()) => *prefs = next,
                Err(error) => warnings.push(format!("收藏列表同步失败：{error}")),
            }
        }
        {
            let mut loops = self.loops.write();
            let mut next = loops.clone();
            next.apply_key_changes(changes, removed);
            if next != *loops {
                match next.save(&self.loops_path) {
                    Ok(()) => *loops = next,
                    Err(error) => warnings.push(format!("循环点同步失败：{error}")),
                }
            }
        }
        warnings
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
        let previous_root = self.runtime.read().root.clone();
        library_root::allow_audio_scope(app, &root)?;
        let categories = self.category_items();
        let mut library = scan_library_cached(&root, source, &categories, &self.meta_cache);
        warnings.append(&mut library.warnings);

        let watcher_missing = self.watcher.lock().is_none();
        if (previous_root != root || watcher_missing)
            && let Err(error) = self.restart_watcher(app, &root)
        {
            warnings.push(format!("文件监听不可用，外部改动需要手动重新扫描：{error}"));
        }

        library.warnings = warnings;
        *self.runtime.write() = Runtime {
            root,
            source,
            library: library.clone(),
        };
        Ok(library)
    }
}

struct SongDiff {
    added: Vec<String>,
    removed: Vec<String>,
    moved: Vec<KeyChange>,
}

/// 只保留真正发生变化的 `from → to` 对，供字幕等按 key 持久化的数据迁移。
fn key_migrations(changes: &[KeyChange]) -> Vec<(String, String)> {
    changes
        .iter()
        .filter(|change| change.from != change.to)
        .map(|change| (change.from.clone(), change.to.clone()))
        .collect()
}

fn same_content(previous: &LibrarySnapshot, next: &LibrarySnapshot) -> bool {
    previous.songs == next.songs
        && previous.categories.len() == next.categories.len()
        && previous
            .categories
            .iter()
            .zip(&next.categories)
            .all(|(left, right)| {
                left.id == right.id
                    && left.count == right.count
                    && left.name_zh == right.name_zh
                    && left.name_en == right.name_en
                    && left.label_en == right.label_en
                    && left.color == right.color
                    && left.icon == right.icon
                    && left.title == right.title
                    && left.description == right.description
            })
}

/// 以 key 为基准比较新旧曲目，并把「同名文件换了分类目录」识别为移动。
fn diff_songs(previous: &[Song], next: &[Song]) -> SongDiff {
    let old_by_key: HashMap<&str, &Song> = previous
        .iter()
        .map(|song| (song.key.as_str(), song))
        .collect();
    let new_by_key: HashMap<&str, &Song> =
        next.iter().map(|song| (song.key.as_str(), song)).collect();

    let mut moved = Vec::new();
    let mut removed = Vec::new();
    let mut consumed: HashSet<&str> = HashSet::new();

    for old_song in previous {
        if new_by_key.contains_key(old_song.key.as_str()) {
            continue;
        }
        let candidate = next.iter().find(|song| {
            !old_by_key.contains_key(song.key.as_str())
                && !consumed.contains(song.key.as_str())
                && song.file_name == old_song.file_name
                && song.title == old_song.title
        });
        match candidate {
            Some(song) => {
                consumed.insert(song.key.as_str());
                moved.push(KeyChange {
                    from: old_song.key.clone(),
                    to: song.key.clone(),
                });
            }
            None => removed.push(old_song.key.clone()),
        }
    }

    let added = next
        .iter()
        .filter(|song| {
            !old_by_key.contains_key(song.key.as_str()) && !consumed.contains(song.key.as_str())
        })
        .map(|song| song.key.clone())
        .collect();

    SongDiff {
        added,
        removed,
        moved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(category: &str, stem: &str) -> Song {
        Song {
            key: format!("{category}/{stem}"),
            title: stem.replace('_', " "),
            file_name: format!("{stem}.mp3"),
            category: category.to_owned(),
            path: String::new(),
            artist: None,
            album: None,
            genre: None,
            duration_ms: None,
        }
    }

    #[test]
    fn diff_reports_added_removed_and_moved() {
        let previous = vec![
            song("cheerful", "a"),
            song("epic", "b"),
            song("solitude", "c"),
        ];
        let next = vec![
            song("cheerful", "a"),
            song("memories", "b"),
            song("country", "d"),
        ];

        let diff = diff_songs(&previous, &next);

        assert_eq!(
            diff.moved,
            vec![KeyChange {
                from: "epic/b".into(),
                to: "memories/b".into(),
            }]
        );
        assert_eq!(diff.removed, vec!["solitude/c"]);
        assert_eq!(diff.added, vec!["country/d"]);
    }

    #[test]
    fn rename_is_reported_as_add_plus_remove() {
        let previous = vec![song("epic", "old_name")];
        let next = vec![song("epic", "new_name")];

        let diff = diff_songs(&previous, &next);

        assert!(diff.moved.is_empty());
        assert_eq!(diff.removed, vec!["epic/old_name"]);
        assert_eq!(diff.added, vec!["epic/new_name"]);
    }
}
