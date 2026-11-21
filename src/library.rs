//! 曲库运行时。扫描、导入、改名、移动、软删除和偏好都交给 `bgm-core`，这里只负责路径和落盘。
//!
//! 对齐主线 `AppState` 的职责面：节目单与循环点的持久化、带 ID3 缓存的增量扫描、
//! 外置曲库解析、按偏好生成导出设置，以及界面壁纸偏好。

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use bgm_core::library::CategorySummary;
use bgm_core::loudness::{EXPORT_PRESETS, ExportPreset, ffmpeg_path};
use bgm_core::ops::{self, KeyChange};
use bgm_core::paths::{
    CATEGORIES_FILE, CONFIG_FILE, LIBRARY_DIR_NAME, LIBRARY_ENV_VAR, LOOPS_FILE, PREFS_FILE,
    SCHEDULE_FILE,
};
use bgm_core::tags::MetaCache;
use bgm_core::{
    AppConfig, Categories, ExportSettings, LibrarySnapshot, LibrarySource, LoopBook, LoopPoint,
    Prefs, Schedule, Song, SortBy, scan_library_cached,
};

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
}

/// watch 事件触发的增量结果；`added/removed/moved` 供界面调整队列并提示。
pub struct LibraryChange {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub moved: Vec<KeyChange>,
}

pub struct Library {
    pub prefs: Prefs,
    pub config: AppConfig,
    pub snapshot: LibrarySnapshot,
    pub schedule: Schedule,
    pub loops: LoopBook,
    prefs_path: PathBuf,
    categories_path: PathBuf,
    config_path: PathBuf,
    schedule_path: PathBuf,
    loops_path: PathBuf,
    categories: Categories,
    root: PathBuf,
    source: LibrarySource,
    meta_cache: MetaCache,
}

