//! 播放控制与通用命令转发。

use axum::Json;
use axum::extract::{Path as ApiPath, State};
use axum::http::HeaderMap;
use serde_json::{Value, json};

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, payload};
use crate::api::request::body_value;

async fn call(api: &ApiBridge, headers: &HeaderMap, action: &str, params: Value) -> ApiResult {
    api.authorize(headers, None)?;
    let state = api
        .command(action, params)
        .await
        .map_err(|failure| ApiError::new(failure.status, failure.message))?;
    Ok(payload(state))
}

pub(crate) async fn play(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    if value.get("key").and_then(Value::as_str).is_none() {
        return Err(ApiError::bad_request(
            "缺少 key：先通过 /v1/library 查询曲目",
        ));
    }
    call(&api, &headers, "play", value).await
}

pub(crate) async fn pause(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "pause", json!({})).await
}

pub(crate) async fn resume(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "resume", json!({})).await
}

pub(crate) async fn toggle(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "toggle", json!({})).await
}

pub(crate) async fn next(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "next", json!({})).await
}

pub(crate) async fn previous(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "previous", json!({})).await
}

pub(crate) async fn seek(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    if value.get("seconds").and_then(Value::as_f64).is_none() {
        return Err(ApiError::bad_request("缺少 seconds"));
    }
    call(&api, &headers, "seek", value).await
}

pub(crate) async fn volume(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    if value.get("volume").and_then(Value::as_f64).is_none() {
        return Err(ApiError::bad_request("缺少 volume（0~1）"));
    }
    call(&api, &headers, "volume", value).await
}

pub(crate) async fn crossfade(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    if value.get("seconds").and_then(Value::as_f64).is_none() {
        return Err(ApiError::bad_request("缺少 seconds（0~12）"));
    }
    call(&api, &headers, "crossfade", value).await
}

pub(crate) async fn outputs(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "outputs.list", json!({})).await
}

pub(crate) async fn update_output(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    if value.get("deviceId").and_then(Value::as_str).is_none() {
        return Err(ApiError::bad_request("缺少 deviceId"));
    }
    call(&api, &headers, "outputs.set", value).await
}

pub(crate) async fn command(
    State(api): State<ApiBridge>,
    ApiPath(action): ApiPath<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    let params = value.get("params").cloned().unwrap_or(value);
    call(&api, &headers, &action, params).await
}
