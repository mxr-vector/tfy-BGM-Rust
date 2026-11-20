//! 曲库分类元数据：目录名、展示文案与主题色的唯一事实来源。
//!
//! 内置 6 个分类作为默认值；用户配置保存在应用配置目录的 `categories.json`，
//! 可直接编辑该文件后重新扫描生效，也可在界面中右键重命名或新增分类。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_COMPONENT_CHARS: usize = 40;
const DEFAULT_DESCRIPTION: &str =
    "自定义分类：把同类风格的音频放入这个分类目录，重新扫描后即可试听与导出。";

const PALETTE: [&str; 8] = [
    "#22c55e", "#2dd4bf", "#84cc16", "#a3e635", "#f59e0b", "#f472b6", "#38bdf8", "#ef4444",
];

/// 一个 BGM 分类。`id` 同时也是曲库根目录下的子目录名。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Category {
    pub id: String,
    pub name_zh: String,
    pub name_en: String,
    pub label_en: String,
    pub color: String,
    pub title: String,
    pub description: String,
}

/// 分类集合，同时也是 `categories.json` 的序列化形态（JSON 数组）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Categories {
    pub items: Vec<Category>,
}

#[derive(Debug, Error)]
pub enum CategoryError {
    #[error("分类名称不能为空")]
    EmptyName,
    #[error("分类名称过长（最多 {MAX_COMPONENT_CHARS} 个字符）")]
    NameTooLong,
    #[error("分类名称包含非法字符：{0}")]
    InvalidName(String),
    #[error("分类目录名不能为空")]
    EmptyId,
    #[error("分类目录名过长（最多 {MAX_COMPONENT_CHARS} 个字符）")]
    IdTooLong,
    #[error("分类目录名包含非法字符：{0}")]
    InvalidId(String),
    #[error("主题色必须是 #RRGGBB 格式：{0}")]
    InvalidColor(String),
    #[error("分类已存在：{0}")]
    DuplicateId(String),
    #[error("同名分类已存在：{0}")]
    DuplicateName(String),
    #[error("分类不存在：{0}")]
    Missing(String),
    #[error("创建分类配置目录失败：{0}")]
    CreateDir(#[source] std::io::Error),
    #[error("序列化分类配置失败：{0}")]
    Serialize(#[from] serde_json::Error),
    #[error("写入分类配置失败：{0}")]
    Write(#[source] std::io::Error),
}

fn builtin(
    id: &str,
    name_zh: &str,
    name_en: &str,
    label_en: &str,
    color: &str,
    title: &str,
    description: &str,
) -> Category {
    Category {
        id: id.to_owned(),
        name_zh: name_zh.to_owned(),
        name_en: name_en.to_owned(),
        label_en: label_en.to_owned(),
        color: color.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
    }
}

pub fn defaults() -> Categories {
    Categories {
        items: vec![
            builtin(
                "cheerful",
                "欢快",
                "Cheerful",
                "CHEERFUL / UPBEAT",
                "#f59e0b",
                "欢快节奏 · Cheerful & Upbeat",
                "适用于生活 Vlog、开箱解说、美食探店、搞笑趣味、快闪卡点等轻松愉悦的视频剪辑。",
            ),
            builtin(
                "solitude",
                "独处",
                "Solitude",
                "SOLITUDE / PEACEFUL",
                "#2dd4bf",
                "独处沉思 · Solitude & Chill",
                "纯音乐、治愈钢琴、深夜独白、静心工作与自律学习视频的最佳背景配乐。",
            ),
            builtin(
                "memories",
                "回忆",
                "Memories",
                "MEMORIES / NOSTALGIA",
                "#f472b6",
                "时光回忆 · Memories & Nostalgia",
                "温情故事、毕业季纪念、告白与重逢、岁月光影、情感纪实剪辑首选配乐。",
            ),
            builtin(
                "epic",
                "史诗",
                "Epic",
                "EPIC / MAJESTIC",
                "#ef4444",
                "震撼史诗 · Epic & Grand",
                "气势磅礴的大片感战歌，适合燃向混剪、企业宣传片、运动高光、游戏集锦、宏大叙事。",
            ),
            builtin(
                "gufeng",
                "古风",
                "GuFeng",
                "GUFENG / TRADITIONAL",
                "#10b981",
                "东方古风 · Traditional Chinese",
                "笛子、古筝、二胡等国风雅韵，完美契合汉服国风、古建文旅、武侠唯美剪辑。",
            ),
            builtin(
                "country",
                "乡村",
                "Country",
                "COUNTRY / FOLK",
                "#84cc16",
                "田园乡村 · Country & Rural",
                "乡野烟火、农家田园、归隐慢生活、悠闲度假风景短视频的绝妙搭档。",
            ),
        ],
    }
}

impl Default for Categories {
    fn default() -> Self {
        defaults()
    }
}

impl Categories {
    /// 读取分类配置；文件缺失返回内置分类，损坏时返回内置分类并附带一条告警。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (defaults(), None);
            }
            Err(error) => {
                return (
                    defaults(),
                    Some(format!("读取分类配置失败，已使用内置分类：{error}")),
                );
            }
        };

        match serde_json::from_slice::<Self>(&bytes) {
            Ok(categories) => {
                let (categories, warnings) = categories.normalize();
                let warning = (!warnings.is_empty())
                    .then(|| format!("分类配置已自动修正：{}", warnings.join("；")));
                (categories, warning)
            }
            Err(error) => (
                defaults(),
                Some(format!("分类配置文件损坏，已重置为内置分类：{error}")),
            ),
        }
    }

