//! 曲库运行状态、增量扫描、文件操作与偏好持久化。

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::settings::{Appearance, OutputRoute, decode_prefs, normalize_prefs};

use bgm_core::library::CategorySummary;
use bgm_core::ops::{self, KeyChange};
use bgm_core::paths::{
    CATEGORIES_FILE, CONFIG_FILE, LIBRARY_DIR_NAME, LIBRARY_ENV_VAR, LOOPS_FILE, PREFS_FILE,
    SCHEDULE_FILE,
};
use bgm_core::tags::MetaCache;
use bgm_core::{
    AppConfig, Categories, ExportSettings, LibrarySnapshot, LibrarySource, LoopBook, LoopPoint,
    Prefs, Schedule, ScheduleSong, Song, SortBy, scan_library_cached,
};

mod asr;
mod export;
pub(crate) mod separation;
mod sherpa;
mod subtitles;
pub(crate) use asr::model_id as api_model_id;
pub use asr::{
    AsrSession, DEFAULT_ASR_DOWNLOAD_URL, DEFAULT_MODEL_NAME, DownloadMode, DownloadProgress,
    ModelCatalog, ModelDownload, ModelPreferences, WORKER_FLAG, run_asr_worker,
};
pub use export::Exporter;
pub(crate) use subtitles::generation as subtitle_generation;
pub use subtitles::{
    MAX_SUBTITLE_SEGMENTS, SeparatedAudio, SubtitleDelivery, SubtitleEdit, SubtitleFormat,
    SubtitleSegment, SubtitleSummary, delete_subtitle, export_subtitle, list_subtitles,
    load_subtitle,
};
pub(crate) use subtitles::{SubtitleWriteError, delete_subtitle_if_present, subtitle_paths};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryFilter {
    All,
    Favorites,
    Category(String),
}

#[derive(Debug, Clone)]
pub struct Mutation {
    pub message: String,
    pub changes: Vec<KeyChange>,
    pub removed: Vec<String>,
    pub report: Option<bgm_core::BatchReport>,
}

/// watch 事件触发的增量结果；`added/removed/moved` 供界面调整队列并提示。
pub struct LibraryChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub moved: Vec<KeyChange>,
    pub migration_warnings: Vec<String>,
}

pub struct WatchScan {
    revision: u64,
    prior_scan_ms: u64,
    root: PathBuf,
    source: LibrarySource,
    categories: Vec<bgm_core::Category>,
    cache: Arc<MetaCache>,
}

impl WatchScan {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn run(self) -> Option<(u64, LibrarySnapshot)> {
        fs::read_dir(&self.root).ok()?;
        let mut snapshot =
            scan_library_cached(&self.root, self.source, &self.categories, &self.cache);
        fs::read_dir(&self.root).ok()?;
        snapshot.scanned_at_ms = snapshot.scanned_at_ms.max(self.prior_scan_ms);
        Some((self.revision, snapshot))
    }
}

pub struct ExportBatch {
    songs: Vec<Song>,
    settings: ExportSettings,
    skipped: Vec<String>,
}

const PREF_WRITE_DELAY: Duration = Duration::from_millis(280);

enum PreferenceWrite {
    Due(Instant),
    Failed,
}

impl ExportBatch {
    pub fn run(self, directory: &Path) -> Result<bgm_core::BatchReport, String> {
        let mut report = Exporter::new(self.settings).batch(&self.songs, directory)?;
        add_batch_skips(&mut report, self.skipped);
        Ok(report)
    }
}

pub struct Library {
    pub prefs: Prefs,
    pub appearance: Appearance,
    pub config: AppConfig,
    pub snapshot: LibrarySnapshot,
    pub schedule: Schedule,
    pub loops: LoopBook,
    prefs_path: PathBuf,
    appearance_path: PathBuf,
    categories_path: PathBuf,
    config_path: PathBuf,
    schedule_path: PathBuf,
    loops_path: PathBuf,
    categories: Categories,
    root: PathBuf,
    source: LibrarySource,
    meta_cache: Arc<MetaCache>,
    scan_revision: u64,
    recommended_order: HashMap<String, usize>,
    extra_prefs: Map<String, Value>,
    preference_write: Option<PreferenceWrite>,
}

impl Library {
    pub fn open() -> Self {
        Self::open_in(config_dir(), resolve_root)
    }

    fn open_in(
        config_dir: PathBuf,
        root_for: impl FnOnce(&Prefs) -> (PathBuf, LibrarySource, Vec<String>),
    ) -> Self {
        let _ = fs::create_dir_all(&config_dir);
        let config_path = config_dir.join(CONFIG_FILE);
        let prefs_path = config_dir.join(PREFS_FILE);
        let appearance_path = config_dir.join("appearance.json");
        let categories_path = config_dir.join(CATEGORIES_FILE);
        let schedule_path = config_dir.join(SCHEDULE_FILE);
        let loops_path = config_dir.join(LOOPS_FILE);

        let (config, config_warning) = AppConfig::load(&config_path);
        if let Err(error) = config.ensure_file(&config_path) {
            eprintln!("无法写入应用配置：{error}");
        }

        let (mut extra_prefs, read_warning) = match fs::read(&prefs_path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(fields) => (fields, None),
                Err(error) => (
                    Map::new(),
                    Some(format!("偏好设置文件损坏，已重置为默认值：{error}")),
                ),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => (Map::new(), None),
            Err(error) => (
                Map::new(),
                Some(format!("读取偏好设置失败，已使用默认值：{error}")),
            ),
        };
        let (appearance, appearance_warning) =
            load_appearance(&appearance_path, &prefs_path, &mut extra_prefs);
        let (mut prefs, prefs_warning) = match decode_prefs(extra_prefs.clone()) {
            Ok(prefs) => (prefs, None),
            Err(error) => (
                Prefs::default(),
                Some(format!("偏好设置文件损坏，已重置为默认值：{error}")),
            ),
        };
        let migrate_export_scene = prefs.version < 2;
        prefs = normalize_prefs(prefs, &config.player.rates);
        let migration_warning = if migrate_export_scene {
            prefs.export_scene.clear();
            write_preferences(&prefs_path, &extra_prefs, &prefs).err()
        } else {
            None
        };

        let (categories, categories_warning) = Categories::load(&categories_path);
        if let Err(error) = categories.ensure_file(&categories_path) {
            eprintln!("无法写入分类配置：{error}");
        }

        let (schedule, schedule_warning) = Schedule::load(&schedule_path);
        let (loops, loops_warning) = LoopBook::load(&loops_path);

        let (root, source, mut warnings) = root_for(&prefs);
        if source == LibrarySource::External {
            warnings.extend(ensure_category_dirs(&root, &categories.items));
        }
        for warning in [
            config_warning,
            read_warning,
            prefs_warning,
            migration_warning,
            appearance_warning,
            categories_warning,
            schedule_warning,
            loops_warning,
        ]
        .into_iter()
        .flatten()
        {
            warnings.push(warning);
        }

        let meta_cache = Arc::new(MetaCache::default());
        let mut snapshot = scan_library_cached(&root, source, &categories.items, &meta_cache);
        warnings.append(&mut snapshot.warnings);
        snapshot.warnings = warnings;

        Self {
            prefs,
            appearance,
            config,
            snapshot,
            schedule,
            loops,
            prefs_path,
            appearance_path,
            categories_path,
            config_path,
            schedule_path,
            loops_path,
            categories,
            root,
            source,
            meta_cache,
            scan_revision: 0,
            recommended_order: HashMap::new(),
            extra_prefs,
            preference_write: None,
        }
    }

    pub fn window_title(&self) -> String {
        self.config.brand.window_title.clone()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    pub fn categories_path(&self) -> &Path {
        &self.categories_path
    }

    pub fn subtitle_directory(&self) -> PathBuf {
        self.config_path.with_file_name("subtitles")
    }

    pub fn subtitle_format(&self) -> SubtitleFormat {
        match self.prefs.subtitle_format.as_str() {
            "lrc" => SubtitleFormat::Lrc,
            _ => SubtitleFormat::Srt,
        }
    }

    pub fn set_subtitle_format(&mut self, format: SubtitleFormat) -> Result<(), String> {
        self.update_playback_settings(|prefs| prefs.subtitle_format = format.extension().into())
    }

    pub fn asr_model_preferences(&self) -> ModelPreferences {
        ModelPreferences::from_prefs(&self.prefs)
    }

    pub fn set_last_asr_model(&mut self, model: Option<&str>) -> Result<(), String> {
        let model = model
            .map(|model| {
                asr::model_id(model)
                    .map(str::to_owned)
                    .ok_or("模型标识无效")
            })
            .transpose()?;
        let mut next = self.prefs.clone();
        next.last_asr_model = model;
        if next.last_asr_model.is_none() {
            next.desktop_lyrics_enabled = false;
        }
        self.commit_preferences(next)
    }

    pub fn set_asr_model_directory(&mut self, directory: Option<&Path>) -> Result<(), String> {
        let directory = directory
            .map(|path| {
                path.to_str()
                    .and_then(asr::model_directory)
                    .map(str::to_owned)
                    .ok_or("模型目录必须是有效文本且不超过 512 个字符")
            })
            .transpose()?;
        let mut next = self.prefs.clone();
        next.models_dir = directory;
        self.commit_preferences(next)
    }

    pub fn set_asr_download_url(&mut self, url: &str) -> Result<(), String> {
        if !asr::valid_download_url(url) {
            return Err("请输入有效的 HTTP(S) 地址，最多 1024 个字符且不含空白".into());
        }
        self.update_playback_settings(|prefs| prefs.asr_download_url = url.into())
    }

    pub fn desktop_lyrics_enabled(&self) -> bool {
        self.prefs.desktop_lyrics_enabled
    }

    pub fn set_desktop_lyrics_enabled(&mut self, enabled: bool) -> Result<(), String> {
        let mut next = self.prefs.clone();
        next.desktop_lyrics_enabled = enabled;
        self.commit_preferences(next)
    }

    pub fn desktop_lyrics_style(&mut self) -> (crate::desktop_lyrics::Style, Option<String>) {
        let (style, migrated) = crate::desktop_lyrics::Style::read(
            self.extra_prefs.get(crate::desktop_lyrics::STYLE_KEY),
        );
        let error = if migrated {
            self.set_desktop_lyrics_style(&style).err()
        } else {
            None
        };
        (style, error)
    }

    pub fn set_desktop_lyrics_style(
        &mut self,
        style: &crate::desktop_lyrics::Style,
    ) -> Result<(), String> {
        let value = serde_json::to_value(style).map_err(|error| error.to_string())?;
        let mut next = self.extra_prefs.clone();
        next.insert(crate::desktop_lyrics::STYLE_KEY.into(), value);
        write_preferences(&self.prefs_path, &next, &self.prefs)?;
        self.extra_prefs = next;
        self.preference_write = None;
        Ok(())
    }

    pub fn save_api_config(&self, config: &crate::api::Config) -> Result<(), String> {
        let mut document: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(
            &fs::read(&self.config_path).map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("配置文件无法解析：{error}"))?;
        document.insert("api".into(), config.document());
        bgm_core::persist::write_json(&self.config_path, &document)
            .map_err(|error| error.to_string())
    }

    pub fn using_custom_dir(&self) -> bool {
        self.prefs.library_dir.is_some()
    }

    /// 当前曲库来源的展示名，与主线的 sourceLabel 一致。
    pub fn source_label(&self) -> &'static str {
        match self.source {
            LibrarySource::Custom => "自定义目录",
            LibrarySource::External => "外置曲库",
            LibrarySource::Env => "环境变量目录",
            LibrarySource::Bundled => "内置曲库",
        }
    }

