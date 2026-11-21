//! 曲库文件监听：系统级 watch 事件 + 低频兜底扫描。
//!
//! 对齐主线 `library_watch.rs` 的行为：notify 事件先做 700ms 防抖，再触发增量扫描；
//! 即使事件丢失（网络盘、休眠恢复），30 秒兜底扫描也能补上变化。
//! 这里只记录「最后事件时间」，防抖与兜底由界面的 200ms 轮询统一处理，
//! 增量扫描因此永远发生在界面线程，不需要与曲库写操作互斥。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// 持续有事件时不断延长，直到安静 DEBOUNCE 后才触发扫描。
const DEBOUNCE: Duration = Duration::from_millis(700);

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

    /// 距最近一次相关事件已经安静了 DEBOUNCE，可以触发扫描；调用方处理完需调 [`Self::consume`]。
    pub fn pending(&self) -> bool {
        self.last_event
            .lock()
            .ok()
            .and_then(|slot| *slot)
            .is_some_and(|at| at.elapsed() >= DEBOUNCE)
    }

    pub fn consume(&self) {
        if let Ok(mut slot) = self.last_event.lock() {
            *slot = None;
        }
    }
}

/// 只关心音频文件与目录级变化，忽略缩略图、配置文件等噪声。
fn is_relevant(event: &Event) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;

    let touches_audio_or_dir = event.paths.iter().any(|path| {
        path.extension().is_none()
            || path.is_dir()
            || path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
    });
    if !touches_audio_or_dir {
        return false;
    }

    matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Name(_))
            | EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Metadata(_))
            | EventKind::Any
    )
}
