//! 音频导出：复制到用户选择的位置，并校验落盘字节数与源文件一致。
//!
//! 开启响度归一化且目标为 mp3 时先经 ffmpeg 两遍 `loudnorm` 处理；ffmpeg 缺失、
//! 目标不是 mp3 或处理失败时回退为原格式复制，并在结果里说明原因。
//! 源与目标扩展名不同又无法归一化时直接报错，避免把错误格式的字节写进导出文件。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::limits::MAX_STEM_CHARS;
use crate::loudness::{self, DEFAULT_PRESET_ID, ExportPreset, preset_or_default};

/// 一次导出使用的响度设置；`preset` 是本次归一化的目标，未选场景时使用默认目标。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportSettings {
    pub normalize: bool,
    pub preset: ExportPreset,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            normalize: true,
            preset: preset_or_default(DEFAULT_PRESET_ID),
        }
    }
}

impl ExportSettings {
    /// 用偏好里的开关与场景 id 构造设置；空字符串或未知场景回退为默认目标。
    pub fn from_prefs(normalize: bool, scene: &str) -> Self {
        Self {
            normalize,
            preset: preset_or_default(scene),
        }
    }
}

/// 单个文件的导出结果：是否完成归一化，以及需要提示用户的回退原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOutcome {
    pub bytes: u64,
    pub normalized: bool,
    pub warning: Option<String>,
}

/// 按设置导出单个音频；归一化不可用时自动降级为原文件复制。
pub fn export_audio(
    source: &Path,
    destination: &Path,
    settings: ExportSettings,
) -> Result<ExportOutcome, ExportError> {
    let ffmpeg = settings.normalize.then(loudness::ffmpeg_path).flatten();
    export_with_ffmpeg(source, destination, settings, ffmpeg.as_deref())
}

/// 批量导出时由调用方只探测一次 ffmpeg，再逐首复用。
pub(crate) fn export_with_ffmpeg(
    source: &Path,
    destination: &Path,
    settings: ExportSettings,
    ffmpeg: Option<&Path>,
) -> Result<ExportOutcome, ExportError> {
    let metadata = fs::metadata(source).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ExportError::SourceMissing(source.to_owned()),
        _ => ExportError::Read(error),
    })?;
    if !metadata.is_file() {
        return Err(ExportError::SourceMissing(source.to_owned()));
    }
    if source == destination || same_file(source, destination) {
        return Err(ExportError::SameFile);
    }

    if !settings.normalize {
        return copy_same_format(
            source,
            destination,
            None,
            "未开启响度归一化，只能按原格式复制",
        );
    }

    let Some(ffmpeg) = ffmpeg else {
        return copy_same_format(
            source,
            destination,
            Some("未检测到 ffmpeg，响度归一化已跳过".to_owned()),
            "未检测到 ffmpeg，无法转码",
        );
    };

    // loudnorm 固定输出 mp3：目标扩展名是其它容器时不能直接写入。
    // 与源同格式时退回复制并提示未归一化，否则明确报错（例如 flac → wav）。
    if extension_of(destination).as_deref() != Some(loudness::NORMALIZED_EXTENSION) {
        return copy_same_format(
            source,
            destination,
            Some("目标扩展名不是 mp3，已按原格式复制（未做响度归一化）".to_owned()),
            "响度归一化固定输出 mp3",
        );
    }

    match loudness::normalize_file(ffmpeg, source, destination, settings.preset) {
        Ok(()) => {
            let bytes = fs::metadata(destination).map_err(ExportError::Read)?.len();
            Ok(ExportOutcome {
                bytes,
                normalized: true,
                warning: None,
            })
        }
        Err(error) => {
            let _ = fs::remove_file(destination);
            // 归一化失败时回退为复制；只有目标扩展名与源一致才安全，
            // 否则会把 flac / wav 的字节写进 .mp3 文件，反而产生损坏素材。
            let warning = format!("响度归一化失败，已按原格式复制：{error}");
            copy_same_format(
                source,
                destination,
                Some(warning),
                &format!("响度归一化失败（{error}），且目标扩展名与源文件不一致"),
            )
        }
    }
}

/// 按原格式复制；格式不一致时把具体原因带进错误，避免只看到笼统的「格式不一致」。
fn copy_same_format(
    source: &Path,
    destination: &Path,
    warning: Option<String>,
    reason: &str,
) -> Result<ExportOutcome, ExportError> {
    ensure_same_format(source, destination, reason)?;
    let bytes = copy_validated(source, destination)?;
    Ok(ExportOutcome {
        bytes,
        normalized: false,
        warning,
    })
}

/// 未归一化时只能原样复制，目标扩展名必须与源文件一致。
fn ensure_same_format(source: &Path, destination: &Path, reason: &str) -> Result<(), ExportError> {
    if extension_of(source) != extension_of(destination) {
        return Err(ExportError::FormatMismatch {
            source_file: source.to_path_buf(),
            destination_file: destination.to_path_buf(),
            reason: reason.to_owned(),
        });
    }
    Ok(())
}

fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

/// 复制源音频到目标路径，成功后返回复制字节数。
/// 大小校验失败时会删除目标文件，避免留下不完整的音频。
fn same_file(source: &Path, destination: &Path) -> bool {
    match (fs::canonicalize(source), fs::canonicalize(destination)) {
        (Ok(source), Ok(destination)) => source == destination,
        _ => false,
    }
}