impl Library {
    pub fn open() -> Self {
        let config_dir = config_dir();
        let _ = fs::create_dir_all(&config_dir);
        let config_path = config_dir.join(CONFIG_FILE);
        let prefs_path = config_dir.join(PREFS_FILE);
        let categories_path = config_dir.join(CATEGORIES_FILE);
        let schedule_path = config_dir.join(SCHEDULE_FILE);
        let loops_path = config_dir.join(LOOPS_FILE);

        let (config, config_warning) = AppConfig::load(&config_path);
        if let Err(error) = config.ensure_file(&config_path) {
            eprintln!("无法写入应用配置：{error}");
        }

        let (mut prefs, prefs_warning) = Prefs::load_with_rates(&prefs_path, &config.player.rates);
        prefs = prefs.normalize_with_rates(&config.player.rates);

        let (categories, categories_warning) = Categories::load(&categories_path);
        if let Err(error) = categories.ensure_file(&categories_path) {
            eprintln!("无法写入分类配置：{error}");
        }

        let (schedule, schedule_warning) = Schedule::load(&schedule_path);
        let (loops, loops_warning) = LoopBook::load(&loops_path);

        let (root, source, mut warnings) = resolve_root(&prefs);
        if source == LibrarySource::External {
            warnings.extend(ensure_category_dirs(&root, &categories.items));
        }
        for warning in [
            config_warning,
            prefs_warning,
            categories_warning,
            schedule_warning,
            loops_warning,
        ]
        .into_iter()
        .flatten()
        {
            warnings.push(warning);
        }

        let meta_cache = MetaCache::default();
        let mut snapshot = scan_library_cached(&root, source, &categories.items, &meta_cache);
        warnings.append(&mut snapshot.warnings);
        snapshot.warnings = warnings;

        Self {
            prefs,
            config,
            snapshot,
            schedule,
            loops,
            prefs_path,
            categories_path,
            config_path,
            schedule_path,
            loops_path,
            categories,
            root,
            source,
            meta_cache,
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
        filter_songs(
            &self.snapshot.songs,
            &self.snapshot.categories,
            &favorites,
            filter,
            query,
            self.prefs.sort_by,
        )
    }

    pub fn rescan(&mut self) -> Vec<String> {
        let (config, config_warning) = AppConfig::load(&self.config_path);
        self.config = config;
        let (categories, categories_warning) = Categories::load(&self.categories_path);
        self.categories = categories;
        self.prefs = self
            .prefs
            .clone()
            .normalize_with_rates(&self.config.player.rates);

        let mut warnings = Vec::new();
        if let Some(warning) = config_warning {
            warnings.push(warning);
        }
        if let Some(warning) = categories_warning {
            warnings.push(warning);
        }
        let mut snapshot = scan_library_cached(
            &self.root,
            self.source,
            &self.categories.items,
            &self.meta_cache,
        );
        warnings.append(&mut snapshot.warnings);
        snapshot.warnings = warnings;
        self.snapshot = snapshot;
        self.snapshot.warnings.clone()
    }

    /// 外部文件变化后的增量扫描；内容未变时返回 `None`，避免界面无谓刷新。
    pub fn handle_watch_event(&mut self) -> Option<LibraryChange> {
        let next = scan_library_cached(
            &self.root,
            self.source,
            &self.categories.items,
            &self.meta_cache,
        );
        if same_content(&self.snapshot, &next) {
            return None;
        }
        let diff = diff_songs(&self.snapshot.songs, &next.songs);
        self.snapshot = next;
        Some(LibraryChange {
            added: diff.added,
            removed: diff.removed,
            moved: diff.moved,
        })
    }

    pub fn choose_dir(&mut self, dir: PathBuf) -> Result<(), String> {
        if !dir.is_dir() {
            return Err(format!("所选曲库目录不存在：{}", dir.display()));
        }
        let root = fs::canonicalize(&dir).unwrap_or(dir);
        self.prefs.library_dir = Some(root.to_string_lossy().into_owned());
        self.persist_prefs()?;
        self.root = root;
        self.source = LibrarySource::Custom;
        self.rescan();
        Ok(())
    }

    pub fn reset_dir(&mut self) -> Result<(), String> {
        self.prefs.library_dir = None;
        self.persist_prefs()?;
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
        self.prefs.volume = volume;
        self.prefs.muted = volume <= 0.0;
        self.persist_prefs()
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<(), String> {
        self.prefs.muted = muted;
        if !muted && self.prefs.volume <= 0.0 {
            self.prefs.volume = 0.85;
        }
        self.persist_prefs()
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
        self.prefs.play_mode = mode;
        self.persist_prefs()
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

    /// 界面壁纸选择；`custom` 需要搭配自定义文件路径。
    pub fn set_wallpaper(&mut self, id: &str, custom_path: Option<&str>) -> Result<(), String> {
        self.prefs.wallpaper = id.to_owned();
        if id == "custom" {
            self.prefs.custom_wallpaper = custom_path.map(str::to_owned);
        }
        self.persist_prefs()
    }

    pub fn set_wallpaper_opacity(&mut self, opacity: f32) -> Result<(), String> {
        self.prefs.wallpaper_opacity = opacity.clamp(0.0, 1.0);
        self.persist_prefs()
    }

    pub fn set_wallpaper_fit(&mut self, fit: &str) -> Result<(), String> {
        self.prefs.wallpaper_fit = if fit == "contain" { "contain" } else { "cover" }.to_owned();
        self.persist_prefs()
    }

    /// 保存整张节目单；时间范围重叠会直接拒绝，避免同一时刻触发多档节目。
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
    pub fn export_info(&self) -> (Option<PathBuf>, Vec<ExportPreset>) {
        (ffmpeg_path(), EXPORT_PRESETS.to_vec())
    }

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
        }
    }

    pub fn rename(&mut self, key: &str, title: &str) -> Result<Mutation, String> {
        let song = self
            .song(key)
            .cloned()
            .ok_or_else(|| format!("曲目不存在：{key}"))?;
        let change = ops::rename_song(&self.root, &self.categories.items, &song, title)
            .map_err(|error| error.to_string())?;
        self.apply_key_changes(std::slice::from_ref(&change), &[]);
        self.rescan();
        Ok(Mutation {
            message: if change.from == change.to {
                "曲名没有变化".into()
            } else {
                format!("已重命名为 {}", title.trim())
            },
            changes: vec![change],
            removed: Vec::new(),
        })
    }

    pub fn move_song(&mut self, key: &str, category: &str) -> Result<Mutation, String> {
        let song = self
            .song(key)
            .cloned()
            .ok_or_else(|| format!("曲目不存在：{key}"))?;
        let change = ops::move_song(&self.root, &self.categories.items, &song, category)
            .map_err(|error| error.to_string())?;
        self.apply_key_changes(std::slice::from_ref(&change), &[]);
        self.rescan();
        Ok(Mutation {
            message: format!("已移动到 {category}"),
            changes: vec![change],
            removed: Vec::new(),
        })
    }

    pub fn trash(&mut self, key: &str) -> Result<Mutation, String> {
        let song = self
            .song(key)
            .cloned()
            .ok_or_else(|| format!("曲目不存在：{key}"))?;
        let outcome = ops::trash_songs(&self.root, &self.categories.items, &[&song]);
        self.apply_key_changes(&outcome.changes, &outcome.removed);
        self.rescan();
        Ok(Mutation {
            message: format_report(&outcome.report),
            changes: outcome.changes,
            removed: outcome.removed,
        })
    }

    /// 导出单曲到指定路径，按偏好决定是否做响度归一化。
    pub fn export_song(&self, key: &str, destination: &Path) -> Result<String, String> {
        let song = self.song(key).ok_or_else(|| format!("曲目不存在：{key}"))?;
        let outcome = bgm_core::export::export_audio(
            Path::new(&song.path),
            destination,
            self.export_settings(),
        )
        .map_err(|error| error.to_string())?;
        let detail = if let Some(warning) = outcome.warning {
            format!("（{warning}）")
        } else if outcome.normalized {
            "（已响度归一化）".to_owned()
        } else {
            String::new()
        };
        Ok(format!("已导出到 {}{}", destination.display(), detail))
    }

    pub fn create_category(&mut self, name: &str) -> Result<String, String> {
        let mut next = self.categories.clone();
        let category = next.add(name, None).map_err(|error| error.to_string())?;
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

    /// 把重命名/移动产生的 key 变化同步到收藏与循环点，删除的 key 直接移除。
    fn apply_key_changes(&mut self, changes: &[KeyChange], removed: &[String]) {
        let removed_set: HashSet<&str> = removed.iter().map(String::as_str).collect();
        self.prefs.favorites = self
            .prefs
            .favorites
            .iter()
            .filter_map(|key| {
                if removed_set.contains(key.as_str()) {
                    return None;
                }
                let mapped = changes
                    .iter()
                    .find(|change| change.from == *key && change.from != change.to)
                    .map(|change| change.to.clone());
                Some(mapped.unwrap_or_else(|| key.clone()))
            })
            .collect();
        let _ = self.persist_prefs();

        let mut loops = self.loops.clone();
        loops.apply_key_changes(changes, removed);
        if loops != self.loops && loops.save(&self.loops_path).is_ok() {
            self.loops = loops;
        }
    }

    fn persist_prefs(&mut self) -> Result<(), String> {
        self.prefs = self
            .prefs
            .clone()
            .normalize_with_rates(&self.config.player.rates);
        self.prefs
            .save(&self.prefs_path)
            .map_err(|error| format!("保存偏好失败：{error}"))
    }
}

impl Mutation {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            changes: Vec::new(),
            removed: Vec::new(),
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
    let mut matched: Vec<Song> = songs
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
            // 搜索同样匹配 ID3 里的艺人 / 专辑，方便按歌手找曲。
            let meta_match = song
                .artist
                .as_deref()
                .is_some_and(|artist| artist.to_lowercase().contains(&query))
                || song
                    .album
                    .as_deref()
                    .is_some_and(|album| album.to_lowercase().contains(&query));
            if meta_match {
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
                        || category.name_en.to_lowercase().contains(&query))
            })
        })
        .cloned()
        .collect();

    if sort == SortBy::Name {
        matched.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.key.cmp(&right.key))
        });
    }
    matched
}

