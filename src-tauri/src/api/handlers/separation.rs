//! 人声分离：模型目录、已有结果与独立任务。

use std::collections::HashMap;
use std::fs;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use serde_json::{Value, json};
use tauri::Manager;

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, payload};
use crate::api::handlers::tasks::submit_task;
use crate::api::request::{body_value, ensure_idle, require_query, separation_target, track_key};
use crate::separation;
use crate::state::AppState;
use crate::tasks::TaskRequest;

pub(crate) async fn separation_models(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let worker = app.clone();
    // 目录扫描与模型文件检查都是同步 I/O，放到阻塞线程池。
    let catalog = tokio::task::spawn_blocking(move || {
        let state = worker.state::<AppState>();
        separation::catalog(&worker, &state.models_root())
    })
    .await
    .map_err(|error| ApiError::internal(format!("读取分离模型失败：{error}")))?;
    let value = serde_json::to_value(&catalog)
        .map_err(|error| ApiError::internal(format!("分离模型序列化失败：{error}")))?;
    Ok(payload(value))
}

pub(crate) async fn separation_result(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    api.authorize(&headers, None)?;
    let key = require_query(&query, "key")?;
    let Some(record) = separation::load_record(&api.app(), key)
        .map_err(|error| ApiError::internal(format!("{error:#}")))?
    else {
        return Err(ApiError::not_found(format!(
            "该曲目尚未执行人声分离：{key}"
        )));
    };
    let mut files = Vec::new();
    for (kind, field) in [
        ("vocals", "vocalsPath"),
        ("accompaniment", "accompanimentPath"),
    ] {
        let path = record.get(field).and_then(Value::as_str);
        let bytes = path
            .and_then(|path| fs::metadata(path).ok())
            .map(|meta| meta.len());
        files.push(json!({
            "kind": kind,
            "path": path,
            "exists": bytes.is_some(),
            "bytes": bytes.unwrap_or(0),
        }));
    }
    Ok(payload(json!({ "separation": record, "files": files })))
}

pub(crate) async fn separation_create(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Option<Json<Value>>,
) -> Result<Response, ApiError> {
    api.authorize(&headers, None)?;
    let value = body_value(body);
    let key = track_key(&query, &value)?;
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request("缺少 model：先通过 /v1/separation/models 选择人声模型")
        })?
        .to_owned();
    let target = separation_target(value.get("target"))?;
    let app = api.app();
    if app.state::<AppState>().song(&key).is_none() {
        return Err(ApiError::not_found(format!("曲目不存在：{key}")));
    }
    ensure_idle(&key)?;
    submit_task(&api, TaskRequest::Separation { key, model, target })
}
