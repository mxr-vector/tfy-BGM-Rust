//! 曲目字幕记录读取、编辑和导出。

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use tempfile::NamedTempFile;

pub(crate) mod generation;
mod records;
pub(crate) use records::delete_subtitle_if_present;
mod text;
pub use records::{SubtitleSummary, delete_subtitle, list_subtitles, migrate_subtitle};
pub(super) use records::{
    delete_standalone_separation, separation_record, stem as separation_stem,
    verified_separation_directory,
};
pub use text::MAX_SEGMENTS as MAX_SUBTITLE_SEGMENTS;
pub use text::SubtitleFormat;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SubtitleSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Copy)]
pub enum SeparatedAudio {
    Vocals,
    Accompaniment,
}

#[derive(Clone, Copy)]
pub enum SubtitleDelivery {
    Audio(SeparatedAudio),
    Text(SubtitleFormat),
}

impl SeparatedAudio {
    pub fn label(self) -> &'static str {
        match self {
            Self::Vocals => "人声",
            Self::Accompaniment => "伴奏",
        }
    }

    fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Vocals => ("vocalsPath", "vocals.wav"),
            Self::Accompaniment => ("accompanimentPath", "accompaniment.wav"),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredSubtitle {
    key: String,
    segments: Vec<SubtitleSegment>,
    created_at_ms: u64,
    updated_at_ms: Option<u64>,
}

pub fn load_subtitle(directory: &Path, key: &str) -> Result<Option<Vec<SubtitleSegment>>, String> {
    find_subtitle(directory, key).map(|found| found.map(|(_, record)| record.segments))
}

fn find_subtitle(directory: &Path, key: &str) -> Result<Option<(PathBuf, StoredSubtitle)>, String> {
    let files = match fs::read_dir(directory) {
        Ok(files) => files,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取字幕目录：{error}")),
    };
    let suffix = format!("-{:016x}.json", key_hash(key));
    let mut latest = None;
    let mut failure = None;
    for file in files {
        let entry = file.map_err(|error| format!("无法读取字幕目录：{error}"))?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(|name| name.ends_with(&suffix)) {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {}
            Ok(_) => continue,
            Err(error) => {
                failure = Some(format!("无法读取字幕文件：{error}"));
                continue;
            }
        }
        let path = entry.path();
        let record: StoredSubtitle = match read_record(&path)
            .and_then(|record: StoredSubtitle| text::validate(&record.segments).map(|()| record))
        {
            Ok(record) => record,
            Err(error) => {
                failure = Some(error);
                continue;
            }
        };
        if record.key != key {
            continue;
        }
        let revision = (record.updated_at_ms.unwrap_or(record.created_at_ms), name);
        if latest
            .as_ref()
            .is_none_or(|(previous, _)| &revision > previous)
        {
            latest = Some((revision, (path, record)));
        }
    }
    match latest {
        Some((_, record)) => Ok(Some(record)),
        None => failure.map_or(Ok(None), Err),
    }
}

