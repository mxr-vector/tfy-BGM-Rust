//! 本机 API 的后台任务状态与请求。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use http::{Method, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::library::separation::Target;

use super::{Failure, Request};

const HISTORY_LIMIT: usize = 50;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Subtitles,
    Separation,
    AsrLoad,
}

impl Kind {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "subtitles" => Some(Self::Subtitles),
            "separation" => Some(Self::Separation),
            "asrLoad" | "asr-load" => Some(Self::AsrLoad),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl Status {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub(super) fn terminal_event(self) -> Option<&'static str> {
        match self {
            Self::Succeeded => Some("done"),
            Self::Failed => Some("failed"),
            _ => None,
        }
    }
}

pub enum Job {
    Audio(AudioJob),
    AsrLoad { id: String, root: PathBuf },
}

pub struct AudioJob {
    pub key: String,
    pub model: String,
    pub target: Option<Target>,
    pub captions: bool,
}

impl Job {
    fn kind(&self) -> Kind {
        match self {
            Self::Audio(job) if job.captions => Kind::Subtitles,
            Self::Audio(_) => Kind::Separation,
            Self::AsrLoad { .. } => Kind::AsrLoad,
        }
    }

    fn key(&self) -> Option<&str> {
        match self {
            Self::Audio(job) => Some(&job.key),
            Self::AsrLoad { .. } => None,
        }
    }
}

pub enum Operation {
    SubmitAudio(AudioJob),
    LoadAsr(String),
    AsrModels,
    AsrUnload,
    SeparationModels,
}

