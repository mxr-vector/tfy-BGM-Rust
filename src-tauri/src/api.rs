//! 本机开放接口：HTTP JSON API + SSE 事件流，供 OBS、Stream Deck、脚本等外部项目接入。
//!
//! 设计约束：
//! - 只监听 `127.0.0.1`，默认端口 8737，避免暴露到局域网；
//! - 浏览器来源默认只信任本机（`tauri://localhost` 与 `localhost` / `127.0.0.1` / `[::1]`），
//!   可在「基本系统配置 · 第三方接入」里改为放行全部来源或追加自定义来源；
//!   非浏览器客户端（curl、Stream Deck 等）不带 `Origin`，不受影响；
//! - `config.json` 的 `api.token` 非空时，所有请求都必须带 `Authorization: Bearer`；
//!   界面保存 token 后立即生效，无需重启；
//! - 播放控制由前端执行，Rust 只负责转发命令与缓存最近一次状态快照；
//! - 曲库读取直接走 [`AppState`]，不依赖界面是否打开。
//!
//! 接口清单见 [`endpoints`]，界面「第三方接入」直接展示同一份数据。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bgm_core::library::audio_extension;
use bgm_core::{ApiConfig, CorsConfig, CorsMode};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{broadcast, oneshot};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::state::AppState;

/// 发给前端的事件名；与 `ui/src/lib/constants.ts` 的 `EVENTS.apiCommand` 对应。
pub const API_COMMAND_EVENT: &str = "api-command";

/// 前端执行单条命令的最长等待时间。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(6);
const EVENT_CHANNEL_CAPACITY: usize = 64;

#[derive(Clone)]
pub struct ApiBridge {
    inner: Arc<Inner>,
}

struct Inner {
    app: AppHandle,
    enabled: bool,
    port: u16,
    token: Mutex<String>,
    cors: Mutex<CorsConfig>,
    running: AtomicBool,
    ready: AtomicBool,
    error: Mutex<Option<String>>,
    snapshot: Mutex<Option<Value>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, CommandFailure>>>>,
    events: broadcast::Sender<String>,
    next_id: AtomicU64,
}

/// 单条命令的失败：保留 HTTP 语义状态，避免所有错误都落到 503。
#[derive(Debug)]
pub(crate) struct CommandFailure {
    status: StatusCode,
    message: String,
}

impl CommandFailure {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    /// 前端回传失败时用它可以指定状态码；缺失或非法值退回 400。
    pub(crate) fn from_frontend(code: Option<u16>, message: String) -> Self {
        let status = code
            .and_then(|code| StatusCode::from_u16(code).ok())
            .filter(|status| status.is_client_error() || status.is_server_error())
            .unwrap_or(StatusCode::BAD_REQUEST);
        Self { status, message }
    }
}

/// 供界面展示的接口状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiStatus {
    pub enabled: bool,
    pub running: bool,
    pub ready: bool,
    pub port: u16,
    pub base_url: String,
    pub token: String,
    /// 当前生效的跨域策略。
    pub cors: CorsConfig,
    pub error: Option<String>,
}

/// 「第三方接入」里展示的一条接口说明；与 [`router`] 注册的路由一一对应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
}

