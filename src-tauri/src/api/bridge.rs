//! 接口运行状态：监听生命周期、鉴权、跨域、命令转发与状态快照缓存。

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::http::{HeaderMap, StatusCode, header};
use bgm_core::{ApiConfig, CorsConfig, CorsMode};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

use super::API_COMMAND_EVENT;
use super::error::ApiError;
use super::router::{origin_allowed, router};
use crate::event_bus::EventBus;
use crate::tasks::TaskStore;

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
    /// `/v1/events` 与逐任务 SSE 共用的广播 + 历史。
    events: Arc<EventBus>,
    /// 长任务串行队列。
    tasks: Arc<TaskStore>,
    next_id: AtomicU64,
}

/// 单条命令的失败：保留 HTTP 语义状态，避免所有错误都落到 503。
#[derive(Debug)]
pub(crate) struct CommandFailure {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

impl CommandFailure {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
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

impl ApiBridge {
    /// 创建接口状态；真正的监听在 [`ApiBridge::serve`] 中建立。
    pub fn new(app: AppHandle, config: &ApiConfig) -> Self {
        let events = Arc::new(EventBus::new(EVENT_CHANNEL_CAPACITY));
        let tasks = Arc::new(TaskStore::new(Arc::clone(&events)));
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
                tasks,
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
    pub(crate) fn cors_allows(&self, origin: &str) -> bool {
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

    pub(crate) fn app(&self) -> AppHandle {
        self.inner.app.clone()
    }

    pub(crate) fn events(&self) -> Arc<EventBus> {
        Arc::clone(&self.inner.events)
    }

    pub(crate) fn tasks(&self) -> Arc<TaskStore> {
        Arc::clone(&self.inner.tasks)
    }

    pub(crate) fn status(&self) -> ApiStatus {
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
        self.inner.events.publish("state", text);
    }

    /// 前端回传命令执行结果。
    pub fn reply(&self, id: u64, result: Result<Value, CommandFailure>) {
        let sender = self.inner.pending.lock().remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }

    /// 把 HTTP 命令转发给前端并等待执行结果。
    pub(crate) async fn command(
        &self,
        action: &str,
        params: Value,
    ) -> Result<Value, CommandFailure> {
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

    pub(crate) fn authorize(
        &self,
        headers: &HeaderMap,
        query_token: Option<&str>,
    ) -> Result<(), ApiError> {
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
}

impl ApiBridge {
    /// 最近一次前端上报的状态快照。
    pub(crate) fn snapshot(&self) -> Option<Value> {
        self.inner.snapshot.lock().clone()
    }
}
