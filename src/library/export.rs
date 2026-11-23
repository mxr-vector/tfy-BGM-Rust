//! GPUI 单曲和批量导出的格式校验与文件落盘。

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use bgm_core::export::{ExportOutcome, ExportSettings, copy_validated, safe_file_name};
use bgm_core::library::audio_extension;
use bgm_core::loudness::{self, NORMALIZED_EXTENSION};
use bgm_core::{BatchReport, Song};
use tempfile::{NamedTempFile, TempPath};

pub struct Exporter {
    settings: ExportSettings,
    ffmpeg: Option<PathBuf>,
}

impl Exporter {
    pub fn new(settings: ExportSettings) -> Self {
        let ffmpeg = settings.normalize.then(loudness::ffmpeg_path).flatten();
        Self { settings, ffmpeg }
    }

    pub fn suggested_name(&self, song: &Song) -> Result<String, String> {
        let extension = self.extension(Path::new(&song.path))?;
        Ok(format!("{}.{extension}", safe_file_name(&song.title)))
    }

    pub(super) fn extension(&self, source: &Path) -> Result<&'static str, String> {
        let source_extension = source_extension(source)?;
        if self.ffmpeg.is_some() {
            Ok(NORMALIZED_EXTENSION)
        } else {
            Ok(source_extension)
        }
    }

    pub fn single(&self, source: &Path, destination: &Path) -> Result<String, String> {
        if source == destination
            || fs::canonicalize(source)
                .ok()
                .zip(fs::canonicalize(destination).ok())
                .is_some_and(|(source, destination)| source == destination)
        {
            return Err("导出目标与源文件相同，已取消导出".into());
        }
        let directory = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let extension = destination.extension().ok_or("请选择导出文件的扩展名")?;
        let (temporary, outcome) = self.stage(source, directory, extension)?;
        temporary
            .persist(destination)
            .map_err(|error| format!("写入导出文件失败：{}", error.error))?;
        let mut message = format!("已导出到 {}", destination.display());
        if let Some(warning) = outcome.warning {
            message.push_str(&format!("（{warning}）"));
        } else if outcome.normalized {
            message.push_str("（已响度归一化）");
        }
        Ok(message)
    }

    pub fn batch(&self, songs: &[Song], directory: &Path) -> Result<BatchReport, String> {
        let existing = directory
            .read_dir()
            .map_err(|error| format!("无法读取导出目录 {}：{error}", directory.display()))?;
        let mut stems = HashSet::new();
        for entry in existing {
            let entry = entry.map_err(|error| format!("无法读取导出目录：{error}"))?;
            if let Some(stem) = entry.path().file_stem() {
                stems.insert(stem.to_string_lossy().to_lowercase());
            }
        }
        let mut report = BatchReport {
            total: songs.len(),
            ..Default::default()
        };
        let mut normalized = 0;
        let mut first_warning = None;
        for song in songs {
            match self.batch_file(song, directory, &mut stems) {
                Ok(outcome) => {
                    report.succeeded += 1;
                    normalized += usize::from(outcome.normalized);
                    first_warning = first_warning.or(outcome.warning);
                    report.messages.push(format!("已导出：{}", song.title));
                }
                Err(error) => {
                    report.failed += 1;
                    report.failures.push(format!("{}：{error}", song.title));
                }
            }
        }

        if report.succeeded > 0 {
            let detail = if normalized == report.succeeded {
                format!(
                    "（响度归一化 · 目标 {} LUFS）",
                    self.settings.preset.target_lufs
                )
            } else if normalized > 0 {
                format!("（其中 {normalized} 首完成响度归一化）")
            } else {
                String::new()
            };
            let mut notice = format!(
                "已导出 {} 首{detail}到：{}",
                report.succeeded,
                directory.display()
            );
            if let Some(warning) = first_warning {
                notice.push_str(&format!("；{warning}"));
            }
            report.notice = Some(notice);
        }
        Ok(report)
    }

    fn batch_file(
        &self,
        song: &Song,
        directory: &Path,
        occupied: &mut HashSet<String>,
    ) -> Result<ExportOutcome, String> {
        let source = Path::new(&song.path);
        let extension = self.extension(source)?;
        let (mut temporary, outcome) = self.stage(source, directory, OsStr::new(extension))?;
        let stem = safe_file_name(&song.title);
        let shortened: String = stem.chars().take(100).collect();
        let mut number = 1_u64;
        loop {
            let candidate = if number == 1 {
                stem.clone()
            } else {
                format!("{shortened} ({number})")
            };
            number += 1;
            let folded = candidate.to_lowercase();
            if occupied.contains(&folded) {
                continue;
            }
            let destination = directory.join(format!("{candidate}.{extension}"));
            match temporary.persist_noclobber(&destination) {
                Ok(()) => {
                    occupied.insert(folded);
                    return Ok(outcome);
                }
                Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                    occupied.insert(folded);
                    temporary = error.path;
                }
                Err(error) => return Err(format!("写入导出文件失败：{}", error.error)),
            }
        }
    }

    fn stage(
        &self,
        source: &Path,
        directory: &Path,
        extension: &OsStr,
    ) -> Result<(TempPath, ExportOutcome), String> {
        if !source.is_file() {
            return Err(format!("源音频不存在或不可读取：{}", source.display()));
        }
        let original_extension = source_extension(source)?;
        let same_format = extension.eq_ignore_ascii_case(original_extension);
        let skip = if !self.settings.normalize {
            Some("已关闭响度归一化")
        } else if self.ffmpeg.is_none() {
            Some("未检测到 ffmpeg，响度归一化已跳过")
        } else if !extension.eq_ignore_ascii_case(NORMALIZED_EXTENSION) {
            Some("响度归一化仅输出 MP3，已保留原格式")
        } else {
            None
        };
        if let Some(reason) = skip
            && !same_format
        {
            return Err(format!(
                "无法导出为所选格式：{reason}。请选择 .{original_extension}"
            ));
        }

        let temporary = NamedTempFile::new_in(directory)
            .map_err(|error| format!("无法创建导出文件：{error}"))?
            .into_temp_path();
        let warning = match (skip, self.ffmpeg.as_deref()) {
            (None, Some(ffmpeg)) => {
                match loudness::normalize_file(ffmpeg, source, &temporary, self.settings.preset) {
                    Ok(()) => None,
                    Err(error) if same_format => {
                        Some(format!("响度归一化失败，已按原文件导出：{error}"))
                    }
                    Err(error) => {
                        return Err(format!(
                            "响度归一化失败：{error}。可另存为 .{original_extension}"
                        ));
                    }
                }
            }
            (reason, _) => reason.map(str::to_owned),
        };
        let normalized = warning.is_none();
        let bytes = if normalized {
            fs::metadata(&temporary)
                .map_err(|error| format!("无法读取导出文件：{error}"))?
                .len()
        } else {
            copy_validated(source, &temporary).map_err(|error| error.to_string())?
        };
        Ok((
            temporary,
            ExportOutcome {
                bytes,
                normalized,
                warning,
            },
        ))
    }
}

