//! 音频导出：复制到用户选择的位置，并校验落盘字节数与源文件一致。
//!
//! 开启响度归一化时先经 ffmpeg 两遍 `loudnorm` 处理；ffmpeg 缺失或处理失败
//! 会自动回退为原文件复制，保证导出始终能完成。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::limits::MAX_STEM_CHARS;
use crate::loudness::{self, DEFAULT_PRESET_ID, ExportPreset, preset_or_default};

/// 一次导出使用的响度设置；`preset` 决定目标 LUFS / 真峰值 / LRA。
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
    /// 用偏好里的开关与场景 id 构造设置；未知场景回退默认预设。
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
        let bytes = copy_validated(source, destination)?;
        return Ok(ExportOutcome {
            bytes,
            normalized: false,
            warning: None,
        });
    }

    let Some(ffmpeg) = ffmpeg else {
        let bytes = copy_validated(source, destination)?;
        return Ok(ExportOutcome {
            bytes,
            normalized: false,
            warning: Some("未检测到 ffmpeg，响度归一化已跳过".to_owned()),
        });
    };

    match loudness::normalize_file(ffmpeg, source, destination, settings.preset) {
        Ok(()) => {
            let bytes = fs::metadata(destination)
                .map_err(ExportError::Read)?
                .len();
            Ok(ExportOutcome {
                bytes,
                normalized: true,
                warning: None,
            })
        }
        Err(error) => {
            let _ = fs::remove_file(destination);
            let bytes = copy_validated(source, destination)?;
            Ok(ExportOutcome {
                bytes,
                normalized: false,
                warning: Some(format!("响度归一化失败，已按原文件导出：{error}")),
            })
        }
    }
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
    fn missing_source_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let error = copy_validated(&dir.path().join("nope.mp3"), &dir.path().join("out.mp3"))
            .expect_err("missing source must fail");
        assert!(matches!(error, ExportError::SourceMissing(_)));
    }
}
