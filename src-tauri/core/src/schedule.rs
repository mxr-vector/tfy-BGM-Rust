//! 节目单（排程）模型：把「时间段 → 分类歌单 + 音量」持久化到 `schedule.json`。
//!
//! 时间按本地系统时间解释；`days` 使用 ISO 周序（1 = 周一 … 7 = 周日），
//! 七个值全选即为每日循环模板。文件缺失或损坏时回退为空节目单并附带告警。

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::limits::MAX_NAME_CHARS;
use crate::persist::{PersistError, write_json};

pub const SCHEDULE_VERSION: u32 = 1;
pub const MAX_SCHEDULE_ENTRIES: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScheduleEntry {
    pub id: String,
    pub name: String,
    /// 本地时间 `HH:MM`。
    pub start: String,
    /// ISO 周序：1 = 周一 … 7 = 周日。
    pub days: Vec<u8>,
    /// 目标分类 id；`None` 表示全部曲目。
    pub category: Option<String>,
    /// 到点后应用的音量（0.0 ~ 1.0）；`None` 表示保持当前音量。
    pub volume: Option<f32>,
    pub enabled: bool,
}

impl Default for ScheduleEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            start: "09:00".to_owned(),
            days: (1..=7).collect(),
            category: None,
            volume: None,
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Schedule {
    pub version: u32,
    pub entries: Vec<ScheduleEntry>,
}

impl Schedule {
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("读取节目单失败，已使用空节目单：{error}")),
                );
            }
        };

        match serde_json::from_slice::<Self>(&bytes) {
            Ok(parsed) => {
                let schedule = parsed.clone().normalize();
                let warning =
                    (schedule != parsed).then(|| "节目单存在无效项，已自动修正或移除".to_owned());
                (schedule, warning)
            }
            Err(error) => (
                Self::default(),
                Some(format!("节目单文件损坏，已重置为空节目单：{error}")),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), PersistError> {
        write_json(path, self)
    }

    pub fn normalize(mut self) -> Self {
        self.version = SCHEDULE_VERSION;
        let mut seen_ids = HashSet::new();
        let mut entries = Vec::new();
        for (index, entry) in std::mem::take(&mut self.entries).into_iter().enumerate() {
            if let Some(entry) = entry.normalize(index, &mut seen_ids) {
                entries.push(entry);
            }
        }
        entries.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.name.cmp(&b.name)));
        entries.truncate(MAX_SCHEDULE_ENTRIES);
        self.entries = entries;
        self
    }

    pub fn entry(&self, id: &str) -> Option<&ScheduleEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

impl ScheduleEntry {
    fn normalize(mut self, index: usize, seen: &mut HashSet<String>) -> Option<Self> {
        self.start = normalize_start(&self.start)?;

        let name: String = self.name.trim().chars().take(MAX_NAME_CHARS).collect();
        self.name = if name.is_empty() {
            format!("节目 {}", index + 1)
        } else {
            name
        };

        let mut days: Vec<u8> = self
            .days
            .into_iter()
            .filter(|day| (1..=7).contains(day))
            .collect();
        days.sort_unstable();
        days.dedup();
        if days.is_empty() {
            days = (1..=7).collect();
        }
        self.days = days;

        self.category = self
            .category
            .map(|category| category.trim().to_owned())
            .filter(|category| !category.is_empty() && category.len() <= 120);

        self.volume = self
            .volume
            .filter(|volume| volume.is_finite())
            .map(|volume| volume.clamp(0.0, 1.0));

        let mut id: String = self.id.trim().chars().take(80).collect();
        if id.is_empty() {
            id = format!("slot-{}", index + 1);
        }
        let mut candidate = id.clone();
        let mut suffix = 2;
        while !seen.insert(candidate.clone()) {
            candidate = format!("{id}-{suffix}");
            suffix += 1;
        }
        self.id = candidate;
        Some(self)
    }
}

fn normalize_start(raw: &str) -> Option<String> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(start: &str, name: &str) -> ScheduleEntry {
        ScheduleEntry {
            start: start.into(),
            name: name.into(),
            ..ScheduleEntry::default()
        }
    }

    #[test]
    fn normalizes_time_and_sorts_entries() {
        let schedule = Schedule {
            entries: vec![
                entry("20:5", "晚上"),
                entry("9:00", "早上"),
                entry("12:00", "中午"),
            ],
            ..Schedule::default()
        }
        .normalize();

        assert_eq!(
            schedule
                .entries
                .iter()
                .map(|entry| entry.start.as_str())
                .collect::<Vec<_>>(),
            ["09:00", "12:00", "20:05"]
        );
    }

    #[test]
    fn drops_invalid_times_and_fills_days() {
        let schedule = Schedule {
            entries: vec![
                entry("25:00", "坏时间"),
                ScheduleEntry {
                    days: vec![],
                    category: Some("  cheerful ".into()),
                    volume: Some(4.0),
                    ..entry("08:00", "早间")
                },
            ],
            ..Schedule::default()
        }
        .normalize();

        assert_eq!(schedule.entries.len(), 1);
        assert_eq!(schedule.entries[0].days, vec![1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(schedule.entries[0].category.as_deref(), Some("cheerful"));
        assert_eq!(schedule.entries[0].volume, Some(1.0));
    }

    #[test]
    fn ids_stay_unique_and_days_dedupe() {
        let mut first = entry("09:00", "A");
        first.id = "same".into();
        first.days = vec![1, 1, 2, 9];
        let mut second = entry("10:00", "B");
        second.id = "same".into();

        let schedule = Schedule {
            entries: vec![first, second],
            ..Schedule::default()
        }
        .normalize();

        assert_eq!(schedule.entries[0].id, "same");
        assert_eq!(schedule.entries[0].days, vec![1, 2]);
        assert_eq!(schedule.entries[1].id, "same-2");
    }

    #[test]
    fn roundtrip_persists_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.json");
        let schedule = Schedule {
            entries: vec![ScheduleEntry {
                id: "shop-open".into(),
                name: "开店轻快".into(),
                start: "09:00".into(),
                days: (1..=5).collect(),
                category: Some("cheerful".into()),
                volume: Some(0.55),
                enabled: true,
            }],
            ..Schedule::default()
        }
        .normalize();
        schedule.save(&path).unwrap();

        let (loaded, warning) = Schedule::load(&path);
        assert!(warning.is_none(), "{warning:?}");
        assert_eq!(loaded, schedule);
    }

    #[test]
    fn corrupt_file_returns_empty_schedule_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.json");
        fs::write(&path, b"{ not json").unwrap();
        let (schedule, warning) = Schedule::load(&path);
        assert!(schedule.entries.is_empty());
        assert!(warning.is_some_and(|text| text.contains("损坏")));
    }
}