fn source_extension(source: &Path) -> Result<&'static str, String> {
    source
        .file_name()
        .and_then(OsStr::to_str)
        .and_then(audio_extension)
        .ok_or_else(|| "源文件扩展名不受支持".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Library;
    use bgm_core::LibrarySource;
    use std::process::Command;

    fn song_in(directory: &Path, extension: &str) -> Song {
        let file_name = format!("input.{extension}");
        Song {
            key: extension.into(),
            title: "tone".into(),
            path: directory.join(&file_name).to_str().unwrap().into(),
            file_name,
            category: "epic".into(),
            artist: None,
            album: None,
            genre: None,
            duration_ms: None,
        }
    }

    #[test]
    fn missing_ffmpeg_copies_all_original_formats() {
        let directory = tempfile::tempdir().unwrap();
        let exporter = Exporter {
            settings: ExportSettings::default(),
            ffmpeg: None,
        };
        for extension in ["mp3", "wav", "flac", "ogg", "m4a"] {
            let source = directory.path().join(format!("input.{extension}"));
            let target = directory
                .path()
                .join(format!("output.{}", extension.to_uppercase()));
            fs::write(&source, extension.as_bytes()).unwrap();
            assert_eq!(exporter.extension(&source).unwrap(), extension);
            let notice = exporter.single(&source, &target).unwrap();
            assert!(notice.contains("未检测到 ffmpeg"));
            assert_eq!(fs::read(target).unwrap(), extension.as_bytes());
            assert!(exporter.single(&source, &source).is_err());
        }
        let source = directory.path().join("input.wav");
        let wrong = directory.path().join("wrong.mp3");
        assert!(exporter.single(&source, &wrong).is_err());
        assert!(!wrong.exists());
    }

    #[cfg(unix)]
    #[test]
    fn failed_encode_preserves_existing_targets_and_falls_back_only_on_same_format() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let fake = directory.path().join("ffmpeg");
        fs::write(&fake, br#"#!/bin/sh
for last do :; done
if [ "$last" = - ]; then
    printf '%s\n' '{"input_i":"-24.0","input_tp":"-2.0","input_lra":"7.0","input_thresh":"-36.0","target_offset":"0.0"}' >&2
else
    printf partial > "$last"
    exit 17
fi
"#).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
        let exporter = Exporter {
            settings: ExportSettings::default(),
            ffmpeg: Some(fake),
        };
        let mp3 = directory.path().join("input.mp3");
        let flac = directory.path().join("input.flac");
        fs::write(&mp3, b"original mp3").unwrap();
        fs::write(&flac, b"original flac").unwrap();
        let target = directory.path().join("saved.mp3");
        fs::write(&target, b"keep this").unwrap();
        assert!(
            exporter
                .single(&flac, &target)
                .unwrap_err()
                .contains("响度归一化失败")
        );
        assert_eq!(fs::read(&target).unwrap(), b"keep this");
        assert!(
            exporter
                .single(&mp3, &target)
                .unwrap()
                .contains("已按原文件导出")
        );
        assert_eq!(fs::read(&target).unwrap(), b"original mp3");

        let same = directory.path().join("saved.flac");
        assert!(
            exporter
                .single(&flac, &same)
                .unwrap()
                .contains("已保留原格式")
        );
        assert_eq!(fs::read(&same).unwrap(), b"original flac");
        assert_eq!(directory.path().read_dir().unwrap().count(), 5);
    }

    #[test]
    fn batch_numbers_case_insensitive_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("TONE.mp3"), b"unrelated").unwrap();
        let songs: Vec<_> = ["wav", "m4a"]
            .into_iter()
            .map(|extension| {
                let song = song_in(directory.path(), extension);
                fs::write(&song.path, extension.as_bytes()).unwrap();
                song
            })
            .collect();
        let exporter = Exporter::new(ExportSettings::from_prefs(false, ""));
        let report = exporter.batch(&songs, &output).unwrap();
        assert_eq!((report.succeeded, report.failed), (2, 0));
        assert_eq!(fs::read(output.join("TONE.mp3")).unwrap(), b"unrelated");
        assert_eq!(fs::read(output.join("tone (2).wav")).unwrap(), b"wav");
        assert_eq!(fs::read(output.join("tone (3).m4a")).unwrap(), b"m4a");
        assert_eq!(output.read_dir().unwrap().count(), 3);
    }

    #[test]
    fn scanned_tracks_export_in_all_five_formats_with_canonical_extensions() {
        let exporter = Exporter::new(ExportSettings::default());
        let Some(ffmpeg) = &exporter.ffmpeg else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("library");
        let category = root.join("epic");
        let copied = directory.path().join("copied");
        let normalized = directory.path().join("normalized");
        let singles = directory.path().join("singles");
        fs::create_dir_all(&category).unwrap();
        fs::create_dir(&copied).unwrap();
        fs::create_dir(&normalized).unwrap();
        fs::create_dir(&singles).unwrap();
        let formats = [
            ("mp3", "libmp3lame"),
            ("wav", "pcm_s16le"),
            ("flac", "flac"),
            ("ogg", "vorbis"),
            ("m4a", "aac"),
        ];
        for (extension, codec) in formats {
            let path = category.join(format!(
                "曲目_{extension}.{}",
                extension.to_ascii_uppercase()
            ));
            let encoded = Command::new(ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=997:sample_rate=48000:duration=4",
                    "-ac",
                    "2",
                    "-c:a",
                    codec,
                    "-strict",
                    "experimental",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                encoded.status.success(),
                "{extension}: {}",
                String::from_utf8_lossy(&encoded.stderr)
            );
        }
        let mut library = Library::open_in(directory.path().join("config"), |_| {
            (root.clone(), LibrarySource::Custom, vec![])
        });
        assert_eq!(library.snapshot.songs.len(), formats.len());
        let keys = formats.map(|(extension, _)| format!("epic/曲目_{extension}"));
        for (extension, _) in formats {
            let song = library.song(&format!("epic/曲目_{extension}")).unwrap();
            assert_eq!(
                exporter.suggested_name(song).unwrap(),
                format!("曲目 {extension}.mp3")
            );
        }
        let report = library
            .prepare_batch_export(&keys)
            .unwrap()
            .run(&normalized)
            .unwrap();
        assert_eq!((report.succeeded, report.failed), (5, 0), "{report:?}");
        assert!(report.notice.unwrap().contains("目标 -14 LUFS"));

        library.set_export_settings(false, "").unwrap();
        let report = library
            .prepare_batch_export(&keys)
            .unwrap()
            .run(&copied)
            .unwrap();
        assert_eq!((report.succeeded, report.failed), (5, 0), "{report:?}");
        let original = Exporter::new(library.export_settings());
        for (extension, _) in formats {
            let song = library.song(&format!("epic/曲目_{extension}")).unwrap();
            let name = format!("曲目 {extension}.{extension}");
            assert_eq!(original.suggested_name(song).unwrap(), name);
            let source = fs::read(&song.path).unwrap();
            let destination = singles.join(&name);
            original
                .single(Path::new(&song.path), &destination)
                .unwrap();
            assert_eq!(fs::read(destination).unwrap(), source);
            assert_eq!(fs::read(copied.join(name)).unwrap(), source);
            assert!(
                fs::read(normalized.join(format!("曲目 {extension}.mp3")))
                    .unwrap()
                    .starts_with(b"ID3")
            );
        }
    }
}