pub fn copy_validated(source: &Path, destination: &Path) -> Result<u64, ExportError> {
    let metadata = fs::metadata(source).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => ExportError::SourceMissing(source.to_owned()),
        _ => ExportError::Read(error),
    })?;
    if !metadata.is_file() {
        return Err(ExportError::SourceMissing(source.to_owned()));
    }
    if source == destination || same_file(source, destination) {
        return Err(ExportError::SameFile);
    }

    let copied = fs::copy(source, destination).map_err(ExportError::Write)?;
    if copied != metadata.len() {
        let _ = fs::remove_file(destination);
        return Err(ExportError::SizeMismatch {
            expected: metadata.len(),
            actual: copied,
        });
    }
    Ok(copied)
}

/// 把曲名收敛为跨平台安全的导出文件名（不含扩展名）。
pub fn safe_file_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "audio".to_owned()
    } else {
        trimmed.chars().take(MAX_STEM_CHARS).collect()
    }
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("源音频不存在或不是文件：{}", .0.display())]
    SourceMissing(PathBuf),
    #[error("导出目标与源文件相同，已取消导出")]
    SameFile,
    #[error(
        "导出目标扩展名必须与源文件一致（源 {}，目标 {}）：{}",
        .source_file.display(),
        .destination_file.display(),
        .reason
    )]
    FormatMismatch {
        source_file: PathBuf,
        destination_file: PathBuf,
        reason: String,
    },
    #[error("读取源音频失败：{0}")]
    Read(#[source] io::Error),
    #[error("写入导出文件失败：{0}")]
    Write(#[source] io::Error),
    #[error("导出文件大小校验失败（期望 {expected} 字节，实际 {actual} 字节）")]
    SizeMismatch { expected: u64, actual: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_normalization_targets_video_preset_and_scene_overrides_it() {
        let defaults = ExportSettings::default();
        assert!(defaults.normalize);
        assert_eq!(defaults.preset.id, DEFAULT_PRESET_ID);

        let base = ExportSettings::from_prefs(true, "");
        assert_eq!(base.preset.target_lufs, -14.0);

        let focus = ExportSettings::from_prefs(true, "focus");
        assert_eq!(focus.preset.target_lufs, -20.0);

        let disabled = ExportSettings::from_prefs(false, "focus");
        assert!(!disabled.normalize);
    }

    #[test]
    fn copies_file_and_matches_size() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.mp3");
        let destination = dir.path().join("export.mp3");
        fs::write(&source, vec![7_u8; 4096]).unwrap();

        let copied = copy_validated(&source, &destination).unwrap();

        assert_eq!(copied, 4096);
        assert_eq!(fs::read(&destination).unwrap(), vec![7_u8; 4096]);
    }

    #[test]
    fn safe_file_name_replaces_reserved_characters() {
        assert_eq!(
            safe_file_name(r#"a/b\c:d*e?f"g<h>i|j"#),
            "a_b_c_d_e_f_g_h_i_j"
        );
        assert_eq!(safe_file_name("  ..  "), "audio");
        assert_eq!(safe_file_name("  风月吻眉梢  "), "风月吻眉梢");
    }

    #[test]
    fn refuses_to_overwrite_source_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.mp3");
        fs::write(&source, b"keep-me").unwrap();

        let error = copy_validated(&source, &source).expect_err("same path must fail");

        assert!(matches!(error, ExportError::SameFile));
        assert_eq!(fs::read(&source).unwrap(), b"keep-me");
    }

    #[test]
    fn copy_fallback_rejects_extension_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.flac");
        let destination = dir.path().join("export.mp3");
        fs::write(&source, b"fake-flac").unwrap();

        let error = export_with_ffmpeg(
            &source,
            &destination,
            ExportSettings::from_prefs(false, ""),
            None,
        )
        .expect_err("mismatched extension must fail");

        assert!(matches!(error, ExportError::FormatMismatch { .. }));
        assert!(!destination.exists());
    }

    #[test]
    fn normalized_export_to_source_format_copies_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.flac");
        let destination = dir.path().join("export.flac");
        fs::write(&source, b"flac-bytes").unwrap();

        let outcome = export_with_ffmpeg(
            &source,
            &destination,
            ExportSettings::from_prefs(true, ""),
            // 目标不是 mp3 时不会真正调用 ffmpeg，用一个占位路径即可。
            Some(Path::new("ffmpeg-placeholder")),
        )
        .expect("same-format copy should succeed");

        assert!(!outcome.normalized);
        assert!(outcome.warning.is_some());
        assert_eq!(fs::read(&destination).unwrap(), b"flac-bytes");
    }

    #[test]
    fn normalized_export_to_other_format_reports_reason() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("song.flac");
        let destination = dir.path().join("export.wav");
        fs::write(&source, b"flac-bytes").unwrap();

        let error = export_with_ffmpeg(
            &source,
            &destination,
            ExportSettings::from_prefs(true, ""),
            Some(Path::new("ffmpeg-placeholder")),
        )
        .expect_err("flac to wav cannot be copied");

        match error {
            ExportError::FormatMismatch { reason, .. } => {
                assert!(reason.contains("mp3"), "{reason}");
            }
            other => panic!("期望 FormatMismatch，实际 {other:?}"),
        }
        assert!(!destination.exists());
    }

    #[test]
    fn missing_source_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let error = copy_validated(&dir.path().join("nope.mp3"), &dir.path().join("out.mp3"))
            .expect_err("missing source must fail");
        assert!(matches!(error, ExportError::SourceMissing(_)));
    }
}
