//! 系统字体来源：系统字体目录、当前用户字体目录，以及注册表登记的字体文件。
//!
//! 注册表里有位于字体目录之外的条目（如 Office 自带字体），只扫目录会漏掉。

use std::path::PathBuf;

use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

use crate::platform::FontSources;

const FONT_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";

pub fn sources() -> FontSources {
    let mut dirs = vec![fonts_dir()];
    // 仅当前用户安装的字体（Windows 10 1809+ 的「为当前用户安装」）。
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
    }
    FontSources {
        dirs,
        files: registry_files(),
    }
}

fn fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("Fonts"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"))
}

fn registry_files() -> Vec<PathBuf> {
    let fallback = fonts_dir();
    let mut files = Vec::new();
    for root in [LOCAL_MACHINE, CURRENT_USER] {
        let Ok(key) = root.open(FONT_KEY) else {
            continue;
        };
        let Ok(values) = key.values() else {
            continue;
        };
        for (_, value) in values {
            let raw = String::from_utf16_lossy(value.as_wide());
            let raw = raw.trim_end_matches('\0').trim();
            if raw.is_empty() {
                continue;
            }
            let path = PathBuf::from(raw);
            files.push(if path.is_absolute() {
                path
            } else {
                fallback.join(path)
            });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirs_cover_system_and_user_fonts() {
        let dirs = sources().dirs;
        assert!(
            dirs.iter().any(|dir| dir.ends_with("Fonts")),
            "缺少系统字体目录：{dirs:?}"
        );
        if std::env::var_os("LOCALAPPDATA").is_some() {
            assert!(
                dirs.iter()
                    .any(|dir| dir.ends_with(r"Microsoft\Windows\Fonts")),
                "缺少当前用户字体目录：{dirs:?}"
            );
        }
    }
}
