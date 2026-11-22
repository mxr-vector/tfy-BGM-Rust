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
//! - 曲库读取直接走 [`AppState`]，不依赖界面是否打开；
//! - 使用模型的长耗时任务（字幕生成 / 人声分离 / ASR 加载）统一进入
//!   [`crate::tasks::TaskStore`] 的串行队列，提交返回 `202` 与任务 id，
//!   进度通过 `GET /v1/tasks/{id}/events` 的 SSE 推送；
//! - SSE 严格使用标准字段：`id`（单调递增，支持 `Last-Event-ID` 续传）、
//!   `event`（`state` / `task` / `snapshot` / `progress` / `done` / `failed` / `resync`）、
//!   `data`（JSON 文本）。
//!
//! 接口清单见 [`catalog`]，界面「第三方接入」按 [`categories`] 的业务分类展示同一份数据。

use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::{Path as ApiPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use bgm_core::export::safe_file_name;
use bgm_core::library::audio_extension;
use bgm_core::{ApiConfig, CorsConfig, CorsMode};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::event_bus::{ApiEvent, EventBus};
use crate::models::LOAD_STATUS_EVENT;
use crate::separation;
use crate::state::AppState;
use crate::subtitles::{self, SubtitleSegment};
use crate::tasks::{TaskKind, TaskRequest, TaskStatus, TaskStore};

/// 发给前端的事件名；与 `ui/src/lib/constants.ts` 的 `EVENTS.apiCommand` 对应。
pub const API_COMMAND_EVENT: &str = "api-command";

/// 前端执行单条命令的最长等待时间。
const COMMAND_TIMEOUT: Duration = Duration::from_secs(6);
const EVENT_CHANNEL_CAPACITY: usize = 64;
/// 长任务列表单次返回上限。
const TASK_LIST_MAX: usize = 200;

// ---------------- 业务分类 ----------------

/// 接口的业务分类；顺序即界面展示顺序。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCategory {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

const CAT_SYSTEM: &str = "system";
const CAT_PLAYBACK: &str = "playback";
const CAT_LIBRARY: &str = "library";
const CAT_OUTPUT: &str = "output";
const CAT_SUBTITLES: &str = "subtitles";
const CAT_SEPARATION: &str = "separation";
const CAT_MODELS: &str = "models";
const CAT_TASKS: &str = "tasks";
const CAT_COMMAND: &str = "command";

/// 当前接口的业务分类清单（固定顺序）。
pub fn categories() -> Vec<ApiCategory> {
    vec![
        ApiCategory {
            id: CAT_SYSTEM,
            label: "系统与状态",
            description: "健康检查、播放状态快照与全局事件流",
        },
        ApiCategory {
            id: CAT_PLAYBACK,
            label: "播放控制",
            description: "播放、切歌、进度、音量与淡入淡出",
        },
        ApiCategory {
            id: CAT_LIBRARY,
            label: "曲库",
            description: "曲目列表与重新扫描",
        },
        ApiCategory {
            id: CAT_OUTPUT,
            label: "音频输出",
            description: "输出设备与多路路由",
        },
        ApiCategory {
            id: CAT_SUBTITLES,
            label: "字幕",
            description: "生成任务、记录检索、编辑与导出",
        },
        ApiCategory {
            id: CAT_SEPARATION,
            label: "人声分离",
            description: "分离模型目录、独立分离任务与交付文件",
        },
        ApiCategory {
            id: CAT_MODELS,
            label: "模型",
            description: "ASR 模型清单、加载与释放",
        },
        ApiCategory {
            id: CAT_TASKS,
            label: "长任务",
            description: "串行任务队列与逐任务 SSE 进度流",
        },
        ApiCategory {
            id: CAT_COMMAND,
            label: "通用",
            description: "把任意界面动作转给前端执行",
        },
    ]
}

/// 「第三方接入」里展示的一条接口说明；与 [`router`] 注册的路由一一对应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    /// 所属业务分类 id，见 [`categories`]。
    pub category: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
}

/// 接口目录：分类顺序 + 全部接口，界面一次读取即可分组渲染。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCatalog {
    pub categories: Vec<ApiCategory>,
    pub endpoints: Vec<ApiEndpoint>,
}