    pub fn song(&self, key: &str) -> Option<&Song> {
        self.snapshot.song(key)
    }

    pub fn visible(&self, filter: &LibraryFilter, query: &str) -> Vec<Song> {
        let favorites: HashSet<&str> = self.prefs.favorites.iter().map(String::as_str).collect();
        let mut songs = filter_songs(
            &self.snapshot.songs,
            &self.snapshot.categories,
            &favorites,
            filter,
            query,
            self.prefs.sort_by,
        );
        if self.prefs.sort_by == SortBy::Default && !self.recommended_order.is_empty() {
            songs.sort_by_key(|song| {
                self.recommended_order
                    .get(&song.key)
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }
        songs
    }

    pub fn reorder_song(&mut self, from: &str, to: &str) {
        if from == to || self.prefs.sort_by != SortBy::Default {
            return;
        }
        let mut songs = self.visible(&LibraryFilter::All, "");
        let Some(source) = songs.iter().position(|song| song.key == from) else {
            return;
        };
        let Some(target) = songs.iter().position(|song| song.key == to) else {
            return;
        };
        let song = songs.remove(source);
        songs.insert(target, song);
        self.recommended_order = songs
            .into_iter()
            .enumerate()
            .map(|(index, song)| (song.key, index))
            .collect();
    }

    pub fn rescan(&mut self) -> Vec<String> {
        self.scan_revision = self.scan_revision.wrapping_add(1);
        let (config, config_warning) = AppConfig::load(&self.config_path);
        self.config = config;
        let (categories, categories_warning) = Categories::load(&self.categories_path);
        self.categories = categories;
        self.prefs = normalize_prefs(self.prefs.clone(), &self.config.player.rates);

        let mut warnings = Vec::new();
        if let Some(warning) = config_warning {
            warnings.push(warning);
        }
        if let Some(warning) = categories_warning {
            warnings.push(warning);
        }
        let same_root = self.snapshot.root == self.root.to_string_lossy()
            && self.snapshot.source == self.source;
        if same_root && fs::read_dir(&self.root).is_err() {
            self.mark_root_offline();
            return self.snapshot.warnings.clone();
        }
        if self.source == LibrarySource::External {
            warnings.extend(ensure_category_dirs(&self.root, &self.categories.items));
        }
        let mut snapshot = scan_library_cached(
            &self.root,
            self.source,
            &self.categories.items,
            &self.meta_cache,
        );
        if same_root && fs::read_dir(&self.root).is_err() {
            self.mark_root_offline();
            return self.snapshot.warnings.clone();
        }
        warnings.append(&mut snapshot.warnings);
        snapshot.warnings = warnings;
        snapshot.scanned_at_ms = snapshot.scanned_at_ms.max(self.snapshot.scanned_at_ms);
        self.snapshot = snapshot;
        self.snapshot.warnings.clone()
    }

    pub fn watch_scan(&mut self) -> WatchScan {
        self.scan_revision = self.scan_revision.wrapping_add(1);
        WatchScan {
            revision: self.scan_revision,
            prior_scan_ms: self.snapshot.scanned_at_ms,
            root: self.root.clone(),
            source: self.source,
            categories: self.categories.items.clone(),
            cache: self.meta_cache.clone(),
        }
    }

    pub fn apply_watch_scan(
        &mut self,
        revision: u64,
        next: LibrarySnapshot,
    ) -> Option<LibraryChange> {
        let was_offline = self.snapshot.warnings.first() == Some(&self.offline_warning());
        if revision != self.scan_revision
            || next.root != self.snapshot.root
            || next.source != self.snapshot.source
            || next.scanned_at_ms < self.snapshot.scanned_at_ms
            || (!was_offline && same_content(&self.snapshot, &next))
        {
            return None;
        }
        let diff = diff_songs(&self.snapshot.songs, &next.songs);
        self.snapshot = next;
        let migration_warnings = self.apply_key_changes(&diff.moved, &diff.removed);
        Some(LibraryChange {
            added: diff.added,
            removed: diff.removed,
            moved: diff.moved,
            migration_warnings,
        })
    }

    fn offline_warning(&self) -> String {
        format!(
            "曲库暂时无法读取，已保留最近的扫描结果：{}",
            self.root.display()
        )
    }

    pub fn mark_root_offline(&mut self) -> bool {
        let warning = self.offline_warning();
        if self.snapshot.warnings.first() == Some(&warning) {
            return false;
        }
        self.snapshot.warnings.insert(0, warning);
        true
    }

    pub fn is_current_watch_scan(&self, revision: u64) -> bool {
        revision == self.scan_revision
    }

    pub fn choose_dir(&mut self, dir: PathBuf) -> Result<(), String> {
        if !dir.is_dir() {
            return Err(format!("所选曲库目录不存在：{}", dir.display()));
        }
        let root = fs::canonicalize(&dir).unwrap_or(dir);
        let mut next = self.prefs.clone();
        next.library_dir = Some(root.to_string_lossy().into_owned());
        self.commit_preferences(next)?;
        self.root = root;
        self.source = LibrarySource::Custom;
        self.rescan();
        Ok(())
    }

    pub fn reset_dir(&mut self) -> Result<(), String> {
        let mut next = self.prefs.clone();
        next.library_dir = None;
        self.commit_preferences(next)?;
        let (root, source, warnings) = resolve_root(&self.prefs);
        self.root = root;
        self.source = source;
        let mut found = self.rescan();
        found.extend(warnings);
        self.snapshot.warnings = found;
        Ok(())
    }

    pub fn toggle_favorite(&mut self, key: &str) -> Result<bool, String> {
        if let Some(index) = self.prefs.favorites.iter().position(|item| item == key) {
            self.prefs.favorites.remove(index);
            self.persist_prefs()?;
            Ok(false)
        } else {
            self.prefs.favorites.push(key.to_owned());
            self.persist_prefs()?;
            Ok(true)
        }
    }

    pub fn set_volume(&mut self, volume: f32) -> Result<(), String> {
        self.set_volume_state(Some(volume), None)
    }

    pub fn set_volume_state(
        &mut self,
        volume: Option<f32>,
        muted: Option<bool>,
    ) -> Result<(), String> {
        let previous = (self.prefs.volume, self.prefs.muted);
        if let Some(volume) = volume {
            self.prefs.volume = volume.clamp(0.0, 1.0);
            self.prefs.muted = self.prefs.volume <= 0.0;
        }
        if let Some(muted) = muted {
            self.prefs.muted = muted;
        }
        if let Err(error) = self.persist_prefs() {
            (self.prefs.volume, self.prefs.muted) = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn preview_volume(&mut self, volume: f32, now: Instant) {
        self.prefs.volume = volume.clamp(0.0, 1.0);
        self.prefs.muted = self.prefs.volume <= 0.0;
        self.defer_preferences(now);
    }

    pub fn set_crossfade(&mut self, seconds: f32) -> Result<(), String> {
        self.update_playback_settings(|prefs| prefs.crossfade_seconds = seconds)
    }

    pub fn set_outputs(&mut self, routes: Vec<OutputRoute>) -> Result<(), String> {
        self.update_playback_settings(|prefs| prefs.output_routes = routes)
    }

    pub fn update_playback_settings(
        &mut self,
        update: impl FnOnce(&mut Prefs),
    ) -> Result<(), String> {
        let mut next = self.prefs.clone();
        update(&mut next);
        next.library_dir.clone_from(&self.prefs.library_dir);
        next.models_dir.clone_from(&self.prefs.models_dir);
        next.last_asr_model.clone_from(&self.prefs.last_asr_model);
        next.desktop_lyrics_enabled = self.prefs.desktop_lyrics_enabled;
        self.commit_preferences(next)
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), String> {
        self.prefs.muted = muted;
        if !muted && self.prefs.volume <= 0.0 {
            self.prefs.volume = 0.85;
        }
        self.persist_prefs()
    }

    pub fn set_appearance(&mut self, appearance: Appearance) -> Result<(), String> {
        bgm_core::persist::write_json(&self.appearance_path, &appearance)
            .map_err(|error| format!("无法保存外观设置：{error}"))?;
        self.appearance = appearance;
        Ok(())
    }

    pub fn set_rate(&mut self, rate: f32) -> Result<(), String> {
        self.prefs.playback_rate = rate;
        self.persist_prefs()
    }

    pub fn cycle_rate(&mut self) -> Result<f32, String> {
        let rates = &self.config.player.rates;
        let index = rates
            .iter()
            .position(|rate| (*rate - self.prefs.playback_rate).abs() < 0.001)
            .unwrap_or(0);
        let next = rates.get((index + 1) % rates.len()).copied().unwrap_or(1.0);
        self.set_rate(next)?;
        Ok(next)
    }

    pub fn set_mode(&mut self, mode: bgm_core::PlayMode) -> Result<(), String> {
        let previous = self.prefs.play_mode;
        self.prefs.play_mode = mode;
        if let Err(error) = self.persist_prefs() {
            self.prefs.play_mode = previous;
            return Err(error);
        }
        Ok(())
    }

    pub fn set_sort(&mut self, sort: SortBy) -> Result<(), String> {
        self.prefs.sort_by = sort;
        self.persist_prefs()
    }

    /// 导出响度归一化开关与场景预设，随下一次导出生效。
    pub fn set_export_settings(
        &mut self,
        loudness_normalization: bool,
        export_scene: &str,
    ) -> Result<(), String> {
        self.prefs.loudness_normalization = loudness_normalization;
        self.prefs.export_scene = export_scene.to_owned();
        self.persist_prefs()
    }

    /// 壁纸预设和最近一次自定义路径。
    pub fn set_wallpaper(&mut self, id: &str, custom_path: Option<&str>) -> Result<(), String> {
        self.prefs.wallpaper = id.to_owned();
        if id == "custom" {
            self.prefs.custom_wallpaper = custom_path.map(str::to_owned);
        }
        self.persist_prefs()
    }

    pub fn set_wallpaper_opacity(&mut self, opacity: f32, now: Instant) {
        self.prefs.wallpaper_opacity = opacity.clamp(0.0, 1.0);
        self.defer_preferences(now);
    }

    pub fn set_wallpaper_blur(&mut self, blur: u32, now: Instant) {
        self.prefs.wallpaper_blur = blur.min(20);
        self.defer_preferences(now);
    }

    pub fn set_wallpaper_fit(&mut self, fit: &str) -> Result<(), String> {
        self.prefs.wallpaper_fit = if fit == "contain" { "contain" } else { "cover" }.to_owned();
        self.persist_prefs()
    }

    /// 节目单中的时间范围互斥。
    pub fn save_schedule(&mut self, incoming: Schedule) -> Result<Schedule, String> {
        let schedule = incoming.normalize();
        if let Some(error) = schedule.overlap_error() {
            return Err(error);
        }
        schedule
            .save(&self.schedule_path)
            .map_err(|error| format!("保存节目单失败：{error}"))?;
        self.schedule = schedule.clone();
        Ok(schedule)
    }

    /// 保存单曲 A/B 循环点；起点/终点单位为秒，终点至少比起点晚 0.2 秒。
    pub fn save_loop_point(
        &mut self,
        key: &str,
        start: f64,
        end: f64,
        enabled: bool,
    ) -> Result<LoopBook, String> {
        if self.song(key).is_none() {
            return Err(format!("曲目不存在：{key}"));
        }
        let mut next = self.loops.clone();
        next.set(
            key,
            LoopPoint {
                start,
                end,
                enabled,
            },
        )
        .map_err(|error| format!("保存循环点失败：{error}"))?;
        next.save(&self.loops_path)
            .map_err(|error| format!("保存循环点失败：{error}"))?;
        self.loops = next.clone();
        Ok(next)
    }

    pub fn remove_loop_point(&mut self, key: &str) -> Result<LoopBook, String> {
        let mut next = self.loops.clone();
        if !next.remove(key) {
            return Ok(next);
        }
        next.save(&self.loops_path)
            .map_err(|error| format!("保存循环点失败：{error}"))?;
        self.loops = next.clone();
        Ok(next)
    }

    pub fn loop_point(&self, key: &str) -> Option<LoopPoint> {
        self.loops.get(key)
    }

    /// 当前偏好对应的导出设置（响度归一化开关 + 场景预设）。
    pub fn export_settings(&self) -> ExportSettings {
        ExportSettings::from_prefs(self.prefs.loudness_normalization, &self.prefs.export_scene)
    }

    /// 导出能力探测：ffmpeg 路径（未安装为 `None`）与场景预设清单。
    pub fn import(&mut self, category: &str, files: &[PathBuf]) -> Mutation {
        if !self.categories.contains(category) {
            return Mutation::message(format!("未注册的分类：{category}"));
        }
        let report = ops::import_files(&self.root, &self.categories.items, category, files);
        self.rescan();
        Mutation {
            message: format_report(&report),
            changes: Vec::new(),
            removed: Vec::new(),
            report: Some(report),
        }
    }

    pub fn update_track(
        &mut self,
        key: &str,
        title: &str,
        category: &str,
    ) -> Result<Mutation, String> {
        let song = self
            .song(key)
            .cloned()
            .ok_or_else(|| format!("曲目不存在：{key}"))?;
        let title = title.trim();
        if title != song.title
            && self.categories.contains(category)
            && fs::read_dir(self.root.join(category)).is_ok_and(|mut entries| {
                entries.any(|entry| {
                    let Ok(entry) = entry else { return false };
                    let name = entry.file_name();
                    let path = Path::new(&name);
                    (song.category != category || name != song.file_name.as_str())
                        && name
                            .to_str()
                            .is_some_and(bgm_core::library::has_audio_extension)
                        && path
                            .file_stem()
                            .and_then(|stem| stem.to_str())
                            .is_some_and(|stem| stem.eq_ignore_ascii_case(title))
                        && entry.path().is_file()
                })
            })
        {
            return Err(format!("目标分类已存在同名曲目：{title}"));
        }
        let change = ops::update_song(
            &self.root,
            &self.categories.items,
            &song,
            Some(title),
            Some(category),
        )
        .map_err(|error| error.to_string())?;
        self.rescan();
        let warnings = self.apply_key_changes(std::slice::from_ref(&change), &[]);
        let message = if change.from == change.to {
            "曲目没有变化".into()
        } else {
            format!("已更新《{}》", title.trim())
        };
        Ok(Mutation {
            message: with_migration_warnings(message, warnings),
            changes: vec![change],
            removed: Vec::new(),
            report: None,
        })
    }

    pub fn move_song(&mut self, key: &str, category: &str) -> Result<Mutation, String> {
        let song = self
            .song(key)
            .cloned()
            .ok_or_else(|| format!("曲目不存在：{key}"))?;
        let change = ops::move_song(&self.root, &self.categories.items, &song, category)
            .map_err(|error| error.to_string())?;
        self.rescan();
        let warnings = self.apply_key_changes(std::slice::from_ref(&change), &[]);
        Ok(Mutation {
            message: with_migration_warnings(format!("已移动到 {category}"), warnings),
            changes: vec![change],
            removed: Vec::new(),
            report: None,
        })
    }

    pub fn move_batch(&mut self, keys: &[String], target: &str) -> Result<Mutation, String> {
        let (songs, skipped) = self.resolve_batch(keys)?;
        let mut outcome = ops::move_songs(&self.root, &self.categories.items, &songs, target);
        for change in &outcome.changes {
            outcome.report.messages.push(if change.from == change.to {
                format!("位置未变：{}", change.from)
            } else {
                format!("已移动：{} → {}", change.from, change.to)
            });
        }
        add_batch_skips(&mut outcome.report, skipped);
        Ok(self.finish_batch(outcome))
    }

    pub fn trash_batch(&mut self, keys: &[String]) -> Result<Mutation, String> {
        let (songs, skipped) = self.resolve_batch(keys)?;
        let mut outcome = ops::trash_songs(&self.root, &self.categories.items, &songs);
        outcome.report.messages.extend(
            outcome
                .removed
                .iter()
                .map(|key| format!("已移入回收站：{key}")),
        );
        add_batch_skips(&mut outcome.report, skipped);
        Ok(self.finish_batch(outcome))
    }

    fn resolve_batch(&self, keys: &[String]) -> Result<(Vec<&Song>, Vec<String>), String> {
        if keys.is_empty() {
            return Err("未选择任何曲目".into());
        }
        let mut songs = Vec::new();
        let mut skipped = Vec::new();
        for key in keys {
            if let Some(song) = self.song(key) {
                songs.push(song);
            } else {
                skipped.push(format!("{key} 已不在曲库中"));
            }
        }
        Ok((songs, skipped))
    }

    fn finish_batch(&mut self, outcome: ops::BatchOutcome) -> Mutation {
        self.rescan();
        let warnings = self.apply_key_changes(&outcome.changes, &outcome.removed);
        Mutation {
            message: with_migration_warnings(format_report(&outcome.report), warnings),
            changes: outcome.changes,
            removed: outcome.removed,
            report: Some(outcome.report),
        }
    }

    pub fn prepare_batch_export(&self, keys: &[String]) -> Result<ExportBatch, String> {
        let (songs, skipped) = self.resolve_batch(keys)?;
        Ok(ExportBatch {
            songs: songs.into_iter().cloned().collect(),
            settings: self.export_settings(),
            skipped,
        })
    }

    pub fn create_category(&mut self, name: &str, id: Option<&str>) -> Result<String, String> {
        let mut next = self.categories.clone();
        let category = next.add(name, id).map_err(|error| error.to_string())?;
        next.save(&self.categories_path)
            .map_err(|error| error.to_string())?;
        self.categories = next;
        let dir = self.root.join(&category.id);
        if let Err(error) = fs::create_dir_all(&dir) {
            self.rescan();
            return Err(format!(
                "分类已保存，但无法创建目录 {}：{error}",
                dir.display()
            ));
        }
        self.rescan();
        Ok(category.id)
    }

    pub fn update_category(
        &mut self,
        id: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<(), String> {
        let mut next = self.categories.clone();
        next.update(id, name, color)
            .map_err(|error| error.to_string())?;
        next.save(&self.categories_path)
            .map_err(|error| error.to_string())?;
        self.rescan();
        Ok(())
    }

    fn apply_key_changes(&mut self, changes: &[KeyChange], removed: &[String]) -> Vec<String> {
        if changes.is_empty() && removed.is_empty() {
            return Vec::new();
        }
        for change in changes {
            if let Some(rank) = self.recommended_order.remove(&change.from) {
                self.recommended_order.insert(change.to.clone(), rank);
            }
        }
        for key in removed {
            self.recommended_order.remove(key);
        }
        let mut warnings = Vec::new();
        let removed_set: HashSet<&str> = removed.iter().map(String::as_str).collect();
        let favorites: Vec<_> = self
            .prefs
            .favorites
            .iter()
            .filter(|key| !removed_set.contains(key.as_str()))
            .map(|key| updated_track_key(key, changes).unwrap_or(key).to_owned())
            .collect();
        if favorites != self.prefs.favorites {
            let previous = std::mem::replace(&mut self.prefs.favorites, favorites);
            if let Err(error) = self.persist_prefs() {
                self.prefs.favorites = previous;
                warnings.push(format!("收藏迁移失败：{error}"));
            }
        }

        let mut loops = self.loops.clone();
        loops.apply_key_changes(changes, removed);
        if loops != self.loops {
            match loops.save(&self.loops_path) {
                Ok(()) => self.loops = loops,
                Err(error) => warnings.push(format!("循环点迁移失败：{error}")),
            }
        }

        if changes.iter().any(|change| change.from != change.to) {
            let mut schedule = self.schedule.clone();
            for entry in &mut schedule.entries {
                remap_scheduled_songs(&mut entry.songs, changes);
            }
            if schedule != self.schedule {
                match schedule.save(&self.schedule_path) {
                    Ok(()) => self.schedule = schedule,
                    Err(error) => warnings.push(format!("节目单迁移失败：{error}")),
                }
            }
        }
        let directory = self.subtitle_directory();
        warnings.extend(
            changes
                .iter()
                .filter(|change| change.from != change.to)
                .filter_map(|change| {
                    let song = self.song(&change.to)?;
                    subtitles::migrate_subtitle(&directory, &change.from, song)
                        .err()
                        .map(|error| {
                            format!(
                                "{} 的字幕迁移失败，原记录可从字幕页查找：{error}",
                                change.from
                            )
                        })
                }),
        );
        self.snapshot.warnings.extend(warnings.iter().cloned());
        warnings
    }

    fn persist_prefs(&mut self) -> Result<(), String> {
        self.commit_preferences(self.prefs.clone())
    }

    fn defer_preferences(&mut self, now: Instant) {
        self.preference_write = Some(PreferenceWrite::Due(now + PREF_WRITE_DELAY));
    }

    pub fn poll_preferences(&mut self, now: Instant) -> Result<(), String> {
        if matches!(self.preference_write, Some(PreferenceWrite::Due(deadline)) if now >= deadline)
        {
            if let Err(error) = self.flush_preferences() {
                self.preference_write = Some(PreferenceWrite::Failed);
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn flush_preferences(&mut self) -> Result<(), String> {
        if self.preference_write.is_some() {
            self.persist_prefs()?;
        }
        Ok(())
    }

    fn commit_preferences(&mut self, next: Prefs) -> Result<(), String> {
        let next = normalize_prefs(next, &self.config.player.rates);
        write_preferences(&self.prefs_path, &self.extra_prefs, &next)?;
        self.prefs = next;
        self.preference_write = None;
        Ok(())
    }
}

fn with_migration_warnings(message: String, warnings: Vec<String>) -> String {
    std::iter::once(message)
        .chain(warnings)
        .collect::<Vec<_>>()
        .join("。")
}

fn updated_track_key<'a>(key: &str, changes: &'a [KeyChange]) -> Option<&'a str> {
    changes
        .iter()
        .find(|change| change.from == key && change.from != change.to)
        .map(|change| change.to.as_str())
}

pub fn remap_scheduled_songs(songs: &mut [ScheduleSong], changes: &[KeyChange]) {
    for song in songs {
        if let Some(key) = updated_track_key(&song.key, changes) {
            song.key = key.to_owned();
        }
    }
}

fn prefs_document(
    original: &Map<String, Value>,
    prefs: &Prefs,
) -> Result<Map<String, Value>, serde_json::Error> {
    let version = original
        .get("version")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        .max(u64::from(prefs.version));
    let mut fields = original.clone();
    let shared: Map<String, Value> = serde_json::from_value(serde_json::to_value(prefs)?)?;
    fields.extend(shared);
    fields.insert("version".into(), version.into());
    Ok(fields)
}

fn write_preferences(
    path: &Path,
    original: &Map<String, Value>,
    prefs: &Prefs,
) -> Result<(), String> {
    let fields =
        prefs_document(original, prefs).map_err(|error| format!("保存偏好失败：{error}"))?;
    bgm_core::persist::write_json(path, &fields).map_err(|error| format!("保存偏好失败：{error}"))
}

fn load_appearance(
    appearance_path: &Path,
    prefs_path: &Path,
    fields: &mut Map<String, Value>,
) -> (Appearance, Option<String>) {
    let legacy: Option<Appearance> = fields
        .get("appearance")
        .map(|value| serde_json::from_value(value.clone()).unwrap_or_default());
    let fallback = legacy.unwrap_or_default();
    let appearance = match fs::read(appearance_path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(appearance) => appearance,
            Err(error) => return (fallback, Some(format!("外观设置无法解析：{error}"))),
        },
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let Some(legacy) = legacy else {
                return (fallback, None);
            };
            if let Err(error) = bgm_core::persist::write_json(appearance_path, &legacy) {
                return (legacy, Some(format!("无法迁移外观设置：{error}")));
            }
            legacy
        }
        Err(error) => return (fallback, Some(format!("无法读取外观设置：{error}"))),
    };
    if let Some(value) = fields.remove("appearance")
        && let Err(error) = bgm_core::persist::write_json(prefs_path, fields)
    {
        fields.insert("appearance".into(), value);
        return (appearance, Some(format!("无法更新共享偏好：{error}")));
    }
    (appearance, None)
}

impl Mutation {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            changes: Vec::new(),
            removed: Vec::new(),
            report: None,
        }
    }
}

pub fn filter_songs(
    songs: &[Song],
    categories: &[CategorySummary],
    favorites: &HashSet<&str>,
    filter: &LibraryFilter,
    query: &str,
    sort: SortBy,
) -> Vec<Song> {
    let query = query.trim().to_lowercase();
    let matched: Vec<Song> = songs
        .iter()
        .filter(|song| {
            let category_ok = match filter {
                LibraryFilter::All => true,
                LibraryFilter::Favorites => favorites.contains(song.key.as_str()),
                LibraryFilter::Category(id) => &song.category == id,
            };
            if !category_ok {
                return false;
            }
            if query.is_empty() {
                return true;
            }
            if song.title.to_lowercase().contains(&query)
                || song.category.to_lowercase().contains(&query)
            {
                return true;
            }
            categories.iter().any(|category| {
                category.id == song.category
                    && (category.name_zh.to_lowercase().contains(&query)
                        || category.name_en.to_lowercase().contains(&query)
                        || category.label_en.to_lowercase().contains(&query))
            })
        })
        .cloned()
        .collect();

    match sort {
        SortBy::Name => sort_by_title(matched),
        SortBy::Default => matched,
    }
}

#[cfg(target_os = "macos")]
fn sort_by_title(songs: Vec<Song>) -> Vec<Song> {
    use std::cmp::Ordering;

    use objc2_foundation::{NSLocale, NSRange, NSString, NSStringCompareOptions};

    let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str("zh_CN"));
    let mut titles: Vec<_> = songs
        .into_iter()
        .map(|song| (NSString::from_str(&song.title), song))
        .collect();
    titles.sort_unstable_by(|(left_title, left), (right_title, right)| {
        let order: Ordering = unsafe {
            left_title.compare_options_range_locale(
                right_title,
                NSStringCompareOptions::CaseInsensitiveSearch,
                NSRange::new(0, left_title.length()),
                Some(&locale),
            )
        }
        .into();
        order.then_with(|| left.key.cmp(&right.key))
    });
    titles.into_iter().map(|(_, song)| song).collect()
}

#[cfg(not(target_os = "macos"))]
fn sort_by_title(mut songs: Vec<Song>) -> Vec<Song> {
    songs.sort_by_cached_key(|song| (song.title.to_lowercase(), song.key.clone()));
    songs
}

fn add_batch_skips(report: &mut bgm_core::BatchReport, skipped: Vec<String>) {
    report.total += skipped.len();
    report.skipped += skipped.len();
    report.messages.extend(skipped);
}

pub fn format_report(report: &bgm_core::BatchReport) -> String {
    if report.total > 1 || report.failed > 0 || report.skipped > 0 {
        return format!(
            "成功 {} 项 · 跳过 {} 项 · 失败 {} 项",
            report.succeeded, report.skipped, report.failed
        );
    }
    if let Some(notice) = &report.notice {
        return notice.clone();
    }
    if let Some(failure) = report.failures.first() {
        return failure.clone();
    }
    if let Some(message) = report.messages.last() {
        return message.clone();
    }
    if report.succeeded > 0 {
        format!("完成 {} 项", report.succeeded)
    } else {
        "没有需要处理的文件".into()
    }
}

fn config_dir() -> PathBuf {
    if let Some(directory) = env::var_os("TFY_BGM_CONFIG_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(directory);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tfy-bgm-gpui")
}

/// 曲库根目录解析，优先级与主线一致：
/// 环境变量 → 用户自选目录 → 开发仓库 `bgm/`（仅 debug）→ 外置曲库。
fn resolve_root(prefs: &Prefs) -> (PathBuf, LibrarySource, Vec<String>) {
    let exe = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|dir| dir.join(LIBRARY_DIR_NAME));
    let cwd = env::current_dir()
        .ok()
        .map(|dir| dir.join(LIBRARY_DIR_NAME));
    let bundled = if cfg!(debug_assertions) {
        dev_bundled_root()
    } else {
        None
    };
    resolve_root_candidates(
        env::var_os(LIBRARY_ENV_VAR).map(PathBuf::from),
        prefs.library_dir.as_deref().map(PathBuf::from),
        bundled,
        [exe, cwd, Some(config_dir().join(LIBRARY_DIR_NAME))],
    )
}

