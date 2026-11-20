//! 曲库分类元数据：目录名、展示文案与主题色的唯一事实来源。

use serde::Serialize;

/// 一个 BGM 分类。`id` 同时也是曲库根目录下的子目录名。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: &'static str,
    pub name_zh: &'static str,
    pub name_en: &'static str,
    pub label_en: &'static str,
    pub color: &'static str,
    pub title: &'static str,
    pub description: &'static str,
}

pub const CATEGORIES: [Category; 6] = [
    Category {
        id: "cheerful",
        name_zh: "欢快",
        name_en: "Cheerful",
        label_en: "CHEERFUL / UPBEAT",
        color: "#f59e0b",
        title: "欢快节奏 · Cheerful & Upbeat",
        description: "适用于生活 Vlog、开箱解说、美食探店、搞笑趣味、快闪卡点等轻松愉悦的视频剪辑。",
    },
    Category {
        id: "solitude",
        name_zh: "独处",
        name_en: "Solitude",
        label_en: "SOLITUDE / PEACEFUL",
        color: "#818cf8",
        title: "独处沉思 · Solitude & Chill",
        description: "纯音乐、治愈钢琴、深夜独白、静心工作与自律学习视频的最佳背景配乐。",
    },
    Category {
        id: "memories",
        name_zh: "回忆",
        name_en: "Memories",
        label_en: "MEMORIES / NOSTALGIA",
        color: "#f472b6",
        title: "时光回忆 · Memories & Nostalgia",
        description: "温情故事、毕业季纪念、告白与重逢、岁月光影、情感纪实剪辑首选配乐。",
    },
    Category {
        id: "epic",
        name_zh: "史诗",
        name_en: "Epic",
        label_en: "EPIC / MAJESTIC",
        color: "#ef4444",
        title: "震撼史诗 · Epic & Grand",
        description: "气势磅礴的大片感战歌，适合燃向混剪、企业宣传片、运动高光、游戏集锦、宏大叙事。",
    },
    Category {
        id: "gufeng",
        name_zh: "古风",
        name_en: "GuFeng",
        label_en: "GUFENG / TRADITIONAL",
        color: "#10b981",
        title: "东方古风 · Traditional Chinese",
        description: "笛子、古筝、二胡等国风雅韵，完美契合汉服国风、古建文旅、武侠唯美剪辑。",
    },
    Category {
        id: "country",
        name_zh: "乡村",
        name_en: "Country",
        label_en: "COUNTRY / FOLK",
        color: "#84cc16",
        title: "田园乡村 · Country & Rural",
        description: "乡野烟火、农家田园、归隐慢生活、悠闲度假风景短视频的绝妙搭档。",
    },
];

pub fn by_id(id: &str) -> Option<&'static Category> {
    CATEGORIES.iter().find(|category| category.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn category_ids_are_unique() {
        let ids: HashSet<_> = CATEGORIES.iter().map(|category| category.id).collect();
        assert_eq!(ids.len(), CATEGORIES.len());
    }

    #[test]
    fn lookup_matches_constant_order() {
        for (index, category) in CATEGORIES.iter().enumerate() {
            let found = by_id(category.id).expect("registered category must resolve");
            assert_eq!(found.id, CATEGORIES[index].id);
        }
        assert!(by_id("unknown").is_none());
    }
}