/// 当前开放接口清单；新增路由时同步补充，界面会自动展示。
pub fn endpoints() -> Vec<ApiEndpoint> {
    vec![
        ApiEndpoint {
            category: CAT_SYSTEM,
            method: "GET",
            path: "/v1/health",
            summary: "健康检查",
            description: "返回服务版本、监听端口与就绪状态，不校验 token。",
        },
        ApiEndpoint {
            category: CAT_SYSTEM,
            method: "GET",
            path: "/v1/state",
            summary: "播放状态",
            description: "当前曲目、进度、音量、播放模式、队列与输出路由快照。",
        },
        ApiEndpoint {
            category: CAT_SYSTEM,
            method: "GET",
            path: "/v1/events",
            summary: "事件流（SSE）",
            description: "标准 SSE：event 为 state / task / resync，id 单调递增；支持 Last-Event-ID 断线续传；?legacy=1 退回无 event 的旧帧。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/play",
            summary: "播放曲目",
            description: "请求体 {\"key\":\"分类/曲名\",\"queue\":[\"分类/曲名\"]}，queue 可选。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/pause",
            summary: "暂停",
            description: "暂停当前播放；未在播放时不做任何操作。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/resume",
            summary: "继续播放",
            description: "从暂停位置继续播放。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/toggle",
            summary: "播放 / 暂停切换",
            description: "等价于界面上的播放按钮。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/next",
            summary: "下一首",
            description: "按当前播放模式切到下一首。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/previous",
            summary: "上一首",
            description: "播放超过重播阈值时先回到开头，否则切到上一首。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/seek",
            summary: "跳转进度",
            description: "请求体 {\"seconds\":12.5}，单位为秒。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/volume",
            summary: "设置主音量",
            description: "请求体 {\"volume\":0.6}，取值 0~1。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/crossfade",
            summary: "设置淡入淡出",
            description: "请求体 {\"seconds\":2}，取值 0~12，0 表示硬切。",
        },
        ApiEndpoint {
            category: CAT_LIBRARY,
            method: "GET",
            path: "/v1/library",
            summary: "曲库内容",
            description: "全部曲目的 key、路径、分类与音频格式，可直接用于 /v1/play。",
        },
        ApiEndpoint {
            category: CAT_LIBRARY,
            method: "POST",
            path: "/v1/library/rescan",
            summary: "重新扫描曲库",
            description: "重新读取磁盘并返回最新曲库快照。",
        },
        ApiEndpoint {
            category: CAT_OUTPUT,
            method: "GET",
            path: "/v1/outputs",
            summary: "输出设备与路由",
            description: "可用输出设备、每路路由的开关、独立音量与静音状态。",
        },
        ApiEndpoint {
            category: CAT_OUTPUT,
            method: "POST",
            path: "/v1/outputs",
            summary: "更新一路输出",
            description: "请求体 {\"deviceId\":\"...\",\"enabled\":true,\"volume\":0.8,\"muted\":false}。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "GET",
            path: "/v1/subtitles",
            summary: "字幕记录列表",
            description: "?q= 检索曲名 / 文件名 / key / 正文；默认仅元信息，include=content 才带 SRT / LRC / 分段；支持 limit / offset。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "GET",
            path: "/v1/subtitles/track",
            summary: "单条字幕记录",
            description: "?key=曲目 key；key 含 / 与中文，放在查询参数里避免路径转义。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "PUT",
            path: "/v1/subtitles/track",
            summary: "按分段更新字幕",
            description: "请求体 {\"segments\":[{\"start\":1.2,\"end\":3.4,\"text\":\"…\"}]}；校验排序后写回并同步 SRT / LRC。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "PUT",
            path: "/v1/subtitles/text",
            summary: "按原文更新字幕",
            description: "?key=&format=srt|lrc，请求体 {\"text\":\"…\"}；解析为分段后写回。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "DELETE",
            path: "/v1/subtitles/track",
            summary: "删除字幕记录",
            description: "?key=；同时清理归一化音频、人声 / 伴奏文件与临时文件。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "GET",
            path: "/v1/subtitles/export",
            summary: "导出字幕文件",
            description: "?key=&format=srt|lrc，直接返回文件内容（Content-Disposition: attachment）。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "POST",
            path: "/v1/subtitles/generate",
            summary: "提交字幕生成任务",
            description: "请求体 {\"key\":…,\"separationModel\":…,\"separationTarget\":\"vocals\"|\"instrumental\"}；返回 202 与 task。",
        },
        ApiEndpoint {
            category: CAT_SEPARATION,
            method: "GET",
            path: "/v1/separation/models",
            summary: "分离模型目录",
            description: "可选人声模型与固定伴奏模型（UVR-MDX-NET-Inst_HQ_3）的可用性。",
        },
        ApiEndpoint {
            category: CAT_SEPARATION,
            method: "GET",
            path: "/v1/separation/result",
            summary: "分离结果",
            description: "?key= 读取 separation.json，附人声 / 伴奏文件的路径、存在性与大小。",
        },
        ApiEndpoint {
            category: CAT_SEPARATION,
            method: "POST",
            path: "/v1/separation",
            summary: "提交人声分离任务",
            description: "请求体 {\"key\":…,\"model\":…,\"target\":\"vocals\"|\"instrumental\"}；返回 202 与 task。",
        },
        ApiEndpoint {
            category: CAT_MODELS,
            method: "GET",
            path: "/v1/models",
            summary: "模型清单与状态",
            description: "扫描模型目录，返回可加载的 ASR 模型、已加载模型与最近一次加载状态。",
        },
        ApiEndpoint {
            category: CAT_MODELS,
            method: "POST",
            path: "/v1/models/load",
            summary: "提交模型加载任务",
            description: "请求体 {\"id\":\"模型 id（见 /v1/models）\"}；返回 202 与 task。",
        },
        ApiEndpoint {
            category: CAT_MODELS,
            method: "POST",
            path: "/v1/models/unload",
            summary: "释放模型",
            description: "释放已加载的 ASR 模型并复位加载状态。",
        },
        ApiEndpoint {
            category: CAT_TASKS,
            method: "GET",
            path: "/v1/tasks",
            summary: "任务列表",
            description: "?kind=subtitles|separation|asrLoad&status=queued|running|succeeded|failed&limit=50。",
        },
        ApiEndpoint {
            category: CAT_TASKS,
            method: "GET",
            path: "/v1/tasks/{id}",
            summary: "任务详情",
            description: "返回状态、阶段、进度与结果；完成后 result 含完整交付物。",
        },
        ApiEndpoint {
            category: CAT_TASKS,
            method: "GET",
            path: "/v1/tasks/{id}/events",
            summary: "任务进度流（SSE）",
            description: "event 为 snapshot / progress / done / failed，data 为任务快照；收到终态后客户端应关闭连接。",
        },
        ApiEndpoint {
            category: CAT_COMMAND,
            method: "POST",
            path: "/v1/command/{action}",
            summary: "通用命令",
            description: "把 action 直接转给界面执行，参数放在 params 里；未知 action 返回 404。",
        },
    ]
}

