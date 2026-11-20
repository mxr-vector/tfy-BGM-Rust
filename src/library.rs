//! 曲库运行时。扫描、导入、改名、移动、软删除和偏好都交给 `bgm-core`，这里只负责路径和落盘。

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use bgm_core::library::CategorySummary;
use bgm_core::ops::{self, KeyChange};
use bgm_core::paths::{
    CATEGORIES_FILE, CONFIG_FILE, LIBRARY_DIR_NAME, LIBRARY_ENV_VAR, PREFS_FILE,
};
use bgm_core::{
    AppConfig, Categories, LibrarySnapshot, LibrarySource, Prefs, Song, SortBy, scan_library,
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

pub struct Library {
    pub prefs: Prefs,
    pub config: AppConfig,
    pub snapshot: LibrarySnapshot,
    prefs_path: PathBuf,
    categories_path: PathBuf,
    config_path: PathBuf,
    categories: Categories,
    root: PathBuf,
    source: LibrarySource,
}

impl Library {
    pub fn open() -> Self {
        let config_dir = config_dir();
        let _ = fs::create_dir_all(&config_dir);
        let config_path = config_dir.join(CONFIG_FILE);
        let prefs_path = config_dir.join(PREFS_FILE);
        let categories_path = config_dir.join(CATEGORIES_FILE);

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

        let (root, source, mut warnings) = resolve_root(&prefs);
        for warning in [config_warning, prefs_warning, categories_warning]
            .into_iter()
            .flatten()
        {
            warnings.push(warning);
        }

        let mut snapshot = scan_library(&root, source, &categories.items);
        warnings.append(&mut snapshot.warnings);
        snapshot.warnings = warnings;

        Self {
            prefs,
            config,
            snapshot,
            prefs_path,
            categories_path,
            config_path,
            categories,
            root,
            source,
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
        let mut snapshot = scan_library(&self.root, self.source, &self.categories.items);
        warnings.append(&mut snapshot.warnings);
        snapshot.warnings = warnings;
        self.snapshot = snapshot;
        self.snapshot.warnings.clone()
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

    pub fn export_song(&self, key: &str, destination: &Path) -> Result<String, String> {
        let song = self.song(key).ok_or_else(|| format!("曲目不存在：{key}"))?;
        let bytes = bgm_core::export::copy_validated(Path::new(&song.path), destination)
            .map_err(|error| error.to_string())?;
        Ok(format!("已导出 {} 字节到 {}", bytes, destination.display()))
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

    fn apply_key_changes(&mut self, changes: &[KeyChange], removed: &[String]) {
        let removed: HashSet<&str> = removed.iter().map(String::as_str).collect();
        self.prefs.favorites = self
            .prefs
            .favorites
            .iter()
            .filter_map(|key| {
                if removed.contains(key.as_str()) {
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
            "上次选择的曲库目录不存在，已回退：{}",
            path.display()
        ));
    }
    if let Some(root) = bundled_root() {
        return (root, LibrarySource::Bundled, warnings);
    }
    let fallback = env::current_dir()
        .map(|dir| dir.join(LIBRARY_DIR_NAME))
        .unwrap_or_else(|_| PathBuf::from(LIBRARY_DIR_NAME));
    warnings.push(format!(
        "没有找到曲库目录。把 mp3 放到 <分类>/ 子目录，或切换曲库目录。当前回退路径：{}",
        fallback.display()
    ));
    (fallback, LibrarySource::Bundled, warnings)
}

fn bundled_root() -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?.join(LIBRARY_DIR_NAME);
    if cwd.is_dir() {
        return Some(absolute(cwd));
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tfy-bgm/bgm");
    if dev.is_dir() {
        return Some(absolute(dev));
    }
    None
}

fn absolute(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bgm_core::library::CategorySummary;

    fn song(key: &str, title: &str, category: &str) -> Song {
        Song {
            key: key.into(),
            title: title.into(),
            file_name: format!("{title}.mp3"),
            category: category.into(),
            path: format!("/tmp/{key}.mp3"),
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
        let songs = vec![song("epic/a", "Heart of Courage", "epic")];
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
    fn favorites_filter_uses_stable_key() {
        let songs = vec![
            song("epic/a", "甲", "epic"),
            song("gufeng/b", "乙", "gufeng"),
        ];
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
}
