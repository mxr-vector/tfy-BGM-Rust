//! 曲库文件监听，含 700 ms 事件防抖和 30 秒补偿扫描。

use std::path::Path;
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
    pub fn start(root: &Path) -> Result<Self, String> {
        if !root.is_dir() {
            return Err(format!("曲库目录不存在：{}", root.display()));
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
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| format!("无法监听曲库目录 {}：{error}", root.display()))?;

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
    use std::path::PathBuf;

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
}