/// 分类 + 接口一次返回；「第三方接入」直接按分类分组渲染。
pub fn catalog() -> ApiCatalog {
    ApiCatalog {
        categories: categories(),
        endpoints: endpoints(),
    }
}

// ---------------- 接口状态 ----------------

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

    fn events(&self) -> Arc<EventBus> {
        Arc::clone(&self.inner.events)
    }

    fn tasks(&self) -> Arc<TaskStore> {
        Arc::clone(&self.inner.tasks)
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
}

/// 路由与跨域中间件；所有接口都在 `/v1` 下，避免与其它本地服务冲突。
fn router(bridge: ApiBridge) -> Router {
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

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
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

/// 长任务提交成功：`202 Accepted`，`data.task` 是任务快照。
fn accepted(data: Value) -> Response {
    (
        StatusCode::ACCEPTED,
        Json(json!({ "ok": true, "data": data })),
    )
        .into_response()
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

/// 从 `Last-Event-ID` 头或 `?lastEventId=` 读取续传位置。
///
/// EventSource 重连时会自动带上该头；非浏览器客户端可以用查询参数。
fn parse_last_event_id(
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
fn sse_event(event: &ApiEvent, legacy: bool) -> Event {
    let frame = Event::default()
        .id(event.id.to_string())
        .data(event.data.clone());
    if legacy {
        frame
    } else {
        frame.event(event.name)
    }
}

fn keep_alive() -> KeepAlive {
    KeepAlive::new()
        .interval(Duration::from_secs(15))
        .text("ping")
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

/// 全局事件流：先补发 `Last-Event-ID` 之后的历史，再接入实时广播。
async fn events(
    State(api): State<ApiBridge>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    api.authorize(&headers, query.get("token").map(String::as_str))?;
    let legacy = legacy_stream(&query);
    // 旧格式不参与断线续传：当年的客户端既不发 Last-Event-ID，也无法区分 resync。
    let last = if legacy {
        None
    } else {
        parse_last_event_id(&headers, &query)?
    };
    let bus = api.events();
    // 先订阅再读历史：中间新产生的事件会同时出现在两处，下面按 id 去重。
    let receiver = bus.subscribe();
    let newest = bus.last_id();
    // 应用重启后 id 会从 1 重新计数；客户端带回更大的 id 说明历史已换代。
    let reset = last.is_some_and(|last| last > newest);
    let mut replay = bus.replay_since(last);
    if reset {
        replay.events.clear();
        replay.gap = false;
    }

    let mut head: Vec<Result<Event, Infallible>> = Vec::new();
    if reset {
        let data = json!({
            "type": "resync",
            "reason": "reset",
            "lastEventId": last,
            "oldestEventId": Value::Null,
        });
        head.push(Ok(Event::default()
            .id(newest.to_string())
            .event("resync")
            .data(data.to_string())));
    } else if replay.gap {
        // 缺口：客户端要的位置已滑出历史窗口，先让它重新拉一次 /v1/state。
        let gap_id = replay
            .oldest
            .map(|id| id.saturating_sub(1))
            .unwrap_or_else(|| bus.last_id());
        let data = json!({
            "type": "resync",
            "reason": "window",
            "lastEventId": last,
            "oldestEventId": replay.oldest,
        });
        head.push(Ok(Event::default()
            .id(gap_id.to_string())
            .event("resync")
            .data(data.to_string())));
    }
    head.extend(
        replay
            .events
            .iter()
            .map(|event| Ok(sse_event(event, legacy))),
    );

    // 已补发过的事件在实时通道里会重复出现；按 id 丢弃，避免客户端看到重复帧。
    let max_replayed = replay.events.last().map(|event| event.id);
    // 换代后从当前最新 id 重新起算，旧 id 不能再当过滤下限，否则新事件会被全部丢弃。
    let floor = if reset { Some(newest) } else { last };
    let live = BroadcastStream::new(receiver).filter_map(move |message| {
        let event = message.ok()?;
        // 旧格式只推送 state：onmessage 客户端不认识 task / resync 的 data 结构。
        if legacy && event.name != "state" {
            return None;
        }
        if max_replayed.is_some_and(|max| event.id <= max) {
            return None;
        }
        if floor.is_some_and(|floor| event.id <= floor) {
            return None;
        }
        Some(Ok(sse_event(&event, legacy)))
    });
    let stream = tokio_stream::iter(head).chain(live);
    Ok(Sse::new(stream).keep_alive(keep_alive()))
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

/// 读取查询参数中的必填字符串。
fn require_query<'a>(query: &'a HashMap<String, String>, name: &str) -> Result<&'a str, ApiError> {
    query
        .get(name)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request(format!("缺少查询参数 {name}")))
}

/// 曲目 key：优先查询参数，其次请求体；两种写法都接受，便于不同客户端。
fn track_key(query: &HashMap<String, String>, body: &Value) -> Result<String, ApiError> {
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
fn ensure_idle(key: &str) -> Result<(), ApiError> {
    if crate::key_lock::is_busy(key) {
        Err(ApiError::conflict(crate::key_lock::BUSY_MESSAGE))
    } else {
        Ok(())
    }
}

/// 字幕格式：`srt` / `lrc`，缺省为 `srt`。
fn subtitle_format(query: &HashMap<String, String>, body: &Value) -> Result<String, ApiError> {
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
fn query_usize(
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
fn storage_error(error: anyhow::Error) -> ApiError {
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
fn legacy_stream(query: &HashMap<String, String>) -> bool {
    query
        .get("legacy")
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
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
    ApiPath(action): ApiPath<String>,
    headers: HeaderMap,
    body: Option<Json<Value>>,
) -> ApiResult {
    let value = body_value(body);
    let params = value.get("params").cloned().unwrap_or(value);
    call(&api, &headers, &action, params).await
}

// ---------------- 字幕 ----------------

/// 字幕记录列表；`?q=` 在曲名、文件名、key 与正文中做不区分大小写的包含匹配。
async fn subtitles_list(
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

async fn subtitle_get(
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

async fn subtitle_update(
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

async fn subtitle_update_text(
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

async fn subtitle_delete(
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

async fn subtitle_export(
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
fn content_disposition(file_name: &str) -> String {
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

/// 提交长任务：成功返回 `202` 与任务快照，重复提交返回 `409`。
///
/// `Location` 指向任务详情，客户端可据此轮询或订阅 `/events`。
fn submit_task(api: &ApiBridge, request: TaskRequest) -> Result<Response, ApiError> {
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

/// 校验 `target` 只接受 `vocals` / `instrumental`。
fn separation_target(value: Option<&Value>) -> Result<Option<String>, ApiError> {
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

async fn subtitles_generate(
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

// ---------------- 人声分离 ----------------

async fn separation_models(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
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

async fn separation_result(
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

async fn separation_create(
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

// ---------------- 模型 ----------------

async fn models_info(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let info = tokio::task::spawn_blocking(move || app.state::<AppState>().models_info())
        .await
        .map_err(|error| ApiError::internal(format!("读取模型信息失败：{error}")))?;
    let value = serde_json::to_value(&info)
        .map_err(|error| ApiError::internal(format!("模型信息序列化失败：{error}")))?;
    Ok(payload(value))
}

async fn models_load(
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

async fn models_unload(State(api): State<ApiBridge>, headers: HeaderMap) -> ApiResult {
    api.authorize(&headers, None)?;
    let app = api.app();
    let state = app.state::<AppState>();
    let runtime = state.model_runtime();
    if runtime.is_loading() {
        return Err(ApiError::conflict("模型正在加载中，请等待加载完成后再释放"));
    }
    let unloaded = runtime.unload();
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

// ---------------- 长任务 ----------------

async fn tasks_list(
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

async fn task_get(
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
async fn task_events(
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

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn every_endpoint_belongs_to_a_declared_category() {
        let ids: Vec<&str> = categories().iter().map(|category| category.id).collect();
        assert_eq!(ids.len(), 9, "分类数量变化时同步更新测试");
        for endpoint in endpoints() {
            assert!(
                ids.contains(&endpoint.category),
                "{} {} 的分类 {} 未声明",
                endpoint.method,
                endpoint.path,
                endpoint.category
            );
        }
    }

    #[test]
    fn every_category_has_endpoints() {
        let list = endpoints();
        for category in categories() {
            assert!(
                list.iter().any(|endpoint| endpoint.category == category.id),
                "分类 {} 没有接口",
                category.id
            );
        }
    }

    #[test]
    fn endpoint_routes_are_unique_with_upper_methods() {
        let list = endpoints();
        let mut seen = std::collections::HashSet::new();
        for endpoint in &list {
            assert!(
                ["GET", "POST", "PUT", "DELETE"].contains(&endpoint.method),
                "非法方法：{}",
                endpoint.method
            );
            assert!(
                endpoint.path.starts_with("/v1/"),
                "路径必须以 /v1/ 开头：{}",
                endpoint.path
            );
            assert!(
                seen.insert((endpoint.method, endpoint.path)),
                "重复接口：{} {}",
                endpoint.method,
                endpoint.path
            );
        }
        // 清单与路由必须同步：数量变化时先确认 router 是否也加了对应路由。
        assert_eq!(list.len(), 33, "接口数量变化时同步更新测试");
    }

    #[test]
    fn catalog_keeps_category_order() {
        let catalog = catalog();
        assert_eq!(catalog.categories.len(), 9);
        assert_eq!(catalog.categories[0].id, "system");
        assert_eq!(catalog.categories[4].id, "subtitles");
        assert_eq!(catalog.endpoints.len(), 33);
    }

    #[test]
    fn catalog_serializes_for_the_settings_dialog() {
        let value = serde_json::to_value(catalog()).unwrap();
        assert!(value["categories"].is_array());
        assert_eq!(value["categories"][0]["id"], "system");
        assert_eq!(value["categories"][0]["label"], "系统与状态");
        assert!(value["endpoints"].is_array());
        let first = &value["endpoints"][0];
        assert_eq!(first["category"], "system");
        assert_eq!(first["method"], "GET");
        assert_eq!(first["path"], "/v1/health");
        assert!(first["summary"].is_string());
        assert!(first["description"].is_string());
    }

    #[test]
    fn content_disposition_encodes_utf8_names() {
        let header = content_disposition("测试 曲目.srt");
        assert!(header.starts_with("attachment; filename=\""));
        assert!(header.contains("filename*=UTF-8''"));
        // 非 ASCII 全部百分号编码，中文标题不会破坏头部编码。
        assert!(header.contains("%E6%B5%8B%E8%AF%95"));
        assert!(!header.chars().any(|ch| ch as u32 > 127));
    }

    #[test]
    fn parse_last_event_id_accepts_header_and_query() {
        let mut headers = HeaderMap::new();
        headers.insert("last-event-id", HeaderValue::from_static("42"));
        assert_eq!(
            parse_last_event_id(&headers, &HashMap::new()).unwrap(),
            Some(42)
        );

        let query = HashMap::from([("lastEventId".to_owned(), "7".to_owned())]);
        assert_eq!(
            parse_last_event_id(&HeaderMap::new(), &query).unwrap(),
            Some(7)
        );

        let bad = HashMap::from([("lastEventId".to_owned(), "abc".to_owned())]);
        assert!(parse_last_event_id(&HeaderMap::new(), &bad).is_err());
    }

    #[test]
    fn separation_target_only_accepts_known_values() {
        assert_eq!(separation_target(None).unwrap(), None);
        assert_eq!(
            separation_target(Some(&json!("vocals"))).unwrap(),
            Some("vocals".to_owned())
        );
        assert!(separation_target(Some(&json!("drums"))).is_err());
        assert_eq!(
            separation_target(Some(&json!("  "))).unwrap(),
            None,
            "空白视为未指定"
        );
    }

    #[test]
    fn query_usize_rejects_garbage_and_clamps_range() {
        let empty = HashMap::new();
        assert_eq!(query_usize(&empty, "limit", 50, 1, 200).unwrap(), 50);

        let over = HashMap::from([("limit".to_owned(), "500".to_owned())]);
        assert_eq!(query_usize(&over, "limit", 50, 1, 200).unwrap(), 200);

        let bad = HashMap::from([("limit".to_owned(), "abc".to_owned())]);
        let error = query_usize(&bad, "limit", 50, 1, 200).unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn storage_errors_split_io_and_validation() {
        let io = storage_error(anyhow::Error::new(std::io::Error::other("磁盘已满")));
        assert_eq!(io.status, StatusCode::INTERNAL_SERVER_ERROR);

        let validation = storage_error(anyhow::anyhow!("分段为空"));
        assert_eq!(validation.status, StatusCode::BAD_REQUEST);

        let busy = storage_error(anyhow::anyhow!(crate::key_lock::BUSY_MESSAGE));
        assert_eq!(busy.status, StatusCode::CONFLICT);
    }

    #[test]
    fn legacy_flag_accepts_common_truthy_values() {
        for value in ["1", "true", "YES"] {
            let query = HashMap::from([("legacy".to_owned(), value.to_owned())]);
            assert!(legacy_stream(&query), "{value} 应视为 legacy");
        }
        assert!(!legacy_stream(&HashMap::new()));
        let off = HashMap::from([("legacy".to_owned(), "0".to_owned())]);
        assert!(!legacy_stream(&off));
    }
}
