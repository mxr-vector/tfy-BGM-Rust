//! 音频导出：复制到用户选择的位置，并校验落盘字节数与源文件一致。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::limits::MAX_STEM_CHARS;

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
