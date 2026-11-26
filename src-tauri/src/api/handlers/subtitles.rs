//! 字幕：记录检索、编辑、导出与生成任务。

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tauri::Manager;

use bgm_core::export::safe_file_name;

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, payload};
use crate::api::handlers::tasks::submit_task;
use crate::api::request::{
    body_value, ensure_idle, query_usize, require_query, separation_target, storage_error,
    subtitle_format, track_key,
};
use crate::state::AppState;
use crate::subtitles::{self, SubtitleSegment};
use crate::tasks::TaskRequest;

// ---------------- 字幕 ----------------

/// 字幕记录列表；`?q=` 在曲名、文件名、key 与正文中做不区分大小写的包含匹配。
pub(crate) async fn subtitles_list(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let limit = query_usize(&query, "limit", 200, 1, 1000)?;
    let offset = query_usize(&query, "offset", 0, 0, usize::MAX)?;
    // 默认只返回元信息：列表可能包含大量正文，完整内容请走 /v1/subtitles/track。
    let include_content = query.get("include").is_some_and(|value| {
        value
            .split(',')
            .any(|part| part.trim().eq_ignore_ascii_case("content"))
    });
    let app = api.app();
    let mut records = subtitles::list_stored(&app);
    if let Some(needle) = query
        .get("q")
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
    {
        records.retain(|record| {
            record.title.to_lowercase().contains(&needle)
                || record.key.to_lowercase().contains(&needle)
                || record
                    .file_name
                    .as_deref()
                    .is_some_and(|name| name.to_lowercase().contains(&needle))
                || record.srt.to_lowercase().contains(&needle)
                || record.lrc.to_lowercase().contains(&needle)
        });
    }
    let total = records.len();
    let mut page = Vec::new();
    for record in records.into_iter().skip(offset).take(limit) {
        let segment_count = record.segments.len();
        let mut value = serde_json::to_value(&record)
            .map_err(|error| ApiError::internal(format!("字幕记录序列化失败：{error}")))?;
        if let Some(object) = value.as_object_mut() {
            object.insert("segmentCount".to_owned(), json!(segment_count));
            if !include_content {
                object.remove("srt");
                object.remove("lrc");
                object.remove("segments");
            }
        }
        page.push(value);
    }
    Ok(payload(json!({
        "records": page,
        "total": total,
        "limit": limit,
        "offset": offset,
        "includeContent": include_content,
    })))
}

pub(crate) async fn subtitle_get(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let key = require_query(&query, "key")?;
    let record = subtitles::load_stored(&api.app(), key)
        .ok_or_else(|| ApiError::not_found(format!("字幕记录不存在：{key}")))?;
    Ok(payload(json!(record)))
}

pub(crate) async fn subtitle_update(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let value = body_value(body);
    let key = track_key(&query, &value)?;
    let raw = value
        .get("segments")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("缺少 segments：请求体应为 {\"segments\":[…]}"))?;
    let segments: Vec<SubtitleSegment> = serde_json::from_value(raw)
        .map_err(|error| ApiError::bad_request(format!("segments 格式错误：{error}")))?;
    ensure_idle(&key)?;
    let app = api.app();
    if subtitles::load_stored(&app, &key).is_none() {
        return Err(ApiError::not_found(format!("字幕记录不存在：{key}")));
    }
    let stored = subtitles::update_stored(&app, &key, segments).map_err(storage_error)?;
    Ok(payload(json!(stored)))
}

pub(crate) async fn subtitle_update_text(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let value = body_value(body);
    let key = track_key(&query, &value)?;
    let format = subtitle_format(&query, &value)?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("缺少 text：请求体应为 {\"text\":\"…\"}"))?;
    ensure_idle(&key)?;
    let app = api.app();
    let state = app.state::<AppState>();
    // 先确认记录存在：否则解析成功后的写入会以 400 报「尚未生成」，语义不对。
    let Some(stored) = subtitles::load_stored(&app, &key) else {
        return Err(ApiError::not_found(format!("字幕记录不存在：{key}")));
    };
    let duration_ms = stored
        .duration_ms
        .or_else(|| state.song(&key).and_then(|song| song.duration_ms));
    let segments = match format.as_str() {
        "srt" => subtitles::parse_srt(text),
        _ => subtitles::parse_lrc(text, duration_ms.map(|ms| f64::from(ms) / 1000.0)),
    }
    .map_err(|error| ApiError::bad_request(format!("{error:#}")))?;
    let stored = subtitles::update_stored(&app, &key, segments).map_err(storage_error)?;
    Ok(payload(json!(stored)))
}

pub(crate) async fn subtitle_delete(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let key = require_query(&query, "key")?;
    ensure_idle(key)?;
    let removed = subtitles::delete_stored(&api.app(), key)
        .map_err(|error| ApiError::internal(format!("{error:#}")))?;
    Ok(payload(json!({ "key": key, "deleted": removed })))
}

pub(crate) async fn subtitle_export(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    api.authorize(&headers, None)?;
    let key = require_query(&query, "key")?;
    let format = subtitle_format(&query, &Value::Null)?;
    let app = api.app();
    let stored = subtitles::load_stored(&app, key)
        .ok_or_else(|| ApiError::not_found(format!("字幕记录不存在：{key}")))?;
    let content = if format == "lrc" {
        stored.lrc
    } else {
        stored.srt
    };
    let title = app
        .state::<AppState>()
        .song(key)
        .map(|song| song.title)
        .unwrap_or_else(|| stored.title.clone());
    let file_name = format!("{}.{format}", safe_file_name(&title));
    let disposition = HeaderValue::from_str(&content_disposition(&file_name))
        .map_err(|error| ApiError::internal(format!("无法生成下载文件名：{error}")))?;
    let mut response = (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        content,
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    Ok(response)
}

/// 下载文件名：ASCII 兜底 + RFC 5987 的 UTF-8 原文。
pub(crate) fn content_disposition(file_name: &str) -> String {
    let fallback: String = file_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | ' ') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let fallback = if fallback.trim().is_empty() {
        "subtitles.txt".to_owned()
    } else {
        fallback
    };
    format!(
        "attachment; filename=\"{fallback}\"; filename*=UTF-8''{}",
        percent_encode(file_name)
    )
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(crate) async fn subtitles_generate(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Result<Response, ApiError> {
    api.authorize(&headers, None)?;
    let value = body_value(body);
    let key = track_key(&query, &value)?;
    let model = value
        .get("separationModel")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request("缺少 separationModel：先通过 /v1/separation/models 选择人声模型")
        })?
        .to_owned();
    let target = separation_target(value.get("separationTarget"))?;
    let app = api.app();
    if app.state::<AppState>().song(&key).is_none() {
        return Err(ApiError::not_found(format!("曲目不存在：{key}")));
    }
    // UI 的生成命令不经过任务队列，提交前先看同一个 key 是否已被占用。
    ensure_idle(&key)?;
    submit_task(
        &api,
        TaskRequest::Subtitles {
            key,
            separation_model: model,
            separation_target: target,
        },
    )
}