    /// 首次启动时把默认分类写入配置文件，方便用户直接用编辑器维护。
    pub fn ensure_file(&self, path: &Path) -> Result<(), CategoryError> {
        if path.exists() {
            return Ok(());
        }
        self.save(path)
    }

    pub fn save(&self, path: &Path) -> Result<(), CategoryError> {
        let parent = path.parent().ok_or_else(|| {
            CategoryError::CreateDir(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("分类配置路径没有父目录：{}", path.display()),
            ))
        })?;
        fs::create_dir_all(parent).map_err(CategoryError::CreateDir)?;
        let bytes = serde_json::to_vec_pretty(self)?;

        let tmp = temp_path(path);
        fs::write(&tmp, &bytes).map_err(CategoryError::Write)?;
        if fs::rename(&tmp, path).is_err() {
            // Windows 上目标已存在时重命名可能失败，退化为直接覆盖。
            fs::write(path, &bytes).map_err(CategoryError::Write)?;
            let _ = fs::remove_file(&tmp);
        }
        Ok(())
    }

    /// 把任意输入收敛到可用的分类列表，返回被修正/忽略项的说明。
    pub fn normalize(mut self) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut seen = HashSet::new();
        let mut items = Vec::with_capacity(self.items.len());

        for (index, mut category) in self.items.into_iter().enumerate() {
            category.id = category.id.trim().to_owned();
            category.name_zh = category.name_zh.trim().to_owned();
            category.name_en = category.name_en.trim().to_owned();
            category.label_en = category.label_en.trim().to_owned();
            category.color = category.color.trim().to_owned();
            category.title = category.title.trim().to_owned();
            category.description = category.description.trim().to_owned();

            if category.id.is_empty() {
                warnings.push("忽略缺少目录名（id）的分类".to_owned());
                continue;
            }
            match validate_id(&category.id) {
                Ok(id) => category.id = id,
                Err(error) => {
                    warnings.push(format!("忽略无效分类：{error}"));
                    continue;
                }
            }
            if !seen.insert(category.id.to_lowercase()) {
                warnings.push(format!("忽略重复分类：{}", category.id));
                continue;
            }

            if category.name_zh.is_empty() {
                category.name_zh = category.id.clone();
            } else if let Err(error) = validate_name(&category.name_zh) {
                warnings.push(format!(
                    "分类 {} {error}，已使用目录名作为名称",
                    category.id
                ));
                category.name_zh = category.id.clone();
            }

            if category.name_en.is_empty() {
                category.name_en = category.name_zh.clone();
            }
            if category.label_en.is_empty() {
                category.label_en = category.name_zh.to_uppercase();
            }
            if !is_hex_color(&category.color) {
                category.color = PALETTE[index % PALETTE.len()].to_owned();
            }
            if category.title.is_empty() {
                category.title = format!("{}音乐", category.name_zh);
            }
            if category.description.is_empty() {
                category.description = DEFAULT_DESCRIPTION.to_owned();
            }
            items.push(category);
        }

        if items.is_empty() {
            warnings.push("分类配置为空，已恢复内置分类".to_owned());
            items = defaults().items;
        }
        self.items = items;
        (self, warnings)
    }

    pub fn by_id(&self, id: &str) -> Option<&Category> {
        self.items.iter().find(|category| category.id == id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.by_id(id).is_some()
    }

    /// 新增分类；`id` 留空时使用分类名称作为目录名。
    pub fn add(&mut self, name: &str, id: Option<&str>) -> Result<Category, CategoryError> {
        let name = validate_name(name)?;
        if self
            .items
            .iter()
            .any(|category| category.name_zh.eq_ignore_ascii_case(&name))
        {
            return Err(CategoryError::DuplicateName(name));
        }
        let id = match id.map(str::trim).filter(|id| !id.is_empty()) {
            Some(id) => validate_id(id)?,
            None => {
                if !is_valid_component(&name) {
                    return Err(CategoryError::InvalidId(name));
                }
                name.clone()
            }
        };
        if self
            .items
            .iter()
            .any(|category| category.id.eq_ignore_ascii_case(&id))
        {
            return Err(CategoryError::DuplicateId(id));
        }

        let category = Category {
            id,
            name_en: name.clone(),
            label_en: name.to_uppercase(),
            color: self.next_color().to_owned(),
            title: format!("{name}音乐"),
            description: DEFAULT_DESCRIPTION.to_owned(),
            name_zh: name,
        };
        self.items.push(category.clone());
        Ok(category)
    }

    /// 修改展示名称与主题色；`color` 为 `None` 时保持原色。
    pub fn update(
        &mut self,
        id: &str,
        name: &str,
        color: Option<&str>,
    ) -> Result<(), CategoryError> {
        let name = validate_name(name)?;
        let color = match color {
            Some(value) => {
                let value = value.trim();
                if !is_hex_color(value) {
                    return Err(CategoryError::InvalidColor(value.to_owned()));
                }
                Some(value.to_owned())
            }
            None => None,
        };
        if self
            .items
            .iter()
            .any(|category| category.id != id && category.name_zh.eq_ignore_ascii_case(&name))
        {
            return Err(CategoryError::DuplicateName(name));
        }
        let category = self
            .items
            .iter_mut()
            .find(|category| category.id == id)
            .ok_or_else(|| CategoryError::Missing(id.to_owned()))?;
        if category.name_zh != name {
            let old = category.name_zh.clone();
            if !category.title.is_empty() && category.title.contains(&old) {
                category.title = category.title.replace(&old, &name);
            }
            category.name_zh = name;
        }
        if let Some(color) = color {
            category.color = color;
        }
        Ok(())
    }

    fn next_color(&self) -> &'static str {
        PALETTE[self.items.len() % PALETTE.len()]
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

fn validate_name(value: &str) -> Result<String, CategoryError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CategoryError::EmptyName);
    }
    if trimmed.chars().count() > MAX_COMPONENT_CHARS {
        return Err(CategoryError::NameTooLong);
    }
    if !is_valid_component(trimmed) {
        return Err(CategoryError::InvalidName(trimmed.to_owned()));
    }
    Ok(trimmed.to_owned())
}

