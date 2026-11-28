use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

mod cosyvoice;
mod edge;
mod zipvoice;
use tauri::{
    AppHandle, Emitter, Manager, State,
    ipc::{InvokeBody, Request, Response},
};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::{
    speech_audio,
    state::AppState,
    tts_models,
    tts_script::{self, TtsSegment},
};

use tokio_util::sync::CancellationToken;

const MAX_DURATION_MS: u64 = 30 * 60 * 1000;
const CACHE_MAX_RESULTS: usize = 8;
const CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
static SYNTHESIS: JobSlot = JobSlot::new();
static INSTALL: JobSlot = JobSlot::new();
static LOAD: JobSlot = JobSlot::new();
static ENGINE: parking_lot::Mutex<Option<LoadedTts>> = parking_lot::Mutex::new(None);
static LOADED_STATUS: parking_lot::RwLock<Option<(String, &'static str)>> =
    parking_lot::RwLock::new(None);

struct LoadedTts {
    path: String,
    engine: TtsEngine,
}

impl LoadedTts {
    fn device(&self) -> &'static str {
        match &self.engine {
            TtsEngine::Sherpa(engine) => engine.device(),
            TtsEngine::CosyVoice(engine) => engine.device(),
            TtsEngine::Edge(engine) => engine.device(),
        }
    }
}

enum TtsEngine {
    Sherpa(SherpaEngine),
    CosyVoice(cosyvoice::Engine),
    Edge(edge::Engine),
}

struct SherpaEngine {
    gpu: Option<crate::sherpa_cuda::Worker>,
    cpu: Option<OfflineTts>,
    config: OfflineTtsConfig,
    rate: u32,
}

impl SherpaEngine {
    fn device(&self) -> &'static str {
        if self.gpu.is_some() { "cuda" } else { "cpu" }
    }

    fn load(
        config: OfflineTtsConfig,
        init: crate::sherpa_cuda::Init,
        token: &CancellationToken,
    ) -> Result<Self> {
        let minimum = crate::speech_device::minimum_vram_mib("TFY_TTS_MIN_VRAM_MIB", 2048);
        let gpu = match crate::sherpa_cuda::Worker::try_start(init, minimum, token) {
            Ok(worker) => worker,
            Err(error) => {
                check_cancelled(token)?;
                eprintln!("[tts] CUDA initialization failed; using CPU: {error:#}");
                None
            }
        };
        if let Some(gpu) = gpu {
            return Ok(Self {
                rate: gpu.sample_rate,
                gpu: Some(gpu),
                cpu: None,
                config,
            });
        }
        check_cancelled(token)?;
        let cpu = OfflineTts::create(&config).context("加载 TTS 模型失败")?;
        Ok(Self {
            rate: sherpa_sample_rate(&cpu)?,
            gpu: None,
            cpu: Some(cpu),
            config,
        })
    }

    fn generate(
        &mut self,
        config: &GenerationConfig,
        text: &str,
        token: &CancellationToken,
        on_fallback: &dyn Fn(),
    ) -> Result<Vec<f32>> {
        if let Some(gpu) = self.gpu.as_ref() {
            match gpu.generate(text, config, token) {
                Ok(audio) => return Ok(audio),
                Err(error) => {
                    self.gpu.take();
                    check_cancelled(token)?;
                    eprintln!("[tts] CUDA inference failed; retrying on CPU: {error:#}");
                }
            }
        }
        if self.cpu.is_none() {
            let cpu = OfflineTts::create(&self.config).context("加载 CPU TTS 模型失败")?;
            anyhow::ensure!(
                sherpa_sample_rate(&cpu)? == self.rate,
                "CPU 回退模型的采样率不一致"
            );
            self.cpu = Some(cpu);
            on_fallback();
        }
        generate_sherpa(self.cpu.as_ref().unwrap(), config, text, self.rate, token)
    }
}

impl LoadedTts {
    fn is_alive(&mut self) -> bool {
        match &mut self.engine {
            TtsEngine::Sherpa(engine) => engine.gpu.is_some() || engine.cpu.is_some(),
            TtsEngine::CosyVoice(engine) => engine.is_alive(),
            TtsEngine::Edge(engine) => engine.is_alive(),
        }
    }
}

fn set_engine(slot: &mut Option<LoadedTts>, engine: Option<LoadedTts>) {
    *LOADED_STATUS.write() = engine
        .as_ref()
        .map(|loaded| (loaded.path.clone(), loaded.device()));
    *slot = engine;
}

fn clear_dead_engine(slot: &mut Option<LoadedTts>) {
    if slot.as_mut().is_some_and(|loaded| !loaded.is_alive()) {
        set_engine(slot, None);
    }
}

fn catalog(root: &Path, sources: &BTreeMap<String, String>) -> Vec<tts_models::TtsModel> {
    if let Some(mut slot) = ENGINE.try_lock() {
        clear_dead_engine(&mut slot);
    }
    let mut models = tts_models::catalog(root);
    let loaded = LOADED_STATUS.read().clone();
    for model in &mut models {
        apply_loaded_status(model, loaded.as_ref());
        if let Some(source) = sources.get(model.id) {
            model.custom_download_url = source.clone();
        }
    }
    models
}

