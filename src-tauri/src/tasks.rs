//! 长任务队列：字幕生成、人声分离与 ASR 模型加载。
//!
//! 这三类操作的耗时从数秒（模型加载）到数十分钟（整曲分片识别），既不能占用
//! HTTP 请求同步等待，也不能交给前端执行 —— 窗口收进托盘时前端的命令通道不可用。
//! 因此统一放进一个**串行**队列：
//!
//! - 同一时刻只运行一个任务，避免 ASR 推理与分离子进程争抢 CPU / 内存；
//! - 提交立即返回任务 id，调用方通过 `GET /v1/tasks/{id}` 轮询，
//!   或订阅 `GET /v1/tasks/{id}/events`（SSE）接收逐条进度；
//! - HTTP 队列不提供独立的取消接口；
//! - 已完成任务保留最近 [`HISTORY_LIMIT`] 条，避免长期运行时无限增长。
//!
//! 队列只依赖 `AppHandle` 获取 `AppState`，不持有任何界面对象；状态变化统一交给
//! [`EventBus`] 广播，`event: task` 的 `data` 是 `{"type":"task","task":{...}}`。

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tokio_util::sync::CancellationToken;

use crate::event_bus::EventBus;
use crate::separation;
use crate::state::AppState;
use crate::subtitles::{self, SubtitleProgress, SubtitleStage};

/// 已完成任务的保留条数；超出后从最旧的已完成任务开始丢弃。
const HISTORY_LIMIT: usize = 50;

/// 任务类型；序列化值同时用于 `?kind=` 过滤参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskKind {
    Subtitles,
    Separation,
    AsrLoad,
}

impl TaskKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Subtitles => "字幕生成",
            Self::Separation => "人声分离",
            Self::AsrLoad => "模型加载",
        }
    }

    /// 解析 `?kind=` 参数；接受序列化值（`subtitles` / `separation` / `asrLoad`）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "subtitles" => Some(Self::Subtitles),
            "separation" => Some(Self::Separation),
            "asrLoad" | "asr-load" => Some(Self::AsrLoad),
            _ => None,
        }
    }

    /// 供错误提示列出全部合法取值。
    pub const ALL: [&'static str; 3] = ["subtitles", "separation", "asrLoad"];
}

/// 任务状态；`queued` 与 `running` 都属于「未完成」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl TaskStatus {
    fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    /// 是否已进入终态；逐任务 SSE 收到终态后即可结束连接。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed)
    }

    /// 解析 `?status=` 参数。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// 供错误提示列出全部合法取值。
    pub const ALL: [&'static str; 4] = ["queued", "running", "succeeded", "failed"];
}

/// 提交任务时携带的参数；不参与序列化，执行时由 worker 取走。
pub enum TaskRequest {
    Subtitles {
        key: String,
        separation_model: String,
        separation_target: Option<String>,
    },
    Separation {
        key: String,
        model: String,
        target: Option<String>,
    },
    AsrLoad {
        dir: PathBuf,
        model_id: Option<String>,
    },
}

impl TaskRequest {
    fn kind(&self) -> TaskKind {
        match self {
            Self::Subtitles { .. } => TaskKind::Subtitles,
            Self::Separation { .. } => TaskKind::Separation,
            Self::AsrLoad { .. } => TaskKind::AsrLoad,
        }
    }

    fn key(&self) -> Option<&str> {
        match self {
            Self::Subtitles { key, .. } | Self::Separation { key, .. } => Some(key),
            Self::AsrLoad { .. } => None,
        }
    }
}

/// 对外暴露的任务快照；界面与 HTTP 共用同一份字段。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskSnapshot {
    pub id: String,
    pub kind: TaskKind,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// 阶段标识：`queued` / `running` / `separating` / `normalizing` /
    /// `transcribing` / `loading` / `done` / `failed`。
    pub stage: String,
    pub message: String,
    pub current: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub created_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// 提交失败：同类型、同曲目的任务已在队列中。
#[derive(Debug)]
pub struct SubmitError {
    message: String,
}

impl SubmitError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

struct TaskEntry {
    snapshot: TaskSnapshot,
    request: Option<TaskRequest>,
}

struct Inner {
    entries: Vec<TaskEntry>,
    queue: VecDeque<String>,
    running: bool,
    next_id: u64,
}

/// 串行任务队列；由 [`crate::api::ApiBridge`] 持有，生命周期与应用一致。
pub struct TaskStore {
    inner: Mutex<Inner>,
    events: Arc<EventBus>,
}

impl TaskStore {
    /// 状态变化以 `event: task` 广播到 `/v1/events`；逐任务 SSE 从同一通道过滤。
    pub fn new(events: Arc<EventBus>) -> Self {
        Self {
            inner: Mutex::new(Inner {
                entries: Vec::new(),
                queue: VecDeque::new(),
                running: false,
                next_id: 1,
            }),
            events,
        }
    }

