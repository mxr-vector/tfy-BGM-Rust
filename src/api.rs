//! 回环地址上的开放接口。播放动作通过有界通道交给 GPUI，查询与 SSE 使用已发布的快照。

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use http::header::{AUTHORIZATION, CONTENT_LENGTH, HOST, ORIGIN, TRANSFER_ENCODING};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, mpsc, oneshot, watch};

use crate::settings::{MAX_OUTPUT_ROUTES, OutputRoute};

pub mod catalog;
pub mod records;
pub mod tasks;

const MAX_HEADERS: usize = 16 * 1024;
const MAX_BODY: usize = 64 * 1024;
const MAX_SUBTITLE_BODY: usize = 2 * 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(6);
const EVENT_HISTORY: usize = 256;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub type Failure = (StatusCode, String);
pub type Reply = Result<Value, Failure>;

#[derive(Debug, Clone)]
pub struct Config {
    pub enabled: bool,
    pub port: u16,
    pub token: String,
    pub cors: String,
    pub cors_origins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            port: 8737,
            token: String::new(),
            cors: "local".into(),
            cors_origins: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Self {
        let value = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .unwrap_or_default();
        Self::from_document(&value["api"])
    }

    fn from_document(value: &Value) -> Self {
        let defaults = Self::default();
        let cors = &value["cors"];
        let origins = cors
            .get("origins")
            .or_else(|| value.get("corsOrigins"))
            .or_else(|| value.get("origins"))
            .and_then(Value::as_array);
        let collect = || origins.into_iter().flatten().filter_map(Value::as_str);
        let mode = cors.as_str().or_else(|| cors["mode"].as_str());
        let mode = match mode {
            Some("any" | "*") => "any",
            Some("custom") if collect().any(|origin| origin.trim() == "*") => "any",
            Some("custom") => "custom",
            _ => "local",
        };
        let mut cors_origins = Vec::<String>::new();
        for origin in collect().filter_map(normalized_origin) {
            if !cors_origins.contains(&origin) {
                cors_origins.push(origin);
                if cors_origins.len() == 16 {
                    break;
                }
            }
        }
        Self {
            enabled: value["enabled"].as_bool().unwrap_or(defaults.enabled),
            port: value["port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port >= 1024)
                .unwrap_or(defaults.port),
            token: value["token"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .chars()
                .take(128)
                .collect(),
            cors: mode.into(),
            cors_origins,
        }
    }

    pub fn document(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "port": self.port,
            "token": self.token,
            "cors": { "mode": self.cors, "origins": self.cors_origins },
        })
    }

    fn allows(&self, origin: &str) -> bool {
        let parsed = origin_url(origin);
        if origin == "tauri://localhost"
            || parsed.as_ref().is_some_and(|url| {
                loopback_host(url.host_str().unwrap_or_default())
                    || url.host_str() == Some("tauri.localhost")
            })
        {
            return true;
        }
        match self.cors.as_str() {
            "any" | "*" => true,
            "custom" => {
                let key = parsed.map(|url| url.origin().ascii_serialization());
                self.cors_origins.iter().any(|allowed| {
                    allowed == "*"
                        || key
                            .as_ref()
                            .is_some_and(|key| Some(key) == normalized_origin(allowed).as_ref())
                })
            }
            _ => false,
        }
    }
}

fn normalized_origin(value: &str) -> Option<String> {
    origin_url(value).map(|url| url.origin().ascii_serialization())
}

