//! 曲库：快照读取与重新扫描。

use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::{Value, json};
use tauri::Manager;

use bgm_core::library::audio_extension;

use crate::api::bridge::ApiBridge;
use crate::api::error::{ApiError, ApiResult, payload};
use crate::state::AppState;

pub(crate) async fn library(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let app_state = app.state::<AppState>();
    let snapshot = app_state.library_snapshot();
    let mut value = serde_json::to_value(snapshot.as_ref())
        .map_err(|error| ApiError::unavailable(format!("曲库序列化失败：{error}")))?;
    // 显式补上 format，方便外部客户端按格式筛选。
    if let Some(songs) = value.get_mut("songs").and_then(Value::as_array_mut) {
        for song in songs {
            let format = song
                .get("fileName")
                .and_then(Value::as_str)
                .and_then(audio_extension);
            song["format"] = json!(format);
        }
    }
    Ok(payload(value))
}

pub(crate) async fn rescan(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    // 全量扫描会持有操作锁并做同步磁盘遍历，放到阻塞线程池，避免占住 async worker。
    let worker = app.clone();
    let snapshot = tokio::task::spawn_blocking(move || worker.state::<AppState>().rescan(&worker))
        .await
        .map_err(|error| ApiError::unavailable(format!("重新扫描失败：{error}")))?
        .map_err(|error| ApiError::unavailable(format!("重新扫描失败：{error:#}")))?;
    let value = serde_json::to_value(&snapshot)
        .map_err(|error| ApiError::unavailable(format!("曲库序列化失败：{error}")))?;
    Ok(payload(value))
}
