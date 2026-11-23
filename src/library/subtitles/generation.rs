//! 字幕生成和配套产物的落盘数据。

use std::fs::{self, File};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, ensure};
use bgm_core::Song;
use serde_json::{Map, Value, json};

use super::{
    SubtitleFormat, SubtitleSegment, find_subtitle, read_record, records::check_duplicate_records,
    separation_stem, text,
};
use crate::library::{AsrSession, separation};

#[derive(Clone, Copy)]
pub enum Stage {
    Separating,
    Normalizing,
    Transcribing { current: usize, total: usize },
    Saving,
}

impl Stage {
    pub fn label(self) -> String {
        match self {
            Self::Separating => "正在分离人声与伴奏…".into(),
            Self::Normalizing => "正在将人声转换为 16 kHz 单声道…".into(),
            Self::Transcribing { total: 0, .. } => "正在识别人声…".into(),
            Self::Transcribing { current, total } => format!("正在识别第 {current}/{total} 段…"),
            Self::Saving => "正在保存字幕…".into(),
        }
    }
}

pub struct Generated {
    separated: separation::Prepared,
    subtitles: PendingSubtitle,
}

impl Generated {
    pub fn publish(self, song: &Song) -> Result<usize> {
        let count = self.subtitles.segments.len();
        self.separated.publish_subtitle(song, self.subtitles)?;
        Ok(count)
    }
}

pub fn generate(
    request: separation::Request,
    song: Song,
    session: &AsrSession,
    progress: impl Fn(Stage),
) -> Result<Generated> {
    let directory = request.subtitles.clone();
    let snapshot = current_record(&directory, &song.key)?;
    ensure!(
        bgm_core::library::has_audio_extension(&song.file_name),
        "该音频格式不支持字幕生成"
    );
    let ffmpeg = bgm_core::loudness::ffmpeg_path().context("字幕生成需要 FFmpeg")?;
    progress(Stage::Separating);
    let separated = request.generate()?;
    let segments = transcribe(&ffmpeg, &separated.staged_vocals(), &progress, |wav| {
        session.transcribe_with_progress(wav, |current, total| {
            progress(Stage::Transcribing { current, total });
        })
    })?;
    Ok(Generated {
        separated,
        subtitles: PendingSubtitle {
            key: song.key,
            snapshot,
            segments,
        },
    })
}

fn transcribe(
    ffmpeg: &Path,
    vocals: &Path,
    progress: impl Fn(Stage),
    recognize: impl FnOnce(PathBuf) -> std::result::Result<Vec<SubtitleSegment>, String>,
) -> Result<Vec<SubtitleSegment>> {
    progress(Stage::Normalizing);
    let normalized = tempfile::Builder::new()
        .prefix(".subtitle-")
        .suffix(".wav")
        .tempfile_in(vocals.parent().context("找不到人声暂存目录")?)?;
    let output = Command::new(ffmpeg)
        .args(["-nostdin", "-y", "-v", "error", "-i"])
        .arg(vocals)
        .args([
            "-map",
            "0:a:0",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ])
        .arg(normalized.path())
        .stdout(Stdio::null())
        .output()
        .context("无法启动 FFmpeg 归一化人声")?;
    ensure!(
        output.status.success() && normalized.as_file().metadata()?.len() > 44,
        "人声归一化失败：{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    progress(Stage::Transcribing {
        current: 0,
        total: 0,
    });
    let mut segments = recognize(normalized.path().to_owned()).map_err(anyhow::Error::msg)?;
    text::validate(&segments).map_err(anyhow::Error::msg)?;
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    Ok(segments)
}

type Snapshot = Option<(PathBuf, Map<String, Value>)>;

fn current_record(directory: &Path, key: &str) -> Result<Snapshot> {
    find_subtitle(directory, key)
        .map_err(anyhow::Error::msg)?
        .map(|(path, _)| {
            let data = read_record(&path).map_err(anyhow::Error::msg)?;
            Ok((path, data))
        })
        .transpose()
}

pub(in crate::library) struct PendingSubtitle {
    key: String,
    snapshot: Snapshot,
    segments: Vec<SubtitleSegment>,
}

impl PendingSubtitle {
    pub(in crate::library) fn stage(
        self,
        directory: &Path,
        staging: &Path,
        destination: &Path,
        song: &Song,
        model_name: &str,
        accompaniment_model_name: &str,
    ) -> Result<StagedSubtitle> {
        ensure!(song.key == self.key, "曲目已变更，请重新生成字幕");
        ensure!(
            current_record(directory, &self.key)? == self.snapshot,
            "字幕已在生成期间修改，请重新生成"
        );
        let path = self
            .snapshot
            .as_ref()
            .map(|(path, _)| path.clone())
            .unwrap_or_else(|| directory.join(format!("{}.json", separation_stem(&self.key))));
        if self.snapshot.is_none() {
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
                Ok(_) => return Err(anyhow!("目标字幕路径已占用")),
            }
        } else {
            check_duplicate_records(directory, &self.key, &path).map_err(anyhow::Error::msg)?;
        }
        let srt = SubtitleFormat::Srt.render(&self.segments);
        let lrc = SubtitleFormat::Lrc.render_named(&song.title, &self.segments);
        fs::write(staging.join("subtitles.srt"), &srt)?;
        fs::write(staging.join("subtitles.lrc"), &lrc)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let json = json!({
            "key": self.key,
            "title": song.title,
            "fileName": song.file_name,
            "durationMs": song.duration_ms,
            "sourcePath": song.path,
            "createdAtMs": now,
            "segments": self.segments,
            "srt": srt,
            "lrc": lrc,
            "separationModelName": model_name,
            "accompanimentModelName": accompaniment_model_name,
            "accompanimentKind": "伴奏",
            "vocalsPath": destination.join("vocals.wav"),
            "accompanimentPath": destination.join("accompaniment.wav"),
            "srtPath": destination.join("subtitles.srt"),
            "lrcPath": destination.join("subtitles.lrc"),
        });
        let file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&file, &json)?;
        file.as_file().sync_all()?;
        Ok(StagedSubtitle {
            file,
            path,
            overwrite: self.snapshot.is_some(),
        })
    }
}

