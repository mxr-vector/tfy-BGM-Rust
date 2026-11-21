//! 单曲循环区间：`loops.json` 以曲目 key 为索引保存 A/B 点。
//!
//! 播放器只在当前曲目启用循环时读取该记录；曲目重命名 / 移动会同步迁移 key，
//! 删除曲目会一并清掉对应循环点，避免配置越积越多。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::limits::MAX_KEY_LEN;
use crate::ops::KeyChange;
use crate::persist::{PersistError, write_json};

pub const LOOPS_VERSION: u32 = 1;
/// 循环区间上限（秒）：24 小时，足以覆盖任何音频。
const MAX_LOOP_END: f64 = 86_400.0;
const MIN_LOOP_SPAN: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LoopPoint {
    pub start: f64,
    pub end: f64,
    pub enabled: bool,
}

impl Default for LoopPoint {
    fn default() -> Self {
        Self {
            start: 0.0,
            end: 0.0,
            enabled: true,
        }
    }
}

impl LoopPoint {
    pub fn span(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    fn normalize(mut self) -> Option<Self> {
        if !self.start.is_finite() || !self.end.is_finite() {
            return None;
        }
        self.start = self.start.clamp(0.0, MAX_LOOP_END);
        self.end = self.end.clamp(0.0, MAX_LOOP_END);
        (self.end - self.start >= MIN_LOOP_SPAN).then_some(self)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LoopBook {
    pub version: u32,
    pub points: BTreeMap<String, LoopPoint>,
}

#[derive(Debug, Error)]
pub enum LoopError {
    #[error("曲目标识无效")]
    InvalidKey,
    #[error("循环终点必须晚于起点至少 {MIN_LOOP_SPAN} 秒")]
    InvalidSpan,
}

impl LoopBook {
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("读取循环点失败，已忽略：{error}")),
                );
            }
        };

        match serde_json::from_slice::<Self>(&bytes) {
            Ok(parsed) => {
                let book = parsed.clone().normalize();
                let warning =
                    (book != parsed).then(|| "循环点配置存在无效项，已自动清理".to_owned());
                (book, warning)
            }
            Err(error) => (
                Self::default(),
                Some(format!("循环点文件损坏，已重置：{error}")),
            ),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), PersistError> {
        write_json(path, self)
    }

    pub fn normalize(mut self) -> Self {
        self.version = LOOPS_VERSION;
        self.points = std::mem::take(&mut self.points)
            .into_iter()
            .filter(|(key, _)| valid_key(key))
            .filter_map(|(key, point)| point.normalize().map(|point| (key, point)))
            .collect();
        self
    }

    pub fn get(&self, key: &str) -> Option<LoopPoint> {
        self.points.get(key).copied()
    }

    pub fn set(&mut self, key: &str, point: LoopPoint) -> Result<LoopPoint, LoopError> {
        if !valid_key(key) {
            return Err(LoopError::InvalidKey);
        }
        let point = point.normalize().ok_or(LoopError::InvalidSpan)?;
        self.version = LOOPS_VERSION;
        self.points.insert(key.to_owned(), point);
        Ok(point)
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.points.remove(key).is_some()
    }

    /// 曲目重命名 / 移动后迁移索引，删除的曲目直接移除。
    pub fn apply_key_changes(&mut self, changes: &[KeyChange], removed: &[String]) {
        for change in changes {
            if change.from == change.to {
                continue;
            }
            if let Some(point) = self.points.remove(&change.from) {
                self.points.insert(change.to.clone(), point);
            }
        }
        for key in removed {
            self.points.remove(key);
        }
    }
}

fn valid_key(key: &str) -> bool {
    let trimmed = key.trim();
    !trimmed.is_empty() && trimmed.len() <= MAX_KEY_LEN && trimmed == key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_span_and_bad_keys() {
        let mut book = LoopBook::default();
        assert!(
            book.set(
                "cheerful/a",
                LoopPoint {
                    start: 10.0,
                    end: 5.0,
                    enabled: true
                }
            )
            .is_err()
        );
        assert!(
            book.set(
                " ",
                LoopPoint {
                    start: 0.0,
                    end: 10.0,
                    enabled: true
                }
            )
            .is_err()
        );
        assert!(book.points.is_empty());

        let point = book
            .set(
                "cheerful/a",
                LoopPoint {
                    start: 1.5,
                    end: 12.0,
                    enabled: true,
                },
            )
            .unwrap();
        assert!((point.span() - 10.5).abs() < 1e-9);
    }

    #[test]
    fn roundtrip_and_key_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loops.json");
        let mut book = LoopBook::default();
        book.set(
            "cheerful/a",
            LoopPoint {
                start: 2.0,
                end: 8.0,
                enabled: true,
            },
        )
        .unwrap();
        book.set(
            "epic/b",
            LoopPoint {
                start: 0.0,
                end: 4.0,
                enabled: false,
            },
        )
        .unwrap();
        book.save(&path).unwrap();

        let (mut loaded, warning) = LoopBook::load(&path);
        assert!(warning.is_none());
        assert_eq!(loaded.points.len(), 2);

        loaded.apply_key_changes(
            &[KeyChange {
                from: "cheerful/a".into(),
                to: "memories/a".into(),
            }],
            &["epic/b".into()],
        );
        assert!(loaded.get("memories/a").is_some());
        assert!(loaded.get("cheerful/a").is_none());
        assert!(loaded.get("epic/b").is_none());
    }

    #[test]
    fn corrupt_file_resets_book() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loops.json");
        fs::write(&path, b"[]").unwrap();
        let (book, warning) = LoopBook::load(&path);
        assert!(book.points.is_empty());
        assert!(warning.is_some());
    }

    #[test]
    fn normalization_drops_bad_points() {
        let book = LoopBook {
            version: 0,
            points: BTreeMap::from([
                (
                    "ok".to_owned(),
                    LoopPoint {
                        start: 1.0,
                        end: 2.0,
                        enabled: true,
                    },
                ),
                (
                    "bad".to_owned(),
                    LoopPoint {
                        start: 5.0,
                        end: 1.0,
                        enabled: true,
                    },
                ),
            ]),
        }
        .normalize();
        assert_eq!(book.points.len(), 1);
        assert_eq!(book.version, LOOPS_VERSION);
    }
}
