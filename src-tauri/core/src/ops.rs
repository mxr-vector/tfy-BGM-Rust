//! 曲库写操作：导入、重命名、移动分类、软删除与批量导出。
//!
//! 所有路径都由「曲库根目录 + 注册分类 + 文件名」推导，不信任前端传入的绝对路径。
//! 删除采用软删除：文件被移动到 `<曲库根>/.tfy-trash/<批次>/`，可手动恢复。

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use thiserror::Error;

use crate::categories::Category;
use crate::export::{ExportError, copy_validated, safe_file_name};
use crate::library::Song;

const MAX_STEM_CHARS: usize = 120;
const MAX_KEY_LEN: usize = 512;
const TRASH_DIR: &str = ".tfy-trash";

/// 单批操作的结果统计，直接序列化给前端展示。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReport {
    pub total: usize,
    pub succeeded: usize,
    pub skipped: usize,
    pub failed: usize,
    pub messages: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
}

impl BatchReport {
    fn new(total: usize) -> Self {
        Self {
            total,
            ..Self::default()
        }
    }

    fn fail(&mut self, message: String) {
        self.failed += 1;
        self.failures.push(message.clone());
        self.messages.push(message);
    }

    fn skip(&mut self, message: String) {
        self.skipped += 1;
        self.messages.push(message);
    }
}

/// 曲目在文件系统中的标识发生了变化（曲名或分类）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyChange {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOutcome {
    pub changes: Vec<KeyChange>,
    /// 已删除曲目的旧 key，便于上层同步收藏与播放队列。
    pub removed: Vec<String>,
    pub report: BatchReport,
}

#[derive(Debug, Error)]
pub enum OpError {
    #[error("未注册的分类：{0}")]
    UnknownCategory(String),
    #[error("曲名不能为空")]
    EmptyTitle,
    #[error("曲名包含非法字符：{0}")]
    InvalidTitle(String),
    #[error("曲名过长（最多 {MAX_STEM_CHARS} 个字符）")]
    TitleTooLong,
    #[error("目标文件已存在：{}", .0.display())]
    TargetExists(PathBuf),
    #[error("源文件不存在或不是文件：{}", .0.display())]
    SourceMissing(PathBuf),
    #[error("只支持 mp3 音频：{}", .0.display())]
    UnsupportedFormat(PathBuf),
    #[error("曲目路径无效：{}", .0.display())]
    InvalidFile(PathBuf),
    #[error("操作文件失败：{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Export(#[from] ExportError),
}

/// 把音频导入到指定分类目录。同名文件自动追加序号，不覆盖已有曲目。
pub fn import_files(
    root: &Path,
    categories: &[Category],
    category_id: &str,
    sources: &[PathBuf],
) -> BatchReport {
    let mut report = BatchReport::new(sources.len());

    let Some(category) = find_category(categories, category_id) else {
        report.fail(format!("未注册的分类：{category_id}"));
        return report;
    };
    let dir = root.join(&category.id);
    if let Err(error) = fs::create_dir_all(&dir) {
        report.fail(format!("无法创建分类目录 {}：{error}", dir.display()));
        return report;
    }
    let dir_canonical = fs::canonicalize(&dir).ok();

    for source in sources {
        match import_one(&dir, dir_canonical.as_deref(), source) {
            Ok(ImportState::Imported(name)) => {
                report.succeeded += 1;
                report.messages.push(format!("已导入：{name}"));
            }
            Ok(ImportState::Skipped(message)) => report.skip(message),
            Err(error) => report.fail(format!("{}：{error}", source.display())),
        }
    }

    report
}

enum ImportState {
    Imported(String),
    Skipped(String),
}

fn import_one(
    dir: &Path,
    dir_canonical: Option<&Path>,
    source: &Path,
) -> Result<ImportState, OpError> {
    let metadata = fs::metadata(source).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => OpError::SourceMissing(source.to_owned()),
        _ => OpError::Io(error),
    })?;
    if !metadata.is_file() {
        return Err(OpError::SourceMissing(source.to_owned()));
    }
    if !is_mp3(source) {
        return Err(OpError::UnsupportedFormat(source.to_owned()));
    }

    if let (Some(dir_canonical), Ok(source_canonical)) = (dir_canonical, fs::canonicalize(source))
        && source_canonical.parent() == Some(dir_canonical)
    {
        return Ok(ImportState::Skipped(format!(
            "{} 已在该分类目录中，已跳过",
            source.display()
        )));
    }

    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.trim())
        .filter(|stem| !stem.is_empty())
        .map(|stem| stem.chars().take(MAX_STEM_CHARS).collect::<String>())
        .unwrap_or_else(|| "audio".to_owned());
    let category = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if format!("{category}/{stem}").len() > MAX_KEY_LEN {
        return Err(OpError::TitleTooLong);
    }

    let target = unique_path(dir, &stem, "mp3");
    copy_validated(source, &target)?;
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| stem.clone());
    Ok(ImportState::Imported(name))
}

