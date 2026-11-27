//! 曲库文件监听，含 700 ms 事件防抖和 30 秒补偿扫描。

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

const DEBOUNCE: Duration = Duration::from_millis(700);
const FALLBACK_INTERVAL: Duration = Duration::from_secs(30);

pub fn fallback_due(now: Instant, last_scan: Instant) -> bool {
    now.saturating_duration_since(last_scan) >= FALLBACK_INTERVAL
}

pub struct LibraryWatcher {
    last_event: Arc<Mutex<Option<Instant>>>,
    /// notify 的 watcher 在 drop 时自动停止监听。
    _watcher: RecommendedWatcher,
}

impl LibraryWatcher {
    pub fn start_many(roots: &[PathBuf]) -> Result<Self, String> {
        let targets = deduplicate_watch_roots(roots);
        if targets.is_empty() {
            return Err("没有可监听的音乐文件夹".into());
        }
        let last_event = Arc::new(Mutex::new(None));
        let thread_last_event = Arc::clone(&last_event);
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |result: notify::Result<Event>| {
                if let Ok(event) = result
                    && is_relevant(&event)
                    && let Ok(mut slot) = thread_last_event.lock()
                {
                    *slot = Some(Instant::now());
                }
            })
            .map_err(|error| format!("无法创建文件监听器：{error}"))?;
        let mut watched = 0usize;
        let mut last_error = None;
        for root in &targets {
            match watcher.watch(root, RecursiveMode::Recursive) {
                Ok(()) => watched += 1,
                Err(error) => {
                    last_error = Some(format!("无法监听文件夹 {}：{error}", root.display()));
                }
            }
        }
        if watched == 0 {
            return Err(last_error.unwrap_or_else(|| "无法启动文件监听".into()));
        }

        Ok(Self {
            last_event,
            _watcher: watcher,
        })
    }

    pub fn take_scan_due(&self, now: Instant, fallback_due: bool) -> bool {
        match self.last_event.lock() {
            Ok(mut event) => take_scan_due(&mut event, now, fallback_due),
            Err(_) => fallback_due,
        }
    }
}

fn deduplicate_watch_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut canonical: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let real = fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if seen.insert(real.clone()) {
            canonical.push((real, root.clone()));
        }
    }
    canonical.sort_by_key(|(real, _)| real.components().count());
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut result = Vec::new();
    for (real, raw) in canonical {
        if kept.iter().any(|ancestor| real.starts_with(ancestor)) {
            continue;
        }
        kept.push(real);
        result.push(raw);
    }
    result
}

fn take_scan_due(event: &mut Option<Instant>, now: Instant, fallback_due: bool) -> bool {
    match *event {
        Some(at) if now.saturating_duration_since(at) >= DEBOUNCE => {
            *event = None;
            true
        }
        Some(_) => false,
        None => fallback_due,
    }
}

/// 影响曲库索引的文件系统事件。
fn is_relevant(event: &Event) -> bool {
    use notify::EventKind;
    use notify::event::{CreateKind, ModifyKind, RemoveKind};

    if event.need_rescan() {
        return true;
    }

    let touches_audio_or_dir = event.paths.iter().any(|path| {
        path.extension().is_none()
            || path.is_dir()
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(bgm_core::library::has_audio_extension)
    });
    let directory_event = matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    );

    (touches_audio_or_dir || directory_event)
        && matches!(
            event.kind,
            EventKind::Create(_)
                | EventKind::Remove(_)
                | EventKind::Modify(ModifyKind::Name(_))
                | EventKind::Modify(ModifyKind::Data(_))
                | EventKind::Modify(ModifyKind::Metadata(_))
                | EventKind::Any
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::EventKind;
    use notify::event::{CreateKind, Flag, RemoveKind};

    #[test]
    fn scan_consumes_only_a_ready_notification() {
        let first = Instant::now();
        let latest = first + Duration::from_millis(100);
        let mut event = Some(latest);
        assert!(!take_scan_due(&mut event, first + DEBOUNCE, true));
        assert_eq!(event, Some(latest));
        assert!(take_scan_due(&mut event, latest + DEBOUNCE, false));
        assert!(!take_scan_due(&mut event, latest + DEBOUNCE, false));
        assert!(take_scan_due(&mut event, latest + DEBOUNCE, true));
    }

    #[test]
    fn quiet_watch_uses_the_thirty_second_fallback() {
        let last_scan = Instant::now();
        let mut event = None;
        for (milliseconds, due) in [(29_999, false), (30_000, true)] {
            let now = last_scan + Duration::from_millis(milliseconds);
            assert_eq!(
                take_scan_due(&mut event, now, fallback_due(now, last_scan)),
                due
            );
        }
    }

    #[test]
    fn accepts_audio_formats_folder_removal_and_backend_rescan() {
        for extension in ["mp3", "FLAC", "wav", "M4A", "ogg"] {
            let event = Event::new(EventKind::Create(CreateKind::File))
                .add_path(PathBuf::from(format!("track.{extension}")));
            assert!(is_relevant(&event));
        }
        let cover =
            Event::new(EventKind::Create(CreateKind::File)).add_path(PathBuf::from("cover.png"));
        assert!(!is_relevant(&cover));
        let folder =
            Event::new(EventKind::Remove(RemoveKind::Folder)).add_path(PathBuf::from("vol.2"));
        assert!(is_relevant(&folder));
        let rescan = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert!(is_relevant(&rescan));
    }

    #[test]
    fn deduplicates_parent_and_child_watch_roots() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("parent");
        let child = parent.join("child");
        let sibling = temp.path().join("sibling");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        let deduped = deduplicate_watch_roots(&[child, parent.clone(), sibling.clone(), parent]);
        assert_eq!(deduped.len(), 2);
    }
}