fn resolve_root_candidates(
    from_env: Option<PathBuf>,
    custom: Option<PathBuf>,
    bundled: Option<PathBuf>,
    [exe_root, cwd_root, config_root]: [Option<PathBuf>; 3],
) -> (PathBuf, LibrarySource, Vec<String>) {
    let mut warnings = Vec::new();
    if let Some(path) = from_env {
        if path.is_dir() {
            return (absolute(path), LibrarySource::Env, warnings);
        }
        warnings.push(format!(
            "环境变量 {LIBRARY_ENV_VAR} 指向的目录不存在，已忽略：{}",
            path.display()
        ));
    }
    if let Some(path) = custom {
        if path.is_dir() {
            return (absolute(path), LibrarySource::Custom, warnings);
        }
        warnings.push(format!(
            "自定义曲库目录不存在，已回退到外置曲库：{}",
            path.display()
        ));
    }
    if let Some(root) = bundled.filter(|path| path.is_dir()) {
        return (absolute(root), LibrarySource::Bundled, warnings);
    }

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
        "未找到可用的曲库目录，请在程序同级创建 {LIBRARY_DIR_NAME}/，或通过「切换曲库目录」选择文件夹"
    ));
    (fallback, LibrarySource::External, warnings)
}

fn ensure_category_dirs(root: &Path, categories: &[bgm_core::Category]) -> Vec<String> {
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

fn dev_bundled_root() -> Option<PathBuf> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    [
        repository.join(LIBRARY_DIR_NAME),
        repository.join("../tfy-bgm").join(LIBRARY_DIR_NAME),
    ]
    .into_iter()
    .find(|path| path.is_dir())
}