fn apply_loaded_status(model: &mut tts_models::TtsModel, loaded: Option<&(String, &'static str)>) {
    model.device = loaded
        .filter(|(path, _)| model.installed && path == &model.path)
        .map(|(_, device)| *device);
    model.loaded = model.device.is_some();
}

fn with_idle_models<T>(operation: impl FnOnce(&mut Option<LoadedTts>) -> Result<T>) -> Result<T> {
    let mut engine = ENGINE
        .try_lock()
        .context("TTS 模型正在加载或合成，请稍后再试")?;
    let install = INSTALL.0.lock();
    if install.active.is_some() {
        bail!("TTS 模型正在安装，请稍后再试");
    }
    operation(&mut engine)
}

pub(crate) fn change_models_root<T>(
    state: &AppState,
    change: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_idle_models(|engine| {
        let previous = state.models_root();
        let result = change()?;
        if state.models_root() != previous {
            set_engine(engine, None);
        }
        Ok(result)
    })
}

pub(crate) struct JobSlot(parking_lot::Mutex<JobState>);

struct JobState {
    active: Option<(uuid::Uuid, CancellationToken)>,
    pending: std::collections::VecDeque<uuid::Uuid>,
}

impl JobSlot {
    pub(crate) const fn new() -> Self {
        Self(parking_lot::Mutex::new(JobState {
            active: None,
            pending: std::collections::VecDeque::new(),
        }))
    }

    pub(crate) fn start(&'static self, id: uuid::Uuid, busy: &str) -> Result<ActiveJob> {
        let mut state = self.0.lock();
        if let Some(index) = state.pending.iter().position(|pending| *pending == id) {
            state.pending.remove(index);
            bail!(SpeechCancelled);
        }
        if state.active.is_some() {
            bail!("{busy}");
        }
        let token = CancellationToken::new();
        state.active = Some((id, token.clone()));
        Ok(ActiveJob { slot: self, token })
    }

    pub(crate) fn cancel(&self, id: uuid::Uuid) {
        let mut state = self.0.lock();
        if let Some((_, token)) = state.active.as_ref().filter(|(active, _)| *active == id) {
            token.cancel();
        } else if !state.pending.contains(&id) {
            // IPC can deliver cancellation before the start command reaches its worker.
            if state.pending.len() == 32 {
                state.pending.pop_front();
            }
            state.pending.push_back(id);
        }
    }
}

pub(crate) struct ActiveJob {
    slot: &'static JobSlot,
    pub(crate) token: CancellationToken,
}

impl Drop for ActiveJob {
    fn drop(&mut self) {
        self.slot.0.lock().active = None;
    }
}

#[derive(Debug)]
pub(crate) struct SpeechCancelled;

impl std::fmt::Display for SpeechCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("语音任务已取消")
    }
}

impl std::error::Error for SpeechCancelled {}

