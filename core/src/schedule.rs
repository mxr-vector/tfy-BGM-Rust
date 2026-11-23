//! 节目单（排程）模型：把「每日时间范围 → 歌曲列表 + 音量」持久化到 `schedule.json`。
//!
//! 时间按本地系统时间解释，每个节目只在当天 `[start, end)` 生效，不跨天、不重复日期。
//! 节目之间时间范围禁止重叠；歌曲的起止时间会被裁剪到所属节目的范围内。
//! 文件缺失或损坏时回退为空节目单并附带告警。

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::limits::MAX_NAME_CHARS;
use crate::persist::{PersistError, write_json};

pub const SCHEDULE_VERSION: u32 = 2;
pub const MAX_SCHEDULE_ENTRIES: usize = 64;
pub const MAX_SCHEDULE_SONGS: usize = 500;
const MINUTES_PER_DAY: u32 = 24 * 60;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScheduleSong {
    /// 曲库曲目 key。
    pub key: String,
    /// 本地时间 `HH:MM`，必须落在所属节目范围内。
    pub start: String,
    /// 本地时间 `HH:MM`，必须晚于 `start`。
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScheduleEntry {
    pub id: String,
    pub name: String,
    /// 本地时间 `HH:MM`。
    pub start: String,
    /// 本地时间 `HH:MM`，必须晚于 `start`。
    pub end: String,
    /// 按播放顺序排列的歌曲，每首都有自己的起止时间。
    pub songs: Vec<ScheduleSong>,
    /// 节目开始时应用的音量（0.0 ~ 1.0）；`None` 表示保持当前音量。
    pub volume: Option<f32>,
    pub enabled: bool,
}

impl Default for ScheduleEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            start: "09:00".to_owned(),
            end: "10:00".to_owned(),
            songs: Vec::new(),
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
                let mut schedule = parsed.clone().normalize();
                if schedule.overlap_error().is_some() {
                    schedule = schedule.drop_overlaps();
                }
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

    /// 返回第一对时间重叠的节目；调用前应先 [`Schedule::normalize`] 使时间规范化并排序。
    pub fn overlap_error(&self) -> Option<String> {
        self.entries.windows(2).find_map(|pair| {
            let [left, right] = pair else { return None };
            (minute_of(&right.start) < minute_of(&left.end)).then(|| {
                format!(
                    "节目「{}」（{}–{}）与「{}」（{}–{}）的时间范围重叠",
                    left.name, left.start, left.end, right.name, right.start, right.end
                )
            })
        })
    }

    /// 移除与前面节目重叠的条目；只用于修复历史数据，保存时仍会拒绝重叠。
    fn drop_overlaps(mut self) -> Self {
        let mut kept: Vec<ScheduleEntry> = Vec::new();
        for entry in std::mem::take(&mut self.entries) {
            let overlaps = kept.iter().any(|other| {
                minute_of(&entry.start) < minute_of(&other.end)
                    && minute_of(&other.start) < minute_of(&entry.end)
            });
            if !overlaps {
                kept.push(entry);
            }
        }
        self.entries = kept;
        self
    }
}