pub(in crate::library) struct StagedSubtitle {
    file: tempfile::NamedTempFile,
    path: PathBuf,
    overwrite: bool,
}

impl StagedSubtitle {
    pub(in crate::library) fn publish(self) -> Result<()> {
        let result = if self.overwrite {
            self.file.persist(self.path)
        } else {
            self.file.persist_noclobber(self.path)
        };
        result
            .map(|_: File| ())
            .map_err(|error| error.error)
            .context("无法提交字幕记录")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{SubtitleEdit, load_subtitle};

    #[test]
    fn normalizes_stereo_vocals_before_recognition_and_cleans_up_on_error() {
        let Some(ffmpeg) = bgm_core::loudness::ffmpeg_path() else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let vocals = directory.path().join("vocals.wav");
        assert!(
            Command::new(&ffmpeg)
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000:duration=0.1",
                    "-ac",
                    "2",
                    "-c:a",
                    "pcm_s16le"
                ])
                .arg(&vocals)
                .status()
                .unwrap()
                .success()
        );
        let input = std::cell::RefCell::new(None);
        let progress = std::cell::RefCell::new(Vec::new());
        let error = transcribe(
            &ffmpeg,
            &vocals,
            |stage| progress.borrow_mut().push(stage.label()),
            |wav| {
                let bytes = fs::read(&wav).unwrap();
                assert_eq!(&bytes[0..4], b"RIFF");
                assert_eq!(&bytes[8..16], b"WAVEfmt ");
                assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
                assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 1);
                assert_eq!(
                    u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
                    16_000
                );
                assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 16);
                *input.borrow_mut() = Some(wav);
                Err("模拟识别失败".into())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("模拟识别失败"));
        assert!(!input.into_inner().unwrap().exists());
        assert!(vocals.exists());
        assert_eq!(
            progress.into_inner(),
            [
                Stage::Normalizing.label(),
                Stage::Transcribing {
                    current: 0,
                    total: 0
                }
                .label()
            ]
        );
    }

    #[test]
    fn generated_records_round_trip_and_keep_later_edits() {
        let config = tempfile::tempdir().unwrap();
        let subtitles = config.path().join("subtitles");
        let staging = config.path().join("stems");
        fs::create_dir(&subtitles).unwrap();
        fs::create_dir(&staging).unwrap();
        let song = Song {
            key: "epic/source".into(),
            title: "测试歌曲".into(),
            file_name: "source.mp3".into(),
            category: "epic".into(),
            path: "/source.mp3".into(),
            artist: None,
            album: None,
            genre: None,
            duration_ms: Some(3000),
        };
        let line = SubtitleSegment {
            start: 0.25,
            end: 1.25,
            text: "测试字幕".into(),
        };
        let prepare = || PendingSubtitle {
            key: song.key.clone(),
            snapshot: current_record(&subtitles, &song.key).unwrap(),
            segments: vec![line.clone()],
        };
        prepare()
            .stage(&subtitles, &staging, &staging, &song, "UVR", "Inst HQ 3")
            .unwrap()
            .publish()
            .unwrap();
        assert_eq!(
            load_subtitle(&subtitles, &song.key).unwrap(),
            Some(vec![line.clone()])
        );
        let (path, stored) = current_record(&subtitles, &song.key).unwrap().unwrap();
        assert_eq!(
            path.file_name().unwrap(),
            format!("{}.json", separation_stem(&song.key)).as_str()
        );
        assert_eq!(stored["vocalsPath"], json!(staging.join("vocals.wav")));
        assert_eq!(
            stored["accompanimentPath"],
            json!(staging.join("accompaniment.wav"))
        );
        assert_eq!(stored["srtPath"], json!(staging.join("subtitles.srt")));
        assert_eq!(stored["lrcPath"], json!(staging.join("subtitles.lrc")));
        assert!(
            fs::read_to_string(staging.join("subtitles.srt"))
                .unwrap()
                .contains("00:00:00,250")
        );
        assert!(
            fs::read_to_string(staging.join("subtitles.lrc"))
                .unwrap()
                .starts_with("[ar:]\n[ti:测试歌曲]\n")
        );

        let stale = prepare();
        let editor = SubtitleEdit::open(&subtitles, &song.key).unwrap();
        let mut edited = line;
        edited.text = "手动修改".into();
        editor.save_segments(vec![edited.clone()]).unwrap();
        let original = fs::read(&path).unwrap();
        let attempted = stale.stage(&subtitles, &staging, &staging, &song, "UVR", "Inst HQ 3");
        assert!(attempted.is_err());
        assert_eq!(fs::read(path).unwrap(), original);
        assert_eq!(
            load_subtitle(&subtitles, &song.key).unwrap(),
            Some(vec![edited])
        );
    }
}