pub(crate) fn check_cancelled(token: &CancellationToken) -> Result<()> {
    if token.is_cancelled() {
        bail!(SpeechCancelled);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TtsJobKind {
    Install,
    Load,
    Synthesis,
}

#[tauri::command]
pub fn cancel_tts_job(kind: TtsJobKind, job_id: uuid::Uuid) {
    match kind {
        TtsJobKind::Install => &INSTALL,
        TtsJobKind::Load => &LOAD,
        TtsJobKind::Synthesis => &SYNTHESIS,
    }
    .cancel(job_id);
}

type CommandResult<T> = Result<T, String>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisRequest {
    family: String,
    model_id: String,
    /// Edge 在线音色短名（如 `zh-CN-XiaoxiaoNeural`）；本地克隆引擎忽略此项。
    voice: Option<String>,
    segments: Vec<TtsSegment>,
    reference_audio: Option<Vec<u8>>,
    reference_text: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisResult {
    name: String,
    path: String,
    sample_rate: u32,
    duration: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SynthesisProgress {
    pub(super) job_id: uuid::Uuid,
    pub(super) current: usize,
    pub(super) total: usize,
    pub(super) device: &'static str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TtsLoadProgress {
    job_id: uuid::Uuid,
    stage: String,
}

fn emit_load_progress(app: &AppHandle, job_id: uuid::Uuid, stage: &str) {
    let _ = app.emit(
        "tts-load-progress",
        TtsLoadProgress {
            job_id,
            stage: stage.to_owned(),
        },
    );
}

async fn worker<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|error| format!("语音任务异常结束：{error}"))?
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub async fn tts_models(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Vec<tts_models::TtsModel>> {
    let root = state.models_root();
    let sources = state.tts_download_urls();
    worker(move || {
        let mut models = catalog(&root, &sources);
        if let Some(model) = models
            .iter_mut()
            .find(|model| model.id == tts_models::COSYVOICE_ID)
        {
            let cache = app.path().app_cache_dir()?;
            model.gpu_environment_ready = cosyvoice::managed_cuda_status(&cache);
            model.gpu_environment_present = cosyvoice::managed_cuda_present(&cache);
        }
        Ok(models)
    })
    .await
}

#[tauri::command]
pub async fn install_tts_gpu(app: AppHandle, job_id: uuid::Uuid) -> CommandResult<()> {
    worker(move || {
        let job = LOAD.start(job_id, "已有 TTS 加载或 GPU 环境安装任务正在进行")?;
        with_idle_models(|engine| {
            let state = app.state::<AppState>();
            let root = state.models_root();
            let (pypi_mirror, pytorch_mirror) = state.cuda_mirrors();
            let model = tts_models::model(&root, tts_models::COSYVOICE_ID)?;
            if !model.installed {
                bail!("请先下载 CosyVoice3 模型，再安装 GPU 环境");
            }
            if engine
                .as_ref()
                .is_some_and(|loaded| loaded.device() == "cuda")
            {
                bail!("请先释放正在使用 GPU 的 TTS 模型，再安装 GPU 环境");
            }
            let app_cache = app.path().app_cache_dir()?;
            cosyvoice::install_managed_cuda(
                Path::new(&model.path),
                &app_cache,
                &pypi_mirror,
                &pytorch_mirror,
                &job.token,
                &|stage| {
                    emit_load_progress(&app, job_id, stage);
                },
            )
        })
    })
    .await
}

#[tauri::command]
pub async fn remove_tts_gpu(app: AppHandle) -> CommandResult<()> {
    worker(move || {
        with_idle_models(|engine| {
            let cache = app.path().app_cache_dir()?;
            if cosyvoice::managed_cuda_present(&cache) {
                if matches!(engine.as_ref().map(|loaded| &loaded.engine), Some(TtsEngine::CosyVoice(worker)) if worker.uses_managed_cuda()) {
                    set_engine(engine, None);
                }
                cosyvoice::remove_managed_cuda(&cache)?;
            }
            Ok(())
        })
    }).await
}

#[tauri::command]
pub async fn install_tts_model(
    app: AppHandle,
    id: String,
    job_id: uuid::Uuid,
) -> CommandResult<()> {
    worker(move || {
        let job = INSTALL.start(job_id, "已有 TTS 模型安装任务正在进行")?;
        let state = app.state::<AppState>();
        let source = state
            .tts_download_urls()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let source = validated_source(&source, true)?;
        tts_models::install(&app, &state.models_root(), &id, &source, job_id, &job.token)
    })
    .await
}

fn validated_source(source: &str, tts: bool) -> Result<String> {
    let source = source.trim();
    if source.is_empty() {
        return Ok(String::new());
    }
    let url = reqwest::Url::parse(source).context("请输入有效的 HTTP 或 HTTPS 地址")?;
    if !bgm_core::prefs::is_download_url(source) || url.host_str().is_none() {
        bail!("请输入有效的 HTTP 或 HTTPS 地址");
    }
    if tts && (url.query().is_some() || url.fragment().is_some()) {
        bail!("TTS 下载源不能包含查询参数或片段");
    }
    Ok(source.trim_end_matches('/').to_owned())
}

#[tauri::command]
pub fn save_model_download_source(
    state: State<'_, AppState>,
    tts: bool,
    id: Option<String>,
    url: String,
) -> CommandResult<String> {
    let source = validated_source(&url, tts).map_err(|error| format!("{error:#}"))?;
    let (id, source) = if tts {
        let id = id
            .filter(|id| !id.trim().is_empty())
            .context("缺少 TTS 模型 id")
            .map_err(|error| format!("{error:#}"))?;
        // 填回默认地址等同于“恢复默认”，避免把默认页误存成覆盖项；
        // 必须先判断默认值，再走 family 校验（默认浏览页可能不是可下载目录）。
        let (browse, base) =
            tts_models::default_urls_for(&id).map_err(|error| format!("{error:#}"))?;
        let source = if source == browse || source == base {
            String::new()
        } else {
            tts_models::validate_source(&id, &source).map_err(|error| format!("{error:#}"))?;
            source
        };
        (Some(id), source)
    } else {
        (None, source)
    };
    state
        .save_model_download_url(tts, id.as_deref(), source)
        .map_err(|error| format!("{error:#}"))
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CudaMirrorInfo {
    pypi: String,
    pytorch: String,
    default_pypi: String,
    default_pytorch: String,
}

fn cuda_mirror_info(state: &AppState) -> CudaMirrorInfo {
    let (pypi, pytorch) = state.cuda_mirrors();
    CudaMirrorInfo {
        pypi,
        pytorch,
        default_pypi: bgm_core::prefs::DEFAULT_CUDA_PYPI_MIRROR.to_owned(),
        default_pytorch: bgm_core::prefs::DEFAULT_CUDA_PYTORCH_MIRROR.to_owned(),
    }
}

/// CUDA 环境安装使用的镜像源；界面据此展示并回填编辑框。
#[tauri::command]
pub fn cuda_mirrors(state: State<'_, AppState>) -> CudaMirrorInfo {
    cuda_mirror_info(&state)
}

#[tauri::command]
pub fn save_cuda_mirrors(
    state: State<'_, AppState>,
    pypi: String,
    pytorch: String,
) -> CommandResult<CudaMirrorInfo> {
    let pypi = validate_mirror(&pypi, "PyPI 镜像源").map_err(|error| format!("{error:#}"))?;
    let pytorch =
        validate_mirror(&pytorch, "PyTorch 镜像源").map_err(|error| format!("{error:#}"))?;
    state
        .save_cuda_mirrors(pypi, pytorch)
        .map_err(|error| format!("{error:#}"))?;
    Ok(cuda_mirror_info(&state))
}

/// 镜像源必须是可解析的 http(s) 地址；pip 索引不需要查询参数或片段。
fn validate_mirror(source: &str, label: &str) -> Result<String> {
    let source = source.trim();
    ensure!(!source.is_empty(), "{label}不能为空");
    let url = reqwest::Url::parse(source)
        .with_context(|| format!("{label}不是有效的 HTTP 或 HTTPS 地址"))?;
    ensure!(
        bgm_core::prefs::is_download_url(source) && url.host_str().is_some(),
        "{label}不是有效的 HTTP 或 HTTPS 地址"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "{label}不能包含查询参数或片段"
    );
    Ok(source.to_owned())
}

#[tauri::command]
pub fn open_model_download_source(
    app: AppHandle,
    state: State<'_, AppState>,
    tts: bool,
    id: Option<String>,
) -> CommandResult<()> {
    let url = if tts {
        let id = id
            .filter(|id| !id.trim().is_empty())
            .context("缺少 TTS 模型 id")
            .map_err(|error| format!("{error:#}"))?;
        let custom = state
            .tts_download_urls()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if custom.is_empty() {
            tts_models::default_urls_for(&id)
                .map_err(|error| format!("{error:#}"))?
                .0
                .to_owned()
        } else {
            custom
        }
    } else {
        state.asr_download_url()
    };
    if url.is_empty() {
        return Err("该引擎为在线朗读，没有可访问的模型下载源".into());
    }
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn load_tts_model(app: AppHandle, id: String, job_id: uuid::Uuid) -> CommandResult<()> {
    worker(move || {
        let job = LOAD.start(job_id, "已有 TTS 模型加载任务正在进行")?;
        with_idle_models(|engine| {
            let root = app.state::<AppState>().models_root();
            let model = tts_models::model(&root, &id)?;
            if !model.installed {
                bail!("模型文件不齐全，请先安装模型");
            }
            set_engine(engine, None);
            let app_cache = app.path().app_cache_dir()?;
            let progress = |stage: &str| emit_load_progress(&app, job_id, stage);
            progress("正在加载 TTS 模型…");
            let loaded = create_engine(&root, &model, &app_cache, &job.token, &progress)?;
            check_cancelled(&job.token)?;
            progress("正在完成模型加载…");
            set_engine(engine, Some(loaded));
            Ok(())
        })
    })
    .await
}

#[tauri::command]
pub async fn remove_tts_model(app: AppHandle, id: String, delete_files: bool) -> CommandResult<()> {
    worker(move || {
        with_idle_models(|engine| {
            let root = app.state::<AppState>().models_root();
            let model = tts_models::model(&root, &id)?;
            if engine
                .as_ref()
                .is_some_and(|loaded| loaded.path == model.path)
            {
                set_engine(engine, None);
            }
            if delete_files && !model.online {
                let path = Path::new(&model.path);
                let target = match path.canonicalize() {
                    Ok(target) => target,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error).context("无法定位 TTS 模型目录"),
                };
                let expected = root
                    .canonicalize()?
                    .join(tts_models::DIR)
                    .join(path.file_name().unwrap());
                if target != expected {
                    bail!("TTS 模型目录指向链接，无法删除");
                }
                fs::remove_dir_all(&target).context("删除 TTS 模型目录失败")?;
            }
            Ok(())
        })
    })
    .await
}

#[tauri::command]
pub fn parse_tts_script(text: String, format: String) -> CommandResult<Vec<TtsSegment>> {
    tts_script::parse_script(&text, &format).map_err(|error| format!("{error:#}"))
}

fn builtin_voices_root(app: &AppHandle) -> Result<PathBuf> {
    if tauri::is_dev() {
        return Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../models/voices"));
    }
    Ok(app
        .path()
        .resolve("voices", tauri::path::BaseDirectory::Resource)?)
}

#[tauri::command]
pub async fn list_tts_builtin_voices(app: AppHandle) -> CommandResult<Vec<String>> {
    worker(move || speech_audio::builtin_voice_files(&builtin_voices_root(&app)?)).await
}

/// Edge 在线朗读的可用音色；联网获取，失败时退回内置清单。
#[tauri::command]
pub async fn list_edge_voices() -> CommandResult<Vec<edge::EdgeVoice>> {
    worker(edge::voices).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinReference {
    audio: Vec<u8>,
    text: Option<String>,
}

#[tauri::command]
pub async fn load_tts_builtin_reference(
    app: AppHandle,
    voice: String,
) -> CommandResult<BuiltinReference> {
    worker(move || {
        let root = builtin_voices_root(&app)?;
        if !speech_audio::builtin_voice_files(&root)?.contains(&voice) {
            bail!("未找到所选内置参考音频");
        }
        let path = root.join(voice);
        let bytes = fs::read(&path).context("无法读取所选内置参考音频")?;
        let (audio, text) = speech_audio::prepare_builtin_reference(&path, &bytes)?;
        Ok(BuiltinReference { audio, text })
    })
    .await
}

#[tauri::command]
pub async fn normalize_tts_reference(request: Request<'_>) -> CommandResult<Response> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("参考音频必须以二进制传输".into());
    };
    if bytes.is_empty() || bytes.len() > 25 * 1024 * 1024 {
        return Err("参考音频为空或超过 25 MB".into());
    }
    let bytes = bytes.clone();
    worker(move || speech_audio::prepare_reference(&bytes).map(Response::new)).await
}

#[tauri::command]
pub async fn generate_tts(
    app: AppHandle,
    request: SynthesisRequest,
    job_id: uuid::Uuid,
) -> CommandResult<SynthesisResult> {
    worker(move || {
        let job = SYNTHESIS.start(job_id, "已有语音合成正在进行")?;
        let mut engine = ENGINE.try_lock().context("TTS 模型正在管理，请稍后再试")?;
        let root = app.state::<AppState>().models_root();
        synthesize(&app, &root, &mut engine, request, job_id, &job.token)
    })
    .await
}

#[tauri::command]
pub async fn save_tts_audio(app: AppHandle, name: String) -> CommandResult<bool> {
    worker(move || {
        if !is_result_name(&name) {
            bail!("无效的语音合成结果");
        }
        let source = result_dir(&app)?.join(name);
        if !source.is_file() {
            bail!("找不到合成音频，请重新合成");
        }
        let Some(target) = app
            .dialog()
            .file()
            .set_title("导出合成语音")
            .set_file_name("合成语音.wav")
            .add_filter("WAV 音频", &["wav"])
            .blocking_save_file()
        else {
            return Ok(false);
        };
        let target = target.into_path()?;
        if target
            .canonicalize()
            .ok()
            .zip(source.canonicalize().ok())
            .is_none_or(|(a, b)| a != b)
        {
            std::fs::copy(source, target).context("导出 WAV 失败")?;
        }
        Ok(true)
    })
    .await
}

pub(super) fn result_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_cache_dir()?.join("tts"))
}

fn is_result_name(name: &str) -> bool {
    name.strip_prefix("speech-")
        .and_then(|tail| tail.strip_suffix(".wav"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub(super) fn prune_results(
    dir: &Path,
    incoming: u64,
    max_results: usize,
    max_bytes: u64,
) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(is_result_name) && entry.file_type()?.is_file() {
            let metadata = entry.metadata()?;
            entries.push((metadata.modified().ok(), metadata.len(), entry.path()));
        }
    }
    entries.sort_unstable();
    let mut bytes = entries
        .iter()
        .fold(incoming, |sum, (_, size, _)| sum.saturating_add(*size));
    let mut count = entries.len() + 1;
    for (_, size, path) in entries {
        if count <= max_results && bytes <= max_bytes {
            break;
        }
        fs::remove_file(path).context("无法清理旧的语音合成缓存")?;
        count -= 1;
        bytes -= size;
    }
    Ok(())
}

fn synthesize(
    app: &AppHandle,
    root: &Path,
    slot: &mut Option<LoadedTts>,
    mut request: SynthesisRequest,
    job_id: uuid::Uuid,
    token: &CancellationToken,
) -> Result<SynthesisResult> {
    check_cancelled(token)?;
    tts_script::validate_segments(&mut request.segments, MAX_DURATION_MS)?;
    if !matches!(
        request.family.as_str(),
        "EdgeTTS" | "ZipVoice" | "CosyVoice3"
    ) {
        bail!("未知的 TTS 模型类别");
    }
    if request
        .reference_audio
        .as_ref()
        .is_some_and(|audio| audio.len() > 500_000)
    {
        bail!("参考音频不能超过 15 秒");
    }
    let model = tts_models::model(root, &request.model_id)?;
    if !model.installed || model.family != request.family {
        bail!("模型文件不齐全或型号不匹配，请检查模型配置");
    }
    // 在线朗读使用平台音色，不需要参考录音。
    let reference = if model.family == "EdgeTTS" {
        None
    } else {
        let audio = request
            .reference_audio
            .as_deref()
            .context("请提供参考录音")?;
        let text = request
            .reference_text
            .as_deref()
            .map(str::trim)
            .filter(|text| speech_audio::valid_reference_text(text))
            .context("请填写与参考录音完全对应的参考文本")?;
        Some((speech_audio::read_reference(audio)?, text))
    };
    let edge_voice = if model.family == "EdgeTTS" {
        Some(edge::validate_voice(
            request.voice.as_deref().unwrap_or_default(),
        )?)
    } else {
        None
    };
    check_cancelled(token)?;
    clear_dead_engine(slot);
    if slot.as_ref().is_none_or(|loaded| loaded.path != model.path) {
        set_engine(slot, None);
        let app_cache = app.path().app_cache_dir()?;
        let loaded = create_engine(root, &model, &app_cache, token, &|_| {})?;
        check_cancelled(token)?;
        set_engine(slot, Some(loaded));
    }
    let loaded = slot.as_mut().context("TTS 模型未加载")?;
    let device = std::cell::Cell::new(loaded.device());
    let result = match &mut loaded.engine {
        TtsEngine::Sherpa(engine) => {
            let sample_rate = engine.rate;
            let (audio, text) = reference.context("请提供参考录音")?;
            let mut generation = zipvoice::generation(audio, text);
            synthesize_track(
                app,
                &request.segments,
                job_id,
                token,
                sample_rate,
                true,
                &device,
                |text, speed, on_cpu| {
                    generation.speed = speed;
                    engine.generate(&generation, text, token, on_cpu)
                },
            )
        }
        TtsEngine::Edge(engine) => {
            let voice = edge_voice.as_deref().context("请选择 Edge 朗读音色")?;
            synthesize_track(
                app,
                &request.segments,
                job_id,
                token,
                engine.sample_rate(),
                true,
                &device,
                |text, speed, _| engine.generate(text, voice, speed, token),
            )
        }
        TtsEngine::CosyVoice(engine) => {
            let (audio, reference_text) = reference.context("请提供参考录音")?;
            let scratch = tempfile::tempdir()?;
            let prompt = scratch.path().join("prompt.wav");
            let output = scratch.path().join("speech.wav");
            crate::separation::write_wav16_slices(
                &prompt,
                &[&audio],
                speech_audio::REFERENCE_RATE,
            )?;
            let sample_rate = engine.sample_rate();
            synthesize_track(
                app,
                &request.segments,
                job_id,
                token,
                sample_rate,
                false,
                &device,
                |text, _, on_cpu| {
                    engine.generate(text, reference_text, &prompt, &output, token, on_cpu)
                },
            )
        }
    };
    clear_dead_engine(slot);
    result
}

fn sherpa_sample_rate(engine: &OfflineTts) -> Result<u32> {
    u32::try_from(engine.sample_rate())
        .ok()
        .filter(|rate| (8_000..=65_535).contains(rate))
        .context("模型的采样率无效")
}

fn create_engine(
    root: &Path,
    model: &tts_models::TtsModel,
    app_cache: &Path,
    token: &CancellationToken,
    progress: &dyn Fn(&str),
) -> Result<LoadedTts> {
    let engine = match model.family {
        "CosyVoice3" => TtsEngine::CosyVoice(cosyvoice::Engine::load(
            Path::new(&model.path),
            app_cache,
            token,
            progress,
        )?),
        "EdgeTTS" => TtsEngine::Edge(edge::Engine::load()?),
        "ZipVoice" => {
            let mut config = OfflineTtsConfig::default();
            config.model.num_threads =
                std::thread::available_parallelism().map_or(2, |count| count.get().min(4)) as i32;
            config.max_num_sentences = 3;
            zipvoice::configure(&mut config, root, model);
            let field = |value: &Option<String>| value.clone().context("TTS 模型配置缺少路径");
            let threads = config.model.num_threads;
            let c = &config.model.zipvoice;
            let init = crate::sherpa_cuda::Init::ZipVoice {
                tokens: field(&c.tokens)?,
                encoder: field(&c.encoder)?,
                decoder: field(&c.decoder)?,
                vocoder: field(&c.vocoder)?,
                lexicon: field(&c.lexicon)?,
                threads,
            };
            TtsEngine::Sherpa(SherpaEngine::load(config, init, token)?)
        }
        _ => bail!("未知的 TTS 模型类别"),
    };
    Ok(LoadedTts {
        path: model.path.clone(),
        engine,
    })
}

fn generate_sherpa(
    engine: &OfflineTts,
    config: &GenerationConfig,
    text: &str,
    sample_rate: u32,
    token: &CancellationToken,
) -> Result<Vec<f32>> {
    let cancel = token.clone();
    let audio = engine.generate_with_config(
        text,
        config,
        Some(move |_: &[f32], _: f32| !cancel.is_cancelled()),
    );
    check_cancelled(token)?;
    let audio = audio
        .filter(|audio| audio.sample_rate() == sample_rate as i32)
        .context("未生成有效语音")?;
    Ok(audio.samples().to_vec())
}

pub(crate) fn benchmark(
    root: &Path,
    id: &str,
    cache: &Path,
    texts: &[String],
    prompt: Option<(&Path, &str)>,
    output: &Path,
    warmup: usize,
    runs: usize,
) -> Result<serde_json::Value> {
    use std::time::Instant;

    anyhow::ensure!(!texts.is_empty() && runs > 0, "no benchmark input or runs");
    let model = tts_models::model(root, id)?;
    let reference = prompt
        .map(|(path, text)| -> Result<_> {
            Ok((speech_audio::read_reference(&fs::read(path)?)?, text))
        })
        .transpose()?;
    let token = CancellationToken::new();
    let scratch = tempfile::tempdir()?;
    let prompt_wav = scratch.path().join("prompt.wav");
    let worker_wav = scratch.path().join("worker.wav");
    let raw_wav = scratch.path().join("raw.wav");
    if let Some((audio, _)) = &reference {
        crate::separation::write_wav16_slices(&prompt_wav, &[audio], speech_audio::REFERENCE_RATE)?;
    }
    let load_started = Instant::now();
    let mut loaded = create_engine(root, &model, cache, &token, &|stage| {
        eprintln!("[tts] {stage}")
    })?;
    let load_ms = load_started.elapsed().as_secs_f64() * 1000.0;
    let (rate, mut generation) = match &loaded.engine {
        TtsEngine::CosyVoice(engine) => (engine.sample_rate(), None),
        TtsEngine::Edge(engine) => (engine.sample_rate(), None),
        TtsEngine::Sherpa(engine) => {
            let (audio, text) = reference
                .as_ref()
                .context("ZipVoice requires a reference WAV and text")?;
            (engine.rate, Some(zipvoice::generation(audio.clone(), text)))
        }
    };
    let segments: Vec<_> = texts
        .iter()
        .map(|text| TtsSegment {
            text: text.clone(),
            start_ms: None,
            end_ms: None,
        })
        .collect();
    let mut iterations = Vec::with_capacity(runs);
    let mut first_segment = Vec::with_capacity(runs);
    let mut rtfs = Vec::with_capacity(runs);
    for n in 0..warmup + runs {
        let run_started = Instant::now();
        let mut track = Vec::new();
        let mut first_segment_ms = 0.0;
        for (index, segment) in segments.iter().enumerate() {
            let text = segment.text.trim();
            let samples = match &mut loaded.engine {
                TtsEngine::Sherpa(engine) => {
                    engine.generate(generation.as_mut().unwrap(), text, &token, &|| {})?
                }
                TtsEngine::Edge(engine) => {
                    engine.generate(text, edge::DEFAULT_VOICE, 1.0, &token)?
                }
                TtsEngine::CosyVoice(engine) => {
                    let (_, prompt_text) = reference
                        .as_ref()
                        .context("CosyVoice3 requires a reference WAV and text")?;
                    engine.generate(text, prompt_text, &prompt_wav, &worker_wav, &token, &|| {})?
                }
            };
            let samples = speech_audio::polish_segment(&samples, rate)?;
            append_segment(&mut track, segment, &samples, rate)?;
            if index == 0 {
                first_segment_ms = run_started.elapsed().as_secs_f64() * 1000.0;
            }
        }
        finalize_track(&mut track, false)?;
        crate::separation::write_wav16_slices(&raw_wav, &[&track], rate)?;
        speech_audio::normalize_output(&raw_wav, output, rate, track.len())?;
        let duration_s = speech_audio::read_wave(output)?.samples().len() as f64 / f64::from(rate);
        let elapsed = run_started.elapsed().as_secs_f64();
        if n >= warmup {
            iterations.push(elapsed * 1000.0);
            first_segment.push(first_segment_ms);
            rtfs.push(elapsed / duration_s);
        }
    }
    let quantile = |values: &[f64], fraction: f64| {
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[((fraction * sorted.len() as f64).ceil() as usize).saturating_sub(1)]
    };
    let duration_s = speech_audio::read_wave(output)?.samples().len() as f64 / f64::from(rate);
    Ok(serde_json::json!({
        "model": id, "load_ms": load_ms, "sample_rate": rate, "duration_s": duration_s,
        "device": match &loaded.engine {
            TtsEngine::Sherpa(engine) => engine.device(),
            TtsEngine::CosyVoice(engine) => engine.device(),
            TtsEngine::Edge(engine) => engine.device(),
        },
        "total_ms": iterations, "p50_ms": quantile(&iterations, 0.5), "p95_ms": quantile(&iterations, 0.95),
        "first_segment_ms": first_segment, "first_segment_p50_ms": quantile(&first_segment, 0.5),
        "playable_first_packet_ms": null, "rtf": rtfs, "output": output,
    }))
}

fn synthesize_track(
    app: &AppHandle,
    segments: &[TtsSegment],
    job_id: uuid::Uuid,
    token: &CancellationToken,
    sample_rate: u32,
    supports_model_speed: bool,
    device: &std::cell::Cell<&'static str>,
    mut generate: impl FnMut(&str, f32, &dyn Fn()) -> Result<Vec<f32>>,
) -> Result<SynthesisResult> {
    let mut track = Vec::new();
    let timed = segments[0].start_ms.is_some();
    let total = segments.len();
    let emit_progress = |current| {
        let _ = app.emit(
            "tts-synthesis-progress",
            SynthesisProgress {
                job_id,
                current,
                total,
                device: device.get(),
            },
        );
    };
    emit_progress(0);
    for (index, segment) in segments.iter().enumerate() {
        let on_cpu = || {
            if device.replace("cpu") != "cpu" {
                if let Some((_, current)) = LOADED_STATUS.write().as_mut() {
                    *current = "cpu";
                }
                emit_progress(index);
            }
        };
        let mut process = || -> Result<()> {
            let mut generate = |speed| {
                check_cancelled(token)?;
                let samples = generate(segment.text.trim(), speed, &on_cpu)?;
                check_cancelled(token)?;
                speech_audio::polish_segment(&samples, sample_rate)
            };
            let mut samples = generate(1.0)?;
            if let Some((start, end)) = segment.start_ms.zip(segment.end_ms) {
                let capacity = frames(end, sample_rate) - frames(start, sample_rate);
                if samples.len() > capacity {
                    if supports_model_speed {
                        let speed = (samples.len() as f32 / capacity as f32 * 1.05).min(2.0);
                        if let Ok(faster) = generate(speed)
                            && faster.len() < samples.len()
                        {
                            samples = faster;
                        }
                    }
                    check_cancelled(token)?;
                    if samples.len() > capacity {
                        samples =
                            speech_audio::fit_subtitle_audio(&samples, capacity, sample_rate)?;
                    }
                }
            }
            check_cancelled(token)?;
            append_segment(&mut track, segment, &samples, sample_rate)
        };
        process().with_context(|| format!("第 {} 段音频处理失败", index + 1))?;
        emit_progress(index + 1);
    }
    finalize_track(&mut track, timed)?;
    check_cancelled(token)?;
    let dir = result_dir(app)?;
    fs::create_dir_all(&dir)?;
    let name = format!("speech-{}.wav", uuid::Uuid::new_v4().simple());
    let raw = tempfile::Builder::new()
        .prefix("speech-raw-")
        .suffix(".wav")
        .tempfile_in(&dir)?;
    crate::separation::write_wav16_slices(raw.path(), &[&track], sample_rate)?;
    check_cancelled(token)?;
    let normalized = tempfile::Builder::new()
        .prefix("speech-normalized-")
        .suffix(".wav")
        .tempfile_in(&dir)?;
    speech_audio::normalize_output(raw.path(), normalized.path(), sample_rate, track.len())
        .context("语音响度归一化失败")?;
    check_cancelled(token)?;
    let wave = speech_audio::read_wave(normalized.path())?;
    prune_results(
        &dir,
        normalized.as_file().metadata()?.len(),
        CACHE_MAX_RESULTS,
        CACHE_MAX_BYTES,
    )?;
    check_cancelled(token)?;
    let path = dir.join(name);
    normalized.persist(&path).context("发布合成音频失败")?;
    Ok(SynthesisResult {
        name: path
            .file_name()
            .context("无法读取音频文件名")?
            .to_string_lossy()
            .into_owned(),
        path: path.to_string_lossy().into_owned(),
        sample_rate,
        duration: wave.samples().len() as f64 / f64::from(sample_rate),
    })
}

pub(super) fn frames(ms: u64, rate: u32) -> usize {
    ((ms * u64::from(rate) + 500) / 1000) as usize
}

fn finalize_track(track: &mut Vec<f32>, timed: bool) -> Result<()> {
    if !timed {
        while track.last() == Some(&0.0) {
            track.pop();
        }
    }
    let peak = track
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak == 0.0 {
        bail!("合成结果没有有效声音");
    }
    if peak > 1.0 {
        for sample in track {
            *sample /= peak;
        }
    }
    Ok(())
}

pub(super) fn append_segment(
    track: &mut Vec<f32>,
    segment: &TtsSegment,
    samples: &[f32],
    rate: u32,
) -> Result<()> {
    let start = segment
        .start_ms
        .map_or(track.len(), |start| frames(start, rate));
    let voice_end = start.checked_add(samples.len()).context("音轨过长")?;
    let end = match (segment.start_ms, segment.end_ms) {
        (_, Some(end)) => {
            let end = frames(end, rate);
            if voice_end > end {
                bail!("字幕时槽太短，无法清晰读完整句");
            }
            end
        }
        (Some(_), None) => voice_end,
        (None, None) => voice_end
            .checked_add(speech_audio::pause_after(&segment.text, rate))
            .context("音轨过长")?,
    };
    if end > frames(MAX_DURATION_MS, rate) {
        bail!("音轨不能超过 30 分钟");
    }
    track.resize(track.len().max(end), 0.0);
    for (target, &sample) in track[start..voice_end].iter_mut().zip(samples) {
        *target += sample;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_reports_the_loaded_backend_and_clears_it_on_release() {
        let mut model = tts_models::model(Path::new("models"), "zipvoice-int8").unwrap();
        let mut loaded = (model.path.clone(), "cuda");
        apply_loaded_status(&mut model, Some(&loaded));
        assert_eq!(model.device, None);
        model.installed = true;
        for device in ["cuda", "cpu"] {
            loaded.1 = device;
            apply_loaded_status(&mut model, Some(&loaded));
            assert!(model.loaded);
            assert_eq!(model.device, Some(device));
        }
        loaded.0 = "another model".into();
        apply_loaded_status(&mut model, Some(&loaded));
        assert_eq!(model.device, None);
        apply_loaded_status(&mut model, None);
        assert!(!model.loaded);
        assert_eq!(model.device, None);
    }

    #[test]
    fn cancellation_matches_job_and_releases_slot_on_drop() {
        static SLOT: JobSlot = JobSlot::new();
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        let third = uuid::Uuid::new_v4();
        let job = SLOT.start(first, "busy").unwrap();
        assert!(SLOT.start(second, "busy").is_err());
        SLOT.cancel(second);
        assert!(!job.token.is_cancelled());
        SLOT.cancel(first);
        assert!(check_cancelled(&job.token).is_err());
        assert!(SLOT.start(third, "busy").is_err());
        drop(job);
        assert!(SLOT.start(second, "busy").is_err());
        assert!(SLOT.start(third, "busy").is_ok());
    }

    #[test]
    fn cache_prunes_old_results_by_count_and_bytes_only() {
        let dir = tempfile::tempdir().unwrap();
        let result = |id: usize| dir.path().join(format!("speech-{id:032x}.wav"));
        for (id, size) in [(1, 10), (2, 20), (3, 30)] {
            let file = fs::File::create(result(id)).unwrap();
            file.set_len(size).unwrap();
            file.set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_780_000_000 + id as u64),
            )
            .unwrap();
        }
        let unrelated = dir.path().join("speech-part-test.wav");
        fs::write(&unrelated, [0; 100]).unwrap();
        prune_results(dir.path(), 5, 3, 40).unwrap();
        assert!(!result(1).exists());
        assert!(!result(2).exists());
        assert!(result(3).exists());
        assert!(unrelated.exists());
        prune_results(dir.path(), 5, 1, 100).unwrap();
        assert!(!result(3).exists());
    }

    #[test]
    fn subtitles_preserve_offsets_gaps_overlaps_and_end_times() {
        let mut track = Vec::new();
        let rate = 8_000;
        for (start_ms, end_ms) in [(100, Some(250)), (150, Some(225)), (300, Some(450))] {
            let cue = TtsSegment {
                text: "字幕。".into(),
                start_ms: Some(start_ms),
                end_ms,
            };
            append_segment(&mut track, &cue, &[0.25; 600], rate).unwrap();
        }
        assert_eq!(track.len(), frames(450, rate));
        assert!(track[..800].iter().all(|&sample| sample == 0.0));
        assert_eq!(track[800], 0.25);
        assert_eq!(track[1_200], 0.5);
        assert_eq!(track[1_400], 0.25);
        assert!(track[1_800..2_400].iter().all(|&sample| sample == 0.0));
        assert_eq!(track[2_400], 0.25);
        assert!(track[3_000..].iter().all(|&sample| sample == 0.0));

        let last = TtsSegment {
            text: "末句。".into(),
            start_ms: Some(500),
            end_ms: None,
        };
        append_segment(&mut track, &last, &[0.25; 600], rate).unwrap();
        assert_eq!(track.len(), frames(500, rate) + 600);
        assert_eq!(track[4_000], 0.25);

        let short = TtsSegment {
            end_ms: Some(501),
            ..last
        };
        assert!(append_segment(&mut track, &short, &[0.25; 600], rate).is_err());
        assert_eq!(track.len(), 4_600);
    }

    #[test]
    fn plain_text_appends_audio_with_punctuation_pauses() {
        let mut track = Vec::new();
        let cue = TtsSegment {
            text: "一句话。".into(),
            start_ms: None,
            end_ms: None,
        };
        for _ in 0..2 {
            append_segment(&mut track, &cue, &[0.25; 2_000], 24_000).unwrap();
        }
        assert_eq!(track.len(), 2 * (2_000 + 3_840));
        assert!(track[2_000..5_840].iter().all(|&sample| sample == 0.0));
        assert_eq!(track[5_840], 0.25);
    }

    #[test]
    fn sentence_and_clause_pauses_are_natural_and_bounded() {
        assert_eq!(speech_audio::pause_after("一句话。", 24_000), 3_840);
        assert_eq!(speech_audio::pause_after("“你好。”", 24_000), 3_840);
        assert_eq!(speech_audio::pause_after("前半句，", 24_000), 1_440);
        assert_eq!(speech_audio::pause_after("跨段继续", 24_000), 0);
    }

    /// 联网验收：走完整的「引擎创建 → 在线合成 → 轨道拼装 → WAV」链路。
    #[test]
    #[ignore = "需要联网访问 Edge 朗读服务"]
    fn live_edge_benchmark_writes_a_24khz_wav() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("edge.wav");
        let report = benchmark(
            dir.path(),
            tts_models::EDGE_ID,
            dir.path(),
            &["在线朗读端到端验收。".to_owned()],
            None,
            &output,
            0,
            1,
        )
        .unwrap();
        assert_eq!(report["sample_rate"], edge::SAMPLE_RATE);
        assert_eq!(report["device"], "cloud");
        let wave = speech_audio::read_wave(&output).unwrap();
        assert_eq!(wave.sample_rate(), edge::SAMPLE_RATE as i32);
        assert!(wave.samples().len() > 8_000, "合成音频过短");
    }
}
