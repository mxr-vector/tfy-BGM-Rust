//! 曲库目录扫描器：文件名即曲名，不维护任何索引文件。
//!
//! 扫描只读取已注册分类目录下的 `.mp3`，并对「放错目录」「目录为空」「文件不可读」
//! 等情况生成可展示的告警，避免音频无声消失。

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::categories::Category;
use crate::limits::MAX_KEY_LEN;
use crate::tags::MetaCache;

/// 曲库唯一接受的音频扩展名（小写，不含点）。
pub const AUDIO_EXTENSION: &str = "mp3";

/// 文件名是否以音频扩展名结尾（大小写不敏感，按字节判断避免切坏多字节字符）。
pub fn has_audio_extension(name: &str) -> bool {
    let bytes = name.as_bytes();
    let ext = AUDIO_EXTENSION.as_bytes();
    bytes.len() > ext.len()
        && bytes[bytes.len() - ext.len() - 1] == b'.'
        && bytes[bytes.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

/// 曲库来源，供界面区分外置曲库、用户自选目录与开发仓库。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LibrarySource {
    /// 随应用/开发仓库提供的曲库（仅开发模式使用）。
    Bundled,
    /// 程序同级或应用配置目录下的外置曲库挂载点。
    External,
    Custom,
    Env,
}

/// 单首曲目。`key`（分类/文件名主干）是收藏与队列的稳定标识。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub key: String,
    pub title: String,
    pub file_name: String,
    pub category: String,
    pub path: String,
    /// ID3 标签，缺失时为 `null`；用于智能随机与诊断展示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategorySummary {
    pub id: String,
    pub name_zh: String,
    pub name_en: String,
    pub label_en: String,
    pub color: String,
    pub icon: String,
    pub title: String,
    pub description: String,
    pub count: usize,
}

impl CategorySummary {
    fn new(category: &Category, count: usize) -> Self {
        Self {
            id: category.id.clone(),
            name_zh: category.name_zh.clone(),
            name_en: category.name_en.clone(),
            label_en: category.label_en.clone(),
            color: category.color.clone(),
            icon: category.icon.clone(),
            title: category.title.clone(),
            description: category.description.clone(),
            count,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibrarySnapshot {
    pub root: String,
    pub source: LibrarySource,
    pub songs: Vec<Song>,
    pub categories: Vec<CategorySummary>,
    pub warnings: Vec<String>,
    pub scanned_at_ms: u64,
}

impl LibrarySnapshot {
    pub fn song(&self, key: &str) -> Option<&Song> {
        self.songs.iter().find(|song| song.key == key)
    }
}

/// 扫描曲库根目录。目录缺失或不可读不会返回错误，而是通过 `warnings` 上报。
pub fn scan_library(
    root: &Path,
    source: LibrarySource,
    categories: &[Category],
) -> LibrarySnapshot {
    scan_library_cached(root, source, categories, &MetaCache::default())
}

/// 带标签缓存的扫描；watch 事件触发的重复扫描只读取新文件的 ID3。
pub fn scan_library_cached(
    root: &Path,
    source: LibrarySource,
    categories: &[Category],
    cache: &MetaCache,
) -> LibrarySnapshot {
    let mut warnings = Vec::new();
    let mut songs = Vec::new();
    let mut summaries = Vec::with_capacity(categories.len());
    let mut seen_keys = HashSet::new();

    if root.is_dir() {
        for category in categories {
            let dir = root.join(&category.id);
            let mut local = scan_category(&dir, category, &mut warnings, &mut seen_keys, cache);
            local.sort_by(|a, b| {
                a.title
                    .to_lowercase()
                    .cmp(&b.title.to_lowercase())
                    .then_with(|| a.file_name.cmp(&b.file_name))
            });
            summaries.push(CategorySummary::new(category, local.len()));
            songs.extend(local);
        }
        collect_stray_warnings(root, categories, &mut warnings);
        if songs.is_empty() {
            warnings.push(format!(
                "曲库目录中未发现任何 mp3：{}（请把音频放入 <分类>/ 子目录）",
                root.display()
            ));
        }
    } else {
        warnings.push(format!("曲库目录不存在或不是文件夹：{}", root.display()));
        summaries.extend(
            categories
                .iter()
                .map(|category| CategorySummary::new(category, 0)),
        );
    }

    LibrarySnapshot {
        root: root.to_string_lossy().into_owned(),
        source,
        songs,
        categories: summaries,
        warnings,
        scanned_at_ms: now_ms(),
    }
}

fn scan_category(
    dir: &Path,
    category: &Category,
    warnings: &mut Vec<String>,
    seen_keys: &mut HashSet<String>,
    cache: &MetaCache,
) -> Vec<Song> {
    if !dir.is_dir() {
        return Vec::new();
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("无法读取分类目录 {}：{error}", dir.display()));
            return Vec::new();
        }
    };

    let mut songs = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("无法读取目录项 {}：{error}", dir.display()));
                continue;
            }
        };

        let path = entry.path();
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            warnings.push(format!(
                "跳过文件名不是有效 UTF-8 的文件：{}",
                path.display()
            ));
            continue;
        };
        if !has_audio_extension(&file_name) {
            continue;
        }

        let meta = match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => continue,
            Err(error) => {
                warnings.push(format!("无法读取文件信息 {}：{error}", path.display()));
                continue;
            }
        };

        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            warnings.push(format!("跳过无法解析文件名的曲目：{}", path.display()));
            continue;
        };
        if stem.is_empty() {
            continue;
        }

        let Some(path_str) = path.to_str() else {
            warnings.push(format!("跳过路径不是有效 UTF-8 的曲目：{}", path.display()));
            continue;
        };

        let key = format!("{}/{}", category.id, stem);
        if key.len() > MAX_KEY_LEN {
            warnings.push(format!("跳过文件名过长的曲目：{file_name}"));
            continue;
        }
        if !seen_keys.insert(key.clone()) {
            warnings.push(format!("跳过重复曲目标识 {key}（{file_name}）"));
            continue;
        }

        let tags = cache.get(&path, &meta);
        songs.push(Song {
            key,
            title: stem.replace('_', " "),
            file_name,
            category: category.id.clone(),
            path: path_str.to_owned(),
            artist: tags.artist,
            album: tags.album,
            genre: tags.genre,
            duration_ms: tags.duration_ms,
        });
    }
    songs
}