fn format_report(report: &bgm_core::BatchReport) -> String {
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
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tfy-bgm-gpui")
}

/// 曲库根目录解析，优先级与主线一致：
/// 环境变量 → 用户自选目录 → 开发仓库 `bgm/`（仅 debug）→ 外置曲库。
fn resolve_root(prefs: &Prefs) -> (PathBuf, LibrarySource, Vec<String>) {
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
            "自定义曲库目录不存在，已回退到外置曲库：{}",
            path.display()
        ));
    }
    // 开发模式优先使用主线仓库的 bgm/，避免在 target/debug 下生成空曲库。
    #[cfg(debug_assertions)]
    if let Some(root) = dev_bundled_root() {
        return (root, LibrarySource::Bundled, warnings);
    }

    resolve_external_root(warnings)
}

/// 外置曲库挂载点：优先 exe 同级，其次当前工作目录，最后应用配置目录。
/// 都不存在时尝试在 exe 同级创建，失败则落到配置目录；分类子目录由调用方补全。
fn resolve_external_root(mut warnings: Vec<String>) -> (PathBuf, LibrarySource, Vec<String>) {
    let exe_root = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|dir| dir.join(LIBRARY_DIR_NAME));
    let cwd_root = env::current_dir()
        .ok()
        .map(|dir| dir.join(LIBRARY_DIR_NAME));
    let config_root = Some(config_dir().join(LIBRARY_DIR_NAME));

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

/// 在外置曲库中补齐分类子目录，首次启动即可直接往对应目录放音频。
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

#[cfg(debug_assertions)]
fn dev_bundled_root() -> Option<PathBuf> {
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tfy-bgm/bgm");
    if dev.is_dir() {
        return Some(absolute(dev));
    }
    None
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
    fn search_matches_title_and_chinese_category() {
        let songs = vec![song("a", "Heart of Courage", "epic")];
        let categories = vec![category("epic", "史诗")];
        let favorites = HashSet::new();
        let found = filter_songs(
            &songs,
            &categories,
            &favorites,
            &LibraryFilter::All,
            "史诗",
            SortBy::Default,
        );
        assert_eq!(found.len(), 1);
        let missed = filter_songs(
            &songs,
            &categories,
            &favorites,
            &LibraryFilter::All,
            "古风",
            SortBy::Default,
        );
        assert!(missed.is_empty());
    }

    #[test]
    fn search_matches_artist_metadata() {
        let mut tagged = song("a", "Victory", "epic");
        tagged.artist = Some("Two Steps From Hell".into());
        let found = filter_songs(
            std::slice::from_ref(&tagged),
            &[],
            &HashSet::new(),
            &LibraryFilter::All,
            "steps",
            SortBy::Default,
        );
        assert_eq!(found.len(), 1);
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
}