fn read_record<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    const MAX_RECORD_BYTES: u64 = 256 * 1024 * 1024;
    let file = File::open(path).map_err(|error| format!("无法打开字幕：{error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("无法读取字幕文件：{error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_RECORD_BYTES {
        return Err("字幕记录不是有效文件或超出大小限制".into());
    }
    serde_json::from_reader(BufReader::new(file.take(MAX_RECORD_BYTES)))
        .map_err(|error| format!("无法读取字幕记录：{error}"))
}

pub(crate) enum SubtitleWriteError {
    Invalid(String),
    Conflict(String),
    Storage(String),
}

impl SubtitleWriteError {
    fn message(self) -> String {
        match self {
            Self::Invalid(message) | Self::Conflict(message) | Self::Storage(message) => message,
        }
    }
}

pub(crate) fn subtitle_paths(directory: &Path) -> Result<Vec<(SubtitleSummary, PathBuf)>, String> {
    records::subtitle_index(directory)
}

pub struct SubtitleEdit {
    path: PathBuf,
    record: StoredSubtitle,
    original: Map<String, Value>,
}

impl SubtitleEdit {
    pub fn open(directory: &Path, key: &str) -> Result<Self, String> {
        Self::find(directory, key)?.ok_or_else(|| "当前曲目没有字幕记录".into())
    }

    pub(crate) fn find(directory: &Path, key: &str) -> Result<Option<Self>, String> {
        find_subtitle(directory, key)?
            .map(|(path, _)| Self::from_path(path, key))
            .transpose()
    }

    pub(crate) fn from_path(path: PathBuf, key: &str) -> Result<Self, String> {
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Err("字幕记录不是普通文件".into());
        }
        let value = read_record::<Value>(&path)?;
        let mut record = StoredSubtitle::deserialize(&value)
            .map_err(|error| format!("无法读取字幕记录：{error}"))?;
        let Value::Object(original) = value else {
            return Err("字幕记录必须是 JSON 对象".into());
        };
        if record.key != key {
            return Err("字幕记录已变更，请重新打开".into());
        }
        text::validate(&record.segments)?;
        record.segments.sort_by(|a, b| a.start.total_cmp(&b.start));
        Ok(Self {
            path,
            record,
            original,
        })
    }

    pub fn text(&self, format: SubtitleFormat) -> String {
        self.render(format, &self.record.segments)
    }

    fn render(&self, format: SubtitleFormat, segments: &[SubtitleSegment]) -> String {
        let title = self.original.get("title").and_then(Value::as_str);
        let named = self
            .original
            .get("lrc")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with("[ar:]\n[ti:"));
        if let Some(title) = title.filter(|_| named) {
            format.render_named(title, segments)
        } else {
            format.render(segments)
        }
    }

    pub fn segments(&self) -> &[SubtitleSegment] {
        &self.record.segments
    }

    pub fn separated_audio(&self, kind: SeparatedAudio) -> Result<PathBuf, String> {
        let directory = self.path.parent().ok_or("字幕目录不存在")?;
        let (field, filename) = kind.names();
        let key = &self.record.key;
        let owned = self
            .original
            .get(field)
            .and_then(Value::as_str)
            .and_then(|path| owned_output(directory, Path::new(path), key, filename));
        let path = match owned {
            Some(path) => path,
            None => records::verified_separation_directory(directory, key)?
                .ok_or_else(|| format!("没有找到本应用保存的{}文件", kind.label()))?
                .join(filename),
        };
        if !fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 44)
        {
            return Err(format!("{}文件不存在或不完整", kind.label()));
        }
        Ok(path)
    }

    pub fn delivery_path(&self, delivery: SubtitleDelivery) -> Result<PathBuf, String> {
        let format = match delivery {
            SubtitleDelivery::Audio(kind) => return self.separated_audio(kind),
            SubtitleDelivery::Text(format) => format,
        };
        let (field, filename) = match format {
            SubtitleFormat::Srt => ("srtPath", "subtitles.srt"),
            SubtitleFormat::Lrc => ("lrcPath", "subtitles.lrc"),
        };
        let directory = self.path.parent().ok_or("字幕目录不存在")?;
        self.original
            .get(field)
            .and_then(Value::as_str)
            .and_then(|path| owned_output(directory, Path::new(path), &self.record.key, filename))
            .filter(|path| fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file()))
            .ok_or_else(|| {
                format!(
                    "没有找到本应用保存的 {} 文件",
                    format.extension().to_uppercase()
                )
            })
    }

    pub(crate) fn record_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn document(&self, include_content: bool) -> Value {
        Self::public_record(
            &self.original,
            &self.record.segments,
            include_content,
            |format| self.text(format),
        )
    }

    fn public_record(
        data: &Map<String, Value>,
        segments: &[SubtitleSegment],
        include_content: bool,
        render: impl Fn(SubtitleFormat) -> String,
    ) -> Value {
        let mut result = Map::new();
        for field in [
            "key",
            "title",
            "fileName",
            "durationMs",
            "createdAtMs",
            "updatedAtMs",
            "separationModelName",
            "accompanimentModelName",
            "vocalsPath",
            "accompanimentPath",
            "accompanimentKind",
            "srtPath",
            "lrcPath",
        ] {
            if let Some(value) = data.get(field).filter(|value| !value.is_null()) {
                result.insert(field.into(), value.clone());
            }
        }
        if !result.contains_key("title")
            && let Some(key) = result.get("key").cloned()
        {
            result.insert("title".into(), key);
        }
        if include_content {
            result.insert("segments".into(), serde_json::json!(segments));
            result.insert("srt".into(), render(SubtitleFormat::Srt).into());
            result.insert("lrc".into(), render(SubtitleFormat::Lrc).into());
        }
        result.into()
    }

    pub fn save(
        &self,
        format: SubtitleFormat,
        raw: &str,
        track_duration_ms: Option<u32>,
    ) -> Result<(), String> {
        self.save_text_record(format, raw, track_duration_ms)
            .map(|_| ())
            .map_err(SubtitleWriteError::message)
    }

    pub(crate) fn save_text_record(
        &self,
        format: SubtitleFormat,
        raw: &str,
        track_duration_ms: Option<u32>,
    ) -> Result<Value, SubtitleWriteError> {
        let record_duration = self.original.get("durationMs").and_then(Value::as_f64);
        let durations = [record_duration, track_duration_ms.map(f64::from)]
            .map(|millis| millis.map(|value| value / 1000.0));
        let segments = format
            .parse(raw, durations)
            .map_err(SubtitleWriteError::Invalid)?;
        self.save_segments_record(segments)
    }

    pub fn save_segments(&self, segments: Vec<SubtitleSegment>) -> Result<(), String> {
        self.save_segments_record(segments)
            .map(|_| ())
            .map_err(SubtitleWriteError::message)
    }

    pub(crate) fn save_segments_record(
        &self,
        mut segments: Vec<SubtitleSegment>,
    ) -> Result<Value, SubtitleWriteError> {
        use SubtitleWriteError::{Conflict, Invalid, Storage};

        for line in &mut segments {
            line.text = line
                .text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
        }
        text::validate(&segments).map_err(Invalid)?;
        segments.sort_by(|a, b| a.start.total_cmp(&b.start));
        let directory = self
            .path
            .parent()
            .ok_or_else(|| Storage("字幕目录不存在".into()))?;
        let _lock = lock_record(directory, key_hash(&self.record.key)).map_err(Storage)?;
        let current = find_subtitle(directory, &self.record.key)
            .map_err(Storage)?
            .map(|(path, _)| path);
        if current.as_ref() != Some(&self.path)
            || read_record::<Map<String, Value>>(&self.path).map_err(Storage)? != self.original
        {
            return Err(Conflict("字幕已在其他窗口或应用中更新，请重新打开".into()));
        }

        let mut data = self.original.clone();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let revision = self
            .record
            .updated_at_ms
            .unwrap_or(self.record.created_at_ms);
        let srt = SubtitleFormat::Srt.render(&segments);
        let lrc = self.render(SubtitleFormat::Lrc, &segments);
        data.insert(
            "segments".into(),
            serde_json::to_value(&segments).map_err(|error| Storage(error.to_string()))?,
        );
        data.insert(
            "updatedAtMs".into(),
            now.max(revision.saturating_add(1)).into(),
        );
        let mut writes = Vec::new();
        for (field, filename, content) in
            [("srt", "subtitles.srt", srt), ("lrc", "subtitles.lrc", lrc)]
        {
            let owned_path = data
                .get(&format!("{field}Path"))
                .and_then(Value::as_str)
                .and_then(|path| {
                    owned_output(directory, Path::new(path), &self.record.key, filename)
                });
            if let Some(path) = owned_path {
                writes.push(StagedWrite::new(path, content.as_bytes(), true).map_err(Storage)?);
            }
            data.insert(field.into(), content.into());
        }
        let json = serde_json::to_vec_pretty(&data).map_err(|error| Storage(error.to_string()))?;
        writes.push(StagedWrite::new(self.path.clone(), &json, false).map_err(Storage)?);
        commit_writes(writes).map_err(Storage)?;
        Ok(Self::public_record(&data, &segments, true, |format| {
            data[format.extension()]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        }))
    }
}