    /// 提交任务；队列空闲时立即启动 worker，否则排队等待。
    pub fn submit(
        self: &Arc<Self>,
        app: AppHandle,
        request: TaskRequest,
    ) -> Result<TaskSnapshot, SubmitError> {
        let kind = request.kind();
        let key = request.key().map(str::to_owned);
        let mut inner = self.inner.lock();

        // 同一曲目 + 同一类型只允许一个未完成任务，避免重复点击/重试白跑一遍分离。
        if let Some(active) = inner.entries.iter().find(|entry| {
            entry.snapshot.kind == kind
                && entry.snapshot.key == key
                && entry.snapshot.status.is_active()
        }) {
            return Err(SubmitError {
                message: format!(
                    "同一{}已在队列中（{}），请等待其结束或改用 /v1/tasks/{} 查询进度",
                    kind.label(),
                    active.snapshot.id,
                    active.snapshot.id
                ),
            });
        }

        let id = format!("task-{}-{:04}", now_ms(), inner.next_id);
        inner.next_id = inner.next_id.wrapping_add(1);
        let snapshot = TaskSnapshot {
            id: id.clone(),
            kind,
            status: TaskStatus::Queued,
            key,
            stage: "queued".to_owned(),
            message: "已加入队列，等待执行".to_owned(),
            current: 0,
            total: 0,
            error: None,
            created_at_ms: now_ms(),
            started_at_ms: None,
            finished_at_ms: None,
            result: None,
        };
        inner.entries.push(TaskEntry {
            snapshot: snapshot.clone(),
            request: Some(request),
        });
        inner.queue.push_back(id);
        let start_worker = !inner.running;
        if start_worker {
            inner.running = true;
        }
        drop(inner);

        self.notify(&snapshot);
        if start_worker {
            let store = Arc::clone(self);
            tauri::async_runtime::spawn_blocking(move || store.drain(app));
        }
        Ok(snapshot)
    }

    pub fn get(&self, id: &str) -> Option<TaskSnapshot> {
        let inner = self.inner.lock();
        inner
            .entries
            .iter()
            .find(|entry| entry.snapshot.id == id)
            .map(|entry| entry.snapshot.clone())
    }

    /// 按 `kind` / `status` 过滤，创建时间倒序返回；`limit` 在调用侧钳制。
    pub fn list(
        &self,
        kind: Option<TaskKind>,
        status: Option<TaskStatus>,
        limit: usize,
    ) -> (Vec<TaskSnapshot>, usize) {
        let inner = self.inner.lock();
        let mut matched: Vec<TaskSnapshot> = inner
            .entries
            .iter()
            .filter(|entry| kind.is_none_or(|kind| entry.snapshot.kind == kind))
            .filter(|entry| status.is_none_or(|status| entry.snapshot.status == status))
            .map(|entry| entry.snapshot.clone())
            .collect();
        matched.sort_by_key(|task| std::cmp::Reverse(task.created_at_ms));
        let total = matched.len();
        matched.truncate(limit);
        (matched, total)
    }