fn origin_url(value: &str) -> Option<url::Url> {
    let url = url::Url::parse(value.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

fn loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

pub enum Command {
    Playback {
        action: String,
        params: Value,
        reply: oneshot::Sender<Reply>,
    },
    Tasks {
        operation: tasks::Operation,
        reply: oneshot::Sender<Reply>,
    },
    Records {
        request: records::Operation,
        reply: oneshot::Sender<records::Reply>,
    },
}

pub struct Server {
    shared: Arc<Shared>,
    commands: mpsc::Receiver<Command>,
    stop: Option<oneshot::Sender<()>>,
}

struct Shared {
    config: RwLock<Config>,
    state: RwLock<StateLog>,
    updates: watch::Sender<()>,
    library: RwLock<Value>,
    commands: mpsc::Sender<Command>,
    tasks: Mutex<tasks::Store>,
}

#[derive(Default)]
struct StateLog {
    current: Value,
    last_id: u64,
    recent: VecDeque<(u64, &'static str, Arc<str>)>,
}

impl StateLog {
    fn publish(&mut self, state: Value) {
        let message = json!({"type": "state", "state": state});
        self.current = state;
        self.event("state", message);
    }

    fn event(&mut self, name: &'static str, message: Value) -> u64 {
        self.last_id += 1;
        if self.recent.len() == EVENT_HISTORY {
            self.recent.pop_front();
        }
        self.recent
            .push_back((self.last_id, name, message.to_string().into()));
        self.last_id
    }
}

pub struct RunningTask {
    shared: Arc<Shared>,
    id: String,
}

impl RunningTask {
    pub fn progress(&self, stage: &'static str, message: String, current: usize, total: usize) {
        self.change(|task| task.progress(stage, message, current, total));
    }

    pub fn finish(&self, result: Result<Value, String>) {
        self.change(|task| task.finish(result));
    }

    fn change(&self, change: impl FnOnce(&mut tasks::Snapshot)) {
        if let Ok(mut tasks) = self.shared.tasks.lock() {
            tasks.update(&self.id, change, |task| self.shared.task_event(task));
        }
    }
}

impl Drop for RunningTask {
    fn drop(&mut self) {
        self.change(|task| task.finish(Err("任务提前结束".into())));
    }
}

impl Shared {
    fn snapshot(&self) -> Value {
        self.state
            .read()
            .map(|log| log.current.clone())
            .unwrap_or_default()
    }

    fn task_event(&self, task: &tasks::Snapshot) -> u64 {
        let mut log = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = log.event("task", json!({ "type": "task", "task": task }));
        self.updates.send_replace(());
        id
    }

    fn ready(&self) -> bool {
        self.state.read().is_ok_and(|log| !log.current.is_null())
    }
}

impl Server {
    pub fn start(mut config: Config) -> io::Result<Self> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, config.port))?;
        listener.set_nonblocking(true)?;
        let (sender, commands) = mpsc::channel(32);
        config.port = listener.local_addr()?.port();
        let (updates, _) = watch::channel(());
        let (stop, shutdown) = oneshot::channel();
        let shared = Arc::new(Shared {
            config: RwLock::new(config),
            state: RwLock::new(StateLog::default()),
            updates,
            library: RwLock::new(Value::Null),
            commands: sender,
            tasks: Mutex::default(),
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let http = shared.clone();
        std::thread::Builder::new()
            .name("tfy-bgm-api".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
                        return;
                    };
                    let connections = Arc::new(Semaphore::new(32));
                    let accept = async {
                        while let Ok((stream, _)) = listener.accept().await {
                            let Ok(permit) = connections.clone().try_acquire_owned() else {
                                continue;
                            };
                            let http = http.clone();
                            tokio::spawn(async move {
                                let _permit = permit;
                                let _ = serve(stream, http).await;
                            });
                        }
                    };
                    use std::future::Future;
                    let mut accept = std::pin::pin!(accept);
                    let mut shutdown = std::pin::pin!(shutdown);
                    std::future::poll_fn(|cx| {
                        if shutdown.as_mut().poll(cx).is_ready() {
                            return std::task::Poll::Ready(());
                        }
                        accept.as_mut().poll(cx)
                    })
                    .await;
                });
            })?;
        Ok(Self {
            shared,
            commands,
            stop: Some(stop),
        })
    }

    pub fn next_command(&mut self) -> Option<Command> {
        self.commands.try_recv().ok()
    }

    pub fn submit_task(&self, job: tasks::Job) -> Reply {
        let mut tasks = self
            .shared
            .tasks
            .lock()
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "任务队列不可用".into()))?;
        let task = tasks.submit(job, |task| self.shared.task_event(task))?;
        Ok(json!(task))
    }

    pub fn next_task(&self) -> Option<(RunningTask, tasks::Job)> {
        let (id, job) = self
            .shared
            .tasks
            .lock()
            .ok()?
            .start(|task| self.shared.task_event(task))?;
        Some((
            RunningTask {
                shared: self.shared.clone(),
                id,
            },
            job,
        ))
    }

    pub fn publish(&self, state: Value) {
        if let Ok(mut log) = self.shared.state.write() {
            log.publish(state);
            self.shared.updates.send_replace(());
        }
    }

    pub fn publish_library(&self, library: &bgm_core::LibrarySnapshot) {
        if let Ok(mut snapshot) = self.shared.library.write() {
            *snapshot = library_payload(library);
        }
    }

    pub fn update_config(&self, mut config: Config) {
        if let Ok(mut active) = self.shared.config.write() {
            config.port = active.port;
            config.enabled = active.enabled;
            *active = config;
            self.shared.updates.send_replace(());
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

struct Request {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> Result<Request, &'static str> {
    let mut bytes = Vec::with_capacity(4096);
    let (method, uri, headers, head_len) = loop {
        let mut parsed_headers = [httparse::EMPTY_HEADER; 32];
        let mut request = httparse::Request::new(&mut parsed_headers);
        match request.parse(&bytes) {
            Ok(httparse::Status::Complete(len)) if len <= MAX_HEADERS => {
                let method = request
                    .method
                    .and_then(|value| value.parse().ok())
                    .ok_or("无效的请求方法")?;
                let uri = request
                    .path
                    .and_then(|value| value.parse::<Uri>().ok())
                    .filter(|uri| uri.scheme().is_none() && uri.authority().is_none())
                    .ok_or("无效的请求路径")?;
                let mut headers = HeaderMap::new();
                for header in request.headers {
                    let name = HeaderName::from_bytes(header.name.as_bytes())
                        .map_err(|_| "无效的请求头")?;
                    let value =
                        HeaderValue::from_bytes(header.value).map_err(|_| "无效的请求头")?;
                    if (matches!(name, HOST | CONTENT_LENGTH | ORIGIN | AUTHORIZATION)
                        || matches!(name.as_str(), "x-tfy-token" | "last-event-id"))
                        && headers.contains_key(&name)
                    {
                        return Err("重复的请求头");
                    }
                    headers.append(name, value);
                }
                break (method, uri, headers, len);
            }
            Ok(httparse::Status::Partial) if bytes.len() < MAX_HEADERS => {
                let mut chunk = [0; 4096];
                let len = stream.read(&mut chunk).await.map_err(|_| "无法读取请求")?;
                if len == 0 {
                    return Err("请求提前结束");
                }
                bytes.extend_from_slice(&chunk[..len]);
            }
            _ => return Err("请求头无效或超出限制"),
        }
    };
    if headers.contains_key(TRANSFER_ENCODING) {
        return Err("请使用 Content-Length 请求体");
    }
    let limit = if method == Method::PUT && records::Operation::is_route(&method, &uri) {
        MAX_SUBTITLE_BODY
    } else {
        MAX_BODY
    };
    let body_len = headers
        .get(CONTENT_LENGTH)
        .map(|size| {
            size.to_str()
                .ok()
                .and_then(|size| size.parse::<usize>().ok())
        })
        .unwrap_or(Some(0))
        .filter(|len| *len <= limit)
        .ok_or("请求体大小无效或超出限制")?;
    let mut body = bytes.split_off(head_len);
    let received = body.len().min(body_len);
    body.resize(body_len, 0);
    stream
        .read_exact(&mut body[received..])
        .await
        .map_err(|_| "请求体提前结束")?;
    Ok(Request {
        method,
        uri,
        headers,
        body,
    })
}

enum Response {
    Json(Reply),
    Events(StreamOptions),
    TaskEvents(watch::Receiver<tasks::Update>),
    Accepted { task: Value, location: String },
    Download { filename: String, content: String },
    NoContent,
}

async fn serve(mut stream: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    let request = match tokio::time::timeout(REQUEST_TIMEOUT, read_request(&mut stream)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            return send_json(
                &mut stream,
                Err((StatusCode::BAD_REQUEST, error.into())),
                None,
            )
            .await;
        }
        Err(_) => return Ok(()),
    };
    let config = shared
        .config
        .read()
        .map(|config| config.clone())
        .unwrap_or_default();
    let origin = request
        .headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let host_ok = request
        .headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<http::uri::Authority>().ok())
        .is_some_and(|host| loopback_host(host.host()));
    if !host_ok
        || (request.headers.contains_key(ORIGIN)
            && origin.as_deref().is_none_or(|s| !config.allows(s)))
    {
        return send_json(
            &mut stream,
            Err((StatusCode::FORBIDDEN, "来源不允许".into())),
            None,
        )
        .await;
    }
    match route(request, &config, shared.clone()).await {
        Response::Json(reply) => send_json(&mut stream, reply, origin.as_deref()).await,
        Response::NoContent => {
            send_headers(
                &mut stream,
                StatusCode::NO_CONTENT,
                "application/json",
                Some(0),
                origin.as_deref(),
                None,
            )
            .await
        }
        Response::Accepted { task, location } => {
            let content = json!({"ok":true, "data":{"task":task}}).to_string();
            send_headers(
                &mut stream,
                StatusCode::ACCEPTED,
                "application/json",
                Some(content.len()),
                origin.as_deref(),
                Some(("Location", &location)),
            )
            .await?;
            tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(content.as_bytes())).await?
        }
        Response::TaskEvents(updates) => {
            serve_task_events(stream, &shared, &config, origin, updates).await
        }
        Response::Events(options) => serve_events(stream, &shared, &config, origin, options).await,
        Response::Download { filename, content } => {
            let disposition = records::disposition(&filename);
            send_headers(
                &mut stream,
                StatusCode::OK,
                "text/plain; charset=utf-8",
                Some(content.len()),
                origin.as_deref(),
                Some(("Content-Disposition", &disposition)),
            )
            .await?;
            tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(content.as_bytes())).await?
        }
    }
}

#[derive(Clone, Copy)]
struct StreamOptions {
    last: Option<u64>,
    legacy: bool,
}