impl ScheduleEntry {
    fn normalize(mut self, index: usize, seen: &mut HashSet<String>) -> Option<Self> {
        self.start = normalize_start(&self.start)?;
        let start_minutes = minute_of(&self.start);
        // 当天至少需要留出 1 分钟，23:59 开始的节目无法构成有效范围。
        if start_minutes + 1 >= MINUTES_PER_DAY {
            return None;
        }

        self.end = normalize_start(&self.end)
            .map(|end| minute_of(&end))
            .filter(|end| *end > start_minutes)
            .map(format_minutes)
            .unwrap_or_else(|| format_minutes((start_minutes + 60).min(MINUTES_PER_DAY - 1)));
        let end_minutes = minute_of(&self.end);

        let name: String = self.name.trim().chars().take(MAX_NAME_CHARS).collect();
        self.name = if name.is_empty() {
            format!("节目 {}", index + 1)
        } else {
            name
        };

        let mut seen_keys = HashSet::new();
        self.songs = std::mem::take(&mut self.songs)
            .into_iter()
            .filter_map(|song| {
                let key: String = song.key.trim().chars().take(200).collect();
                if key.is_empty() || !seen_keys.insert(key.clone()) {
                    return None;
                }
                let song_start = normalize_start(&song.start)
                    .map(|time| minute_of(&time))
                    .unwrap_or(start_minutes)
                    .clamp(start_minutes, end_minutes - 1);
                let song_end = normalize_start(&song.end)
                    .map(|time| minute_of(&time))
                    .unwrap_or(end_minutes)
                    .clamp(song_start + 1, end_minutes);
                Some(ScheduleSong {
                    key,
                    start: format_minutes(song_start),
                    end: format_minutes(song_end),
                })
            })
            .take(MAX_SCHEDULE_SONGS)
            .collect();

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
    Some(format_minutes(hour * 60 + minute))
}

fn minute_of(time: &str) -> u32 {
    let (hour, minute) = time.split_once(':').unwrap_or(("0", "0"));
    hour.parse::<u32>().unwrap_or(0) * 60 + minute.parse::<u32>().unwrap_or(0)
}

fn format_minutes(total: u32) -> String {
    format!("{:02}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(start: &str, end: &str, name: &str) -> ScheduleEntry {
        ScheduleEntry {
            start: start.into(),
            end: end.into(),
            name: name.into(),
            ..ScheduleEntry::default()
        }
    }

    fn song(key: &str, start: &str, end: &str) -> ScheduleSong {
        ScheduleSong {
            key: key.into(),
            start: start.into(),
            end: end.into(),
        }
    }

    #[test]
    fn normalizes_time_and_sorts_entries() {
        let schedule = Schedule {
            entries: vec![
                entry("20:5", "21:00", "晚上"),
                entry("9:00", "10:00", "早上"),
                entry("12:00", "13:00", "中午"),
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
    fn fills_missing_end_and_drops_invalid_ranges() {
        let schedule = Schedule {
            entries: vec![
                entry("22:30", "", "缺失结束"),
                entry("25:00", "26:00", "坏时间"),
                entry("23:59", "", "没有空间"),
                entry("18:00", "17:00", "结束早于开始"),
            ],
            ..Schedule::default()
        }
        .normalize();

        assert_eq!(schedule.entries.len(), 2);
        assert_eq!(schedule.entries[0].end, "19:00");
        assert_eq!(schedule.entries[1].start, "22:30");
        assert_eq!(schedule.entries[1].end, "23:30");
    }

    #[test]
    fn clamps_song_times_into_program_range() {
        let schedule = Schedule {
            entries: vec![ScheduleEntry {
                songs: vec![
                    song("a", "08:30", "10:30"),
                    song("b", "09:50", "09:55"),
                    song("c", "10:30", "09:00"),
                    song("a", "09:00", "09:10"),
                ],
                ..entry("09:00", "10:00", "早间")
            }],
            ..Schedule::default()
        }
        .normalize();

        let songs = &schedule.entries[0].songs;
        assert_eq!(songs.len(), 3);
        assert_eq!(
            (songs[0].start.as_str(), songs[0].end.as_str()),
            ("09:00", "10:00")
        );
        assert_eq!(
            (songs[1].start.as_str(), songs[1].end.as_str()),
            ("09:50", "09:55")
        );
        assert_eq!(
            (songs[2].start.as_str(), songs[2].end.as_str()),
            ("09:59", "10:00")
        );
    }

    #[test]
    fn ids_stay_unique() {
        let mut first = entry("09:00", "10:00", "A");
        first.id = "same".into();
        let mut second = entry("10:00", "11:00", "B");
        second.id = "same".into();

        let schedule = Schedule {
            entries: vec![first, second],
            ..Schedule::default()
        }
        .normalize();

        assert_eq!(schedule.entries[0].id, "same");
        assert_eq!(schedule.entries[1].id, "same-2");
    }

    #[test]
    fn detects_overlapping_ranges() {
        let schedule = Schedule {
            entries: vec![
                entry("09:00", "10:30", "A"),
                entry("10:00", "11:00", "B"),
                entry("11:30", "12:00", "C"),
            ],
            ..Schedule::default()
        }
        .normalize();

        let error = schedule.overlap_error().expect("应检测到重叠");
        assert!(error.contains("A"));
        assert!(error.contains("B"));
    }

    #[test]
    fn repairs_overlaps_by_dropping_later_entries() {
        let schedule = Schedule {
            entries: vec![
                entry("09:00", "11:00", "A"),
                entry("10:00", "12:00", "B"),
                entry("12:00", "13:00", "C"),
            ],
            ..Schedule::default()
        }
        .normalize()
        .drop_overlaps();

        assert_eq!(
            schedule
                .entries
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            ["A", "C"]
        );
        assert!(schedule.overlap_error().is_none());
    }

    #[test]
    fn adjacent_ranges_do_not_overlap() {
        let schedule = Schedule {
            entries: vec![entry("09:00", "10:00", "A"), entry("10:00", "11:00", "B")],
            ..Schedule::default()
        }
        .normalize();

        assert!(schedule.overlap_error().is_none());
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
                end: "12:00".into(),
                songs: vec![song("cheerful-a", "09:00", "09:03")],
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
