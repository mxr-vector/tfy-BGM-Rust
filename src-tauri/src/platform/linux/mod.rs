//! Linux（X11 / Wayland）专有实现。

pub mod autostart;
pub mod fonts;
pub mod gpu;
pub mod media;
pub mod session;

use std::path::{Path, PathBuf};

pub const NAME: &str = "linux";

/// 配置目录：`XDG_CONFIG_HOME`，缺省 `~/.config`，与 Tauri 的目录约定一致。
pub fn config_dir(identifier: &str) -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|base| base.join(identifier))
}

/// Linux 文件系统通常大小写敏感，按原始字节比较。
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