impl StreamOptions {
    fn parse(headers: &HeaderMap, query: &HashMap<String, String>) -> Result<Self, &'static str> {
        let legacy = query.get("legacy").is_some_and(|mode| {
            matches!(
                mode.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        });
        let last = if legacy {
            None
        } else {
            let header = headers
                .get("last-event-id")
                .map(|value| value.to_str())
                .transpose()
                .map_err(|_| "Last-Event-ID 无效")?;
            header
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .or_else(|| {
                    query
                        .get("lastEventId")
                        .map(|id| id.trim())
                        .filter(|id| !id.is_empty())
                })
                .map(str::parse)
                .transpose()
                .map_err(|_| "Last-Event-ID 无效")?
        };
        Ok(Self { last, legacy })
    }
}

struct EventBatch {
    cursor: u64,
    resync: Option<(u64, String)>,
    events: Vec<(u64, &'static str, Arc<str>)>,
}

impl EventBatch {
    fn read(log: &StateLog, cursor: u64, legacy: bool) -> Self {
        if cursor > log.last_id {
            return Self {
                cursor: log.last_id,
                resync: Some((log.last_id, json!({
                    "type": "resync", "reason": "reset", "lastEventId": cursor, "oldestEventId": null,
                }).to_string())),
                events: Vec::new(),
            };
        }
        let resync = log
            .recent
            .front()
            .filter(|(id, _, _)| !legacy && id.saturating_sub(1) > cursor)
            .map(|(id, _, _)| {
                (id - 1, json!({
                "type": "resync", "reason": "window", "lastEventId": cursor, "oldestEventId": id,
            }).to_string())
            });
        let events = log
            .recent
            .iter()
            .filter(|(id, name, _)| *id > cursor && (!legacy || *name == "state"))
            .cloned()
            .collect();
        Self {
            cursor: log.last_id,
            resync,
            events,
        }
    }
}

async fn serve_events(
    mut stream: TcpStream,
    shared: &Shared,
    config: &Config,
    origin: Option<String>,
    options: StreamOptions,
) -> io::Result<()> {
    let mut updates = shared.updates.subscribe();
    let mut cursor = options
        .last
        .unwrap_or_else(|| shared.state.read().map_or(0, |log| log.last_id));
    send_headers(
        &mut stream,
        StatusCode::OK,
        "text/event-stream",
        None,
        origin.as_deref(),
        None,
    )
    .await?;
    let allowed = || stream_allowed(shared, config, origin.as_deref());
    loop {
        if !allowed() {
            return Ok(());
        }
        let batch = shared
            .state
            .read()
            .map(|log| EventBatch::read(&log, cursor, options.legacy))
            .map_err(|_| io::Error::other("无法读取事件历史"))?;
        cursor = batch.cursor;
        if let Some((id, data)) = batch.resync {
            send_event(&mut stream, id, Some("resync"), &data).await?;
        }
        for (id, name, data) in batch.events {
            if !allowed() {
                return Ok(());
            }
            send_event(&mut stream, id, (!options.legacy).then_some(name), &data).await?;
        }
        match tokio::time::timeout(KEEP_ALIVE, updates.changed()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Ok(()),
            Err(_) if allowed() => {
                tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(b": ping\n\n")).await??;
            }
            Err(_) => return Ok(()),
        }
    }
}

fn stream_allowed(shared: &Shared, config: &Config, origin: Option<&str>) -> bool {
    shared.config.read().is_ok_and(|active| {
        active.token == config.token && origin.is_none_or(|origin| active.allows(origin))
    })
}

async fn serve_task_events(
    mut stream: TcpStream,
    shared: &Shared,
    config: &Config,
    origin: Option<String>,
    mut task: watch::Receiver<tasks::Update>,
) -> io::Result<()> {
    let mut configuration = shared.updates.subscribe();
    send_headers(
        &mut stream,
        StatusCode::OK,
        "text/event-stream",
        None,
        origin.as_deref(),
        None,
    )
    .await?;
    let allowed = || stream_allowed(shared, config, origin.as_deref());
    let mut latest = None;
    loop {
        if !allowed() {
            return Ok(());
        }
        let update = task.borrow_and_update().clone();
        if latest != Some(update.version) {
            let terminal = update.snapshot.status.terminal_event();
            let event = terminal.unwrap_or_else(|| {
                if latest.is_none() {
                    "snapshot"
                } else {
                    "progress"
                }
            });
            let data = json!({"type":"task", "task":update.snapshot}).to_string();
            send_event(&mut stream, update.version, Some(event), &data).await?;
            latest = Some(update.version);
            if terminal.is_some() {
                return Ok(());
            }
        }
        use std::future::Future;
        let wait = async {
            let mut update = std::pin::pin!(task.changed());
            let mut config = std::pin::pin!(configuration.changed());
            std::future::poll_fn(|cx| {
                if let std::task::Poll::Ready(event) = update.as_mut().poll(cx) {
                    return std::task::Poll::Ready(event);
                }
                config.as_mut().poll(cx)
            })
            .await
        };
        match tokio::time::timeout(KEEP_ALIVE, wait).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Ok(()),
            Err(_) if allowed() => {
                tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(b": ping\n\n")).await??;
            }
            Err(_) => return Ok(()),
        }
    }
}

async fn send_event(
    stream: &mut TcpStream,
    id: u64,
    event: Option<&str>,
    data: &str,
) -> io::Result<()> {
    let event = event
        .map(|name| format!("event: {name}\n"))
        .unwrap_or_default();
    let frame = format!("id: {id}\n{event}data: {data}\n\n");
    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(frame.as_bytes())).await?
}

