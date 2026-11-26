use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

mod cosyvoice;
mod kokoro;
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
static ENGINE: parking_lot::Mutex<Option<LoadedTts>> = parking_lot::Mutex::new(None);
static LOADED_PATH: parking_lot::RwLock<Option<String>> = parking_lot::RwLock::new(None);

struct LoadedTts {
    path: String,
    engine: TtsEngine,
}

enum TtsEngine {
    Sherpa(OfflineTts),
    CosyVoice(cosyvoice::Engine),
}

impl LoadedTts {
    fn is_alive(&mut self) -> bool {
        match &mut self.engine {
            TtsEngine::Sherpa(_) => true,
            TtsEngine::CosyVoice(engine) => engine.is_alive(),
        }
    }
}

fn set_engine(slot: &mut Option<LoadedTts>, engine: Option<LoadedTts>) {
    *LOADED_PATH.write() = engine.as_ref().map(|loaded| loaded.path.clone());
    *slot = engine;
}

fn clear_dead_engine(slot: &mut Option<LoadedTts>) {
    if slot.as_mut().is_some_and(|loaded| !loaded.is_alive()) {
        set_engine(slot, None);
    }
}

fn catalog(root: &Path) -> Vec<tts_models::TtsModel> {
    if let Some(mut slot) = ENGINE.try_lock() {
        clear_dead_engine(&mut slot);
    }
    let mut models = tts_models::catalog(root);
    let loaded = LOADED_PATH.read();
    for model in &mut models {
        model.loaded = model.installed && loaded.as_deref() == Some(model.path.as_str());
    }
    models
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

struct JobSlot(parking_lot::Mutex<JobState>);

struct JobState {
    active: Option<(uuid::Uuid, CancellationToken)>,
    pending: std::collections::VecDeque<uuid::Uuid>,
}

impl JobSlot {
    const fn new() -> Self {
        Self(parking_lot::Mutex::new(JobState {
            active: None,
            pending: std::collections::VecDeque::new(),
        }))
    }

    fn start(&'static self, id: uuid::Uuid, busy: &str) -> Result<ActiveJob> {
        let mut state = self.0.lock();
        if let Some(index) = state.pending.iter().position(|pending| *pending == id) {
            state.pending.remove(index);
            bail!("语音任务已取消");
        }
        if state.active.is_some() {
            bail!("{busy}");
        }
        let token = CancellationToken::new();
        state.active = Some((id, token.clone()));
        Ok(ActiveJob { slot: self, token })
    }

    fn cancel(&self, id: uuid::Uuid) {
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

struct ActiveJob {
    slot: &'static JobSlot,
    token: CancellationToken,
}

impl Drop for ActiveJob {
    fn drop(&mut self) {
        self.slot.0.lock().active = None;
    }
}

pub(crate) fn check_cancelled(token: &CancellationToken) -> Result<()> {
    if token.is_cancelled() {
        bail!("语音任务已取消");
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TtsJobKind {
    Install,
    Synthesis,
}

#[tauri::command]
pub fn cancel_tts_job(kind: TtsJobKind, job_id: uuid::Uuid) {
    match kind {
        TtsJobKind::Install => &INSTALL,
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
    voice_id: i32,
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
pub async fn tts_models(state: State<'_, AppState>) -> CommandResult<Vec<tts_models::TtsModel>> {
    let root = state.models_root();
    worker(move || Ok(catalog(&root))).await
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
        let source = validated_source(&state.tts_download_url(), true)?;
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
    url: String,
) -> CommandResult<String> {
    let source = validated_source(&url, tts).map_err(|error| format!("{error:#}"))?;
    if tts {
        tts_models::validate_download_source(&source).map_err(|error| error.to_string())?;
    }
    state
        .save_model_download_url(tts, source)
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
pub fn open_model_download_source(
    app: AppHandle,
    state: State<'_, AppState>,
    tts: bool,
) -> CommandResult<()> {
    let source = if tts {
        state.tts_download_url()
    } else {
        state.asr_download_url()
    };
    let source = validated_source(&source, tts).map_err(|error| format!("{error:#}"))?;
    let url = if source.is_empty() && tts {
        bgm_core::prefs::DEFAULT_TTS_DOWNLOAD_URL
    } else {
        &source
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn load_tts_model(app: AppHandle, id: String) -> CommandResult<()> {
    worker(move || {
        with_idle_models(|engine| {
            let root = app.state::<AppState>().models_root();
            let model = tts_models::model(&root, &id)?;
            if !model.installed {
                bail!("模型文件不齐全，请先安装模型");
            }
            set_engine(engine, None);
            let loaded = create_engine(&root, &model, &CancellationToken::new())?;
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
            if delete_files {
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
        "Kokoro" | "ZipVoice" | "CosyVoice3"
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
    let reference = if model.family == "Kokoro" {
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
    check_cancelled(token)?;
    clear_dead_engine(slot);
    if slot.as_ref().is_none_or(|loaded| loaded.path != model.path) {
        let _ = app.emit(
            "tts-synthesis-progress",
            SynthesisProgress {
                job_id,
                current: 0,
                total: request.segments.len(),
            },
        );
        set_engine(slot, None);
        let loaded = create_engine(root, &model, token)?;
        check_cancelled(token)?;
        set_engine(slot, Some(loaded));
    }
    let result = match &mut slot.as_mut().context("TTS 模型未加载")?.engine {
        TtsEngine::Sherpa(engine) => {
            let sample_rate = sherpa_sample_rate(engine)?;
            let mut generation = match (model.family, reference) {
                ("Kokoro", _) => kokoro::generation(request.voice_id, engine)?,
                ("ZipVoice", Some((audio, text))) => zipvoice::generation(audio, text),
                _ => bail!("未知的 TTS 模型类别"),
            };
            synthesize_track(
                app,
                &request.segments,
                job_id,
                token,
                sample_rate,
                |text, speed| {
                    generation.speed = speed;
                    generate_sherpa(engine, &generation, text, sample_rate, token)
                },
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
                |text, speed| engine.generate(text, reference_text, &prompt, &output, speed, token),
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
    token: &CancellationToken,
) -> Result<LoadedTts> {
    let engine = if model.family == "CosyVoice3" {
        TtsEngine::CosyVoice(cosyvoice::Engine::load(Path::new(&model.path), token)?)
    } else {
        let mut config = OfflineTtsConfig::default();
        config.model.num_threads =
            std::thread::available_parallelism().map_or(2, |count| count.get().min(4)) as i32;
        config.max_num_sentences = 3;
        match model.family {
            "Kokoro" => kokoro::configure(&mut config, model),
            "ZipVoice" => zipvoice::configure(&mut config, root, model),
            _ => bail!("未知的 TTS 模型类别"),
        }
        let engine = OfflineTts::create(&config).context("加载 TTS 模型失败")?;
        sherpa_sample_rate(&engine)?;
        TtsEngine::Sherpa(engine)
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

fn synthesize_track(
    app: &AppHandle,
    segments: &[TtsSegment],
    job_id: uuid::Uuid,
    token: &CancellationToken,
    sample_rate: u32,
    mut generate: impl FnMut(&str, f32) -> Result<Vec<f32>>,
) -> Result<SynthesisResult> {
    let mut track = Vec::new();
    let timed = segments[0].start_ms.is_some();
    let total = segments.len();
    let _ = app.emit(
        "tts-synthesis-progress",
        SynthesisProgress {
            job_id,
            current: 0,
            total,
        },
    );
    for (index, segment) in segments.iter().enumerate() {
        let mut process = || -> Result<()> {
            let mut generate = |speed| {
                check_cancelled(token)?;
                let samples = generate(segment.text.trim(), speed)?;
                check_cancelled(token)?;
                speech_audio::polish_segment(&samples, sample_rate)
            };
            let mut samples = generate(1.0)?;
            if let Some((start, end)) = segment.start_ms.zip(segment.end_ms) {
                let capacity = frames(end, sample_rate) - frames(start, sample_rate);
                if samples.len() > capacity {
                    let speed = (samples.len() as f32 / capacity as f32 * 1.05).min(2.0);
                    if let Ok(faster) = generate(speed)
                        && faster.len() < samples.len()
                    {
                        samples = faster;
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
        let _ = app.emit(
            "tts-synthesis-progress",
            SynthesisProgress {
                job_id,
                current: index + 1,
                total,
            },
        );
    }
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
        for sample in &mut track {
            *sample /= peak;
        }
    }
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
}
