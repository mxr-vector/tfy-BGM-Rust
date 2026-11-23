//! 字幕目录索引、稳定键迁移与删除。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use bgm_core::Song;
use serde::Deserialize;
use serde_json::{Map, Value};
use tempfile::NamedTempFile;

use super::{find_subtitle, key_hash, lock_record, read_record, stage_file, write_error};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleSummary {
    pub key: String,
    pub title: Option<String>,
    pub created_at_ms: u64,
    updated_at_ms: Option<u64>,
}

pub fn list_subtitles(directory: &Path) -> Result<Vec<SubtitleSummary>, String> {
    subtitle_index(directory)
        .map(|records| records.into_iter().map(|(summary, _)| summary).collect())
}

pub(super) fn subtitle_index(directory: &Path) -> Result<Vec<(SubtitleSummary, PathBuf)>, String> {
    let Some(files) = read_directory(directory)? else {
        return Ok(Vec::new());
    };
    let mut records = HashMap::<String, (SubtitleSummary, PathBuf)>::new();
    for entry in files {
        let entry = entry.map_err(write_error)?;
        if !entry.file_type().map_err(write_error)?.is_file()
            || entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("json")
        {
            continue;
        }
        let path = entry.path();
        let Ok(summary) = read_record::<SubtitleSummary>(&path) else {
            continue;
        };
        let suffix = format!("-{:016x}.json", key_hash(&summary.key));
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(&suffix))
        {
            continue;
        }
        let revision =
            |record: &SubtitleSummary| record.updated_at_ms.unwrap_or(record.created_at_ms);
        if records
            .get(&summary.key)
            .is_none_or(|(previous, previous_path)| {
                (revision(&summary), path.file_name())
                    > (revision(previous), previous_path.file_name())
            })
        {
            records.insert(summary.key.clone(), (summary, path));
        }
    }
    let mut records: Vec<_> = records.into_values().collect();
    records.sort_by(|(a, _), (b, _)| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| a.key.cmp(&b.key))
    });
    Ok(records)
}

pub fn migrate_subtitle(directory: &Path, from: &str, song: &Song) -> Result<(), String> {
    if from == song.key {
        return Ok(());
    }
    let separations = directory.with_file_name("separations");
    if find_subtitle(directory, from)?.is_none()
        && separation_directory(&separations, from)?.is_none()
    {
        return Ok(());
    }
    fs::create_dir_all(directory).map_err(write_error)?;
    let mut hashes = [key_hash(from), key_hash(&song.key)];
    hashes.sort_unstable();
    let _first = lock_record(directory, hashes[0])?;
    let _second = (hashes[0] != hashes[1])
        .then(|| lock_record(directory, hashes[1]))
        .transpose()?;
    let record = find_subtitle(directory, from)?.map(|(path, _)| path);
    let separation = separation_directory(&separations, from)?;
    if record.is_none() && separation.is_none() {
        return Ok(());
    }
    if find_subtitle(directory, &song.key)?.is_some()
        || separation_directory(&separations, &song.key)?.is_some()
    {
        return Err("目标曲目已有字幕或分离产物".into());
    }
    if let Some(path) = &record {
        check_duplicate_records(directory, from, path)?;
    }

    let stem = stem(&song.key);
    let destination_record = directory.join(format!("{stem}.json"));
    let separation = separation
        .map(|source| {
            let destination = source.with_file_name(stem);
            SeparationMove::prepare(source, destination, from, song)
        })
        .transpose()?;
    let staged_record = record
        .as_ref()
        .map(|source| {
            let mut data = read_record::<Map<String, Value>>(source)?;
            if data.get("key").and_then(Value::as_str) != Some(from) {
                return Err("字幕曲目键已经变更".into());
            }
            update_document(&mut data, song, separation.as_ref());
            data.insert("title".into(), song.title.clone().into());
            data.insert("fileName".into(), song.file_name.clone().into());
            stage_json(directory, &data)
        })
        .transpose()?;
    if let Some(separation) = &separation {
        fs::rename(&separation.source, &separation.destination).map_err(write_error)?;
    }

    let mut published_record = false;
    let mut separation = separation;
    let operation = (|| {
        if let Some(separation) = &mut separation {
            separation.publish_manifest()?;
        }
        if let Some(staged) = staged_record {
            staged
                .persist_noclobber(&destination_record)
                .map_err(|error| write_error(error.error))?;
            published_record = true;
        }
        if let Some(record) = record {
            fs::remove_file(record).map_err(write_error)?;
        }
        Ok::<_, String>(())
    })();
    if let Err(mut error) = operation {
        if published_record && let Err(rollback) = fs::remove_file(destination_record) {
            error.push_str(&format!("，无法撤销目标字幕：{rollback}"));
        }
        if let Some(separation) = separation {
            separation.revert(&mut error);
        }
        return Err(error);
    }
    Ok(())
}

