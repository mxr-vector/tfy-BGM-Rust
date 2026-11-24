//! 未提供专用实现的平台：桌面集成能力不可用，但保证接口齐全、可编译。

use std::path::{Path, PathBuf};

use crate::platform::FontSources;

pub const NAME: &str = "other";

pub fn config_dir(_identifier: &str) -> Option<PathBuf> {
    None
}

pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub mod autostart {
    use anyhow::{Result, bail};

    pub fn set(_enabled: bool) -> Result<()> {
        bail!("当前平台不支持开机自动启动")
    }
}

pub mod gpu {
    pub fn apply<R: tauri::Runtime>(_context: &mut tauri::Context<R>, _enabled: bool) {}
}

pub mod fonts {
    use crate::platform::FontSources;

    pub fn sources() -> FontSources {
        FontSources::default()
    }
}
