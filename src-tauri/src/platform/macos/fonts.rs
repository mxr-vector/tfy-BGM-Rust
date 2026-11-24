//! 系统字体目录：系统宗卷、本地宗卷与用户字体目录。

use std::path::PathBuf;

use crate::platform::FontSources;

pub fn sources() -> FontSources {
    let mut dirs: Vec<PathBuf> = [
        "/System/Library/Fonts/Supplemental",
        "/System/Library/Fonts",
        "/Library/Fonts",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("Library/Fonts"));
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
    fn dirs_cover_system_font_locations() {
        let dirs = sources().dirs;
        for expected in ["/System/Library/Fonts", "/Library/Fonts"] {
            assert!(
                dirs.iter().any(|dir| dir == Path::new(expected)),
                "缺少字体目录 {expected}：{dirs:?}"
            );
        }
    }
}
