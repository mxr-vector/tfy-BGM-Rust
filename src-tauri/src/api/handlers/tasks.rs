//! 长任务：串行队列查询与逐任务 SSE 进度流。

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::extract::{Path as ApiPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::Response;
use axum::response::sse::{Event, Sse};
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, accepted, payload};
use crate::api::request::query_usize;
use crate::api::router::keep_alive;
use crate::event_bus::ApiEvent;
use crate::tasks::{TaskKind, TaskRequest, TaskStatus};

/// 长任务列表单次返回上限。
const TASK_LIST_MAX: usize = 200;

/// 提交长任务：成功返回 `202` 与任务快照，重复提交返回 `409`。
///
/// `Location` 指向任务详情，客户端可据此轮询或订阅 `/events`。
pub(crate) fn submit_task(api: &ApiBridge, request: TaskRequest) -> Result<Response, ApiError> {
    let task = api
        .tasks()
        .submit(api.app(), request)
        .map_err(|error| ApiError::conflict(error.message().to_owned()))?;
    let location = format!("/v1/tasks/{}", task.id);
    let mut response = accepted(json!({ "task": task }));
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    Ok(response)
}

// ---------------- 长任务 ----------------

pub(crate) async fn tasks_list(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let kind = match query.get("kind").map(String::as_str).map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(TaskKind::parse(raw).ok_or_else(|| {
            ApiError::bad_request(format!(
                "未知的任务类型：{raw}（可选 {}）",
                TaskKind::ALL.join(" / ")
            ))
        })?),
    };
    let status = match query.get("status").map(String::as_str).map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(TaskStatus::parse(raw).ok_or_else(|| {
            ApiError::bad_request(format!(
                "未知的任务状态：{raw}（可选 {}）",
                TaskStatus::ALL.join(" / ")
            ))
        })?),
    };
    let limit = query_usize(&query, "limit", 50, 1, TASK_LIST_MAX)?;
    let (tasks, total) = api.tasks().list(kind, status, limit);
    Ok(payload(json!({
        "tasks": tasks,
        "total": total,
        "limit": limit,
    })))
}

pub(crate) async fn task_get(
    State(api): State<ApiBridge>,
    ApiPath(id): ApiPath<String>,
    headers: HeaderMap,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let task = api
        .tasks()
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("任务不存在：{id}")))?;
    Ok(payload(json!(task)))
}

/// 逐任务进度流：首帧是当前快照，随后推送 `progress`，终态发 `done` / `failed` 后结束。
pub(crate) async fn task_events(
    State(api): State<ApiBridge>,
    ApiPath(id): ApiPath<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    api.authorize(&headers, query.get("token").map(String::as_str))?;
    let bus = api.events();
    // 先占一个比历史事件更大的 id，再订阅，最后读快照：保证帧 id 单调递增且不漏事件。
    let head_id = bus.reserve_id();
    let receiver = bus.subscribe();
    let task = api
        .tasks()
        .get(&id)
        .ok_or_else(|| ApiError::not_found(format!("任务不存在：{id}")))?;
    let name = match task.status {
        TaskStatus::Succeeded => "done",
        TaskStatus::Failed => "failed",
        _ => "snapshot",
    };
    let head = Event::default()
        .id(head_id.to_string())
        .event(name)
        .data(json!({ "type": "task", "task": task }).to_string());
    // 终态任务只发首帧：把 tail 标为 finished，保证两个分支返回同一个流类型。
    let tail = TaskEventStream {
        inner: BroadcastStream::new(receiver),
        id,
        finished: task.status.is_terminal(),
    };
    let stream = tokio_stream::once(Ok(head)).chain(tail);
    Ok(Sse::new(stream).keep_alive(keep_alive()))
}

/// 过滤单个任务的广播事件；收到终态后结束流。
struct TaskEventStream {
    inner: BroadcastStream<ApiEvent>,
    id: String,
    finished: bool,
}

impl Stream for TaskEventStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => {
                    if event.name != "task" {
                        continue;
                    }
                    let Ok(payload) = serde_json::from_str::<Value>(&event.data) else {
                        continue;
                    };
                    let Some(task) = payload.get("task") else {
                        continue;
                    };
                    if task.get("id").and_then(Value::as_str) != Some(this.id.as_str()) {
                        continue;
                    }
                    let name = match task.get("status").and_then(Value::as_str) {
                        Some("succeeded") => "done",
                        Some("failed") => "failed",
                        _ => "progress",
                    };
                    if name != "progress" {
                        this.finished = true;
                    }
                    let frame = Event::default()
                        .id(event.id.to_string())
                        .event(name)
                        .data(event.data);
                    return Poll::Ready(Some(Ok(frame)));
                }
                // 慢客户端丢帧：结束本次流，客户端重连后会收到最新快照。
                Poll::Ready(Some(Err(_))) => return Poll::Ready(None),
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
