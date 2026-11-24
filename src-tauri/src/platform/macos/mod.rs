//! macOS（WKWebView）专有实现。

pub mod autostart;
pub mod fonts;
pub mod gpu;

use std::path::{Path, PathBuf};

pub const NAME: &str = "macos";

/// 配置目录：`~/Library/Application Support/<identifier>`，与 Tauri 的目录约定一致。
pub fn config_dir(identifier: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join(identifier)
    })
}

/// 默认宗卷（APFS）大小写不敏感，但区分大小写的宗卷确实存在；
/// 与旧行为保持一致，按原始字节比较。
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
