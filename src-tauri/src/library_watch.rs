//! 曲库文件监听：系统级 watch 事件 + 低频兜底扫描。
//!
//! `notify` 事件先做 700ms 防抖，再交给 [`AppState::handle_watch_event`] 计算增量；
//! 即使事件丢失（网络盘、休眠恢复），30 秒兜底扫描也能补上变化。
//! 监听线程不 join：切换曲库目录时主线程会被写操作锁占用，join 会与扫描互锁。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::{AppState, LIBRARY_CHANGED_EVENT};

const DEBOUNCE: Duration = Duration::from_millis(700);
const FALLBACK_INTERVAL: Duration = Duration::from_secs(30);

pub struct LibraryWatcher {
    stop: Arc<AtomicBool>,
    /// 保留句柄但不在 Drop 里 join：停止标志让线程自行退出。
    _thread: Option<JoinHandle<()>>,
}

impl LibraryWatcher {
    pub fn start(app: AppHandle, root: PathBuf) -> Result<Self> {
        if !root.is_dir() {
            anyhow::bail!("曲库目录不存在：{}", root.display());
        }

        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher: RecommendedWatcher =
            notify::recommended_watcher(move |result: notify::Result<Event>| {
                if let Ok(event) = result
                    && is_relevant(&event)
                {
                    let _ = tx.send(());
                }
            })
            .context("无法创建文件监听器")?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .with_context(|| format!("无法监听曲库目录：{}", root.display()))?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("tfy-bgm-watch".to_owned())
            .spawn(move || {
                let _watcher = watcher;
                let mut last_fallback = Instant::now();
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match rx.recv_timeout(Duration::from_millis(400)) {
                        Ok(()) => {
                            // 防抖：持续有事件时不断延长，直到安静 DEBOUNCE。
                            while rx.recv_timeout(DEBOUNCE).is_ok() {
                                if thread_stop.load(Ordering::Relaxed) {
                                    return;
                                }
                            }
                            if thread_stop.load(Ordering::Relaxed) {
                                break;
                            }
                            notify_frontend(&app);
                            last_fallback = Instant::now();
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if last_fallback.elapsed() >= FALLBACK_INTERVAL {
                                notify_frontend(&app);
                                last_fallback = Instant::now();
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .context("无法启动文件监听线程")?;

        Ok(Self {
            stop,
            _thread: Some(thread),
        })
    }
}

impl Drop for LibraryWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn notify_frontend(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if let Some(change) = state.handle_watch_event() {
        let _ = app.emit(LIBRARY_CHANGED_EVENT, change);
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