/// 当前开放接口清单；新增路由时同步补充，界面会自动展示。
pub fn endpoints() -> Vec<ApiEndpoint> {
    vec![
        ApiEndpoint {
            method: "GET",
            path: "/v1/health",
            summary: "健康检查",
            description: "返回服务版本、监听端口与就绪状态，不校验 token。",
        },
        ApiEndpoint {
            method: "GET",
            path: "/v1/state",
            summary: "播放状态",
            description: "当前曲目、进度、音量、播放模式、队列与输出路由快照。",
        },
        ApiEndpoint {
            method: "GET",
            path: "/v1/library",
            summary: "曲库内容",
            description: "全部曲目的 key、路径、分类与音频格式，可直接用于 /v1/play。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/library/rescan",
            summary: "重新扫描曲库",
            description: "重新读取磁盘并返回最新曲库快照。",
        },
        ApiEndpoint {
            method: "GET",
            path: "/v1/events",
            summary: "状态事件流（SSE）",
            description: "每秒推送一次播放状态；SSE 无法自定义请求头，可用 ?token= 鉴权。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/play",
            summary: "播放曲目",
            description: "请求体 {\"key\":\"分类/曲名\",\"queue\":[\"分类/曲名\"]}，queue 可选。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/pause",
            summary: "暂停",
            description: "暂停当前播放；未在播放时不做任何操作。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/resume",
            summary: "继续播放",
            description: "从暂停位置继续播放。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/toggle",
            summary: "播放 / 暂停切换",
            description: "等价于界面上的播放按钮。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/next",
            summary: "下一首",
            description: "按当前播放模式切到下一首。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/previous",
            summary: "上一首",
            description: "播放超过重播阈值时先回到开头，否则切到上一首。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/seek",
            summary: "跳转进度",
            description: "请求体 {\"seconds\":12.5}，单位为秒。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/volume",
            summary: "设置主音量",
            description: "请求体 {\"volume\":0.6}，取值 0~1。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/crossfade",
            summary: "设置淡入淡出",
            description: "请求体 {\"seconds\":2}，取值 0~12，0 表示硬切。",
        },
        ApiEndpoint {
            method: "GET",
            path: "/v1/outputs",
            summary: "输出设备与路由",
            description: "可用输出设备、每路路由的开关、独立音量与静音状态。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/outputs",
            summary: "更新一路输出",
            description: "请求体 {\"deviceId\":\"...\",\"enabled\":true,\"volume\":0.8,\"muted\":false}。",
        },
        ApiEndpoint {
            method: "POST",
            path: "/v1/command/{action}",
            summary: "通用命令",
            description: "把 action 直接转给界面执行，参数放在 params 里；未知 action 返回 404。",
        },
    ]
}

impl ApiBridge {
    /// 创建接口状态；真正的监听在 [`ApiBridge::serve`] 中建立。
    pub fn new(app: AppHandle, config: &ApiConfig) -> Self {
        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                app,
                enabled: config.enabled,
                port: config.port,
                token: Mutex::new(config.token.trim().to_owned()),
                cors: Mutex::new(config.cors.clone()),
                running: AtomicBool::new(false),
                ready: AtomicBool::new(false),
                error: Mutex::new(None),
                snapshot: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                events,
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// 界面保存 token / 跨域策略后立即生效，不需要重启应用。
    pub fn apply_config(&self, token: &str, cors: &CorsConfig) {
        *self.inner.token.lock() = token.trim().to_owned();
        *self.inner.cors.lock() = cors.clone();
    }

    /// 该浏览器来源是否允许跨域访问；非浏览器请求不带 Origin，由中间件直接放行。
    fn cors_allows(&self, origin: &str) -> bool {
        if origin_allowed(origin) {
            return true;
        }
        let cors = self.inner.cors.lock().clone();
        match cors.mode {
            CorsMode::Local => false,
            CorsMode::Any => true,
            CorsMode::Custom => cors
                .origins
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(origin)),
        }
    }

