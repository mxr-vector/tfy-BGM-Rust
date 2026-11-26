//! 系统与状态：健康检查、前端状态快照与全局事件流。

use std::collections::HashMap;
use std::convert::Infallible;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, payload};
use crate::api::request::legacy_stream;
use crate::api::router::{keep_alive, parse_last_event_id, sse_event};

pub(crate) async fn health(State(api): State<ApiBridge>) -> Json<Value> {
    let status = api.status();
    Json(json!({
        "ok": true,
        "data": {
            "name": "tfy-bgm",
            "version": env!("CARGO_PKG_VERSION"),
            "api": "v1",
            "running": status.running,
            "ready": status.ready,
            "port": status.port,
        }
    }))
}

pub(crate) async fn get_state(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let snapshot = api.snapshot();
    match snapshot {
        Some(state) => Ok(payload(state)),
        None => Err(ApiError::unavailable("界面尚未完成启动")),
    }
}

/// 全局事件流：先补发 `Last-Event-ID` 之后的历史，再接入实时广播。
pub(crate) async fn events(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    api.authorize(&headers, query.get("token").map(String::as_str))?;
    let legacy = legacy_stream(&query);
    // 旧格式不参与断线续传：当年的客户端既不发 Last-Event-ID，也无法区分 resync。
    let last = if legacy {
        None
    } else {
        parse_last_event_id(&headers, &query)?
    };
    let bus = api.events();
    // 先订阅再读历史：中间新产生的事件会同时出现在两处，下面按 id 去重。
    let receiver = bus.subscribe();
    let newest = bus.last_id();
    // 应用重启后 id 会从 1 重新计数；客户端带回更大的 id 说明历史已换代。
    let reset = last.is_some_and(|last| last > newest);
    let mut replay = bus.replay_since(last);
    if reset {
        replay.events.clear();
        replay.gap = false;
    }

    let mut head: Vec<Result<Event, Infallible>> = Vec::new();
    if reset {
        let data = json!({
            "type": "resync",
            "reason": "reset",
            "lastEventId": last,
            "oldestEventId": Value::Null,
        });
        head.push(Ok(Event::default()
            .id(newest.to_string())
            .event("resync")
            .data(data.to_string())));
    } else if replay.gap {
        // 缺口：客户端要的位置已滑出历史窗口，先让它重新拉一次 /v1/state。
        let gap_id = replay
            .oldest
            .map(|id| id.saturating_sub(1))
            .unwrap_or_else(|| bus.last_id());
        let data = json!({
            "type": "resync",
            "reason": "window",
            "lastEventId": last,
            "oldestEventId": replay.oldest,
        });
        head.push(Ok(Event::default()
            .id(gap_id.to_string())
            .event("resync")
            .data(data.to_string())));
    }
    head.extend(
        replay
            .events
            .iter()
            .map(|event| Ok(sse_event(event, legacy))),
    );

    // 已补发过的事件在实时通道里会重复出现；按 id 丢弃，避免客户端看到重复帧。
    let max_replayed = replay.events.last().map(|event| event.id);
    // 换代后从当前最新 id 重新起算，旧 id 不能再当过滤下限，否则新事件会被全部丢弃。
    let floor = if reset { Some(newest) } else { last };
    let live = BroadcastStream::new(receiver).filter_map(move |message| {
        let event = message.ok()?;
        // 旧格式只推送 state：onmessage 客户端不认识 task / resync 的 data 结构。
        if legacy && event.name != "state" {
            return None;
        }
        if max_replayed.is_some_and(|max| event.id <= max) {
            return None;
        }
        if floor.is_some_and(|floor| event.id <= floor) {
            return None;
        }
        Some(Ok(sse_event(&event, legacy)))
    });
    let stream = tokio_stream::iter(head).chain(live);
    Ok(Sse::new(stream).keep_alive(keep_alive()))
}
