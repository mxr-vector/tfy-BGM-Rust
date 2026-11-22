//! 系统字体枚举。
//!
//! 字体选择不再维护硬编码清单，而是扫描操作系统已安装的字体文件，
//! 解析 name 表得到真实字体族名（含简体中文名），避免「选了没反应」：
//! 选项在列表里出现，就意味着本机确实装了这款字体。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;
use ttf_parser::{Face, name_id};

/// 参与解析的字体文件扩展名（ttc / otc 为字体集合，内部可能含多个字族）。
const FONT_EXTENSIONS: [&str; 4] = ["ttf", "ttc", "otf", "otc"];
/// 字体目录的最大递归深度：足够覆盖 Linux 的 `truetype/<family>/` 层级，同时兜住软链环。
const FONT_DIR_DEPTH: usize = 4;
/// 字体族名长度上限，过滤 name 表里的异常数据。
const MAX_FAMILY_LEN: usize = 96;

/// 一个可选字体族。
#[derive(Debug, Clone, Serialize)]
pub struct SystemFont {
    /// CSS `font-family` 使用的族名；优先英文名，兼容性最好。
    pub family: String,
    /// 界面展示名；优先简体中文名（如「微软雅黑」）。
    pub label: String,
    /// 是否含中文字形，用于把中文字体排在列表前面。
    pub cjk: bool,
}

/// 扫描系统字体；单个文件解析失败只跳过该文件，不影响整体结果。
///
/// 字体列表在进程内缓存：解析要读取并遍历全部字体文件，歌词设置窗每次打开
/// 都重新扫描会有明显延迟。运行期新装的字体重启应用后出现即可。
pub fn list() -> Vec<SystemFont> {
    static CACHE: OnceLock<Vec<SystemFont>> = OnceLock::new();
    CACHE.get_or_init(scan).clone()
}

fn scan() -> Vec<SystemFont> {
    let mut families: BTreeMap<String, SystemFont> = BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();

    for path in font_files() {
        if !seen.insert(path_key(&path)) {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        // 字体集合（.ttc/.otc）里每个 face 都可能是独立字族，例如 msyh.ttc
        // 同时包含「微软雅黑」与「Microsoft YaHei UI」。
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        for index in 0..count {
            let Ok(face) = Face::parse(&data, index) else {
                continue;
            };
            let Some(font) = describe(&face) else {
                continue;
            };
            match families.get_mut(&font.family.to_lowercase()) {
                Some(existing) => {
                    existing.cjk |= font.cjk;
                    // 已有条目只有英文名时，补上中文展示名。
                    if existing.label == existing.family && font.label != font.family {
                        existing.label = font.label;
                    }
                }
                None => {
                    families.insert(font.family.to_lowercase(), font);
                }
            }
        }
    }

    let mut fonts: Vec<SystemFont> = families.into_values().collect();
    // 中文界面下：能显示中文的字体优先，其中带中文名的更靠前，同组按名称排序。
    fonts.sort_by(|a, b| {
        b.cjk
            .cmp(&a.cjk)
            .then_with(|| has_cjk(&b.label).cmp(&has_cjk(&a.label)))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    fonts
}

/// 展示名是否含中文字符（用于把「微软雅黑」排在英文名的中文字体之前）。
fn has_cjk(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}'))
}

/// 从单个 face 的 name 表提取族名与展示名。
fn describe(face: &Face<'_>) -> Option<SystemFont> {
    let mut family = NameSet::default();
    let mut typographic = NameSet::default();

    for name in face.names() {
        if name.name_id != name_id::FAMILY && name.name_id != name_id::TYPOGRAPHIC_FAMILY {
            continue;
        }
        let Some(value) = name.to_string() else {
            continue;
        };
        let value = value.trim();
        // 竖排字体的族名以 @ 开头，不能直接写进 CSS。
        if value.is_empty() || value.starts_with('@') {
            continue;
        }
        let target = if name.name_id == name_id::TYPOGRAPHIC_FAMILY {
            &mut typographic
        } else {
            &mut family
        };
        target.add(classify(name.language_id), value);
    }

    // 有排版族名时优先使用：它对应「常规 + 字重」的完整字族
    // （如 Microsoft YaHei UI Light 的排版族名是 Microsoft YaHei UI）。
    let css = typographic
        .english()
        .or_else(|| family.english())
        .or_else(|| typographic.chinese())
        .or_else(|| family.chinese())?;
    let label = typographic
        .chinese()
        .or_else(|| family.chinese())
        .or_else(|| typographic.english())
        .or_else(|| family.english())?;

    if css.chars().count() > MAX_FAMILY_LEN {
        return None;
    }

    Some(SystemFont {
        family: css.to_owned(),
        label: label.to_owned(),
        cjk: face.glyph_index('中').is_some(),
    })
}

/// 同一个字族在不同语言下的名称。
#[derive(Default)]
struct NameSet {
    en: Option<String>,
    zh: Option<String>,
}

impl NameSet {
    fn add(&mut self, lang: Lang, value: &str) {
        let slot = match lang {
            Lang::Zh => &mut self.zh,
            Lang::En => &mut self.en,
        };
        if slot.is_none() {
            *slot = Some(value.to_owned());
        }
    }

    fn english(&self) -> Option<&str> {
        self.en.as_deref()
    }

    fn chinese(&self) -> Option<&str> {
        self.zh.as_deref()
    }
}

enum Lang {
    Zh,
    En,
}

/// name 记录的语言归类：Windows LCID 的中文（简繁、港澳、新加坡）视为中文，
/// 其余（Unicode 平台与英文 LCID）视为英文；其他语言的名称不参与命名，
/// 避免界面出现日文/韩文字族名。
fn classify(language_id: u16) -> Lang {
    match language_id {
        0x0804 | 0x1004 | 0x0404 | 0x0C04 | 0x1404 => Lang::Zh,
        _ => Lang::En,
    }
}

/// 待解析的字体文件；注册表项与各平台常见字体目录合并去重。
fn font_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut seen_dirs: HashSet<String> = HashSet::new();
    for dir in font_dirs() {
        // 不同来源可能指向同一目录（如 /usr/share/X11/fonts 是指向 fonts 的软链），只扫一次。
        let key = std::fs::canonicalize(&dir)
            .map(|path| path_key(&path))
            .unwrap_or_else(|_| path_key(&dir));
        if !seen_dirs.insert(key) {
            continue;
        }
        // 深度足以覆盖 /usr/share/fonts/truetype/<family>/<font>，并由 depth 兜住软链环。
        collect_dir(&dir, FONT_DIR_DEPTH, &mut files);
    }
    #[cfg(windows)]
    files.extend(registry_files());
    files
}

/// 各平台常见字体目录；不存在的目录会被 `collect_dir` 忽略。
///
/// 用 `cfg!` 而不是 `#[cfg]`，保证所有分支在各平台都参与类型检查。
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if cfg!(windows) {
        let fonts = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .map(|dir| dir.join("Fonts"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"));
        dirs.push(fonts);
        // 仅当前用户安装的字体（Windows 10 1809+ 的「为当前用户安装」）。
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
    }

    if cfg!(target_os = "macos") {
        for dir in [
            "/System/Library/Fonts/Supplemental",
            "/System/Library/Fonts",
            "/Library/Fonts",
        ] {
            dirs.push(PathBuf::from(dir));
        }
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
    }

    if cfg!(all(unix, not(target_os = "macos"))) {
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
    }

    dirs
}

/// Windows 注册表登记的字体文件：可能位于系统字体目录之外（如 Office 自带字体）。
#[cfg(windows)]
fn registry_files() -> Vec<PathBuf> {
    use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

    const FONT_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Fonts";
    let fonts_dir = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .map(|dir| dir.join("Fonts"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"));

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
                fonts_dir.join(path)
            });
        }
    }
    files
}