    /// worker 主循环：一次取一个任务，执行完再取下一个，直到队列为空。
    fn drain(self: Arc<Self>, app: AppHandle) {
        loop {
            let started = {
                let mut inner = self.inner.lock();
                let Some(id) = inner.queue.pop_front() else {
                    inner.running = false;
                    return;
                };
                let Some(entry) = inner
                    .entries
                    .iter_mut()
                    .find(|entry| entry.snapshot.id == id)
                else {
                    continue;
                };
                entry.snapshot.status = TaskStatus::Running;
                entry.snapshot.stage = "running".to_owned();
                entry.snapshot.message = format!("{}开始执行", entry.snapshot.kind.label());
                entry.snapshot.started_at_ms = Some(now_ms());
                (entry.snapshot.clone(), entry.request.take())
            };
            let (snapshot, request) = started;
            self.notify(&snapshot);

            let outcome = match request {
                // 任务内部的 panic（例如 sherpa-onnx FFI 异常）不能带走 worker：
                // 否则 running 会永久为 true，后续提交全部卡在队列里。
                Some(request) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.execute(&app, &snapshot.id, request)
                }))
                .unwrap_or_else(|_| Err("任务执行异常终止（内部错误），队列已继续".to_owned())),
                None => Err("任务参数缺失，可能已被消费".to_owned()),
            };
            if let Some(finished) = self.finish(&snapshot.id, outcome) {
                self.notify(&finished);
            }
            self.trim();
        }
    }

    fn execute(
        self: &Arc<Self>,
        app: &AppHandle,
        id: &str,
        request: TaskRequest,
    ) -> Result<Value, String> {
        match request {
            TaskRequest::Subtitles {
                key,
                separation_model,
                separation_target,
            } => {
                let state = app.state::<AppState>();
                let song = state
                    .song(&key)
                    .ok_or_else(|| format!("曲目不存在：{key}"))?;
                let runtime = state.model_runtime();
                let root = state.models_root();
                let store = Arc::clone(self);
                let job_id = id.to_owned();
                let result = subtitles::generate_with_progress(
                    app,
                    &runtime,
                    &song,
                    &separation_model,
                    separation_target.as_deref(),
                    &root,
                    &CancellationToken::new(),
                    move |progress| store.apply_progress(&job_id, progress),
                )
                .map_err(|error| format!("{error:#}"))?;
                serde_json::to_value(&result)
                    .map_err(|error| format!("序列化字幕结果失败：{error}"))
            }
            TaskRequest::Separation { key, model, target } => {
                let state = app.state::<AppState>();
                let song = state
                    .song(&key)
                    .ok_or_else(|| format!("曲目不存在：{key}"))?;
                let root = state.models_root();
                self.set_stage(id, "separating", "正在分离人声与伴奏（模型并行运行）…");
                let info = separation::separate(app, &song, &model, &root, target.as_deref())
                    .map_err(|error| format!("{error:#}"))?;
                // 终态由 `finish` 统一写入（stage=done / message=任务完成），
                // 这里再发一次 "done" 会让 status 仍是 running 的帧带上完成态。
                Ok(json!({ "key": key, "separation": info }))
            }
            TaskRequest::AsrLoad { dir, model_id } => {
                let state = app.state::<AppState>();
                let runtime = state.model_runtime();
                if runtime.is_loading() {
                    return Err("已有模型加载任务正在进行".to_owned());
                }
                self.set_stage(id, "loading", "正在加载 ASR 模型…");
                let root = state.models_root();
                let loaded = runtime
                    .load(&root, &dir, |status| {
                        crate::models::emit_load_status(app, status)
                    })
                    .map_err(|error| format!("{error:#}"))?;
                state.remember_loaded_model(model_id);
                serde_json::to_value(&loaded)
                    .map(|value| json!({ "loaded": value }))
                    .map_err(|error| format!("序列化模型信息失败：{error}"))
            }
        }
    }

    /// 字幕分片识别的细粒度进度；只在任务仍存在时更新。
    fn apply_progress(&self, id: &str, progress: &SubtitleProgress) {
        let snapshot = {
            let mut inner = self.inner.lock();
            let Some(entry) = inner
                .entries
                .iter_mut()
                .find(|entry| entry.snapshot.id == id)
            else {
                return;
            };
            entry.snapshot.stage = stage_id(progress.stage).to_owned();
            entry.snapshot.message = progress.message.clone();
            entry.snapshot.current = progress.current;
            entry.snapshot.total = progress.total;
            if let Some(error) = &progress.error {
                entry.snapshot.error = Some(error.clone());
            }
            entry.snapshot.clone()
        };
        self.notify(&snapshot);
    }

    /// 分离 / 模型加载这类没有细粒度进度的阶段，只更新阶段与说明。
    fn set_stage(&self, id: &str, stage: &str, message: impl Into<String>) {
        let snapshot = {
            let mut inner = self.inner.lock();
            let Some(entry) = inner
                .entries
                .iter_mut()
                .find(|entry| entry.snapshot.id == id)
            else {
                return;
            };
            entry.snapshot.stage = stage.to_owned();
            entry.snapshot.message = message.into();
            entry.snapshot.clone()
        };
        self.notify(&snapshot);
    }

    fn finish(&self, id: &str, outcome: Result<Value, String>) -> Option<TaskSnapshot> {
        let mut inner = self.inner.lock();
        let entry = inner
            .entries
            .iter_mut()
            .find(|entry| entry.snapshot.id == id)?;
        entry.snapshot.finished_at_ms = Some(now_ms());
        match outcome {
            Ok(result) => {
                entry.snapshot.status = TaskStatus::Succeeded;
                entry.snapshot.stage = "done".to_owned();
                entry.snapshot.message = "任务完成".to_owned();
                entry.snapshot.error = None;
                entry.snapshot.result = Some(result);
            }
            Err(error) => {
                entry.snapshot.status = TaskStatus::Failed;
                entry.snapshot.stage = "failed".to_owned();
                entry.snapshot.message = "任务失败".to_owned();
                entry.snapshot.error = Some(error);
            }
        }
        Some(entry.snapshot.clone())
    }

    /// 只丢弃最旧的**已完成**任务；排队与运行中的任务永不回收。
    fn trim(&self) {
        let mut inner = self.inner.lock();
        while inner.entries.len() > HISTORY_LIMIT {
            let Some(index) = inner
                .entries
                .iter()
                .position(|entry| !entry.snapshot.status.is_active())
            else {
                return;
            };
            inner.entries.remove(index);
        }
    }

    fn notify(&self, task: &TaskSnapshot) {
        let payload = json!({ "type": "task", "task": task }).to_string();
        self.events.publish("task", payload);
    }
}

fn stage_id(stage: SubtitleStage) -> &'static str {
    match stage {
        SubtitleStage::Separating => "separating",
        SubtitleStage::Normalizing => "normalizing",
        SubtitleStage::Transcribing => "transcribing",
        SubtitleStage::Done => "done",
        SubtitleStage::Error => "failed",
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
