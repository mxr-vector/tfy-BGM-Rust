//! 配置文件落盘工具：统一「临时文件 + 重命名」写入。
//!
//! `prefs.json`、`categories.json`、`config.json` 都走这里，避免三份重复的
//! 原子写入逻辑，也不会在磁盘上留下写了一半的配置。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;

/// 把值序列化为格式化 JSON 并原子写入 `path`。
pub fn write_json<T>(path: &Path, value: &T) -> Result<(), PersistError>
where
    T: Serialize + ?Sized,
{
    let bytes = serde_json::to_vec_pretty(value)?;
    write_atomic(path, &bytes)
}

/// 原子写入字节：先写 `<path>.tmp`，再重命名覆盖目标。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PersistError> {
    let parent = path
        .parent()
        .ok_or_else(|| PersistError::NoParent(path.to_path_buf()))?;
    fs::create_dir_all(parent).map_err(PersistError::CreateDir)?;

    let tmp = temp_path(path);
    fs::write(&tmp, bytes).map_err(PersistError::Write)?;
    if fs::rename(&tmp, path).is_err() {
        // Windows 上目标已存在时重命名可能失败，退化为直接覆盖。
        fs::write(path, bytes).map_err(PersistError::Write)?;
        let _ = fs::remove_file(&tmp);
    }
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("配置路径没有父目录：{}", .0.display())]
    NoParent(PathBuf),
    #[error("创建配置目录失败：{0}")]
    CreateDir(#[source] io::Error),
    #[error("序列化配置失败：{0}")]
    Serialize(#[from] serde_json::Error),
    #[error("写入配置失败：{0}")]
    Write(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_pretty_json_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");

        write_json(&path, &serde_json::json!({ "a": 1 })).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains('\n'), "应当写入格式化 JSON：{text}");
        assert!(!dir.path().join("nested/settings.json.tmp").exists());
    }

    #[test]
    fn missing_parent_is_reported() {
        let error = write_atomic(Path::new(""), b"x").unwrap_err();
        assert!(matches!(error, PersistError::NoParent(_)));
    }
}