    /// 启动接口服务；端口占用等错误记录在状态里，不阻止应用启动。
    ///
    /// 调用前应先把本对象 `manage` 进 Tauri，避免服务刚启动时请求找不到状态。
    pub fn serve(&self) {
        if !self.inner.enabled {
            return;
        }

        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.inner.port));
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                if let Err(error) = listener.set_nonblocking(true) {
                    *self.inner.error.lock() = Some(format!("监听套接字设置失败：{error}"));
                    return;
                }
                self.inner.running.store(true, Ordering::Relaxed);
                let router = router(self.clone());
                tauri::async_runtime::spawn(async move {
                    match tokio::net::TcpListener::from_std(listener) {
                        Ok(listener) => {
                            if let Err(error) = axum::serve(listener, router).await {
                                eprintln!("[api] 服务退出：{error}");
                            }
                        }
                        Err(error) => eprintln!("[api] 无法接管监听套接字：{error}"),
                    }
                });
            }
            Err(error) => {
                *self.inner.error.lock() = Some(format!(
                    "无法监听 127.0.0.1:{}（端口可能被占用）：{error}",
                    self.inner.port
                ));
            }
        }
    }

    pub fn app(&self) -> AppHandle {
        self.inner.app.clone()
    }

    pub fn status(&self) -> ApiStatus {
        ApiStatus {
            enabled: self.inner.enabled,
            running: self.inner.running.load(Ordering::Relaxed),
            ready: self.inner.ready.load(Ordering::Relaxed),
            port: self.inner.port,
            base_url: format!("http://127.0.0.1:{}", self.inner.port),
            token: self.inner.token.lock().clone(),
            cors: self.inner.cors.lock().clone(),
            error: self.inner.error.lock().clone(),
        }
    }

    /// 前端上报状态快照；同时广播给所有 SSE 订阅者。
    pub fn publish(&self, snapshot: Value) {
        let text = json!({ "type": "state", "state": snapshot }).to_string();
        *self.inner.snapshot.lock() = Some(snapshot);
        self.inner.ready.store(true, Ordering::Relaxed);
        let _ = self.inner.events.send(text);
    }

    /// 前端回传命令执行结果。
    pub fn reply(&self, id: u64, result: Result<Value, CommandFailure>) {
        let sender = self.inner.pending.lock().remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }

    /// 把 HTTP 命令转发给前端并等待执行结果。
    async fn command(&self, action: &str, params: Value) -> Result<Value, CommandFailure> {
        if !self.inner.ready.load(Ordering::Relaxed) {
            return Err(CommandFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "界面尚未就绪，请等待应用完成启动",
            ));
        }

        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.inner.pending.lock().insert(id, sender);

        let payload = json!({ "id": id, "action": action, "params": params });
        if let Err(error) = self.inner.app.emit(API_COMMAND_EVENT, payload) {
            self.inner.pending.lock().remove(&id);
            return Err(CommandFailure::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("无法把命令发送到界面：{error}"),
            ));
        }

        match tokio::time::timeout(COMMAND_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(CommandFailure::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "界面已关闭命令通道",
            )),
            Err(_) => {
                self.inner.pending.lock().remove(&id);
                Err(CommandFailure::new(
                    StatusCode::GATEWAY_TIMEOUT,
                    format!("命令 {action} 执行超时"),
                ))
            }
        }
    }

    fn authorize(&self, headers: &HeaderMap, query_token: Option<&str>) -> Result<(), ApiError> {
        let token = self.inner.token.lock().clone();
        if token.is_empty() {
            return Ok(());
        }
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(str::trim);
        let custom = headers
            .get("x-tfy-token")
            .and_then(|value| value.to_str().ok())
            .map(str::trim);
        let supplied = bearer.or(custom).or(query_token);
        if supplied == Some(token.as_str()) {
            Ok(())
        } else {
            Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "缺少或错误的 API token",
            ))
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<String> {
        self.inner.events.subscribe()
    }
}