/// 递归收集字体文件；跟随符号链接（Linux 字体包常用软链），由深度限制避免链接环。
fn collect_dir(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.is_dir() {
            collect_dir(&path, depth - 1, out);
        } else if metadata.is_file() && is_font_file(&path) {
            out.push(path);
        }
    }
}

fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| FONT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// 去重键：Windows 路径大小写不敏感。
fn path_key(path: &Path) -> String {
    let key = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        key.to_lowercase()
    } else {
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_installed_fonts() {
        let fonts = list();
        // 桌面环境必然装有字体；无字体的容器环境允许为空，
        // 但只要存在字体文件，就必须能解析出合法条目。
        if font_files().iter().any(|path| path.is_file()) {
            assert!(!fonts.is_empty(), "存在字体文件却未解析出任何字体");
        }
        assert!(fonts.iter().all(|font| {
            !font.family.is_empty()
                && !font.label.is_empty()
                && font.family.chars().count() <= MAX_FAMILY_LEN
                && !font.family.starts_with('@')
        }));
    }

    #[test]
    fn font_dirs_cover_platform_defaults() {
        let dirs = font_dirs();
        if cfg!(windows) {
            assert!(
                dirs.iter().any(|dir| dir.ends_with("Fonts")),
                "缺少 Windows 系统字体目录"
            );
        }
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert!(
                dirs.iter().any(|dir| dir == Path::new("/usr/share/fonts")),
                "缺少 Linux 字体目录"
            );
            assert!(
                dirs.iter()
                    .any(|dir| dir == Path::new("/usr/local/share/fonts"))
            );
        }
        if cfg!(target_os = "macos") {
            assert!(dirs.iter().any(|dir| dir == Path::new("/Library/Fonts")));
        }
    }

    #[test]
    fn font_extension_detection_is_case_insensitive() {
        assert!(is_font_file(Path::new("/usr/share/fonts/a.TTF")));
        assert!(is_font_file(Path::new(r"C:\Windows\Fonts\msyh.ttc")));
        assert!(is_font_file(Path::new("font.otf")));
        assert!(is_font_file(Path::new("font.OTC")));
        // 位图字体与备份文件不参与解析。
        assert!(!is_font_file(Path::new("vga.fon")));
        assert!(!is_font_file(Path::new("font.ttf.bak")));
        assert!(!is_font_file(Path::new("font")));
    }

    #[test]
    fn cjk_label_detection() {
        assert!(has_cjk("微软雅黑"));
        assert!(has_cjk("微軟正黑體"));
        assert!(!has_cjk("Microsoft YaHei"));
        assert!(!has_cjk("Noto Sans SC"));
    }
}