impl SubtitleSegment {
    pub fn from_fields(start: &str, end: &str, text: &str) -> Result<Self, String> {
        let seconds = |value: &str| {
            value
                .trim()
                .parse::<f64>()
                .map_err(|_| "时间请输入秒数，例如 12.5".to_owned())
        };
        let segment = Self {
            start: seconds(start)?,
            end: seconds(end)?,
            text: text.trim().into(),
        };
        text::validate(std::slice::from_ref(&segment))?;
        Ok(segment)
    }
}

pub(super) fn lock_record(directory: &Path, hash: u64) -> Result<File, String> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(format!(".{hash:016x}.lock")))
        .map_err(|error| format!("无法打开字幕记录锁：{error}"))?;
    lock.lock()
        .map_err(|error| format!("无法锁定字幕记录：{error}"))?;
    Ok(lock)
}

fn owned_output(directory: &Path, path: &Path, key: &str, filename: &str) -> Option<PathBuf> {
    let base = directory.with_file_name("separations");
    if !fs::symlink_metadata(&base).ok()?.is_dir() {
        return None;
    }
    let base = base.canonicalize().ok()?;
    let parent = path.parent()?.canonicalize().ok()?;
    let suffix = format!("-{:016x}", key_hash(key));
    (path.file_name()?.to_str()? == filename
        && parent.parent() == Some(base.as_path())
        && parent.file_name()?.to_str()?.ends_with(&suffix))
    .then(|| parent.join(filename))
}