fn collect_stray_warnings(root: &Path, categories: &[Category], warnings: &mut Vec<String>) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("无法读取曲库根目录 {}：{error}", root.display()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if categories.iter().any(|category| category.id == name) || name.starts_with('.') {
                continue;
            }
            let stray_count = count_mp3(&path);
            if stray_count > 0 {
                warnings.push(format!(
                    "目录 {name}/ 不是已注册分类，其中 {stray_count} 个 mp3 已被忽略"
                ));
            }
        } else if has_audio_extension(&name) {
            warnings.push(format!("根目录下的 {name} 不在任何分类目录中，已被忽略"));
        }
    }
}

fn count_mp3(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry.path().is_file()
                        && entry.file_name().to_str().is_some_and(has_audio_extension)
                })
                .count()
        })
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(path: &Path) {
        fs::write(path, b"fake-mp3").expect("fixture file must be writable");
    }

    fn categories() -> Vec<Category> {
        crate::categories::defaults().items
    }

    #[test]
    fn audio_extension_matches_case_insensitively() {
        assert!(has_audio_extension("track.mp3"));
        assert!(has_audio_extension("track.MP3"));
        assert!(has_audio_extension("中文.mp3"));
        assert!(!has_audio_extension("mp3"));
        assert!(!has_audio_extension("track.mp3.txt"));
        assert!(!has_audio_extension("track.flac"));
    }

    #[test]
    fn scans_registered_categories_in_declared_order() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        fs::create_dir_all(root.join("epic")).unwrap();
        fs::create_dir_all(root.join("cheerful")).unwrap();
        touch(&root.join("cheerful/z_last.mp3"));
        touch(&root.join("cheerful/a_first.mp3"));
        touch(&root.join("cheerful/notes.txt"));
        touch(&root.join("epic/x.MP3"));

        let snapshot = scan_library(root, LibrarySource::Bundled, &categories());

        let keys: Vec<_> = snapshot
            .songs
            .iter()
            .map(|song| song.key.as_str())
            .collect();
        assert_eq!(keys, ["cheerful/a_first", "cheerful/z_last", "epic/x"]);
        assert_eq!(snapshot.songs[0].title, "a first");
        assert_eq!(snapshot.categories[0].count, 2);
        assert_eq!(snapshot.categories[0].icon, "sparkles");
        assert_eq!(snapshot.categories[3].count, 1);
        assert_eq!(snapshot.categories[5].count, 0);
        assert!(snapshot.warnings.is_empty(), "{:?}", snapshot.warnings);
    }

    #[test]
    fn warns_about_stray_files_and_unknown_directories() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        fs::create_dir_all(root.join("unknown")).unwrap();
        touch(&root.join("unknown/stray.mp3"));
        touch(&root.join("root-level.mp3"));

        let snapshot = scan_library(root, LibrarySource::Custom, &categories());

        assert!(snapshot.songs.is_empty());
        assert_eq!(snapshot.warnings.len(), 3, "{:?}", snapshot.warnings);
        assert!(snapshot.warnings.iter().any(|w| w.contains("unknown/")));
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("root-level.mp3"))
        );
        assert!(
            snapshot
                .warnings
                .iter()
                .any(|w| w.contains("未发现任何 mp3"))
        );
    }

    #[test]
    fn missing_root_is_reported_without_panicking() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("nope");

        let snapshot = scan_library(&missing, LibrarySource::Custom, &categories());

        assert!(snapshot.songs.is_empty());
        assert_eq!(snapshot.categories.len(), 6);
        assert!(snapshot.warnings[0].contains("不存在"));
    }

    #[test]
    #[cfg_attr(
        windows,
        ignore = "Windows 文件系统大小写不敏感，无法在同一目录制造同名不同扩展的重复文件"
    )]
    fn skips_duplicate_keys() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        fs::create_dir_all(root.join("cheerful")).unwrap();
        touch(&root.join("cheerful/same.mp3"));
        touch(&root.join("cheerful/same.MP3"));

        let snapshot = scan_library(root, LibrarySource::Bundled, &categories());

        assert_eq!(snapshot.songs.len(), 1);
        assert!(snapshot.warnings.iter().any(|w| w.contains("重复曲目标识")));
    }
}