/// 修改曲名（重命名文件），文件名冲突时返回错误而不是覆盖。
pub fn rename_song(
    root: &Path,
    categories: &[Category],
    song: &Song,
    new_title: &str,
) -> Result<KeyChange, OpError> {
    update_song(root, categories, song, Some(new_title), None)
}

/// 把曲目移动到另一个分类，保留原文件名主干，重名时追加序号。
pub fn move_song(
    root: &Path,
    categories: &[Category],
    song: &Song,
    target_category: &str,
) -> Result<KeyChange, OpError> {
    update_song(root, categories, song, None, Some(target_category))
}

/// 批量移动曲目到目标分类，逐首记录成功与失败。
pub fn move_songs(
    root: &Path,
    categories: &[Category],
    songs: &[&Song],
    target_category: &str,
) -> BatchOutcome {
    let mut outcome = BatchOutcome {
        report: BatchReport::new(songs.len()),
        ..BatchOutcome::default()
    };
    if find_category(categories, target_category).is_none() {
        outcome
            .report
            .fail(format!("未注册的分类：{target_category}"));
        return outcome;
    }

    for song in songs {
        match move_song(root, categories, song, target_category) {
            Ok(change) => {
                outcome.report.succeeded += 1;
                outcome.changes.push(change);
            }
            Err(error) => outcome.report.fail(format!("{}：{error}", song.title)),
        }
    }
    outcome
}

/// 同时修改曲名与分类。曲名未变时保留原文件主干（含下划线）。
pub fn update_song(
    root: &Path,
    categories: &[Category],
    song: &Song,
    new_title: Option<&str>,
    new_category: Option<&str>,
) -> Result<KeyChange, OpError> {
    let target_category = new_category.unwrap_or(&song.category);
    if find_category(categories, target_category).is_none() {
        return Err(OpError::UnknownCategory(target_category.to_owned()));
    }

    let source = song_file_path(root, categories, song)?;
    let rename = match new_title {
        Some(title) => {
            let trimmed = title.trim();
            if trimmed == song.title {
                None
            } else {
                Some(validate_title(trimmed)?)
            }
        }
        None => None,
    };
    let moved = target_category != song.category;

    if rename.is_none() && !moved {
        return Ok(KeyChange {
            from: song.key.clone(),
            to: song.key.clone(),
        });
    }

    let renamed = rename.is_some();
    let stem = rename.unwrap_or_else(|| {
        Path::new(&song.file_name)
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| song.title.replace(' ', "_"))
    });
    if format!("{target_category}/{stem}").len() > MAX_KEY_LEN {
        return Err(OpError::TitleTooLong);
    }

    let target_dir = root.join(target_category);
    fs::create_dir_all(&target_dir)?;
    let target = if renamed {
        let exact = target_dir.join(format!("{stem}.mp3"));
        if exact.exists() && !same_file(&source, &exact) {
            return Err(OpError::TargetExists(exact));
        }
        exact
    } else {
        unique_path(&target_dir, &stem, "mp3")
    };

    if !same_file(&source, &target) {
        fs::rename(&source, &target)?;
    }

    let final_stem = target
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or(stem);
    Ok(KeyChange {
        from: song.key.clone(),
        to: format!("{target_category}/{final_stem}"),
    })
}

/// 软删除：移动到曲库根目录下的 `.tfy-trash/<批次>/`，返回被移除的 key。
pub fn trash_songs(root: &Path, categories: &[Category], songs: &[&Song]) -> BatchOutcome {
    let mut outcome = BatchOutcome {
        report: BatchReport::new(songs.len()),
        ..BatchOutcome::default()
    };
    if songs.is_empty() {
        return outcome;
    }

    let batch_dir = unique_trash_dir(root);
    if let Err(error) = fs::create_dir_all(&batch_dir) {
        outcome.report.fail(format!(
            "无法创建回收站目录 {}：{error}",
            batch_dir.display()
        ));
        return outcome;
    }
    let mut used = HashSet::new();

    for song in songs {
        let source = match song_file_path(root, categories, song) {
            Ok(path) => path,
            Err(error) => {
                outcome.report.fail(format!("{}：{error}", song.title));
                continue;
            }
        };
        if !source.is_file() {
            outcome
                .report
                .skip(format!("{}：文件已不存在，已跳过", song.title));
            continue;
        }

        let target = unique_trash_path(&batch_dir, song, &mut used);
        match fs::rename(&source, &target) {
            Ok(()) => {
                outcome.report.succeeded += 1;
                outcome.removed.push(song.key.clone());
            }
            Err(error) => outcome.report.fail(format!("{}：{error}", song.title)),
        }
    }

    if outcome.report.succeeded > 0 {
        outcome.report.notice = Some(format!(
            "已移入回收站：{}（可手动恢复）",
            batch_dir.display()
        ));
    }
    outcome
}