fn absolute(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

/// 以 key 为基准比较新旧曲目快照，分类逐字段比较（`CategorySummary` 未实现 PartialEq）。
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

struct SongDiff {
    added: Vec<String>,
    removed: Vec<String>,
    moved: Vec<KeyChange>,
}

/// 以 key 为基准比较新旧曲目，并把「同名文件换了分类目录」识别为移动。
fn diff_songs(previous: &[Song], next: &[Song]) -> SongDiff {
    let old_keys: HashSet<_> = previous.iter().map(|song| song.key.as_str()).collect();
    let new_keys: HashSet<_> = next.iter().map(|song| song.key.as_str()).collect();
    let disappeared = previous
        .iter()
        .filter(|song| !new_keys.contains(song.key.as_str()));
    let appeared = next
        .iter()
        .filter(|song| !old_keys.contains(song.key.as_str()));
    let mut candidates = HashMap::<(&str, &str), (usize, usize, Option<&Song>)>::new();
    for song in disappeared.clone() {
        let pair = candidates
            .entry((&song.file_name, &song.title))
            .or_default();
        pair.0 += 1;
    }
    for song in appeared.clone() {
        if let Some(pair) = candidates.get_mut(&(song.file_name.as_str(), song.title.as_str())) {
            pair.1 += 1;
            pair.2 = Some(song);
        }
    }

    let mut moved = Vec::new();
    let mut removed = Vec::new();
    let mut consumed: HashSet<&str> = HashSet::new();

    for song in disappeared {
        if let Some((1, 1, Some(target))) =
            candidates.get(&(song.file_name.as_str(), song.title.as_str()))
        {
            consumed.insert(target.key.as_str());
            moved.push(KeyChange {
                from: song.key.clone(),
                to: target.key.clone(),
            });
        } else {
            removed.push(song.key.clone());
        }
    }

    let added = appeared
        .filter(|song| !consumed.contains(song.key.as_str()))
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
    use bgm_core::library::CategorySummary;

    fn song(stem: &str, title: &str, category: &str) -> Song {
        Song {
            key: format!("{category}/{stem}"),
            title: title.into(),
            file_name: format!("{title}.mp3"),
            category: category.into(),
            path: format!("/tmp/{category}/{stem}.mp3"),
            artist: None,
            album: None,
            genre: None,
            duration_ms: None,
        }
    }

    fn category(id: &str, name: &str) -> CategorySummary {
        CategorySummary {
            id: id.into(),
            name_zh: name.into(),
            name_en: id.into(),
            label_en: id.into(),
            color: "#e2a15a".into(),
            icon: "music".into(),
            title: name.into(),
            description: String::new(),
            count: 1,
        }
    }

    #[test]
    fn migrates_appearance_and_preserves_newer_shared_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let prefs_path = directory.path().join(PREFS_FILE);
        let appearance_path = directory.path().join("appearance.json");
        let original = serde_json::json!({
            "version": 4,
            "appearance": "dark",
            "modelsDir": "/models",
            "desktopLyricsEnabled": true
        });
        let mut fields = original.as_object().unwrap().clone();
        bgm_core::persist::write_json(&prefs_path, &fields).unwrap();

        assert_eq!(
            load_appearance(&appearance_path, &prefs_path, &mut fields),
            (Appearance::Dark, None)
        );
        let on_disk: Value = serde_json::from_slice(&fs::read(&prefs_path).unwrap()).unwrap();
        assert!(on_disk.get("appearance").is_none());
        assert_eq!(on_disk["version"], 4);
        assert_eq!(on_disk["modelsDir"], "/models");
        assert_eq!(on_disk["desktopLyricsEnabled"], true);
        assert_eq!(
            load_appearance(&appearance_path, &prefs_path, &mut fields),
            (Appearance::Dark, None)
        );

        let prefs = serde_json::from_value(Value::Object(fields.clone())).unwrap();
        let updated = prefs_document(&fields, &prefs).unwrap();
        assert!(updated.get("appearance").is_none());
        assert_eq!(updated["version"], 4);
        assert_eq!(updated["modelsDir"], "/models");
        assert_eq!(updated["desktopLyricsEnabled"], true);
    }

    #[test]
    fn desktop_lyrics_migrates_style_and_keeps_it_when_disabled() {
        let config = tempfile::tempdir().unwrap();
        let music = tempfile::tempdir().unwrap();
        let prefs = config.path().join(PREFS_FILE);
        bgm_core::persist::write_json(
            &prefs,
            &serde_json::json!({
                "desktopLyricsEnabled": true,
                "desktopLyricsStyle": {
                    "version": 3,
                    "fontSize": 30,
                    "highlight": "#8ecbff",
                    "height": 520
                }
            }),
        )
        .unwrap();
        let reopen = || {
            Library::open_in(config.path().into(), |_| {
                (music.path().into(), LibrarySource::Custom, Vec::new())
            })
        };
        let mut library = reopen();
        assert!(library.desktop_lyrics_enabled());
        let (mut style, error) = library.desktop_lyrics_style();
        assert_eq!(error, None);
        assert_eq!(style.highlight, "#5fa8e8");
        assert_eq!(style.height, 120);
        assert_eq!(style.font_size, 30);
        let persisted: Value = serde_json::from_slice(&fs::read(&prefs).unwrap()).unwrap();
        assert_eq!(persisted["desktopLyricsStyle"]["version"], 4);
        assert_eq!(persisted["desktopLyricsEnabled"], true);

        style.height = 240;
        style.lead_seconds = 2;
        library.set_desktop_lyrics_style(&style).unwrap();
        library.set_desktop_lyrics_enabled(false).unwrap();
        let mut restored = reopen();
        assert!(!restored.desktop_lyrics_enabled());
        assert_eq!(restored.desktop_lyrics_style(), (style, None));
    }

    #[test]
    fn asr_model_preferences_survive_reopening_and_keep_last_model() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let music = directory.path().join("music");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&music).unwrap();
        let prefs_path = config.join(PREFS_FILE);
        bgm_core::persist::write_json(
            &prefs_path,
            &serde_json::json!({
                "version": 4,
                "lastAsrModel": "saved/qwen",
                "desktopLyricsEnabled": true
            }),
        )
        .unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (music.clone(), LibrarySource::Custom, Vec::new())
            })
        };
        let mut library = reopen();
        let models = directory.path().join("自选模型");
        let download_url = "https://example.org/models/resolve/main";
        library.set_asr_model_directory(Some(&models)).unwrap();
        library.set_asr_download_url(download_url).unwrap();
        assert!(library.set_asr_download_url("file:///not-http").is_err());

        let mut library = reopen();
        let preferences = library.asr_model_preferences();
        assert_eq!(preferences.directory, Some(models));
        assert_eq!(preferences.download_url, download_url);
        library.set_asr_model_directory(None).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&prefs_path).unwrap()).unwrap();
        assert_eq!(written["version"], 4);
        assert_eq!(written["modelsDir"], Value::Null);
        assert_eq!(written["asrDownloadUrl"], download_url);
        assert_eq!(written["lastAsrModel"], "saved/qwen");
        assert_eq!(written["desktopLyricsEnabled"], true);

        assert!(library.set_last_asr_model(Some("../other")).is_err());
        library.set_last_asr_model(Some("local/qwen")).unwrap();
        assert_eq!(
            reopen().asr_model_preferences().last_model.as_deref(),
            Some("local/qwen")
        );
        assert!(reopen().desktop_lyrics_enabled());
        library.set_last_asr_model(None).unwrap();
        let unloaded = reopen();
        assert!(unloaded.asr_model_preferences().last_model.is_none());
        assert!(!unloaded.desktop_lyrics_enabled());
    }

    #[test]
    fn output_preferences_preserve_explicit_selection_after_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let music = directory.path().join("music");
        fs::create_dir_all(&music).unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (music.clone(), LibrarySource::Custom, Vec::new())
            })
        };
        let mut library = reopen();
        let route = OutputRoute {
            device_id: "CoreAudio:headset".into(),
            label: "耳机".into(),
            volume: 0.4,
            muted: true,
            enabled: true,
        };
        library.set_outputs(vec![route.clone()]).unwrap();
        let saved = reopen().prefs.output_routes;
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[0].device_id, "default");
        assert!(!saved[0].enabled);
        assert_eq!(saved[1], route);
    }

    #[test]
    fn desktop_preferences_restore_close_action_and_preserve_other_platform_fields() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let music = directory.path().join("music");
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&music).unwrap();
        let path = config.join(PREFS_FILE);
        bgm_core::persist::write_json(
            &path,
            &serde_json::json!({
                "version": 4, "gpuAcceleration": false, "smtc": true,
                "spatialAudio": true, "audioEnhancement": true, "mpris2": true,
            }),
        )
        .unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (music.clone(), LibrarySource::Custom, Vec::new())
            })
        };
        let mut library = reopen();
        library
            .update_playback_settings(|settings| {
                crate::settings::set_minimize_to_tray(settings, true);
                settings.close_action = bgm_core::CloseAction::Tray;
                settings.ui_font = "PingFang SC".into();
                settings.prevent_sleep = true;
            })
            .unwrap();

        let mut library = reopen();
        assert_eq!(library.prefs.close_action, bgm_core::CloseAction::Tray);
        assert!(library.prefs.minimize_to_tray);
        assert_eq!(library.prefs.ui_font, "PingFang SC");
        assert!(library.prefs.prevent_sleep);
        library
            .update_playback_settings(|settings| {
                crate::settings::set_minimize_to_tray(settings, false)
            })
            .unwrap();

        let playback = reopen().prefs;
        assert_eq!(playback.close_action, bgm_core::CloseAction::Ask);
        assert!(!playback.minimize_to_tray);
        let prefs: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(prefs["gpuAcceleration"], false);
        for field in ["smtc", "spatialAudio", "audioEnhancement", "mpris2"] {
            assert_eq!(prefs[field], true);
        }
    }

    #[test]
    fn wallpaper_preserves_custom_video_path_and_display_preferences_after_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let music = directory.path().join("music");
        fs::create_dir_all(&music).unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (music.clone(), LibrarySource::Custom, Vec::new())
            })
        };
        let mut library = reopen();
        let video = directory.path().join("壁纸.MP4");
        let path = video.to_str().unwrap();
        library.set_wallpaper("custom", Some(path)).unwrap();
        library.set_wallpaper_opacity(1.0, Instant::now());
        library.set_wallpaper_blur(25, Instant::now());
        library.set_wallpaper_fit("contain").unwrap();
        library.set_wallpaper("builtin-2", None).unwrap();

        let prefs = reopen().prefs;
        assert_eq!(prefs.wallpaper, "builtin-2");
        assert_eq!(prefs.custom_wallpaper.as_deref(), Some(path));
        assert_eq!(prefs.wallpaper_opacity, 1.0);
        assert_eq!(prefs.wallpaper_blur, 20);
        assert_eq!(prefs.wallpaper_fit, "contain");
    }

    #[test]
    fn retains_legacy_appearance_if_migration_cannot_write() {
        let directory = tempfile::tempdir().unwrap();
        let prefs_path = directory.path().join(PREFS_FILE);
        let appearance_path = directory.path().join("appearance.json");
        fs::create_dir(directory.path().join("appearance.json.tmp")).unwrap();
        let mut fields = serde_json::json!({"appearance":"light","version":3})
            .as_object()
            .unwrap()
            .clone();
        bgm_core::persist::write_json(&prefs_path, &fields).unwrap();

        let (appearance, warning) = load_appearance(&appearance_path, &prefs_path, &mut fields);
        assert_eq!(appearance, Appearance::Light);
        assert!(warning.is_some());
        assert_eq!(fields["appearance"], "light");
        let on_disk: Map<String, Value> =
            serde_json::from_slice(&fs::read(&prefs_path).unwrap()).unwrap();
        assert_eq!(on_disk, fields);
    }

    #[test]
    fn library_search_uses_title_and_category_fields() {
        let mut track = song("a", "Heart of Courage", "epic");
        track.artist = Some("Two Steps From Hell".into());
        track.album = Some("Invincible".into());
        let mut category = category("epic", "史诗");
        category.name_en = "Heroic".into();
        category.label_en = "Cinematic Battle".into();
        let songs = [track];
        let categories = [category];
        let favorites = HashSet::from(["epic/a"]);
        for filter in [
            LibraryFilter::All,
            LibraryFilter::Category("epic".into()),
            LibraryFilter::Favorites,
        ] {
            let search = |query| {
                filter_songs(
                    &songs,
                    &categories,
                    &favorites,
                    &filter,
                    query,
                    SortBy::Default,
                )
            };
            for query in [" courage ", "EPIC", "史诗", "Heroic", "battle"] {
                assert_eq!(search(query).len(), 1, "{query}");
            }
            for query in ["steps", "invincible", "古风"] {
                assert!(search(query).is_empty(), "{query}");
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn title_sort_uses_chinese_collation_and_stable_keys() {
        let songs = vec![
            song("b", "春日", "epic"),
            song("c", "北京", "epic"),
            song("d", "阿里", "epic"),
            song("a", "春日", "epic"),
        ];
        let names = sort_by_title(songs);
        let keys: Vec<_> = names.iter().map(|song| song.key.as_str()).collect();
        assert_eq!(keys, ["epic/d", "epic/c", "epic/a", "epic/b"]);
    }

    #[test]
    fn favorites_filter_uses_stable_key() {
        let songs = vec![song("a", "甲", "epic"), song("b", "乙", "gufeng")];
        let favorites = HashSet::from(["gufeng/b"]);
        let found = filter_songs(
            &songs,
            &[],
            &favorites,
            &LibraryFilter::Favorites,
            "",
            SortBy::Default,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "gufeng/b");
    }

    #[test]
    fn scheduled_track_keeps_its_slot_when_renamed_and_moved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SCHEDULE_FILE);
        let mut schedule = Schedule {
            entries: vec![bgm_core::ScheduleEntry {
                songs: vec![ScheduleSong {
                    key: "epic/old".into(),
                    start: "09:15".into(),
                    end: "09:30".into(),
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
        .normalize();
        for (old, new) in [("epic/old", "epic/new"), ("epic/new", "gufeng/new")] {
            remap_scheduled_songs(
                &mut schedule.entries[0].songs,
                &[KeyChange {
                    from: old.into(),
                    to: new.into(),
                }],
            );
            assert_eq!(schedule.entries[0].songs[0].key, new);
        }

        schedule.save(&path).unwrap();
        let (saved, warning) = Schedule::load(&path);
        assert!(warning.is_none());
        assert_eq!(
            saved.entries[0].songs[0],
            ScheduleSong {
                key: "gufeng/new".into(),
                start: "09:15".into(),
                end: "09:30".into(),
            }
        );
    }

    #[test]
    fn shared_v4_preferences_preserve_authoritative_fields_and_migrate_v1() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let music = directory.path().join("music");
        fs::create_dir_all(&music).unwrap();
        let saved = config.join(PREFS_FILE);
        bgm_core::persist::write_json(
            &saved,
            &serde_json::json!({
                "version": 1, "exportScene": "video", "uiFont": "pingfang",
                "libraryDir": music, "modelsDir": "/models", "lastAsrModel": "local/qwen",
                "desktopLyricsEnabled": true, "futureField": 42,
            }),
        )
        .unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (music.clone(), LibrarySource::Custom, vec![])
            })
        };
        let mut library = reopen();
        assert_eq!(library.prefs.ui_font, "PingFang SC");
        assert!(library.prefs.export_scene.is_empty());
        assert_eq!(library.prefs.version, 4);
        library
            .update_playback_settings(|prefs| {
                prefs.library_dir = None;
                prefs.models_dir = None;
                prefs.last_asr_model = None;
                prefs.desktop_lyrics_enabled = false;
                prefs.crossfade_seconds = 3.5;
            })
            .unwrap();
        let (canonical, warning) = Prefs::load(&saved);
        assert!(warning.is_none());
        assert_eq!(canonical.library_dir.as_deref(), music.to_str());
        assert_eq!(canonical.models_dir.as_deref(), Some("/models"));
        assert_eq!(canonical.last_asr_model.as_deref(), Some("local/qwen"));
        assert!(canonical.desktop_lyrics_enabled);
        assert_eq!(canonical.crossfade_seconds, 3.5);
        let fields: Map<String, Value> =
            serde_json::from_slice(&fs::read(&saved).unwrap()).unwrap();
        let shared: Map<String, Value> =
            serde_json::from_value(serde_json::to_value(&canonical).unwrap()).unwrap();
        assert_eq!(shared.len(), 34);
        assert!(
            shared
                .iter()
                .all(|(key, value)| fields.get(key) == Some(value))
        );
        assert_eq!(fields["futureField"], 42);
        let upstream_file = directory.path().join("upstream-prefs.json");
        canonical.save(&upstream_file).unwrap();
        fs::copy(&upstream_file, &saved).unwrap();
        assert_eq!(reopen().prefs, canonical);
    }

    #[test]
    fn preference_debounce_coalesces_writes_and_flushes_latest_values() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config");
        let root = directory.path().join("music");
        fs::create_dir_all(&root).unwrap();
        let mut library =
            Library::open_in(config.clone(), |_| (root, LibrarySource::Custom, vec![]));
        let path = config.join(PREFS_FILE);
        let now = Instant::now();
        library.preview_volume(0.25, now);
        library.preview_volume(0.5, now + Duration::from_millis(80));
        library.set_wallpaper_blur(12, now + Duration::from_millis(120));
        library
            .poll_preferences(now + Duration::from_millis(399))
            .unwrap();
        assert!(!path.exists());
        assert_eq!(library.prefs.volume, 0.5);
        library
            .poll_preferences(now + Duration::from_millis(400))
            .unwrap();
        let (saved, warning) = Prefs::load(&path);
        assert!(warning.is_none());
        assert_eq!(saved.volume, 0.5);
        assert_eq!(saved.wallpaper_blur, 12);

        library.preview_volume(0.75, now + Duration::from_millis(500));
        assert_eq!(Prefs::load(&path).0.volume, 0.5);
        library.flush_preferences().unwrap();
        assert_eq!(Prefs::load(&path).0.volume, 0.75);

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        library.preview_volume(0., now);
        assert!(library.poll_preferences(now + PREF_WRITE_DELAY).is_err());
        assert!(
            library
                .poll_preferences(now + Duration::from_secs(10))
                .is_ok()
        );
        fs::remove_dir(&path).unwrap();
        library.flush_preferences().unwrap();
        let saved = Prefs::load(&path).0;
        assert_eq!(saved.volume, 0.);
        assert!(saved.muted);
    }

    #[test]
    fn track_edit_moves_and_renames_in_one_write() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap().join("library");
        let old = root.join("epic/old.mp3");
        let target = root.join("gufeng/new.mp3");
        fs::create_dir_all(old.parent().unwrap()).unwrap();
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&old, b"source").unwrap();
        fs::write(&target, b"existing").unwrap();
        let mut library = Library::open_in(temp.path().join("config"), |_| {
            (root.clone(), LibrarySource::Custom, Vec::new())
        });
        library.toggle_favorite("epic/old").unwrap();
        library.save_loop_point("epic/old", 0.5, 2.0, true).unwrap();

        assert!(library.update_track("epic/old", "new", "gufeng").is_err());
        assert_eq!(fs::read(&old).unwrap(), b"source");
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert_eq!(library.prefs.favorites, ["epic/old"]);

        fs::remove_file(&target).unwrap();
        let other_format = root.join("gufeng/new.flac");
        fs::write(&other_format, b"other format").unwrap();
        assert!(library.update_track("epic/old", "new", "gufeng").is_err());
        assert_eq!(fs::read(&old).unwrap(), b"source");
        assert_eq!(fs::read(&other_format).unwrap(), b"other format");
        fs::remove_file(other_format).unwrap();
        let mutation = library.update_track("epic/old", "new", "gufeng").unwrap();
        assert_eq!(mutation.changes.len(), 1);
        assert_eq!(mutation.changes[0].from, "epic/old");
        assert_eq!(mutation.changes[0].to, "gufeng/new");
        assert_eq!(fs::read(&target).unwrap(), b"source");
        assert!(!old.exists());
        assert!(!root.join("epic/new.mp3").exists());
        assert_eq!(library.prefs.favorites, ["gufeng/new"]);
        assert!(library.loop_point("epic/old").is_none());
        assert!(library.loop_point("gufeng/new").is_some());
    }

    #[test]
    fn saves_clipped_song_times_and_rejects_overlaps_without_replacing_the_schedule() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        let root = temp.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (root.clone(), LibrarySource::Custom, vec![])
            })
        };
        let mut library = reopen();
        let mut schedule = Schedule {
            entries: vec![
                bgm_core::ScheduleEntry {
                    name: "早间".into(),
                    start: "09:00".into(),
                    end: "10:00".into(),
                    volume: Some(0.0),
                    songs: vec![
                        ScheduleSong {
                            key: "first".into(),
                            start: "08:00".into(),
                            end: "11:00".into(),
                        },
                        ScheduleSong {
                            key: "second".into(),
                            start: "11:00".into(),
                            end: "08:00".into(),
                        },
                    ],
                    ..Default::default()
                },
                bgm_core::ScheduleEntry {
                    name: "午间".into(),
                    start: "10:00".into(),
                    end: "11:00".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let saved = library.save_schedule(schedule.clone()).unwrap();
        let first = &saved.entries[0];
        assert_eq!(first.volume, Some(0.0));
        assert_eq!(
            first
                .songs
                .iter()
                .map(|song| (song.start.as_str(), song.end.as_str()))
                .collect::<Vec<_>>(),
            [("09:00", "10:00"), ("09:59", "10:00")]
        );

        schedule.entries[1].start = "09:59".into();
        let error = library.save_schedule(schedule).unwrap_err();
        assert!(error.contains("早间") && error.contains("午间"));
        assert_eq!(library.schedule, saved);
        assert_eq!(reopen().schedule, saved);
    }

    #[test]
    fn batch_move_reports_partial_success_and_persists_new_references() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let config = temp.path().join("config");
        let original = root.join("epic/track.wav");
        let missing = root.join("epic/vanished.wav");
        let destination = root.join("gufeng/track.flac");
        fs::create_dir_all(original.parent().unwrap()).unwrap();
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&original, b"source").unwrap();
        fs::write(&missing, b"removed outside the application").unwrap();
        fs::write(&destination, b"existing").unwrap();
        let reopen = || {
            Library::open_in(config.clone(), |_| {
                (root.clone(), LibrarySource::Custom, vec![])
            })
        };
        let mut library = reopen();
        library.toggle_favorite("epic/track").unwrap();
        library
            .save_loop_point("epic/track", 0.5, 2.0, true)
            .unwrap();
        let schedule = Schedule {
            entries: vec![bgm_core::ScheduleEntry {
                songs: vec![ScheduleSong {
                    key: "epic/track".into(),
                    start: "09:15".into(),
                    end: "09:30".into(),
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        library.save_schedule(schedule).unwrap();
        fs::remove_file(&missing).unwrap();
        let keys = ["epic/vanished", "epic/track", "epic/stale"].map(str::to_owned);
        let batch = library.move_batch(&keys, "gufeng").unwrap();
        let report = batch.report.unwrap();
        assert_eq!(
            (
                report.total,
                report.succeeded,
                report.skipped,
                report.failed
            ),
            (3, 1, 1, 1)
        );
        assert!(report.failures[0].contains("vanished"));
        let key = "gufeng/track (2)";
        assert_eq!(batch.changes[0].to, key);
        assert_eq!(
            fs::read(root.join("gufeng/track (2).wav")).unwrap(),
            b"source"
        );
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
        assert!(!original.exists());
        let saved = reopen();
        assert_eq!(saved.prefs.favorites, [key]);
        assert!(saved.loop_point(key).is_some());
        assert_eq!(saved.schedule.entries[0].songs[0].key, key);
    }

    #[test]
    fn diff_reports_added_removed_and_moved() {
        let previous = vec![
            song("a", "a", "cheerful"),
            song("b", "b", "epic"),
            song("c", "c", "solitude"),
        ];
        let next = vec![
            song("a", "a", "cheerful"),
            song("b", "b", "memories"),
            song("d", "d", "country"),
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
        let previous = vec![song("old_name", "old_name", "epic")];
        let next = vec![song("new_name", "new_name", "epic")];

        let diff = diff_songs(&previous, &next);

        assert!(diff.moved.is_empty());
        assert_eq!(diff.removed, vec!["epic/old_name"]);
        assert_eq!(diff.added, vec!["epic/new_name"]);
    }

    #[test]
    fn simultaneous_same_name_moves_are_not_matched_arbitrarily() {
        let previous = [
            song("track", "track", "epic"),
            song("track", "track", "gufeng"),
        ];
        let next = [song("track", "track", "memories")];
        let diff = diff_songs(&previous, &next);
        assert!(diff.moved.is_empty());
        assert_eq!(diff.removed, ["epic/track", "gufeng/track"]);
        assert_eq!(diff.added, ["memories/track"]);
    }

    #[test]
    fn library_sources_follow_priority_and_can_create_the_external_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let env = root.join("env");
        let custom = root.join("custom");
        let bundled = root.join("bundled");
        let exe = root.join("exe");
        let cwd = root.join("cwd");
        let config = root.join("config");
        for candidate in [&env, &custom, &bundled, &exe, &cwd] {
            fs::create_dir(candidate).unwrap();
        }
        let resolve = || {
            resolve_root_candidates(
                Some(env.clone()),
                Some(custom.clone()),
                Some(bundled.clone()),
                [Some(exe.clone()), Some(cwd.clone()), Some(config.clone())],
            )
        };
        for (expected, source) in [
            (&env, LibrarySource::Env),
            (&custom, LibrarySource::Custom),
            (&bundled, LibrarySource::Bundled),
            (&exe, LibrarySource::External),
            (&cwd, LibrarySource::External),
        ] {
            let (found, kind, _) = resolve();
            assert_eq!(found, *expected);
            assert_eq!(kind, source);
            fs::remove_dir(expected).unwrap();
        }
        fs::write(&exe, b"blocks external directory creation").unwrap();
        let (found, kind, warnings) = resolve();
        assert_eq!(found, config);
        assert_eq!(kind, LibrarySource::External);
        assert!(config.is_dir());
        assert_eq!(warnings.len(), 3);
    }

    #[test]
    fn library_switch_discards_old_scans_and_updates_the_persisted_root() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let first = base.join("first");
        let second = base.join("second");
        for (root, name) in [(&first, "a.mp3"), (&second, "b.wav")] {
            fs::create_dir_all(root.join("epic")).unwrap();
            fs::write(root.join("epic").join(name), []).unwrap();
        }
        let config = base.join("config");
        let mut library =
            Library::open_in(config.clone(), |_| (first, LibrarySource::Custom, vec![]));
        let scan = library.watch_scan();
        let old_revision = scan.revision();
        let (_, old) = scan.run().unwrap();
        library.choose_dir(second.clone()).unwrap();
        assert!(!library.is_current_watch_scan(old_revision));
        assert!(library.apply_watch_scan(old_revision, old).is_none());
        assert_eq!(library.root(), second);
        assert!(library.song("epic/a").is_none());
        assert!(library.song("epic/b").is_some());
        assert_eq!(
            Prefs::load(&config.join(PREFS_FILE))
                .0
                .library_dir
                .as_deref(),
            second.to_str()
        );
    }

    #[test]
    fn recommended_order_survives_rescan_and_only_applies_in_default_sort() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let collection = root.join("epic");
        fs::create_dir_all(&collection).unwrap();
        for file in ["a.wav", "b.wav", "c.wav"] {
            fs::write(collection.join(file), []).unwrap();
        }
        let open = || {
            Library::open_in(temp.path().join("config"), |_| {
                (root.clone(), LibrarySource::Custom, vec![])
            })
        };
        let mut library = open();
        let keys = |library: &Library| {
            library
                .visible(&LibraryFilter::All, "")
                .into_iter()
                .map(|song| song.key)
                .collect::<Vec<_>>()
        };
        library.reorder_song("epic/c", "epic/a");
        library.rescan();
        assert_eq!(keys(&library), ["epic/c", "epic/a", "epic/b"]);
        library.set_sort(SortBy::Name).unwrap();
        library.reorder_song("epic/a", "epic/c");
        assert_eq!(keys(&library), ["epic/a", "epic/b", "epic/c"]);
        library.set_sort(SortBy::Default).unwrap();
        assert_eq!(keys(&library), ["epic/c", "epic/a", "epic/b"]);
        assert_eq!(keys(&open()), ["epic/a", "epic/b", "epic/c"]);
    }

    #[test]
    fn scan_reports_ten_new_files_as_one_batch_then_identifies_moves() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        let first = root.join("epic");
        let second = root.join("gufeng");
        let nested = first.join("nested");
        let unknown = root.join("unregistered");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let mut library = Library::open_in(temp.path().join("config"), |_| {
            (root.clone(), LibrarySource::Custom, vec![])
        });
        for index in 0..9 {
            fs::write(first.join(format!("track{index}.wav")), []).unwrap();
        }
        fs::write(first.join("Track_9.WAV"), []).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&unknown).unwrap();
        fs::write(nested.join("ignored.wav"), []).unwrap();
        fs::write(unknown.join("ignored.wav"), []).unwrap();
        fs::write(root.join("stray.wav"), []).unwrap();
        assert!(
            library
                .create_category("无效目录", Some("../outside"))
                .is_err()
        );
        let (revision, snapshot) = library.watch_scan().run().unwrap();
        let batch = library.apply_watch_scan(revision, snapshot).unwrap();
        assert_eq!(batch.added.len(), 10);
        assert!(batch.removed.is_empty() && batch.moved.is_empty());
        assert_eq!(library.song("epic/Track_9").unwrap().title, "Track 9");
        assert!(
            library
                .snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("stray.wav"))
        );
        assert!(
            library
                .snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("unregistered"))
        );
        let (revision, snapshot) = library.watch_scan().run().unwrap();
        assert!(library.apply_watch_scan(revision, snapshot).is_none());
        fs::rename(first.join("track0.wav"), second.join("track0.wav")).unwrap();
        fs::rename(first.join("track1.wav"), first.join("renamed.wav")).unwrap();
        let (revision, snapshot) = library.watch_scan().run().unwrap();
        let batch = library.apply_watch_scan(revision, snapshot).unwrap();
        assert_eq!(batch.added, ["epic/renamed"]);
        assert_eq!(batch.removed, ["epic/track1"]);
        assert_eq!(
            batch.moved,
            [KeyChange {
                from: "epic/track0".into(),
                to: "gufeng/track0".into()
            }]
        );
    }

    #[test]
    fn watcher_rejects_superseded_and_outdated_scans() {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap().join("library");
        fs::create_dir_all(root.join("epic")).unwrap();
        let mut library = Library::open_in(temp.path().join("config"), |_| {
            (root.clone(), LibrarySource::Custom, Vec::new())
        });
        fs::write(root.join("epic").join("track.mp3"), []).unwrap();
        let (old_revision, old) = library.watch_scan().run().unwrap();
        library.rescan();
        assert!(library.apply_watch_scan(old_revision, old).is_none());

        fs::remove_file(root.join("epic").join("track.mp3")).unwrap();
        let (revision, mut next) = library.watch_scan().run().unwrap();
        next.scanned_at_ms = library.snapshot.scanned_at_ms - 1;
        assert!(library.apply_watch_scan(revision, next).is_none());
        assert!(library.song("epic/track").is_some());
    }

    #[test]
    fn watcher_preserves_metadata_offline_and_cleans_up_confirmed_removal() {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("library");
        let offline = base.join("offline");
        let track = root.join("epic").join("track.mp3");
        fs::create_dir_all(track.parent().unwrap()).unwrap();
        fs::write(&track, []).unwrap();
        let mut library = Library::open_in(base.join("config"), |_| {
            (root.clone(), LibrarySource::External, Vec::new())
        });
        let key = "epic/track";
        assert!(library.toggle_favorite(key).unwrap());
        library.save_loop_point(key, 0.5, 2.0, true).unwrap();

        fs::rename(&root, &offline).unwrap();
        assert!(library.watch_scan().run().is_none());
        assert!(library.rescan()[0].contains("暂时无法读取"));
        assert!(!root.exists());
        assert!(library.song(key).is_some());
        assert!(library.prefs.favorites.iter().any(|item| item == key));
        assert!(library.loop_point(key).is_some());

        fs::rename(offline, root).unwrap();
        let (revision, snapshot) = library.watch_scan().run().unwrap();
        let reconnect = library.apply_watch_scan(revision, snapshot).unwrap();
        assert!(
            reconnect.added.is_empty()
                && reconnect.removed.is_empty()
                && reconnect.moved.is_empty()
        );
        assert!(
            !library
                .snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("暂时无法读取"))
        );
        fs::remove_file(track).unwrap();
        let (revision, snapshot) = library.watch_scan().run().unwrap();
        let change = library.apply_watch_scan(revision, snapshot).unwrap();
        assert_eq!(change.removed, [key]);
        assert!(library.prefs.favorites.is_empty());
        assert!(library.loop_point(key).is_none());
    }

    #[test]
    fn same_content_ignores_identical_snapshots() {
        let categories = vec![category("epic", "史诗")];
        let snapshot = LibrarySnapshot {
            root: "/tmp".into(),
            source: LibrarySource::Bundled,
            songs: vec![song("a", "甲", "epic")],
            categories,
            warnings: Vec::new(),
            scanned_at_ms: 0,
        };
        assert!(same_content(&snapshot, &snapshot));
        let mut changed = snapshot.clone();
        changed.songs[0].title = "乙".into();
        assert!(!same_content(&snapshot, &changed));
    }
    #[test]
    fn ffmpeg_export_reaches_all_loudness_targets() {
        let Some(ffmpeg) = bgm_core::loudness::ffmpeg_path() else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("tone.wav");
        let status = std::process::Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=997:sample_rate=48000:duration=9",
                "-ac",
                "2",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let measure = |destination: &Path| {
            let output = std::process::Command::new(&ffmpeg)
                .args(["-hide_banner", "-nostats", "-i"])
                .arg(destination)
                .args(["-af", "loudnorm=print_format=json", "-f", "null", "-"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            let json = &stderr[stderr.rfind('{').unwrap()..=stderr.rfind('}').unwrap()];
            let measured: serde_json::Value = serde_json::from_str(json).unwrap();
            measured["input_i"]
                .as_str()
                .unwrap()
                .parse::<f64>()
                .unwrap()
        };
        for preset in bgm_core::loudness::EXPORT_PRESETS {
            let destination = directory.path().join(format!("{}.mp3", preset.id));
            let exporter = Exporter::new(ExportSettings::from_prefs(true, preset.id));
            assert_eq!(exporter.extension(&source).unwrap(), "mp3");
            let message = exporter.single(&source, &destination).unwrap();
            assert!(message.contains("已响度归一化"), "{message}");
            let measured = measure(&destination);
            assert!(
                (measured - f64::from(preset.target_lufs)).abs() <= 1.0,
                "{}: {measured} LUFS",
                preset.id
            );
        }
        let wrong = directory.path().join("wrong.ogg");
        assert!(
            Exporter::new(ExportSettings::default())
                .single(&source, &wrong)
                .is_err()
        );
        assert!(!wrong.exists());
    }
    #[test]
    fn imports_scans_and_updates_metadata_across_five_formats() {
        let Some(ffmpeg) = bgm_core::loudness::ffmpeg_path() else {
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let mut library = Library::open_in(temp.path().join("config"), |_| {
            (root.clone(), LibrarySource::Custom, vec![])
        });
        let make_audio = |path: &Path, duration: &str, artist: &str| {
            let mut command = std::process::Command::new(&ffmpeg);
            command.args([
                "-nostdin",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                duration,
            ]);
            if path.extension().and_then(|ext| ext.to_str()) == Some("ogg") {
                command.args(["-ac", "2", "-c:a", "vorbis", "-strict", "experimental"]);
            }
            let output = command
                .arg("-metadata")
                .arg(format!("artist={artist}"))
                .arg(path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let formats = bgm_core::library::AUDIO_EXTENSIONS;
        let inputs: Vec<_> = formats
            .iter()
            .map(|ext| {
                let path = temp.path().join(format!("track_{ext}.{ext}"));
                make_audio(&path, "0.5", "Fixture Artist");
                path
            })
            .collect();
        library.import("epic", &inputs);
        assert_eq!(library.snapshot.songs.len(), formats.len());
        for ext in formats {
            let key = format!("epic/track_{ext}");
            let song = library.song(&key).unwrap();
            assert_eq!(song.artist.as_deref(), Some("Fixture Artist"), "{ext}");
            assert!((450..=600).contains(&song.duration_ms.unwrap()), "{ext}");
            assert_eq!(song.title, format!("track {ext}"));
            assert_eq!(
                bgm_core::library::audio_extension(&song.file_name),
                Some(ext)
            );
        }
        let collection = root.join("epic");
        fs::copy(&inputs[0], collection.join("duplicate.flac")).unwrap();
        fs::copy(&inputs[3], collection.join("duplicate.mp3")).unwrap();
        library.rescan();
        assert_eq!(
            library.song("epic/duplicate").unwrap().file_name,
            "duplicate.flac"
        );
        assert!(
            library
                .snapshot
                .warnings
                .iter()
                .any(|warning| warning.contains("duplicate.mp3"))
        );
        let original = library.snapshot.clone();
        library.rescan();
        assert!(same_content(&original, &library.snapshot));
        make_audio(&collection.join("track_wav.wav"), "1.0", "Updated Artist");
        library.rescan();
        let changed = library.song("epic/track_wav").unwrap();
        assert_eq!(changed.artist.as_deref(), Some("Updated Artist"));
        assert_eq!(changed.duration_ms, Some(1_000));
        let updated = library
            .update_track("epic/track_flac", "new", "gufeng")
            .unwrap();
        assert_eq!(updated.changes[0].to, "gufeng/new");
        assert!(root.join("gufeng/new.flac").is_file());
        assert!(inputs[0].is_file());
    }

    #[test]
    fn rescans_large_library_with_stable_results() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let categories = Categories::default();
        let collection = root.join(&categories.items[0].id);
        fs::create_dir(&collection).unwrap();
        for index in 0..512 {
            fs::write(collection.join(format!("{index}.mp3")), [0; 256]).unwrap();
        }
        let cache = MetaCache::default();
        let first = scan_library_cached(root, LibrarySource::Custom, &categories.items, &cache);
        let second = scan_library_cached(root, LibrarySource::Custom, &categories.items, &cache);
        assert_eq!(second.songs.len(), 512);
        assert!(same_content(&first, &second));
    }
}