async fn route(request: Request, config: &Config, shared: Arc<Shared>) -> Response {
    let success = |data| Response::Json(Ok(data));
    let failure = |status, error: &str| Response::Json(Err((status, error.into())));
    if request.method == Method::OPTIONS {
        return Response::NoContent;
    }
    let path = request.uri.path();
    if request.method == Method::GET && path == "/v1/health" {
        return success(json!({
            "name": "tfy-bgm",
            "version": env!("CARGO_PKG_VERSION"),
            "api": "v1",
            "running": true,
            "ready": shared.ready(),
            "port": config.port,
        }));
    }
    let task_path = path.strip_prefix("/v1/tasks/");
    let is_events = request.method == Method::GET
        && (path == "/v1/events" || task_path.is_some_and(|path| path.ends_with("/events")));
    let query: HashMap<_, _> =
        url::form_urlencoded::parse(request.uri.query().unwrap_or_default().as_bytes())
            .into_owned()
            .collect();
    let auth = request
        .headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .or_else(|| {
            request
                .headers
                .get("x-tfy-token")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
        })
        .or_else(|| {
            is_events
                .then(|| query.get("token").map(String::as_str))
                .flatten()
        });
    if !config.token.is_empty() && auth != Some(config.token.as_str()) {
        return failure(StatusCode::UNAUTHORIZED, "缺少或错误的 API token");
    }
    if request.method == Method::GET && (path == "/v1/tasks" || task_path.is_some()) {
        let Ok(tasks) = shared.tasks.lock() else {
            return failure(StatusCode::INTERNAL_SERVER_ERROR, "任务队列不可用");
        };
        if path == "/v1/tasks" {
            return Response::Json(tasks.list(&query));
        }
        let path = task_path.unwrap_or_default();
        let id = path.strip_suffix("/events").unwrap_or(path);
        let Some(task) = tasks.subscribe(id) else {
            return failure(StatusCode::NOT_FOUND, "任务不存在");
        };
        if is_events {
            return Response::TaskEvents(task);
        }
        let snapshot = json!(task.borrow().snapshot);
        return success(snapshot);
    }
    if let Some(operation) = tasks::Operation::parse(&request, &query) {
        let operation = match operation {
            Ok(operation) => operation,
            Err(error) => return Response::Json(Err(error)),
        };
        let submitted = matches!(
            operation,
            tasks::Operation::SubmitAudio(_) | tasks::Operation::LoadAsr(_)
        );
        let result = dispatch(&shared, |reply| Command::Tasks { operation, reply }).await;
        return match result {
            Ok(task) if submitted => {
                let Some(id) = task["id"].as_str() else {
                    return failure(StatusCode::INTERNAL_SERVER_ERROR, "任务 ID 缺失");
                };
                let location = format!("/v1/tasks/{id}");
                Response::Accepted { task, location }
            }
            result => Response::Json(result),
        };
    }
    if records::Operation::is_route(&request.method, &request.uri) {
        let result = match records::Operation::parse(&request) {
            Ok(request) => dispatch(&shared, |reply| Command::Records { request, reply }).await,
            Err(failure) => Err(failure),
        };
        return match result {
            Ok(records::Payload::Json(value)) => success(value),
            Ok(records::Payload::Download { filename, content }) => {
                Response::Download { filename, content }
            }
            Err(failure) => Response::Json(Err(failure)),
        };
    }
    if request.method == Method::GET {
        let snapshot = |value: Value| {
            if value.is_null() {
                failure(StatusCode::SERVICE_UNAVAILABLE, "界面尚未完成启动")
            } else {
                success(value)
            }
        };
        match path {
            "/v1/state" => return snapshot(shared.snapshot()),
            "/v1/library" => {
                return snapshot(
                    shared
                        .library
                        .read()
                        .map(|value| value.clone())
                        .unwrap_or_default(),
                );
            }
            "/v1/events" => {
                return match StreamOptions::parse(&request.headers, &query) {
                    Ok(options) => Response::Events(options),
                    Err(error) => failure(StatusCode::BAD_REQUEST, error),
                };
            }
            "/v1/outputs" => {}
            _ => return failure(StatusCode::NOT_FOUND, "接口不存在"),
        }
    }
    if request.method != Method::POST && !(request.method == Method::GET && path == "/v1/outputs") {
        return failure(StatusCode::NOT_FOUND, "接口不存在");
    }

    let action = match path {
        "/v1/library/rescan" => "library.rescan",
        "/v1/outputs" if request.method == Method::GET => "outputs.list",
        "/v1/outputs" => "outputs.set",
        "/v1/play" | "/v1/pause" | "/v1/resume" | "/v1/toggle" | "/v1/next" | "/v1/previous"
        | "/v1/seek" | "/v1/volume" | "/v1/crossfade" => &path[4..],
        path if path.starts_with("/v1/command/") => &path[12..],
        _ => return failure(StatusCode::NOT_FOUND, "接口不存在"),
    };
    if !matches!(
        action,
        "state"
            | "play"
            | "pause"
            | "resume"
            | "toggle"
            | "next"
            | "previous"
            | "seek"
            | "volume"
            | "crossfade"
            | "outputs.list"
            | "outputs.refresh"
            | "outputs.unlock"
            | "outputs.set"
            | "library.rescan"
    ) {
        return failure(StatusCode::NOT_FOUND, "命令不存在");
    }
    let params = if request.body.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&request.body) {
            Ok(value) if value.is_object() => value,
            _ => return failure(StatusCode::BAD_REQUEST, "请求体需要 JSON 对象"),
        }
    };
    let params = if path.starts_with("/v1/command/") && params.get("params").is_some() {
        params["params"].clone()
    } else {
        params
    };
    if !params.is_object() {
        return failure(StatusCode::BAD_REQUEST, "params 需要 JSON 对象");
    }
    Response::Json(
        dispatch(&shared, |reply| Command::Playback {
            action: action.to_owned(),
            params,
            reply,
        })
        .await,
    )
}

async fn dispatch<T>(
    shared: &Shared,
    command: impl FnOnce(oneshot::Sender<Result<T, Failure>>) -> Command,
) -> Result<T, Failure> {
    let unavailable = |text: &str| (StatusCode::SERVICE_UNAVAILABLE, text.to_owned());
    if !shared.ready() {
        return Err(unavailable("界面尚未完成启动"));
    }
    let (sender, receiver) = oneshot::channel();
    shared
        .commands
        .try_send(command(sender))
        .map_err(|_| unavailable("界面未就绪或当前繁忙"))?;
    match tokio::time::timeout(COMMAND_TIMEOUT, receiver).await {
        Ok(Ok(reply)) => reply,
        Ok(Err(_)) => Err(unavailable("界面未就绪")),
        Err(_) => Err((StatusCode::GATEWAY_TIMEOUT, "命令执行超时".into())),
    }
}

pub fn update_output_routes(
    existing: &[OutputRoute],
    params: &Value,
) -> Result<Vec<OutputRoute>, (StatusCode, String)> {
    let bad = |text: &str| (StatusCode::BAD_REQUEST, text.to_owned());
    let routes = if let Some(incoming) = params.get("outputs").or_else(|| params.get("routes")) {
        serde_json::from_value(incoming.clone()).map_err(|_| bad("输出路由列表无效"))?
    } else {
        let payload = params.get("route").unwrap_or(params);
        let id = payload["deviceId"]
            .as_str()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| bad("需要输出设备 ID"))?;
        let volume = payload
            .get("volume")
            .map(|value| {
                value
                    .as_f64()
                    .map(|volume| volume.clamp(0., 1.) as f32)
                    .ok_or_else(|| bad("volume 必须是 0~1 的数字"))
            })
            .transpose()?;
        let muted = payload["muted"].as_bool();
        let enabled = payload["enabled"].as_bool();
        let label = payload["label"].as_str();
        if volume.is_none() && muted.is_none() && enabled.is_none() && label.is_none() {
            return Err(bad("没有可更新的字段"));
        }
        let mut routes = existing.to_vec();
        let index = if let Some(index) = routes.iter().position(|route| route.device_id == id) {
            index
        } else {
            routes.push(OutputRoute {
                device_id: id.into(),
                label: id.into(),
                ..OutputRoute::default()
            });
            routes.len() - 1
        };
        let route = &mut routes[index];
        if let Some(volume) = volume {
            route.volume = volume;
        }
        if let Some(muted) = muted {
            route.muted = muted;
        }
        if let Some(enabled) = enabled {
            route.enabled = enabled;
        }
        if let Some(label) = label {
            route.label = label.into();
        }
        routes
    };
    let ids: HashSet<_> = routes
        .iter()
        .map(|route| route.device_id.trim())
        .filter(|id| !id.is_empty())
        .collect();
    if ids.len() + usize::from(!ids.contains("default")) > MAX_OUTPUT_ROUTES {
        return Err((
            StatusCode::CONFLICT,
            format!("输出设备已达上限（{MAX_OUTPUT_ROUTES} 路）"),
        ));
    }
    Ok(routes)
}