struct SeparationMove {
    source: PathBuf,
    destination: PathBuf,
    manifest: Option<(NamedTempFile, NamedTempFile)>,
    previous_manifest: Option<NamedTempFile>,
}

impl SeparationMove {
    fn prepare(
        source: PathBuf,
        destination: PathBuf,
        from: &str,
        song: &Song,
    ) -> Result<Self, String> {
        let mut migration = Self {
            source,
            destination,
            manifest: None,
            previous_manifest: None,
        };
        let manifest = migration.source.join("separation.json");
        match fs::symlink_metadata(&manifest) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(metadata) if metadata.is_file() => {
                let mut data = read_record::<Map<String, Value>>(&manifest)?;
                if data.get("key").and_then(Value::as_str) != Some(from) {
                    return Err("分离记录的曲目键与源曲目不一致".into());
                }
                let parent = migration.source.parent().ok_or("没有分离产物目录")?;
                let backup = stage_file(parent, &fs::read(&manifest).map_err(write_error)?)
                    .map_err(write_error)?;
                update_document(&mut data, song, Some(&migration));
                migration.manifest = Some((stage_json(parent, &data)?, backup));
            }
            Ok(_) => return Err("分离记录不是普通文件".into()),
            Err(error) => return Err(write_error(error)),
        }
        Ok(migration)
    }

    fn publish_manifest(&mut self) -> Result<(), String> {
        if let Some((next, backup)) = self.manifest.take() {
            next.persist(self.destination.join("separation.json"))
                .map_err(|error| write_error(error.error))?;
            self.previous_manifest = Some(backup);
        }
        Ok(())
    }

    fn revert(self, error: &mut String) {
        if let Some(backup) = self.previous_manifest
            && let Err(rollback) = backup.persist(self.destination.join("separation.json"))
        {
            error.push_str(&format!("，无法恢复分离记录：{}", rollback.error));
        }
        if let Err(rollback) = fs::rename(self.destination, self.source) {
            error.push_str(&format!("，无法恢复分离目录：{rollback}"));
        }
    }
}

fn update_document(
    data: &mut Map<String, Value>,
    song: &Song,
    separation: Option<&SeparationMove>,
) {
    data.insert("key".into(), song.key.clone().into());
    if data.contains_key("sourcePath") {
        data.insert("sourcePath".into(), song.path.clone().into());
    }
    if let Some(separation) = separation {
        for (field, filename) in [
            ("vocalsPath", "vocals.wav"),
            ("accompanimentPath", "accompaniment.wav"),
            ("srtPath", "subtitles.srt"),
            ("lrcPath", "subtitles.lrc"),
        ] {
            let Some(existing) = data.get(field).and_then(Value::as_str).map(Path::new) else {
                continue;
            };
            let owned = existing.file_name().and_then(|name| name.to_str()) == Some(filename)
                && existing
                    .parent()
                    .and_then(|parent| parent.canonicalize().ok())
                    .as_deref()
                    == Some(separation.source.as_path());
            if owned {
                data.insert(
                    field.into(),
                    separation
                        .destination
                        .join(filename)
                        .to_string_lossy()
                        .into_owned()
                        .into(),
                );
            }
        }
    }
}