/// 把曲目复制到指定目录，同名时追加序号，成功后校验字节数。
pub fn export_songs(
    root: &Path,
    categories: &[Category],
    dest_dir: &Path,
    songs: &[&Song],
) -> BatchOutcome {
    let mut outcome = BatchOutcome {
        report: BatchReport::new(songs.len()),
        ..BatchOutcome::default()
    };
    if !dest_dir.is_dir() {
        outcome.report.fail(format!(
            "导出目录不存在或不是文件夹：{}",
            dest_dir.display()
        ));
        return outcome;
    }

    for song in songs {
        let source = match song_file_path(root, categories, song) {
            Ok(path) => path,
            Err(error) => {
                outcome.report.fail(format!("{}：{error}", song.title));
                continue;
            }
        };
        let stem = safe_file_name(&song.title);
        let target = unique_path(dest_dir, &stem, "mp3");
        match copy_validated(&source, &target) {
            Ok(_) => outcome.report.succeeded += 1,
            Err(error) => outcome.report.fail(format!("{}：{error}", song.title)),
        }
    }

    if outcome.report.succeeded > 0 {
        outcome.report.notice = Some(format!(
            "已导出 {} 首到：{}",
            outcome.report.succeeded,
            dest_dir.display()
        ));
    }
    outcome
}

fn validate_title(title: &str) -> Result<String, OpError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(OpError::EmptyTitle);
    }
    if title.starts_with('.') {
        return Err(OpError::InvalidTitle(title.to_owned()));
    }
    if title.chars().any(|ch| {
        ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    }) {
        return Err(OpError::InvalidTitle(title.to_owned()));
    }
    let stem: String = title.chars().take(MAX_STEM_CHARS).collect();
    if title.chars().count() > MAX_STEM_CHARS {
        return Err(OpError::TitleTooLong);
    }
    Ok(stem)
}

fn find_category<'a>(categories: &'a [Category], id: &str) -> Option<&'a Category> {
    categories.iter().find(|category| category.id == id)
}

fn song_file_path(root: &Path, categories: &[Category], song: &Song) -> Result<PathBuf, OpError> {
    if find_category(categories, &song.category).is_none() {
        return Err(OpError::UnknownCategory(song.category.clone()));
    }
    let file_name = Path::new(&song.file_name);
    let plain = matches!(file_name.components().next(), Some(Component::Normal(_)))
        && file_name.components().count() == 1;
    if !plain {
        return Err(OpError::InvalidFile(PathBuf::from(&song.file_name)));
    }
    let path = root.join(&song.category).join(&song.file_name);
    if !path.is_file() {
        return Err(OpError::SourceMissing(path));
    }
    Ok(path)
}

fn is_mp3(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}.{ext}"));
    let mut suffix = 2;
    while candidate.exists() {
        let shortened: String = stem.chars().take(100).collect();
        candidate = dir.join(format!("{shortened} ({suffix}).{ext}"));
        suffix += 1;
    }
    candidate
}

fn unique_trash_dir(root: &Path) -> PathBuf {
    let base = root.join(TRASH_DIR);
    let stamp = now_ms();
    let mut candidate = base.join(stamp.to_string());
    let mut suffix = 2;
    while candidate.exists() {
        candidate = base.join(format!("{stamp}-{suffix}"));
        suffix += 1;
    }
    candidate
}

