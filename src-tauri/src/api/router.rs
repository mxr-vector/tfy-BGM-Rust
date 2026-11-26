//! 路由注册、跨域中间件与 SSE 帧辅助。

use std::collections::HashMap;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};

use super::bridge::ApiBridge;
use super::error::ApiError;
use super::handlers::{
    command, crossfade, events, get_state, health, library, models_info, models_load,
    models_unload, next, outputs, pause, play, previous, rescan, resume, seek, separation_create,
    separation_models, separation_result, subtitle_delete, subtitle_export, subtitle_get,
    subtitle_update, subtitle_update_text, subtitles_generate, subtitles_list, task_events,
    task_get, tasks_list, toggle, update_output, volume,
};
use crate::event_bus::ApiEvent;

/// 路由与跨域中间件；所有接口都在 `/v1` 下，避免与其它本地服务冲突。
pub(crate) fn router(bridge: ApiBridge) -> Router {
    let cors_state = bridge.clone();
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/state", get(get_state))
        .route("/v1/events", get(events))
        .route("/v1/library", get(library))
        .route("/v1/library/rescan", post(rescan))
        .route("/v1/play", post(play))
        .route("/v1/pause", post(pause))
        .route("/v1/resume", post(resume))
        .route("/v1/toggle", post(toggle))
        .route("/v1/next", post(next))
        .route("/v1/previous", post(previous))
        .route("/v1/seek", post(seek))
        .route("/v1/volume", post(volume))
        .route("/v1/crossfade", post(crossfade))
        .route("/v1/outputs", get(outputs).post(update_output))
        // 字幕：记录检索、编辑、导出与生成任务
        .route("/v1/subtitles", get(subtitles_list))
        .route(
            "/v1/subtitles/track",
            get(subtitle_get)
                .put(subtitle_update)
                .delete(subtitle_delete),
        )
        .route("/v1/subtitles/text", put(subtitle_update_text))
        .route("/v1/subtitles/export", get(subtitle_export))
        .route("/v1/subtitles/generate", post(subtitles_generate))
        // 人声分离：模型目录、已有结果与独立任务
        .route("/v1/separation/models", get(separation_models))
        .route("/v1/separation/result", get(separation_result))
        .route("/v1/separation", post(separation_create))
        // 模型：清单、加载任务、释放
        .route("/v1/models", get(models_info))
        .route("/v1/models/load", post(models_load))
        .route("/v1/models/unload", post(models_unload))
        // 长任务队列
        .route("/v1/tasks", get(tasks_list))
        .route("/v1/tasks/{id}", get(task_get))
        .route("/v1/tasks/{id}/events", get(task_events))
        .route("/v1/command/{action}", post(command))
        .layer(middleware::from_fn_with_state(cors_state, cors))
        .with_state(bridge)
}

/// 跨域策略：默认只信任本机来源，非浏览器客户端（不带 `Origin`）直接放行。
///
/// 服务本身已在回环地址上，这里再拦一层浏览器来源，避免用户访问的任意网页
/// 通过跨域请求读取曲库路径或控制播放。本机的 `localhost` 页面（例如自建控制台）
/// 与应用自身的 WebView 仍然可用；放行范围由「第三方接入」里的 CORS 配置决定。
async fn cors(State(api): State<ApiBridge>, request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if let Some(origin) = origin.as_deref()
        && !api.cors_allows(origin)
    {
        return ApiError::forbidden("不允许的来源访问本机接口，请检查 CORS 配置").into_response();
    }

    if request.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        cors_headers(response.headers_mut(), origin.as_deref());
        return response;
    }
    let mut response = next.run(request).await;
    cors_headers(response.headers_mut(), origin.as_deref());
    response
}

fn cors_headers(headers: &mut HeaderMap, origin: Option<&str>) {
    // 回写被中间件放行的来源（本机或 CORS 配置允许的来源）；脚本请求没有 Origin，不需要该头。
    if let Some(origin) = origin
        && let Ok(value) = origin.parse::<HeaderValue>()
    {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
        headers.insert(header::VARY, "Origin".parse().unwrap());
    }
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET, POST, PUT, DELETE, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Content-Type, Authorization, X-TFY-Token, Last-Event-ID"
            .parse()
            .unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
}

/// 来源是否属于本机：应用 WebView（`tauri://localhost` 等）或回环 HTTP 页面。
pub(crate) fn origin_allowed(origin: &str) -> bool {
    if matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    ) {
        return true;
    }

    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6：取到 `]` 为止，避免把端口一起比较。
        match stripped.split_once(']') {
            Some((host, _)) => &authority[..host.len() + 2],
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case("tauri.localhost")
        || host == "127.0.0.1"
        || host == "[::1]"
}

/// 从 `Last-Event-ID` 头或 `?lastEventId=` 读取续传位置。
///
/// EventSource 重连时会自动带上该头；非浏览器客户端可以用查询参数。
pub(crate) fn parse_last_event_id(
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<Option<u64>, ApiError> {
    let from_header = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let from_query = query
        .get("lastEventId")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match from_header.or(from_query) {
        Some(raw) => raw
            .parse::<u64>()
            .map(Some)
            .map_err(|_| ApiError::bad_request(format!("Last-Event-ID 不是有效的事件 id：{raw}"))),
        None => Ok(None),
    }
}

/// 把总线事件转成标准 SSE 帧：`id:` + `event:` + `data:`。
///
/// `legacy` 为 true 时不写 `event:`，恢复旧版无事件名的帧，
/// 让只监听 `EventSource.onmessage` 的脚本继续工作。
pub(crate) fn sse_event(event: &ApiEvent, legacy: bool) -> Event {
    let frame = Event::default()
        .id(event.id.to_string())
        .data(event.data.clone());
    if legacy {
        frame
    } else {
        frame.event(event.name)
    }
}

pub(crate) fn keep_alive() -> KeepAlive {
    KeepAlive::new()
        .interval(Duration::from_secs(15))
        .text("ping")
}