/// 路由与跨域中间件；所有接口都在 `/v1` 下，避免与其它本地服务冲突。
fn router(bridge: ApiBridge) -> Router {
    let cors_state = bridge.clone();
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/state", get(get_state))
        .route("/v1/library", get(library))
        .route("/v1/library/rescan", post(rescan))
        .route("/v1/events", get(events))
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
        .route("/v1/command/{action}", post(command))
        .layer(middleware::from_fn_with_state(cors_state, cors))
        .with_state(bridge)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({ "ok": false, "error": self.message });
        (self.status, Json(body)).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn payload(data: Value) -> Json<Value> {
    Json(json!({ "ok": true, "data": data }))
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
        "GET, POST, OPTIONS".parse().unwrap(),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "Content-Type, Authorization, X-TFY-Token".parse().unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
}

/// 来源是否属于本机：应用 WebView（`tauri://localhost` 等）或回环 HTTP 页面。
fn origin_allowed(origin: &str) -> bool {
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

async fn health(State(api): State<ApiBridge>) -> Json<Value> {
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

async fn get_state(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let snapshot = api.inner.snapshot.lock().clone();
    match snapshot {
        Some(state) => Ok(payload(state)),
        None => Err(ApiError::unavailable("界面尚未完成启动")),
    }
}

async fn library(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let app_state = app.state::<AppState>();
    let snapshot = app_state.library_snapshot();
    let mut value = serde_json::to_value(&snapshot)
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

async fn rescan(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
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

async fn events(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>, ApiError>
{
    api.authorize(&headers, query.get("token").map(String::as_str))?;
    let receiver = api.subscribe();
    let stream = BroadcastStream::new(receiver).filter_map(|message| match message {
        Ok(text) => Some(Ok(Event::default().data(text))),
        Err(_) => None,
    });
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}

async fn call(api: &ApiBridge, headers: &HeaderMap, action: &str, params: Value) -> ApiResult {
    api.authorize(headers, None)?;
    let state = api
        .command(action, params)
        .await
        .map_err(|failure| ApiError::new(failure.status, failure.message))?;
    Ok(payload(state))
}

fn body_value(body: Option<Json<Value>>) -> Value {
    body.map(|Json(value)| value).unwrap_or(Value::Null)
}

async fn play(
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

async fn pause(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "pause", json!({})).await
}

async fn resume(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "resume", json!({})).await
}

async fn toggle(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "toggle", json!({})).await
}

async fn next(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "next", json!({})).await
}

async fn previous(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "previous", json!({})).await
}

async fn seek(
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

async fn volume(
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

async fn crossfade(
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

async fn outputs(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    call(&api, &headers, "outputs.list", json!({})).await
}

async fn update_output(
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

async fn command(
    State(api): State<ApiBridge>,
    Path(action): Path<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    let params = value.get("params").cloned().unwrap_or(value);
    call(&api, &headers, &action, params).await
}

#[cfg(test)]
mod tests {
    use super::{CommandFailure, origin_allowed};
    use axum::http::StatusCode;

    #[test]
    fn allows_local_and_webview_origins() {
        for origin in [
            "tauri://localhost",
            "http://tauri.localhost",
            "https://tauri.localhost",
            "http://localhost:5173",
            "http://127.0.0.1:8737",
            "https://127.0.0.1",
            "http://[::1]:5173",
            "http://LOCALHOST",
            "http://tauri.localhost:1420",
        ] {
            assert!(origin_allowed(origin), "{origin} 应被允许");
        }
    }

    #[test]
    fn rejects_external_and_malformed_origins() {
        for origin in [
            "https://evil.example",
            "http://localhost.evil.example:5173",
            "http://127.0.0.1.evil.example",
            "http://192.168.1.10:8737",
            "null",
            "file://",
            "http://[::1",
            "ws://localhost:5173",
        ] {
            assert!(!origin_allowed(origin), "{origin} 应被拒绝");
        }
    }

    #[test]
    fn frontend_codes_map_to_semantic_status() {
        let status = |code| CommandFailure::from_frontend(code, "x".to_owned()).status;
        assert_eq!(status(Some(400)), StatusCode::BAD_REQUEST);
        assert_eq!(status(Some(404)), StatusCode::NOT_FOUND);
        assert_eq!(status(Some(409)), StatusCode::CONFLICT);
        assert_eq!(status(Some(504)), StatusCode::GATEWAY_TIMEOUT);
        // 非错误状态或缺失时退回 400，避免把成功码当成错误返回。
        assert_eq!(status(Some(200)), StatusCode::BAD_REQUEST);
        assert_eq!(status(None), StatusCode::BAD_REQUEST);
    }
}