impl Operation {
    pub(super) fn parse(
        request: &Request,
        query: &HashMap<String, String>,
    ) -> Option<Result<Self, Failure>> {
        let path = request.uri.path();
        match (&request.method, path) {
            (&Method::GET, "/v1/models") => return Some(Ok(Self::AsrModels)),
            (&Method::GET, "/v1/separation/models") => return Some(Ok(Self::SeparationModels)),
            (&Method::POST, "/v1/models/unload") => return Some(Ok(Self::AsrUnload)),
            (&Method::POST, "/v1/models/load" | "/v1/separation" | "/v1/subtitles/generate") => {}
            _ => return None,
        }
        Some((|| {
            let bad = |field: &str| (StatusCode::BAD_REQUEST, format!("缺少或无效的 {field}"));
            let body: Value = serde_json::from_slice(&request.body)
                .ok()
                .filter(Value::is_object)
                .ok_or_else(|| bad("JSON 请求体"))?;
            let nonempty = |value: &str| !value.trim().is_empty();
            let property = |field: &str| {
                body[field]
                    .as_str()
                    .filter(|value| nonempty(value))
                    .map(str::trim)
            };
            if path == "/v1/models/load" {
                let id = property("id").ok_or_else(|| bad("id"))?;
                return Ok(Self::LoadAsr(id.into()));
            }
            let captions = path == "/v1/subtitles/generate";
            let key = query
                .get("key")
                .filter(|value| nonempty(value))
                .map(|key| key.trim())
                .or_else(|| property("key"))
                .ok_or_else(|| bad("key"))?;
            let (model_field, target_field) = if captions {
                ("separationModel", "separationTarget")
            } else {
                ("model", "target")
            };
            let model = property(model_field).ok_or_else(|| bad(model_field))?;
            let target = match property(target_field) {
                None => None,
                Some("vocals") => Some(Target::Vocals),
                Some("instrumental") => Some(Target::Instrumental),
                _ => return Err(bad(target_field)),
            };
            Ok(Self::SubmitAudio(AudioJob {
                key: key.into(),
                model: model.into(),
                target,
                captions,
            }))
        })())
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Snapshot {
    pub id: String,
    kind: Kind,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    stage: &'static str,
    message: String,
    current: usize,
    total: usize,
    created_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
}

#[derive(Clone)]
pub(super) struct Update {
    pub version: u64,
    pub snapshot: Snapshot,
}

struct Entry {
    state: watch::Sender<Update>,
    job: Option<Job>,
}

#[derive(Default)]
pub(super) struct Store {
    jobs: VecDeque<Entry>,
    sequence: u64,
}

impl Store {
    pub fn submit(
        &mut self,
        job: Job,
        publish: impl FnOnce(&Snapshot) -> u64,
    ) -> Result<Snapshot, Failure> {
        let kind = job.kind();
        let key = job.key();
        for entry in &self.jobs {
            let current = entry.state.borrow();
            let task = &current.snapshot;
            if task.status.terminal_event().is_none()
                && task.kind == kind
                && task.key.as_deref() == key
            {
                return Err((StatusCode::CONFLICT, format!("任务已在队列中：{}", task.id)));
            }
        }
        self.sequence += 1;
        let now = now_ms();
        let snapshot = Snapshot {
            id: format!("task-{now}-{:04}", self.sequence),
            kind,
            status: Status::Queued,
            key: key.map(str::to_owned),
            stage: "queued",
            message: "已加入队列，等待执行".into(),
            current: 0,
            total: 0,
            created_at_ms: now,
            started_at_ms: None,
            finished_at_ms: None,
            error: None,
            result: None,
        };
        let (state, _) = watch::channel(Update {
            version: publish(&snapshot),
            snapshot: snapshot.clone(),
        });
        self.jobs.push_back(Entry {
            state,
            job: Some(job),
        });
        Ok(snapshot)
    }

    pub fn start(&mut self, publish: impl FnOnce(&Snapshot) -> u64) -> Option<(String, Job)> {
        if self
            .jobs
            .iter()
            .any(|entry| entry.state.borrow().snapshot.status == Status::Running)
        {
            return None;
        }
        let entry = self.jobs.iter_mut().find(|entry| entry.job.is_some())?;
        let mut snapshot = entry.state.borrow().snapshot.clone();
        snapshot.status = Status::Running;
        snapshot.stage = "running";
        snapshot.message = "任务开始执行".into();
        snapshot.started_at_ms = Some(now_ms());
        let id = snapshot.id.clone();
        entry.state.send_replace(Update {
            version: publish(&snapshot),
            snapshot,
        });
        Some((id, entry.job.take()?))
    }

    pub fn update(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut Snapshot),
        publish: impl FnOnce(&Snapshot) -> u64,
    ) {
        let Some(entry) = self
            .jobs
            .iter()
            .find(|entry| entry.state.borrow().snapshot.id == id)
        else {
            return;
        };
        let mut snapshot = entry.state.borrow().snapshot.clone();
        if snapshot.status != Status::Running {
            return;
        }
        change(&mut snapshot);
        entry.state.send_replace(Update {
            version: publish(&snapshot),
            snapshot,
        });
        while self.jobs.len() > HISTORY_LIMIT {
            let Some(old) = self.jobs.iter().position(|entry| {
                entry
                    .state
                    .borrow()
                    .snapshot
                    .status
                    .terminal_event()
                    .is_some()
            }) else {
                break;
            };
            self.jobs.remove(old);
        }
    }

    pub fn subscribe(&self, id: &str) -> Option<watch::Receiver<Update>> {
        self.jobs
            .iter()
            .find(|entry| entry.state.borrow().snapshot.id == id)
            .map(|entry| entry.state.subscribe())
    }

    pub fn list(&self, query: &HashMap<String, String>) -> Result<Value, Failure> {
        let filter = |key| {
            query
                .get(key)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
        };
        let invalid = |key: &str| (StatusCode::BAD_REQUEST, format!("无效的任务筛选参数 {key}"));
        let kind = filter("kind")
            .map(|raw| Kind::parse(raw).ok_or_else(|| invalid("kind")))
            .transpose()?;
        let status = filter("status")
            .map(|raw| Status::parse(raw).ok_or_else(|| invalid("status")))
            .transpose()?;
        let limit = filter("limit")
            .map(|raw| raw.parse::<usize>().map_err(|_| invalid("limit")))
            .transpose()?
            .unwrap_or(50)
            .clamp(1, 200);
        let mut total = 0;
        let mut page = Vec::new();
        for entry in self.jobs.iter().rev() {
            let update = entry.state.borrow();
            let task = &update.snapshot;
            if kind.is_none_or(|kind| task.kind == kind)
                && status.is_none_or(|status| task.status == status)
            {
                total += 1;
                if page.len() < limit {
                    page.push(task.clone());
                }
            }
        }
        Ok(json!({ "tasks": page, "total": total, "limit": limit }))
    }
}

impl Snapshot {
    pub fn progress(&mut self, stage: &'static str, message: String, current: usize, total: usize) {
        self.stage = stage;
        self.message = message;
        self.current = current;
        self.total = total;
    }