fn stage_json(directory: &Path, data: &Map<String, Value>) -> Result<NamedTempFile, String> {
    let bytes = serde_json::to_vec_pretty(data).map_err(|error| error.to_string())?;
    stage_file(directory, &bytes).map_err(write_error)
}

pub fn delete_subtitle(directory: &Path, key: &str) -> Result<(), String> {
    if delete_subtitle_if_present(directory, key)? {
        Ok(())
    } else {
        Err("字幕记录不存在".into())
    }
}

pub(crate) fn delete_subtitle_if_present(directory: &Path, key: &str) -> Result<bool, String> {
    if read_directory(directory)?.is_none() {
        return Ok(false);
    }
    delete_artifacts(directory, key, true)
}

pub(in crate::library) fn delete_standalone_separation(
    directory: &Path,
    key: &str,
) -> Result<(), String> {
    delete_artifacts(directory, key, false).map(|_| ())
}

fn delete_artifacts(directory: &Path, key: &str, delete_subtitle: bool) -> Result<bool, String> {
    let _lock = lock_record(directory, key_hash(key))?;
    let record = find_subtitle(directory, key)?.map(|(path, _)| path);
    match (&record, delete_subtitle) {
        (Some(record), true) => check_duplicate_records(directory, key, record)?,
        (Some(_), false) => return Err("分离产物已关联字幕，请在字幕页管理".into()),
        (None, true) => return Ok(false),
        (None, false) => {}
    }
    let separation = verified_separation_directory(directory, key)?;
    if record.is_none() && separation.is_none() {
        return Err("当前曲目没有分离产物".into());
    }
    let staging = tempfile::Builder::new()
        .prefix(".deleted-")
        .tempdir_in(directory)
        .map_err(write_error)?;
    let staged_separation = staging.path().join("separation");
    if let Some(source) = &separation {
        fs::rename(source, &staged_separation).map_err(write_error)?;
    }
    if let Some(record) = record
        && let Err(error) = fs::rename(record, staging.path().join("record.json"))
    {
        let mut error = write_error(error);
        if let Some(source) = &separation
            && let Err(rollback) = fs::rename(staged_separation, source)
        {
            let retained = staging.keep();
            error.push_str(&format!(
                "，分离产物保留于 {}：{rollback}",
                retained.display()
            ));
        }
        return Err(error);
    }
    staging
        .close()
        .map(|()| true)
        .map_err(|error| format!("记录已移出，清理暂存文件失败：{error}"))
}

pub(in crate::library) fn verified_separation_directory(
    directory: &Path,
    key: &str,
) -> Result<Option<PathBuf>, String> {
    separation_record(directory, key).map(|record| record.map(|(path, _)| path))
}

pub(in crate::library) fn separation_record(
    directory: &Path,
    key: &str,
) -> Result<Option<(PathBuf, Map<String, Value>)>, String> {
    let Some(source) = separation_directory(&directory.with_file_name("separations"), key)? else {
        return Ok(None);
    };
    let manifest = source.join("separation.json");
    if !fs::symlink_metadata(&manifest).is_ok_and(|metadata| metadata.is_file()) {
        return Err("无法确认分离目录属于当前曲目".into());
    }
    let record = read_record::<Map<String, Value>>(&manifest)?;
    if record.get("key").and_then(Value::as_str) != Some(key) {
        return Err("无法确认分离目录属于当前曲目".into());
    }
    Ok(Some((source, record)))
}

pub(super) fn check_duplicate_records(
    directory: &Path,
    key: &str,
    current: &Path,
) -> Result<(), String> {
    let suffix = format!("-{:016x}.json", key_hash(key));
    for entry in fs::read_dir(directory).map_err(write_error)? {
        let entry = entry.map_err(write_error)?;
        if entry.path() != current
            && entry.file_type().map_err(write_error)?.is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(&suffix))
            && read_record::<SubtitleSummary>(&entry.path()).is_ok_and(|record| record.key == key)
        {
            return Err("同一曲目存在多份字幕，已保留原文件".into());
        }
    }
    Ok(())
}

