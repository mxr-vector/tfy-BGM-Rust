//! HTTP 请求辅助：请求体 / 查询参数解析与错误分流。

use std::collections::HashMap;

use axum::Json;
use serde_json::Value;

use super::error::ApiError;

pub(crate) fn body_value(body: Option<Json<Value>>) -> Value {
    body.map(|Json(value)| value).unwrap_or(Value::Null)
}

/// 读取查询参数中的必填字符串。
pub(crate) fn require_query<'a>(
    query: &'a HashMap<String, String>,
    name: &str,
) -> Result<&'a str, ApiError> {
    query
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request(format!("缺少查询参数 {name}")))
}

/// 曲目 key：优先查询参数，其次请求体；两种写法都接受，便于不同客户端。
pub(crate) fn track_key(query: &HashMap<String, String>, body: &Value) -> Result<String, ApiError> {
    query
        .get("key")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            body.get("key")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
        .ok_or_else(|| ApiError::bad_request("缺少 key：曲目 key 形如 分类/曲名"))
}

/// 该曲目正在生成 / 分离时拒绝写操作，避免编辑被后台任务覆盖。
pub(crate) fn ensure_idle(key: &str) -> Result<(), ApiError> {
    if crate::key_lock::is_busy(key) {
        Err(ApiError::conflict(crate::key_lock::BUSY_MESSAGE))
    } else {
        Ok(())
    }
}

/// 字幕格式：`srt` / `lrc`，缺省为 `srt`。
pub(crate) fn subtitle_format(
    query: &HashMap<String, String>,
    body: &Value,
) -> Result<String, ApiError> {
    let raw = query
        .get("format")
        .map(String::as_str)
        .or_else(|| body.get("format").and_then(Value::as_str))
        .unwrap_or("srt")
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "srt" | "lrc" => Ok(raw),
        other => Err(ApiError::bad_request(format!(
            "不支持的字幕格式：{other}（仅支持 srt / lrc）"
        ))),
    }
}

/// 读取分页参数：缺省用 `default`，语法错误返回 400，超出范围钳制到 `[min, max]`。
pub(crate) fn query_usize(
    query: &HashMap<String, String>,
    name: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, ApiError> {
    match query
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        None => Ok(default),
        Some(raw) => raw
            .parse::<usize>()
            .map(|value| value.clamp(min, max))
            .map_err(|_| ApiError::bad_request(format!("{name} 必须是非负整数"))),
    }
}

/// 把存储层错误按根因分流：key 被占用是 409，I/O 失败是 500，校验 / 解析错误才是 400。
pub(crate) fn storage_error(error: anyhow::Error) -> ApiError {
    let message = format!("{error:#}");
    if message.contains(crate::key_lock::BUSY_MESSAGE) {
        return ApiError::conflict(crate::key_lock::BUSY_MESSAGE);
    }
    let io = error
        .chain()
        .any(|cause| cause.downcast_ref::<std::io::Error>().is_some());
    if io {
        ApiError::internal(message)
    } else {
        ApiError::bad_request(message)
    }
}

/// `?legacy=1`：退回无 `event:` 字段的旧 SSE 格式，兼容 `EventSource.onmessage`。
pub(crate) fn legacy_stream(query: &HashMap<String, String>) -> bool {
    query
        .get("legacy")
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
}

/// 校验 `target` 只接受 `vocals` / `instrumental`。
pub(crate) fn separation_target(value: Option<&Value>) -> Result<Option<String>, ApiError> {
    let Some(raw) = value.and_then(Value::as_str).map(str::trim) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    match raw {
        "vocals" | "instrumental" => Ok(Some(raw.to_owned())),
        other => Err(ApiError::bad_request(format!(
            "separationTarget 只能是 vocals 或 instrumental，收到：{other}"
        ))),
    }
}