pub fn library_payload(library: &bgm_core::LibrarySnapshot) -> Value {
    let mut snapshot = json!(library);
    if let Some(songs) = snapshot["songs"].as_array_mut() {
        for song in songs {
            song["format"] = json!(song["fileName"].as_str().and_then(|file| {
                Path::new(file)
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(str::to_ascii_lowercase)
            }));
        }
    }
    snapshot
}

async fn send_json(stream: &mut TcpStream, reply: Reply, origin: Option<&str>) -> io::Result<()> {
    let (status, body) = match reply {
        Ok(data) => (StatusCode::OK, json!({"ok": true, "data": data})),
        Err((status, error)) => (status, json!({"ok": false, "error": error})),
    };
    let body = body.to_string();
    send_headers(
        stream,
        status,
        "application/json",
        Some(body.len()),
        origin,
        None,
    )
    .await?;
    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(body.as_bytes())).await?
}

async fn send_headers(
    stream: &mut TcpStream,
    status: StatusCode,
    content_type: &str,
    len: Option<usize>,
    origin: Option<&str>,
    extra: Option<(&str, &str)>,
) -> io::Result<()> {
    use std::fmt::Write;
    let mut headers = format!(
        "HTTP/1.1 {} {}\r\nConnection: close\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or(""),
    );
    if let Some(len) = len {
        write!(headers, "Content-Length: {len}\r\n").unwrap();
    }
    if let Some(origin) = origin {
        write!(headers, "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Headers: Authorization, Content-Type, X-TFY-Token, Last-Event-ID\r\nAccess-Control-Allow-Methods: GET, POST, PUT, DELETE, OPTIONS\r\n").unwrap();
    }
    if let Some((name, value)) = extra {
        write!(headers, "{name}: {value}\r\n").unwrap();
        if origin.is_some() {
            write!(headers, "Access-Control-Expose-Headers: {name}\r\n").unwrap();
        }
    }
    headers.push_str("\r\n");
    tokio::time::timeout(REQUEST_TIMEOUT, stream.write_all(headers.as_bytes())).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(server: &Server, request: &str) -> std::io::BufReader<std::net::TcpStream> {
        use std::io::Write;
        let port = server.shared.config.read().unwrap().port;
        let mut socket = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        socket.write_all(request.as_bytes()).unwrap();
        std::io::BufReader::new(socket)
    }

    fn exchange(server: &Server, request: &str) -> (u16, String, Value) {
        use std::io::Read;
        let mut reply = String::new();
        socket(server, request).read_to_string(&mut reply).unwrap();
        let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
        let status = headers
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (
            status,
            headers.to_owned(),
            serde_json::from_str(body).unwrap_or_default(),
        )
    }

    fn frame(stream: &mut impl std::io::BufRead) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            assert_ne!(stream.read_line(&mut line).unwrap(), 0, "事件流提前关闭");
            if line.trim_end().is_empty() {
                return lines;
            }
            lines.push(line.trim_end().to_owned());
        }
    }

    #[test]
    fn real_loopback_http_and_sse_replay_and_revoke_idle_subscribers() {
        use std::io::Read;
        let server = Server::start(Config {
            port: 0,
            token: "fixture".into(),
            ..Config::default()
        })
        .unwrap();
        let (_, _, health) = exchange(
            &server,
            "GET /v1/health HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        assert_eq!(health["data"]["name"], "tfy-bgm");
        assert_eq!(health["data"]["api"], "v1");
        assert_eq!(health["data"]["ready"], false);
        assert_eq!(
            health["data"]["port"],
            server.shared.config.read().unwrap().port
        );
        assert_eq!(
            exchange(
                &server,
                "GET /v1/state HTTP/1.1\r\nHost: localhost\r\nX-TFY-Token: fixture\r\n\r\n"
            )
            .0,
            503
        );
        assert_eq!(
            exchange(&server, "GET /v1/state HTTP/1.1\r\nHost: localhost\r\n\r\n").0,
            401
        );
        assert_eq!(
            exchange(
                &server,
                "GET /v1/state HTTP/1.1\r\nHost: localhost\r\nOrigin: https://other.example\r\n\r\n"
            )
            .0,
            403
        );
        let (status, headers, _) = exchange(
            &server,
            "OPTIONS /v1/state HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:3000\r\n\r\n",
        );
        assert_eq!(status, 204);
        assert!(headers.contains("Access-Control-Allow-Origin: http://localhost:3000"));
        assert!(headers.contains("X-TFY-Token, Last-Event-ID"));
        assert_eq!(
            exchange(
                &server,
                "GET /v1/events?token=fixture&lastEventId=wrong HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .0,
            400
        );

        server.publish(json!({"ready":true,"track":null}));
        let (_, _, snapshot) = exchange(
            &server,
            "GET /v1/state HTTP/1.1\r\nHost: localhost\r\nX-TFY-Token: fixture\r\n\r\n",
        );
        assert_eq!(snapshot["data"]["ready"], true);

        let mut live = socket(
            &server,
            "GET /v1/events?token=fixture HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        let headers = frame(&mut live);
        assert!(
            headers
                .iter()
                .any(|line| line == "Content-Type: text/event-stream")
        );
        let first = json!({"ready":true,"track":{"key":"epic/one"}});
        server.publish(first.clone());
        let event = frame(&mut live);
        assert_eq!(event[..2], ["id: 2", "event: state"]);
        assert_eq!(
            serde_json::from_str::<Value>(&event[2][6..]).unwrap(),
            json!({"type":"state","state":first})
        );
        drop(live);

        server.publish(json!({"ready":true,"track":{"key":"epic/two"}}));
        server.publish(json!({"ready":true,"track":{"key":"epic/three"}}));
        let mut replay = socket(
            &server,
            "GET /v1/events?token=fixture HTTP/1.1\r\nHost: localhost\r\nLast-Event-ID: 2\r\n\r\n",
        );
        frame(&mut replay);
        assert_eq!(frame(&mut replay)[0], "id: 3");
        assert_eq!(frame(&mut replay)[0], "id: 4");
        server.update_config(Config {
            token: "replacement".into(),
            ..Config::default()
        });
        assert_eq!(replay.read(&mut [0; 1]).unwrap(), 0);
        assert_eq!(
            exchange(
                &server,
                "GET /v1/state HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture\r\n\r\n"
            )
            .0,
            401
        );

        let mut legacy = socket(
            &server,
            "GET /v1/events?token=replacement&legacy=1 HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        frame(&mut legacy);
        server.publish(json!({"ready":true}));
        let event = frame(&mut legacy);
        assert_eq!(event.len(), 2);
        assert_eq!(event[0], "id: 5");
        assert!(event[1].starts_with("data: "));
    }

    #[test]
    fn real_command_timeout_closes_unhandled_command_after_six_seconds() {
        let mut server = Server::start(Config {
            port: 0,
            ..Config::default()
        })
        .unwrap();
        server.publish(json!({"ready": true}));
        let start = std::time::Instant::now();
        let (status, _, body) = exchange(
            &server,
            "POST /v1/pause HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
        );
        assert_eq!(status, 504);
        assert_eq!(body["ok"], false);
        assert!(start.elapsed() >= COMMAND_TIMEOUT);
        let Some(Command::Playback { reply, .. }) = server.next_command() else {
            panic!("缺少播放命令")
        };
        assert!(reply.is_closed());
    }

    #[test]
    fn real_command_errors_keep_handler_status_and_json_envelope() {
        let mut server = Server::start(Config {
            port: 0,
            ..Config::default()
        })
        .unwrap();
        server.publish(json!({"ready": true}));
        let config = server.shared.config.read().unwrap().clone();
        let client = std::thread::spawn(move || {
            use std::io::{Read, Write};
            for action in ["/v1/outputs", "/v1/volume"] {
                let mut stream =
                    std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, config.port)).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                write!(
                    stream,
                    "POST {action} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n"
                )
                .unwrap();
                let mut reply = String::new();
                stream.read_to_string(&mut reply).unwrap();
                let (headers, data) = reply.split_once("\r\n\r\n").unwrap();
                let code = if action == "/v1/outputs" {
                    "409"
                } else {
                    "500"
                };
                assert_eq!(headers.split_ascii_whitespace().nth(1), Some(code));
                assert_eq!(
                    serde_json::from_str::<Value>(data).unwrap(),
                    json!({"ok":false,"error":"fixture"})
                );
            }
        });
        for (action, status) in [
            ("outputs.set", StatusCode::CONFLICT),
            ("volume", StatusCode::INTERNAL_SERVER_ERROR),
        ] {
            let until = std::time::Instant::now() + Duration::from_secs(4);
            let command = loop {
                if let Some(command) = server.next_command() {
                    break command;
                }
                assert!(std::time::Instant::now() < until);
                std::thread::sleep(Duration::from_millis(2));
            };
            let Command::Playback {
                action: actual,
                reply,
                ..
            } = command
            else {
                panic!("缺少播放命令")
            };
            assert_eq!(actual, action);
            reply.send(Err((status, "fixture".into()))).unwrap();
        }
        client.join().unwrap();
    }

    #[test]
    fn real_subtitle_routes_filter_edit_export_and_delete_persisted_records() {
        use std::io::{Read, Write};

        let sandbox = tempfile::tempdir().unwrap();
        let root = sandbox.path();
        let directory = root.join("subtitles");
        let separation = root.join("separations/music_fixture-c93c349e274b7b86");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::create_dir_all(&separation).unwrap();
        let active = directory.join("music_fixture-c93c349e274b7b86.json");
        let retired = directory.join("retired_fixture-ed5261fe29cc41ac.json");
        let source_audio = root.join("original.mp3");
        std::fs::write(&source_audio, b"original").unwrap();
        let old_srt = separation.join("subtitles.srt");
        std::fs::write(&old_srt, "old").unwrap();
        std::fs::write(
            separation.join("separation.json"),
            json!({
                "key":"music/fixture", "modelName":"fixture",
                "vocalsPath": separation.join("vocals.wav"),
                "accompanimentPath": separation.join("accompaniment.wav"),
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(separation.join("vocals.wav"), b"mock-wave").unwrap();
        std::fs::write(&active, json!({
            "key":"music/fixture", "title":"测试 曲目", "fileName":"original.mp3", "durationMs": 3000,
            "createdAtMs":10, "segments":[{"start":0.25,"end":1.5,"text":"First VERSE"}],
            "srtPath":old_srt, "sourcePath":source_audio, "extensionField": {"keep":true}
        }).to_string()).unwrap();
        std::fs::write(
            &retired,
            json!({
                "key":"retired/fixture", "title":"已移除的歌曲", "createdAtMs":5,
                "segments":[{"start":0.,"end":1.,"text":"Another verse"}]
            })
            .to_string(),
        )
        .unwrap();

        let mut server = Server::start(Config {
            port: 0,
            token: "fixture".into(),
            ..Config::default()
        })
        .unwrap();
        server.publish(json!({"ready":true}));
        let port = server.shared.config.read().unwrap().port;
        let (finished, stop) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            loop {
                match stop.try_recv() {
                    Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
                match server.next_command() {
                    Some(Command::Records { request, reply }) => {
                        let _ = reply.send(request.execute(&directory, None));
                    }
                    Some(_) => panic!("字幕请求错误进入其他队列"),
                    None => std::thread::sleep(Duration::from_millis(1)),
                }
            }
        });

        let request = |method: &str, uri: &str, token: &str, body: &str| {
            let mut stream = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(8)))
                .unwrap();
            write!(stream, "{method} {uri} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:3000\r\nX-TFY-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            let (headers, body) = response.split_once("\r\n\r\n").unwrap();
            let status = headers
                .split_ascii_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u16>()
                .unwrap();
            (status, headers.to_owned(), body.to_owned())
        };
        let json_request = |method, uri, body, expected| {
            let (status, _, response) = request(method, uri, "fixture", body);
            assert_eq!(status, expected, "{response}");
            let envelope: Value = serde_json::from_str(&response).unwrap();
            assert_eq!(envelope["ok"], expected < 400);
            envelope["data"].clone()
        };
        assert_eq!(request("GET", "/v1/subtitles", "incorrect", "").0, 401);
        json_request("GET", "/v1/subtitles?limit=nope", "", 400);
        json_request("GET", "/v1/subtitles/track", "", 400);
        json_request("GET", "/v1/subtitles/track?key=other", "", 404);
        let list = json_request("GET", "/v1/subtitles?q=verse&limit=1", "", 200);
        assert_eq!(list["total"], 2);
        assert_eq!(list["records"][0]["key"], "music/fixture");
        assert_eq!(list["records"][0]["segmentCount"], 1);
        assert!(list["records"][0].get("segments").is_none());
        let page = json_request(
            "GET",
            "/v1/subtitles?q=VeRsE&offset=1&include=content",
            "",
            200,
        );
        assert_eq!(page["records"][0]["key"], "retired/fixture");
        assert_eq!(page["records"][0]["segments"][0]["text"], "Another verse");

        let track = "/v1/subtitles/track?key=music%2Ffixture";
        assert_eq!(
            json_request("GET", track, "", 200)["segments"][0]["text"],
            "First VERSE"
        );
        let original = std::fs::read(&active).unwrap();
        json_request(
            "PUT",
            track,
            r#"{"segments":[{"start":2,"end":1,"text":"invalid"}]}"#,
            400,
        );
        assert_eq!(std::fs::read(&active).unwrap(), original);
        let updated = json_request(
            "PUT",
            track,
            r#"{"segments":[{"start":0.1,"end":1.1,"text":"New text"}]}"#,
            200,
        );
        assert_eq!(updated["segments"][0]["text"], "New text");
        assert!(updated["updatedAtMs"].as_u64().is_some());
        assert!(updated.get("extensionField").is_none());
        assert!(
            std::fs::read_to_string(&old_srt)
                .unwrap()
                .contains("New text")
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(&active).unwrap()).unwrap();
        assert_eq!(saved["extensionField"]["keep"], true);

        let typed = r#"{"key":"music/fixture","format":"LRC","text":"[00:00.50]已编辑"}"#;
        let updated = json_request("PUT", "/v1/subtitles/text", typed, 200);
        assert_eq!(updated["segments"][0]["end"], 3.0);
        let (status, headers, download) = request(
            "GET",
            "/v1/subtitles/export?key=music%2Ffixture&format=lrc",
            "fixture",
            "",
        );
        assert_eq!(status, 200);
        assert!(headers.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(headers.contains("filename*=UTF-8''%E6%B5%8B%E8%AF%95%20%E6%9B%B2%E7%9B%AE.lrc"));
        assert!(headers.contains("Access-Control-Expose-Headers: Content-Disposition"));
        assert!(download.contains("已编辑"));
        let result_path = "/v1/separation/result?key=music%2Ffixture";
        let separated = json_request("GET", result_path, "", 200);
        assert_eq!(separated["separation"]["modelName"], "fixture");
        assert_eq!(separated["files"][0]["exists"], true);
        assert_eq!(separated["files"][0]["bytes"], 9);
        assert_eq!(separated["files"][1]["exists"], false);
        assert_eq!(separated["files"][1]["bytes"], 0);
        assert_eq!(json_request("DELETE", track, "", 200)["deleted"], true);
        assert_eq!(json_request("DELETE", track, "", 200)["deleted"], false);
        assert!(!active.exists());
        assert!(!separation.exists());
        json_request("GET", result_path, "", 404);
        assert!(retired.exists());
        assert_eq!(std::fs::read(&source_audio).unwrap(), b"original");
        finished.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn loopback_tasks_submit_filter_stream_and_release_queue_after_failure() {
        use std::io::{Read, Write};

        let mut server = Server::start(Config {
            port: 0,
            token: "fixture".into(),
            ..Config::default()
        })
        .unwrap();
        server.publish(json!({"ready": true}));
        let port = server.shared.config.read().unwrap().port;
        let post = |server: &mut Server, path: &'static str, data: Value| {
            let body = data.to_string();
            let response = std::thread::spawn(move || {
                let mut connection =
                    std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(8)))
                    .unwrap();
                write!(connection, "POST {path} HTTP/1.1\r\nHost: localhost\r\nX-TFY-Token: fixture\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
                let mut response = String::new();
                connection.read_to_string(&mut response).unwrap();
                let (headers, body) = response.split_once("\r\n\r\n").unwrap();
                let code: u16 = headers
                    .split_ascii_whitespace()
                    .nth(1)
                    .unwrap()
                    .parse()
                    .unwrap();
                (
                    code,
                    headers.to_owned(),
                    serde_json::from_str::<Value>(body).unwrap(),
                )
            });
            let deadline = std::time::Instant::now() + Duration::from_secs(4);
            loop {
                if let Some(Command::Tasks { operation, reply }) = server.next_command() {
                    let job = match operation {
                        tasks::Operation::SubmitAudio(job) => tasks::Job::Audio(job),
                        tasks::Operation::LoadAsr(id) => tasks::Job::AsrLoad {
                            id,
                            root: "/fixture".into(),
                        },
                        _ => panic!("缺少任务"),
                    };
                    let _ = reply.send(server.submit_task(job));
                    break;
                }
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            response.join().unwrap()
        };
        assert_eq!(
            exchange(&server, "GET /v1/tasks HTTP/1.1\r\nHost: localhost\r\n\r\n").0,
            401
        );
        assert_eq!(exchange(&server,"GET /v1/tasks?status=unknown HTTP/1.1\r\nHost: localhost\r\nX-TFY-Token: fixture\r\n\r\n").0,400);

        let mut global = socket(
            &server,
            "GET /v1/events?token=fixture HTTP/1.1\r\nHost: localhost\r\n\r\n",
        );
        frame(&mut global);
        let input = json!({"key":"library/song","separationModel":"uvr-voc-ft","separationTarget":"vocals"});
        let (status, headers, submitted) =
            post(&mut server, "/v1/subtitles/generate", input.clone());
        assert_eq!(status, 202);
        let task = &submitted["data"]["task"];
        let id = task["id"].as_str().unwrap();
        assert_eq!(task["status"], "queued");
        assert!(headers.contains(&format!("Location: /v1/tasks/{id}")));
        let first = frame(&mut global);
        assert_eq!(first[1], "event: task");
        assert_eq!(
            serde_json::from_str::<Value>(&first[2][6..]).unwrap()["task"]["id"],
            id
        );
        assert_eq!(post(&mut server, "/v1/subtitles/generate", input).0, 409);
        let (status, _, queued) = post(&mut server, "/v1/models/load", json!({"id":"fixture"}));
        assert_eq!(status, 202);
        assert_eq!(queued["data"]["task"]["kind"], "asrLoad");

        let (work, request) = server.next_task().unwrap();
        assert!(matches!(request, tasks::Job::Audio(job) if job.captions));
        assert!(server.next_task().is_none());
        let (_, _, details) = exchange(
            &server,
            &format!(
                "GET /v1/tasks/{id} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer fixture\r\n\r\n"
            ),
        );
        assert_eq!(details["data"]["status"], "running");
        let endpoint =
            format!("GET /v1/tasks/{id}/events?token=fixture HTTP/1.1\r\nHost: localhost\r\n\r\n");
        let mut progress = socket(&server, &endpoint);
        frame(&mut progress);
        assert_eq!(frame(&mut progress)[1], "event: snapshot");
        work.progress("transcribing", "识别中".into(), 2, 5);
        let update = frame(&mut progress);
        assert_eq!(update[1], "event: progress");
        assert_eq!(
            serde_json::from_str::<Value>(&update[2][6..]).unwrap()["task"]["current"],
            2
        );
        work.finish(Err("fixture failure".into()));
        drop(work);
        let final_event = frame(&mut progress);
        assert_eq!(final_event[1], "event: failed");
        assert_eq!(
            serde_json::from_str::<Value>(&final_event[2][6..]).unwrap()["task"]["error"],
            "fixture failure"
        );
        assert_eq!(progress.read(&mut [0; 1]).unwrap(), 0);
        let mut late = socket(&server, &endpoint);
        frame(&mut late);
        assert_eq!(frame(&mut late)[1], "event: failed");

        let (load, request) = server.next_task().unwrap();
        assert!(matches!(request, tasks::Job::AsrLoad { .. }));
        load.finish(Ok(json!({"loaded":{"name":"fixture"}})));
        drop(load);
        assert!(server.next_task().is_none());
        let (_, _, filtered) = exchange(
            &server,
            "GET /v1/tasks?kind=asrLoad&status=succeeded&limit=1 HTTP/1.1\r\nHost: localhost\r\nX-TFY-Token: fixture\r\n\r\n",
        );
        assert_eq!(filtered["data"]["total"], 1);
        assert_eq!(
            filtered["data"]["tasks"][0]["result"]["loaded"]["name"],
            "fixture"
        );
    }

    #[test]
    fn replay_resets_future_ids_and_reports_evicted_history() {
        let mut log = StateLog::default();
        for _ in 0..EVENT_HISTORY + 2 {
            log.publish(json!({"ready": true}));
        }
        let future = EventBatch::read(&log, 1_000, false);
        assert_eq!(future.cursor, log.last_id);
        assert!(future.events.is_empty());
        assert_eq!(
            serde_json::from_str::<Value>(&future.resync.unwrap().1).unwrap()["reason"],
            "reset"
        );
        let gap = EventBatch::read(&log, 1, false);
        assert_eq!(gap.events.len(), EVENT_HISTORY);
        assert_eq!(gap.events[0].0, 3);
        let (id, data) = gap.resync.unwrap();
        assert_eq!(id, 2);
        assert_eq!(
            serde_json::from_str::<Value>(&data).unwrap()["oldestEventId"],
            3
        );
    }

    #[test]
    fn config_round_trips_upstream_and_legacy_cors_without_dropping_tokens() {
        let nested = Config::from_document(&json!({
            "port": 9123, "token": " fixture ",
            "cors": {"mode":"custom","origins":[" HTTPS://OBS.example/ ", "invalid"]}
        }));
        assert_eq!(nested.port, 9123);
        assert_eq!(nested.token, "fixture");
        assert_eq!(
            nested.document()["cors"]["origins"],
            json!(["https://obs.example"])
        );
        assert!(nested.allows("http://localhost:3000"));
        assert!(nested.allows("https://OBS.example"));
        assert!(!nested.allows("https://other.example"));
        let legacy = Config::from_document(
            &json!({"enabled":false,"token":"fixture","cors":"any","corsOrigins":[]}),
        );
        assert!(!legacy.enabled);
        assert!(legacy.allows("https://other.example"));
        assert_eq!(Config::from_document(&legacy.document()).token, "fixture");
        let malformed = Config::from_document(&json!({"token":"fixture","cors":14}));
        assert_eq!(malformed.token, "fixture");
        assert!(!malformed.allows("https://other.example"));
    }

    #[test]
    fn output_patches_require_values_and_reject_a_ninth_route() {
        let routes = [OutputRoute::default()];
        assert_eq!(
            update_output_routes(&routes, &json!({"deviceId":"default"}))
                .unwrap_err()
                .0,
            400
        );
        let result = update_output_routes(
            &routes,
            &json!({"deviceId":"default","muted":true,"volume":-1}),
        )
        .unwrap();
        assert!(result[0].muted);
        assert_eq!(result[0].volume, 0.);
        let many: Vec<_> = (1..=8)
            .map(|index| json!({"deviceId": format!("route-{index}")}))
            .collect();
        assert_eq!(
            update_output_routes(&routes, &json!({"outputs": many}))
                .unwrap_err()
                .0,
            409
        );
    }

    #[test]
    fn cors_matches_origins_by_host_and_scheme() {
        let mut config = Config::default();
        for origin in [
            "http://localhost:3000",
            "https://127.0.0.1",
            "http://[::1]:3000",
            "tauri://localhost",
        ] {
            assert!(config.allows(origin), "{origin}");
        }
        for origin in [
            "null",
            "https://localhost.example.com",
            "https://localhost@evil.example",
            "ftp://localhost",
            "https://localhost/a",
        ] {
            assert!(!config.allows(origin), "{origin}");
        }
        config.cors = "custom".into();
        config.cors_origins = vec!["https://obs.example".into()];
        assert!(config.allows("https://obs.example"));
        assert!(config.allows("http://localhost"));
    }
    #[test]
    fn routes_authenticate_and_dispatch_all_documented_actions() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let config = Config { token: "fixture-secret".into(), ..Config::default() };
            let (commands, mut incoming) = mpsc::channel::<Command>(32);
            let (updates, _) = watch::channel(());
            let mut log = StateLog::default();
            log.publish(json!({"ready": true}));
            let shared = Arc::new(Shared {
                config: RwLock::new(config.clone()),
                state: RwLock::new(log),
                updates,
                library: RwLock::new(json!({"songs": []})),
                commands,
                tasks: Mutex::default(),
            });
            let worker = tokio::spawn(async move {
                while let Some(command) = incoming.recv().await {
                    if let Command::Playback { action, params, reply } = command {
                        let _ = reply.send(Ok(json!({"action": action, "params": params})));
                    }
                }
            });
            let request = |method, path: &str, token: Option<&str>, body: &str| {
                let mut headers = HeaderMap::new();
                if let Some(token) = token {
                    headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
                }
                Request { method, uri: path.parse().unwrap(), headers, body: body.as_bytes().to_vec() }
            };
            let get = |path, token| route(request(Method::GET, path, token, ""), &config, shared.clone());
            assert!(matches!(get("/v1/health", None).await, Response::Json(Ok(_))));
            assert!(matches!(get("/v1/state", Some("incorrect")).await, Response::Json(Err((StatusCode::UNAUTHORIZED, _)))));
            assert!(matches!(get("/v1/state", Some("fixture-secret")).await, Response::Json(Ok(value)) if value["ready"] == true));
            assert!(matches!(get("/v1/events?token=fixture-secret", None).await, Response::Events(_)));
            assert!(matches!(get("/v1/events?token=incorrect", None).await, Response::Json(Err((StatusCode::UNAUTHORIZED, _)))));
            for (method, path, action) in [
                (Method::GET, "/v1/outputs", "outputs.list"),
                (Method::POST, "/v1/outputs", "outputs.set"),
                (Method::POST, "/v1/library/rescan", "library.rescan"),
                (Method::POST, "/v1/play", "play"),
                (Method::POST, "/v1/pause", "pause"),
                (Method::POST, "/v1/resume", "resume"),
                (Method::POST, "/v1/toggle", "toggle"),
                (Method::POST, "/v1/next", "next"),
                (Method::POST, "/v1/previous", "previous"),
                (Method::POST, "/v1/seek", "seek"),
                (Method::POST, "/v1/volume", "volume"),
                (Method::POST, "/v1/crossfade", "crossfade"),
            ] {
                let query = request(method, path, Some("fixture-secret"), "");
                assert!(matches!(route(query, &config, shared.clone()).await,
                    Response::Json(Ok(value)) if value["action"] == action));
            }
            assert!(matches!(route(request(Method::POST, "/v1/command/play", Some("fixture-secret"), r#"{"params":[]}"#), &config, shared.clone()).await,
                Response::Json(Err((StatusCode::BAD_REQUEST,_)))));
            assert!(matches!(get("/v1/library", Some("fixture-secret")).await, Response::Json(Ok(value)) if value["songs"] == json!([])));
            assert!(matches!(get("/v1/unknown", Some("fixture-secret")).await, Response::Json(Err((StatusCode::NOT_FOUND,_)))));
            for action in [
                "state", "play", "pause", "resume", "toggle", "next", "previous",
                "seek", "volume", "crossfade", "outputs.list", "outputs.refresh", "outputs.unlock", "outputs.set", "library.rescan",
            ] {
                let query = request(Method::POST, &format!("/v1/command/{action}"),
                    Some("fixture-secret"), r#"{"params":{"fixture":7}}"#);
                assert!(matches!(route(query, &config, shared.clone()).await,
                    Response::Json(Ok(value)) if value["action"] == action && value["params"]["fixture"] == 7));
            }
            worker.abort();
        });
    }
}