fn read_directory(path: &Path) -> Result<Option<fs::ReadDir>, String> {
    match fs::read_dir(path) {
        Ok(files) => Ok(Some(files)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("无法读取字幕目录：{error}")),
    }
}

fn separation_directory(base: &Path, key: &str) -> Result<Option<PathBuf>, String> {
    let Some(files) = read_directory(base)? else {
        return Ok(None);
    };
    if !fs::symlink_metadata(base).map_err(write_error)?.is_dir() {
        return Err("分离产物根目录不是普通目录".into());
    }
    let suffix = format!("-{:016x}", key_hash(key));
    let mut found = None;
    for entry in files {
        let entry = entry.map_err(write_error)?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(&suffix))
        {
            if found.is_some() || !entry.file_type().map_err(write_error)?.is_dir() {
                return Err("曲目匹配的分离目录存在冲突".into());
            }
            found = Some(entry.path().canonicalize().map_err(write_error)?);
        }
    }
    Ok(found)
}

pub(in crate::library) fn stem(key: &str) -> String {
    let prefix: String = bgm_core::export::safe_file_name(key)
        .chars()
        .take(48)
        .collect();
    format!("{prefix}-{:016x}", key_hash(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Library, SubtitleEdit, SubtitleFormat, export_subtitle, load_subtitle};
    use bgm_core::LibrarySource;
    use serde_json::json;

    fn make_record(config: &Path, key: &str) -> (PathBuf, PathBuf) {
        let directory = config.join("subtitles");
        let separation = config.join("separations").join(stem(key));
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&separation).unwrap();
        let vocals = separation.join("vocals.wav");
        fs::write(&vocals, b"test vocals").unwrap();
        fs::write(
            separation.join("separation.json"),
            json!({
                "key": key, "sourcePath": "old.mp3", "vocalsPath": vocals,
            })
            .to_string(),
        )
        .unwrap();
        let record = directory.join(format!("{}.json", stem(key)));
        fs::write(&record, json!({
            "key": key, "title": "旧标题", "fileName": "old.mp3", "createdAtMs": 10, "updatedAtMs": 11,
            "segments": [{"start": 0.4, "end": 1.2, "text": "测试字幕"}],
            "vocalsPath": vocals, "extensionField": true,
        }).to_string()).unwrap();
        (record, separation)
    }

    #[test]
    fn libraries_rekey_on_rename_and_move_then_keep_an_exportable_orphan() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("music");
        let config = temporary.path().join("config");
        let old_key = "epic/old";
        let audio = root.join("epic/old.mp3");
        fs::create_dir_all(audio.parent().unwrap()).unwrap();
        fs::create_dir_all(root.join("memories")).unwrap();
        fs::write(&audio, []).unwrap();
        let (old_record, old_separation) = make_record(&config, old_key);
        let mut library = Library::open_in(config.clone(), |_| {
            (root.clone(), LibrarySource::Custom, Vec::new())
        });
        let renamed = library.update_track(old_key, "新标题", "epic").unwrap();
        let renamed_key = &renamed.changes[0].to;
        let subtitles = library.subtitle_directory();
        assert!(load_subtitle(&subtitles, renamed_key).unwrap().is_some());
        assert!(!old_record.exists());
        assert!(!old_separation.exists());
        let moved = library.move_song(renamed_key, "memories").unwrap();
        let key = &moved.changes[0].to;
        let new_separation = config.join("separations").join(stem(key));
        let manifest: Value = read_record(&new_separation.join("separation.json")).unwrap();
        assert_eq!(manifest["key"], key.as_str());
        let moved_audio = library.song(key).unwrap().path.clone();
        assert_eq!(manifest["sourcePath"], moved_audio);
        assert!(Path::new(manifest["vocalsPath"].as_str().unwrap()).is_file());
        let record: Value = read_record(&subtitles.join(format!("{}.json", stem(key)))).unwrap();
        assert_eq!(record["createdAtMs"], 10);
        assert_eq!(record["updatedAtMs"], 11);
        assert_eq!(record["title"], "新标题");
        assert_eq!(record["extensionField"], true);
        assert!(load_subtitle(&subtitles, renamed_key).unwrap().is_none());

        library.trash_batch(std::slice::from_ref(key)).unwrap();
        assert!(library.song(key).is_none());
        assert!(root.join(".tfy-trash").is_dir());
        assert!(new_separation.is_dir());
        let records = list_subtitles(&subtitles).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(&records[0].key, key);
        let preview = SubtitleEdit::open(&subtitles, key)
            .unwrap()
            .text(SubtitleFormat::Srt);
        let output = temporary.path().join("orphan.srt");
        export_subtitle(&output, &preview).unwrap();
        assert!(fs::read_to_string(&output).unwrap().contains("测试字幕"));
        let batch = fs::read_dir(root.join(".tfy-trash"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let recoverable = fs::read_dir(batch).unwrap().next().unwrap().unwrap().path();
        fs::rename(recoverable, &moved_audio).unwrap();
        library.rescan();
        assert!(library.song(key).is_some());
        assert!(load_subtitle(&subtitles, key).unwrap().is_some());
        assert!(new_separation.is_dir());
        library.trash_batch(std::slice::from_ref(key)).unwrap();
        delete_subtitle(&subtitles, key).unwrap();
        assert!(!new_separation.exists());
        assert!(list_subtitles(&subtitles).unwrap().is_empty());
        assert!(output.exists());
        assert!(root.join(".tfy-trash").is_dir());
    }

    #[test]
    fn detected_external_move_rolls_back_artifacts_when_destination_record_is_blocked() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap().join("music");
        let config = temporary.path().join("config");
        let first = root.join("epic/track.mp3");
        let next = root.join("memories/track.mp3");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(next.parent().unwrap()).unwrap();
        fs::write(&first, []).unwrap();
        let (original_record, separation) = make_record(&config, "epic/track");
        let original_json = fs::read(separation.join("separation.json")).unwrap();
        let mut library = Library::open_in(config.clone(), |_| {
            (root.clone(), LibrarySource::Custom, Vec::new())
        });
        let subtitles = library.subtitle_directory();
        let destination = subtitles.join(format!("{}.json", stem("memories/track")));
        fs::create_dir(&destination).unwrap();
        fs::rename(&first, &next).unwrap();
        let (revision, snapshot) = library.watch_scan().run().unwrap();
        let change = library.apply_watch_scan(revision, snapshot).unwrap();
        assert_eq!(change.moved[0].to, "memories/track");
        assert_eq!(change.migration_warnings.len(), 1);
        assert!(original_record.is_file());
        assert_eq!(
            fs::read(separation.join("separation.json")).unwrap(),
            original_json
        );
        assert!(separation.join("vocals.wav").is_file());
        assert!(
            !config
                .join("separations")
                .join(stem("memories/track"))
                .exists()
        );
        assert!(destination.is_dir());
        assert!(next.is_file());

        fs::remove_dir(destination).unwrap();
        for (source, target) in [(&next, &first), (&first, &next)] {
            fs::rename(source, target).unwrap();
            let (revision, snapshot) = library.watch_scan().run().unwrap();
            let change = library.apply_watch_scan(revision, snapshot).unwrap();
            assert!(change.migration_warnings.is_empty());
        }
        assert!(
            load_subtitle(&subtitles, "memories/track")
                .unwrap()
                .is_some()
        );
        assert!(
            config
                .join("separations")
                .join(stem("memories/track"))
                .is_dir()
        );
        assert!(!original_record.exists());
    }
}