pub fn export_subtitle(path: &Path, text: &str) -> Result<(), String> {
    commit_writes(vec![StagedWrite::new(path.into(), text.as_bytes(), false)?])
}

struct StagedWrite {
    path: PathBuf,
    next: NamedTempFile,
    original: Option<NamedTempFile>,
}

fn stage_file(directory: &Path, bytes: &[u8]) -> io::Result<NamedTempFile> {
    let mut file = NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    Ok(file)
}

impl StagedWrite {
    fn new(path: PathBuf, bytes: &[u8], backup: bool) -> Result<Self, String> {
        let directory = path.parent().ok_or("字幕保存目录不存在")?;
        let stage = |bytes: &[u8]| stage_file(directory, bytes);
        let original = if backup {
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() => {
                    Some(stage(&fs::read(&path).map_err(write_error)?).map_err(write_error)?)
                }
                Ok(_) => return Err("字幕输出路径不是普通文件".into()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(write_error(error)),
            }
        } else {
            None
        };
        let next = stage(bytes).map_err(write_error)?;
        Ok(Self {
            path,
            next,
            original,
        })
    }
}

fn commit_writes(writes: Vec<StagedWrite>) -> Result<(), String> {
    let mut applied = Vec::<(PathBuf, Option<NamedTempFile>)>::new();
    for StagedWrite {
        path,
        next,
        original,
    } in writes
    {
        if let Err(error) = next.persist(&path) {
            let mut message = write_error(error.error);
            for (path, backup) in applied.into_iter().rev() {
                let result = match backup {
                    Some(backup) => backup
                        .persist(&path)
                        .map(|_| ())
                        .map_err(|error| error.error),
                    None => fs::remove_file(&path),
                };
                if let Err(error) = result {
                    message.push_str(&format!("，无法恢复 {}：{error}", path.display()));
                }
            }
            return Err(message);
        }
        applied.push((path, original));
    }
    Ok(())
}

fn write_error(error: io::Error) -> String {
    format!("无法保存字幕：{error}")
}

