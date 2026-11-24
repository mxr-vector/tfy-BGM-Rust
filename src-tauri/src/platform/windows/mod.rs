//! Windows（WebView2）专有实现。

pub mod autostart;
pub mod fonts;
pub mod gpu;

use std::path::{Path, PathBuf};

pub const NAME: &str = "windows";

/// 配置目录：`%APPDATA%\<identifier>`，与 Tauri 的目录约定一致。
pub fn config_dir(identifier: &str) -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|base| base.join(identifier))
}

/// Windows 文件系统大小写不敏感，去重键折叠为小写。
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}
