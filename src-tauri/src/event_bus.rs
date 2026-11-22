//! 本机接口的事件总线：为 SSE 提供单调递增的 `id`、事件名与断线续传历史。
//!
//! 每个事件固定对应 SSE 的三个标准字段：
//! - `id`：进程内单调递增，客户端重连时通过 `Last-Event-ID` 回传；
//! - `name`：事件名（`state` / `task` / `resync`），写入 `event:`；
//! - `data`：JSON 文本，写入 `data:`。
//!
//! 历史只保留最近 [`HISTORY_LIMIT`] 条，用于断线重连时补发缺口内的事件；
//! 请求的 id 已滑出窗口时由调用方先发 `resync`，让客户端重新拉取快照。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tokio::sync::broadcast;

/// 历史事件保留条数；1 秒一次的状态推送约覆盖 4 分钟。
const HISTORY_LIMIT: usize = 256;

/// 一条可写入 SSE 的事件。
#[derive(Debug, Clone)]
pub struct ApiEvent {
    pub id: u64,
    pub name: &'static str,
    pub data: String,
}

/// 断线续传结果。
#[derive(Debug, Default)]
pub struct Replay {
    /// `id` 大于 `Last-Event-ID` 的历史事件，按 id 升序。
    pub events: Vec<ApiEvent>,
    /// 请求的 id 已滑出历史窗口，调用方应先发 `resync`。
    pub gap: bool,
    /// 当前历史中最旧的事件 id；历史为空时为 `None`。
    pub oldest: Option<u64>,
}

/// 广播通道 + 历史环形缓冲；由 [`crate::api::ApiBridge`] 与任务队列共享。
pub struct EventBus {
    sender: broadcast::Sender<ApiEvent>,
    next_id: AtomicU64,
    history: Mutex<VecDeque<ApiEvent>>,
}

impl EventBus {
    /// `capacity` 是每个订阅者的待消费缓冲；慢客户端落后时会收到 `Lagged` 并被跳过。
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            next_id: AtomicU64::new(1),
            history: Mutex::new(VecDeque::with_capacity(HISTORY_LIMIT)),
        }
    }

    /// 分配一个不广播的事件 id；用于逐任务 SSE 的首帧快照。
    pub fn reserve_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// 广播一条事件：先写历史，再发给所有订阅者；返回带 id 的完整事件。
    ///
    /// id 在历史锁内分配：并发 `publish`（前端状态推送与任务队列同时上报）时，
    /// 入队顺序必须与 id 顺序一致，否则 `replay_since` 会返回乱序事件。
    pub fn publish(&self, name: &'static str, data: String) -> ApiEvent {
        let event = {
            let mut history = self.history.lock();
            let event = ApiEvent {
                id: self.next_id.fetch_add(1, Ordering::Relaxed),
                name,
                data,
            };
            history.push_back(event.clone());
            while history.len() > HISTORY_LIMIT {
                history.pop_front();
            }
            event
        };
        // 没有订阅者时 send 返回 Err，事件仍然进入历史，供后续重连补发。
        let _ = self.sender.send(event.clone());
        event
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ApiEvent> {
        self.sender.subscribe()
    }

    /// 最近一条已分配的事件 id；从未发布过事件时为 0。
    pub fn last_id(&self) -> u64 {
        self.next_id.load(Ordering::Relaxed).saturating_sub(1)
    }

    /// 取出 `id > last` 的历史事件；`last` 为 `None` 表示全新连接，不补发。
    pub fn replay_since(&self, last: Option<u64>) -> Replay {
        let Some(last) = last else {
            return Replay::default();
        };
        let history = self.history.lock();
        let oldest = history.front().map(|event| event.id);
        let newest = self.last_id();
        // 缺口：客户端要的位置早于历史窗口，中间的事件已经丢弃。
        // `Last-Event-ID` 来自请求头，用 saturating_add 避免 u64::MAX 在 debug 下溢出 panic。
        let gap = last < newest && oldest.is_none_or(|oldest| oldest > last.saturating_add(1));
        let events = history
            .iter()
            .filter(|event| event.id > last)
            .cloned()
            .collect();
        Replay {
            events,
            gap,
            oldest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_monotonic_and_replay_filters_by_id() {
        let bus = EventBus::new(8);
        let first = bus.publish("state", "{}".to_owned());
        let second = bus.publish("task", "{}".to_owned());
        assert_eq!(first.id + 1, second.id);
        assert_eq!(bus.last_id(), second.id);

        let replay = bus.replay_since(Some(first.id));
        assert!(!replay.gap);
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.events[0].id, second.id);
        assert_eq!(replay.events[0].name, "task");
    }

    #[test]
    fn fresh_connection_gets_no_replay() {
        let bus = EventBus::new(8);
        bus.publish("state", "{}".to_owned());
        let replay = bus.replay_since(None);
        assert!(replay.events.is_empty());
        assert!(!replay.gap);
        assert_eq!(replay.oldest, None);
    }

    #[test]
    fn replay_reports_gap_beyond_history_window() {
        let bus = EventBus::new(8);
        for index in 0..(HISTORY_LIMIT + 20) {
            bus.publish("state", format!("{{\"n\":{index}}}"));
        }
        let newest = bus.last_id();
        let replay = bus.replay_since(Some(1));
        assert!(replay.gap, "滑出窗口的 id 应标记缺口");
        assert_eq!(replay.events.len(), HISTORY_LIMIT);
        assert_eq!(replay.events.last().map(|event| event.id), Some(newest));
        assert_eq!(replay.oldest, replay.events.first().map(|event| event.id));
    }

    #[test]
    fn replay_at_latest_returns_empty_without_gap() {
        let bus = EventBus::new(8);
        bus.publish("state", "{}".to_owned());
        let replay = bus.replay_since(Some(bus.last_id()));
        assert!(replay.events.is_empty());
        assert!(!replay.gap);
    }

    #[test]
    fn reserve_id_does_not_publish() {
        let bus = EventBus::new(8);
        let reserved = bus.reserve_id();
        assert_eq!(bus.last_id(), reserved);
        assert!(bus.replay_since(None).events.is_empty());
        let next = bus.publish("state", "{}".to_owned());
        assert_eq!(next.id, reserved + 1);
    }
}