fn unique_trash_path(batch_dir: &Path, song: &Song, used: &mut HashSet<String>) -> PathBuf {
    let safe = safe_file_name(&song.title);
    let mut name = format!("{}__{safe}.mp3", song.category);
    let mut suffix = 2;
    while used.contains(&name) || batch_dir.join(&name).exists() {
        name = format!("{}__{safe} ({suffix}).mp3", song.category);
        suffix += 1;
    }
    used.insert(name.clone());
    batch_dir.join(name)
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
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

    fn song(category: &str, stem: &str) -> Song {
        Song {
            key: format!("{category}/{stem}"),
            title: stem.replace('_', " "),
            file_name: format!("{stem}.mp3"),
            category: category.to_owned(),
            path: String::new(),
        }
    }

    fn categories() -> Vec<Category> {
        crate::categories::defaults().items
    }

    fn fixture(root: &Path, category: &str, stem: &str) -> Song {
        let dir = root.join(category);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("{stem}.mp3")), b"audio-bytes").unwrap();
        let mut song = song(category, stem);
        song.path = dir.join(&song.file_name).to_string_lossy().into_owned();
        song
    }

    #[test]
    fn import_copies_files_and_suffixes_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        fs::create_dir_all(&root).unwrap();
        let source = dir.path().join("track.mp3");
        fs::write(&source, b"new-audio").unwrap();

        let report = import_files(&root, &categories(), "epic", &[source.clone(), source]);

        assert_eq!(report.succeeded, 2);
        assert_eq!(report.failed, 0);
        assert!(root.join("epic/track.mp3").is_file());
        assert!(root.join("epic/track (2).mp3").is_file());
    }

    #[test]
    fn import_rejects_unknown_category_and_non_mp3() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        let source = dir.path().join("notes.txt");
        fs::write(&source, b"text").unwrap();

        let unknown = import_files(
            &root,
            &categories(),
            "unknown",
            std::slice::from_ref(&source),
        );
        assert_eq!(unknown.failed, 1);
        assert!(unknown.messages[0].contains("未注册"));

        let unsupported = import_files(&root, &categories(), "epic", &[source]);
        assert_eq!(unsupported.failed, 1);
        assert!(root.join("epic").is_dir());
    }

    #[test]
    fn rename_preserves_category_and_rejects_unsafe_titles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let track = fixture(root, "memories", "old_name");

        let change = rename_song(root, &categories(), &track, "new name").unwrap();
        assert_eq!(change.to, "memories/new name");
        assert!(root.join("memories/new name.mp3").is_file());
        assert!(!root.join("memories/old_name.mp3").exists());

        let current = Song {
            key: change.to.clone(),
            title: "new name".into(),
            file_name: "new name.mp3".into(),
            category: "memories".into(),
            path: String::new(),
        };
        assert!(matches!(
            rename_song(root, &categories(), &current, "../escape"),
            Err(OpError::InvalidTitle(_))
        ));
        assert!(matches!(
            rename_song(root, &categories(), &current, "   "),
            Err(OpError::EmptyTitle)
        ));
    }

    #[test]
    fn rename_conflict_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fixture(root, "epic", "one");
        let second = fixture(root, "epic", "two");

        let error = rename_song(root, &categories(), &second, "one").unwrap_err();

        assert!(matches!(error, OpError::TargetExists(_)));
        assert!(root.join("epic/two.mp3").is_file());
    }

    #[test]
    fn move_keeps_file_stem_and_suffixes_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let track = fixture(root, "cheerful", "same_name");
        fixture(root, "country", "same_name");

        let change = move_song(root, &categories(), &track, "country").unwrap();

        assert_eq!(change.to, "country/same_name (2)");
        assert!(root.join("country/same_name (2).mp3").is_file());
        assert!(!root.join("cheerful/same_name.mp3").exists());
    }

    #[test]
    fn update_can_change_title_and_category_together() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let track = fixture(root, "cheerful", "before");

        let change =
            update_song(root, &categories(), &track, Some("after"), Some("gufeng")).unwrap();

        assert_eq!(change.from, "cheerful/before");
        assert_eq!(change.to, "gufeng/after");
        assert!(root.join("gufeng/after.mp3").is_file());
    }

    #[test]
    fn trash_moves_files_out_of_scan_scope() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let first = fixture(root, "epic", "one");
        let second = fixture(root, "epic", "two");

        let outcome = trash_songs(root, &categories(), &[&first, &second]);

        assert_eq!(outcome.report.succeeded, 2);
        assert_eq!(outcome.removed.len(), 2);
        assert!(!root.join("epic/one.mp3").exists());
        assert!(outcome.report.notice.unwrap().contains(TRASH_DIR));
        let trash = root.join(TRASH_DIR);
        let files = fs::read_dir(trash)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .read_dir()
            .unwrap()
            .count();
        assert_eq!(files, 2);
    }

    #[test]
    fn export_uses_safe_unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        let dest = dir.path().join("out");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&dest).unwrap();
        let first = fixture(&root, "epic", "one");
        let second = fixture(&root, "epic", "two");

        let same_title = |source: &Song| Song {
            title: "a/b".into(),
            ..source.clone()
        };
        let first = same_title(&first);
        let second = same_title(&second);

        let outcome = export_songs(&root, &categories(), &dest, &[&first, &second]);

        assert_eq!(outcome.report.succeeded, 2);
        assert!(dest.join("a_b.mp3").is_file());
        assert!(dest.join("a_b (2).mp3").is_file());
    }
}
