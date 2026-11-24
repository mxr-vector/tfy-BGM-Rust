//! 系统字体目录：fontconfig 的规范目录 + XDG 数据目录 + 老式 X11 目录。

use std::path::PathBuf;

use crate::platform::FontSources;

pub fn sources() -> FontSources {
    let mut dirs = Vec::new();
    // fontconfig 的规范默认目录；发行版/字体包通常在其下再分子目录。
    for dir in ["/usr/share/fonts", "/usr/local/share/fonts"] {
        dirs.push(PathBuf::from(dir));
    }
    // 再按 XDG_DATA_DIRS 展开，覆盖 NixOS、Flatpak 导出等自定义前缀。
    if let Some(value) = std::env::var_os("XDG_DATA_DIRS") {
        for dir in std::env::split_paths(&value) {
            dirs.push(dir.join("fonts"));
        }
    }
    // 老式 X11 字体目录，部分发行版仍在使用。
    dirs.push(PathBuf::from("/usr/share/X11/fonts"));
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/share/fonts"));
        // fontconfig 仍然兼容的老目录。
        dirs.push(home.join(".fonts"));
    }
    // 自定义 XDG_DATA_HOME 时，fontconfig 也会读取 <data>/fonts（默认即 ~/.local/share）。
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data).join("fonts"));
    }
    FontSources {
        dirs,
        files: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn dirs_cover_fontconfig_defaults() {
        let dirs = sources().dirs;
        for expected in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            assert!(
                dirs.iter().any(|dir| dir == Path::new(expected)),
                "缺少字体目录 {expected}：{dirs:?}"
            );
        }
    }
}
