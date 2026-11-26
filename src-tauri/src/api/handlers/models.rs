//! 模型：清单、加载任务与释放。

use std::path::PathBuf;

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde_json::{Value, json};
use tauri::{Emitter, Manager};

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, payload};
use crate::api::handlers::tasks::submit_task;
use crate::api::request::body_value;
use crate::models::LOAD_STATUS_EVENT;
use crate::state::AppState;
use crate::tasks::TaskRequest;

pub(crate) async fn models_info(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let info = tokio::task::spawn_blocking(move || app.state::<AppState>().models_info())
        .await
        .map_err(|error| ApiError::internal(format!("读取模型信息失败：{error}")))?;
    let value = serde_json::to_value(&info)
        .map_err(|error| ApiError::internal(format!("模型信息序列化失败：{error}")))?;
    Ok(payload(value))
}

pub(crate) async fn models_load(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> Result<Response, ApiError> {
    api.authorize(&headers, None)?;
    let value = body_value(body);
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ApiError::bad_request("缺少 id：先通过 /v1/models 查询模型 id"))?
        .to_owned();
    let app = api.app();
    if app.state::<AppState>().model_runtime().is_loading() {
        return Err(ApiError::conflict("已有模型加载任务正在进行"));
    }
    let worker = app.clone();
    let info = tokio::task::spawn_blocking(move || worker.state::<AppState>().models_info())
        .await
        .map_err(|error| ApiError::internal(format!("读取模型信息失败：{error}")))?;
    let Some(model) = info.models.into_iter().find(|model| model.id == id) else {
        return Err(ApiError::not_found(format!(
            "模型不存在：{id}；可通过 /v1/models 查看可用 id"
        )));
    };
    if !model.loadable {
        return Err(ApiError::conflict(format!(
            "模型 {} 缺少文件：{}",
            model.name,
            model.missing.join(" / ")
        )));
    }
    submit_task(
        &api,
        TaskRequest::AsrLoad {
            dir: PathBuf::from(&model.path),
            model_id: Some(model.id),
        },
    )
}

pub(crate) async fn models_unload(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let state = app.state::<AppState>();
    let runtime = state.model_runtime();
    if runtime.is_loading() {
        return Err(ApiError::conflict("模型正在加载中，请等待加载完成后再释放"));
    }
    let unloaded = runtime
        .unload()
        .map_err(|error| ApiError::conflict(format!("{error:#}")))?;
    // 与界面「释放模型」一致：下次启动不再自动恢复。
    state.remember_loaded_model(None);
    let _ = app.emit(LOAD_STATUS_EVENT, runtime.load_status());
    let worker = app.clone();
    let info = tokio::task::spawn_blocking(move || worker.state::<AppState>().models_info())
        .await
        .map_err(|error| ApiError::internal(format!("读取模型信息失败：{error}")))?;
    let value = serde_json::to_value(&info)
        .map_err(|error| ApiError::internal(format!("模型信息序列化失败：{error}")))?;
    Ok(payload(json!({ "unloaded": unloaded, "info": value })))
}