fn validate_id(value: &str) -> Result<String, CategoryError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(CategoryError::EmptyId);
    }
    if trimmed.chars().count() > MAX_COMPONENT_CHARS {
        return Err(CategoryError::IdTooLong);
    }
    if !is_valid_component(trimmed) {
        return Err(CategoryError::InvalidId(trimmed.to_owned()));
    }
    Ok(trimmed.to_owned())
}

/// 目录名与展示名共用的基础校验：必须是单个普通路径分量。
fn is_valid_component(value: &str) -> bool {
    if value.is_empty() || value == "." || value == ".." || value.starts_with('.') {
        return false;
    }
    !value.chars().any(|ch| {
        ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
    })
}

fn is_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].chars().all(|ch| ch.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_ids_are_unique() {
        let categories = defaults();
        let ids: HashSet<_> = categories.items.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), categories.items.len());
        assert_eq!(categories.items.len(), 6);
    }

    #[test]
    fn roundtrip_persists_user_categories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/categories.json");
        let mut categories = defaults();
        let added = categories.add("旅行", None).unwrap();
        assert_eq!(added.id, "旅行");
        assert!(categories.update("旅行", "旅拍", None).is_ok());
        categories.save(&path).unwrap();

        let (loaded, warning) = Categories::load(&path);
        assert!(warning.is_none());
        let travel = loaded.by_id("旅行").unwrap();
        assert_eq!(travel.name_zh, "旅拍");
        assert_eq!(travel.title, "旅拍音乐");
    }

    #[test]
    fn update_changes_name_and_color_but_not_id() {
        let mut categories = defaults();
        let added = categories.add("旅行", None).unwrap();
        assert_eq!(added.title, "旅行音乐");

        categories.update("旅行", "旅拍", Some("#22d3ee")).unwrap();

        let travel = categories.by_id("旅行").unwrap();
        assert_eq!(travel.name_zh, "旅拍");
        assert_eq!(travel.color, "#22d3ee");
        assert_eq!(travel.title, "旅拍音乐");

        assert!(matches!(
            categories.update("旅行", "欢快", None),
            Err(CategoryError::DuplicateName(_))
        ));
        assert!(matches!(
            categories.update("旅行", "旅拍", Some("red")),
            Err(CategoryError::InvalidColor(_))
        ));
        assert!(matches!(
            categories.update("missing", "新名字", None),
            Err(CategoryError::Missing(_))
        ));
    }

    #[test]
    fn add_rejects_duplicates_and_unsafe_ids() {
        let mut categories = defaults();
        assert!(matches!(
            categories.add("欢快", None),
            Err(CategoryError::DuplicateName(_))
        ));
        assert!(matches!(
            categories.add("新分类", Some("cheerful")),
            Err(CategoryError::DuplicateId(_))
        ));
        assert!(matches!(
            categories.add("新分类", Some("../escape")),
            Err(CategoryError::InvalidId(_))
        ));
        assert!(matches!(
            categories.add("   ", None),
            Err(CategoryError::EmptyName)
        ));
    }

    #[test]
    fn normalize_fills_derived_fields_and_drops_bad_entries() {
        let raw = r#"[
            {"id": " travel ", "nameZh": "旅行"},
            {"id": "Travel", "nameZh": "重名"},
            {"id": "bad/id", "nameZh": "非法"},
            {"nameZh": "缺少 id"}
        ]"#;
        let categories: Categories = serde_json::from_str(raw).unwrap();
        let (categories, warnings) = categories.normalize();

        assert_eq!(categories.items.len(), 1);
        let travel = &categories.items[0];
        assert_eq!(travel.id, "travel");
        assert_eq!(travel.name_zh, "旅行");
        assert_eq!(travel.label_en, "旅行");
        assert_eq!(travel.title, "旅行音乐");
        assert!(is_hex_color(&travel.color));
        assert_eq!(warnings.len(), 3, "{warnings:?}");
    }

    #[test]
    fn empty_config_falls_back_to_defaults() {
        let (categories, warnings) = Categories { items: Vec::new() }.normalize();
        assert_eq!(categories.items.len(), 6);
        assert!(
            warnings
                .iter()
                .any(|warning| warning.contains("恢复内置分类"))
        );
    }
}