pub(super) fn key_hash(key: &str) -> u64 {
    key.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loads_the_latest_valid_record_by_utf8_key_and_ignores_its_filename_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let key = "史诗/海风";
        let hash = key_hash(key);
        assert_eq!(key_hash("hello"), 0xa430d84680aabd0b);
        let record = |key, text, updated| {
            json!({
                "key": key,
                "createdAtMs": 10,
                "updatedAtMs": updated,
                "segments": [{"start": 0.4, "end": 1.2, "text": text}]
            })
        };
        for (name, value) in [
            ("Legacy", record(key, "初次生成", None)),
            ("新前缀", record(key, "编辑后", Some(20))),
            ("Other", record("别的曲目", "冲突键", Some(30))),
        ] {
            fs::write(
                directory.path().join(format!("{name}-{hash:016x}.json")),
                value.to_string(),
            )
            .unwrap();
        }

        let segments = load_subtitle(directory.path(), key).unwrap().unwrap();
        assert_eq!(segments[0].text, "编辑后");
        assert!(load_subtitle(directory.path(), "不存在").unwrap().is_none());
    }

    #[test]
    fn reports_invalid_subtitle_timings() {
        let directory = tempfile::tempdir().unwrap();
        let key = "epic/fixture";
        let file = directory
            .path()
            .join(format!("fixture-{:016x}.json", key_hash(key)));
        let value = json!({
            "key": key, "createdAtMs": 10,
            "segments": [{"start": 4, "end": 2, "text": "无效"}]
        });
        fs::write(file, value.to_string()).unwrap();
        assert!(load_subtitle(directory.path(), key).is_err());
    }

    #[test]
    fn segment_edits_preserve_explicit_endings_sort_and_rebuild_both_formats() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        let key = "epic/track";
        let path = directory.join(format!("track-{:016x}.json", key_hash(key)));
        fs::write(
            &path,
            json!({
                "key": key, "createdAtMs": 10,
                "segments": [{"start": 0.1, "end": 1.2, "text": "旧分段"}],
            })
            .to_string(),
        )
        .unwrap();
        let edit = SubtitleEdit::open(directory, key).unwrap();
        let original = fs::read(&path).unwrap();
        assert!(SubtitleSegment::from_fields("NaN", "2.000", "无效").is_err());
        assert!(edit.save_segments(Vec::new()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);

        let a = SubtitleSegment::from_fields("2.5", "3.25", "后段").unwrap();
        let b = SubtitleSegment::from_fields("0.125", "1.75", "前段\n追加").unwrap();
        edit.save_segments(vec![a, b]).unwrap();
        let record: Value = read_record(&path).unwrap();
        assert_eq!(record["createdAtMs"], 10);
        assert_eq!(
            record["segments"][0],
            json!({"start": 0.125, "end": 1.75, "text": "前段 追加"})
        );
        assert!(record["updatedAtMs"].as_u64().unwrap() > 10);
        assert!(
            record["srt"]
                .as_str()
                .unwrap()
                .contains("00:00:00,125 --> 00:00:01,750")
        );
        assert!(
            record["lrc"]
                .as_str()
                .unwrap()
                .starts_with("[00:00.125]前段 追加")
        );
        assert!(edit.save_segments(edit.segments().to_vec()).is_err());
    }

    #[test]
    fn separated_audio_uses_verified_local_artifacts_and_rejects_incomplete_files() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("subtitles");
        let key = "epic/track";
        let hash = key_hash(key);
        let external = temporary.path().join("vocals.wav");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&external, [0; 48]).unwrap();
        let record = directory.join(format!("track-{hash:016x}.json"));
        fs::write(
            &record,
            json!({
                "key": key, "createdAtMs": 10,
                "segments": [{"start": 0.1, "end": 1.2, "text": "分段"}],
                "vocalsPath": external,
            })
            .to_string(),
        )
        .unwrap();
        let edit = SubtitleEdit::open(&directory, key).unwrap();
        assert!(edit.separated_audio(SeparatedAudio::Vocals).is_err());

        let separation = temporary
            .path()
            .join(format!("separations/track-{hash:016x}"));
        fs::create_dir_all(&separation).unwrap();
        let manifest = separation.join("separation.json");
        let vocals = separation.join("vocals.wav");
        let accompaniment = separation.join("accompaniment.wav");
        fs::write(&manifest, json!({"key": key}).to_string()).unwrap();
        fs::write(&vocals, [0; 48]).unwrap();
        fs::write(&accompaniment, [0; 44]).unwrap();
        assert_eq!(
            edit.separated_audio(SeparatedAudio::Vocals).unwrap(),
            vocals.canonicalize().unwrap()
        );
        assert!(edit.separated_audio(SeparatedAudio::Accompaniment).is_err());
        fs::write(&accompaniment, [0; 48]).unwrap();
        assert_eq!(
            edit.separated_audio(SeparatedAudio::Accompaniment).unwrap(),
            accompaniment.canonicalize().unwrap()
        );
        fs::write(manifest, json!({"key": "epic/other"}).to_string()).unwrap();
        assert!(edit.separated_audio(SeparatedAudio::Vocals).is_err());
    }

    #[test]
    fn valid_edits_preserve_metadata_refresh_exports_and_reject_stale_writes() {
        let temporary = tempfile::tempdir().unwrap();
        let subtitles = temporary.path().join("subtitles");
        let key = "史诗/海风";
        let hash = key_hash(key);
        let separation = temporary
            .path()
            .join(format!("separations/海风-{hash:016x}"));
        fs::create_dir_all(&subtitles).unwrap();
        fs::create_dir_all(&separation).unwrap();
        let srt = separation.join("subtitles.srt");
        let outside = temporary.path().join("subtitles.lrc");
        fs::write(&srt, "原始 SRT").unwrap();
        fs::write(&outside, "其他目录").unwrap();
        let record = subtitles.join(format!("海风-{hash:016x}.json"));
        fs::write(
            &record,
            json!({
                "key": key, "title": "海风", "createdAtMs": 10, "durationMs": 5000,
                "segments": [{"start": 0.2, "end": 1.2, "text": "原词"}],
                "srtPath": srt, "lrcPath": outside, "futureMetadata": {"retained": true},
            })
            .to_string(),
        )
        .unwrap();
        let editor = SubtitleEdit::open(&subtitles, key).unwrap();
        let stale = SubtitleEdit::open(&subtitles, key).unwrap();
        assert_eq!(
            editor
                .delivery_path(SubtitleDelivery::Text(SubtitleFormat::Srt))
                .unwrap(),
            srt.canonicalize().unwrap()
        );
        assert!(
            editor
                .delivery_path(SubtitleDelivery::Text(SubtitleFormat::Lrc))
                .is_err()
        );
        let before = fs::read(&record).unwrap();
        assert!(
            editor
                .save(SubtitleFormat::Srt, "00:04,000 --> 00:01,000\n无效", None)
                .is_err()
        );
        assert_eq!(fs::read(&record).unwrap(), before);
        assert_eq!(fs::read_to_string(&srt).unwrap(), "原始 SRT");

        editor
            .save(SubtitleFormat::Lrc, "[00:00.500]新词", Some(8000))
            .unwrap();
        let edited: Value = read_record(&record).unwrap();
        assert_eq!(edited["createdAtMs"], 10);
        assert!(edited["updatedAtMs"].as_u64().unwrap() > 10);
        assert_eq!(edited["futureMetadata"]["retained"], true);
        assert_eq!(edited["segments"][0]["end"], 5.0);
        assert_eq!(
            edited["srt"].as_str().unwrap(),
            fs::read_to_string(&srt).unwrap()
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "其他目录");
        assert!(
            stale
                .save(SubtitleFormat::Lrc, "[00:01.000]旧编辑", None)
                .is_err()
        );

        let output = temporary.path().join("导出.lrc");
        let fresh = SubtitleEdit::open(&subtitles, key).unwrap();
        export_subtitle(&output, &fresh.text(SubtitleFormat::Lrc)).unwrap();
        assert_eq!(fs::read_to_string(output).unwrap(), "[00:00.500]新词\n");
    }

    #[test]
    fn failed_final_replace_restores_previous_derived_outputs() {
        let temporary = tempfile::tempdir().unwrap();
        let existing = temporary.path().join("subtitles.srt");
        let missing = temporary.path().join("subtitles.lrc");
        let blocked = temporary.path().join("record.json");
        fs::write(&existing, "旧字幕").unwrap();
        fs::create_dir(&blocked).unwrap();
        let writes = vec![
            StagedWrite::new(existing.clone(), b"new", true).unwrap(),
            StagedWrite::new(missing.clone(), b"new", true).unwrap(),
            StagedWrite::new(blocked.clone(), b"new", false).unwrap(),
        ];
        assert!(commit_writes(writes).is_err());
        assert_eq!(fs::read_to_string(existing).unwrap(), "旧字幕");
        assert!(!missing.exists());
        assert!(blocked.is_dir());
    }
}