    pub fn finish(&mut self, result: Result<Value, String>) {
        self.finished_at_ms = Some(now_ms());
        match result {
            Ok(value) => {
                self.status = Status::Succeeded;
                self.stage = "done";
                self.message = "任务完成".into();
                self.result = Some(value);
            }
            Err(error) => {
                self.status = Status::Failed;
                self.stage = "failed";
                self.message = "任务失败".into();
                self.error = Some(error);
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn loaded_model(root: &Path, id: &str) -> Value {
    json!({
        "name": id.rsplit('/').next().unwrap_or(id),
        "path": root.join(id),
        "engine": "sherpa-onnx",
        "threads": std::thread::available_parallelism().map_or(1, |count| count.get().min(4)),
    })
}

pub fn models_info(
    preferences: crate::library::ModelPreferences,
    loaded: Option<&str>,
    loading: Option<&str>,
    failure: Option<&str>,
    downloading: bool,
) -> Value {
    use crate::library::{DEFAULT_ASR_DOWNLOAD_URL, DEFAULT_MODEL_NAME, ModelCatalog};

    let catalog = ModelCatalog::scan(&preferences);
    let root = &catalog.root;
    let default_path = root.join(DEFAULT_MODEL_NAME);
    let models: Vec<_> = catalog.models.iter().map(|model| {
        let missing = model.missing_components.as_deref();
        let (path, name, size) = if let (None, [file]) = (missing, model.files.as_slice()) {
            let size = std::fs::symlink_metadata(root.join(file))
                .ok()
                .filter(|meta| meta.is_file())
                .map_or(0, |meta| meta.len());
            (root.clone(), model.id.trim_start_matches("file:"), size)
        } else {
            let path = root.join(&model.id);
            let size = model_bytes(&path);
            (path, model.id.rsplit('/').next().unwrap_or(&model.id), size)
        };
        json!({
            "id":model.id, "name":name, "path":path,
            "files":model.files, "sizeBytes":size, "isDefault":model.id == DEFAULT_MODEL_NAME,
            "loadable":missing.is_some_and(|missing| missing.is_empty()), "missing":missing.unwrap_or(&[]),
        })
    }).collect();
    let (phase, path) = if let Some(id) = loading {
        ("loading", Some(root.join(id)))
    } else if failure.is_some() {
        ("failed", None)
    } else if let Some(id) = loaded {
        ("success", Some(root.join(id)))
    } else {
        ("idle", None)
    };
    json!({
        "root":root, "isDefaultRoot":catalog.is_default_root,
        "defaultDownloadUrl":DEFAULT_ASR_DOWNLOAD_URL, "downloadUrl":preferences.download_url,
        "defaultModel":{
            "name":DEFAULT_MODEL_NAME, "path":default_path, "installed":catalog.default_files.missing.is_empty(),
            "missing":catalog.default_files.missing, "sizeBytes":model_bytes(&default_path),
            "expectedBytes":catalog.default_files.expected_bytes,
        },
        "models":models, "loaded":loaded.map(|id| loaded_model(root, id)),
        "loadStatus":{"phase":phase,"path":path,"error":failure}, "downloading":downloading,
        "warnings":catalog.warnings,
    })
}

fn model_bytes(path: &Path) -> u64 {
    let mut total = 0_u64;
    let mut directories = vec![path.to_path_buf()];
    while let Some(directory) = directories.pop() {
        if !std::fs::symlink_metadata(&directory).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(directory) {
            for entry in entries.flatten() {
                if let Ok(kind) = entry.file_type() {
                    if kind.is_dir() {
                        directories.push(entry.path());
                    } else if kind.is_file() {
                        total = total.saturating_add(entry.metadata().map_or(0, |meta| meta.len()));
                    }
                }
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{DEFAULT_ASR_DOWNLOAD_URL, ModelPreferences};

    #[test]
    fn model_list_uses_actual_paths_for_root_files_and_nested_models() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("standalone.onnx"), b"fixture").unwrap();
        let nested = root.path().join("collection/example");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("encoder.int8.onnx"), b"nested").unwrap();
        let info = models_info(
            ModelPreferences {
                directory: Some(root.path().to_owned()),
                download_url: DEFAULT_ASR_DOWNLOAD_URL.into(),
                last_model: None,
            },
            None,
            None,
            None,
            false,
        );
        let models = info["models"].as_array().unwrap();
        assert_eq!(
            models[0],
            json!({
                "id":"collection/example", "name":"example", "path":nested,
                "files":["encoder.int8.onnx"], "sizeBytes":6, "isDefault":false,
                "loadable":false, "missing":["conv_frontend.onnx","decoder.int8.onnx","tokenizer/"],
            })
        );
        assert_eq!(
            models[1],
            json!({
                "id":"file:standalone", "name":"standalone", "path":root.path(),
                "files":["standalone.onnx"], "sizeBytes":7, "isDefault":false,
                "loadable":false, "missing":[],
            })
        );
    }
}
