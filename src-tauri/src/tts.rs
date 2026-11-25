use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};
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
struct SynthesisProgress {
    job_id: uuid::Uuid,
    current: usize,
    total: usize,
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
    worker(move || Ok(tts_models::catalog(&root))).await
}

#[tauri::command]
pub async fn install_tts_model(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    job_id: uuid::Uuid,
) -> CommandResult<()> {
    let root = state.models_root();
    worker(move || {
        let job = INSTALL.start(job_id, "已有 TTS 模型安装任务正在进行")?;
        tts_models::install(&app, &root, &id, job_id, &job.token)
    })
    .await
}

#[tauri::command]
pub fn open_tts_release(app: AppHandle) -> CommandResult<()> {
    app.opener()
        .open_url(tts_models::RELEASE_URL, None::<&str>)
        .map_err(|error| error.to_string())
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

#[tauri::command]
pub async fn load_tts_builtin_reference(app: AppHandle, voice: String) -> CommandResult<Response> {
    worker(move || {
        let root = builtin_voices_root(&app)?;
        if !speech_audio::builtin_voice_files(&root)?.contains(&voice) {
            bail!("未找到所选内置参考音频");
        }
        let bytes = fs::read(root.join(voice)).context("无法读取所选内置参考音频")?;
        speech_audio::prepare_builtin_reference(&bytes).map(Response::new)
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
    state: State<'_, AppState>,
    request: SynthesisRequest,
    job_id: uuid::Uuid,
) -> CommandResult<SynthesisResult> {
    let root = state.models_root();
    worker(move || {
        let job = SYNTHESIS.start(job_id, "已有语音合成正在进行")?;
        synthesize(&app, &root, request, job_id, &job.token)
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

fn result_dir(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_cache_dir()?.join("tts"))
}

fn is_result_name(name: &str) -> bool {
    name.strip_prefix("speech-")
        .and_then(|tail| tail.strip_suffix(".wav"))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

fn prune_results(dir: &Path, incoming: u64, max_results: usize, max_bytes: u64) -> Result<()> {
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
    mut request: SynthesisRequest,
    job_id: uuid::Uuid,
    token: &CancellationToken,
) -> Result<SynthesisResult> {
    check_cancelled(token)?;
    tts_script::validate_segments(&mut request.segments, MAX_DURATION_MS)?;
    if !matches!(request.family.as_str(), "Kokoro" | "ZipVoice") {
        bail!("未知的 TTS 模型类别");
    }
    if request
        .reference_audio
        .as_ref()
        .is_some_and(|audio| audio.len() > 330_000)
    {
        bail!("参考音频不能超过 10 秒");
    }
    let catalog = tts_models::catalog(root);
    let model = catalog
        .iter()
        .find(|item| item.id == request.model_id && item.family == request.family && item.installed)
        .context("模型文件不齐全，请前往模型管理下载安装")?;
    let path = Path::new(&model.path);
    let mut config = OfflineTtsConfig::default();
    config.model.num_threads =
        std::thread::available_parallelism().map_or(2, |count| count.get().min(4)) as i32;
    config.max_num_sentences = 1;
    let mut generation = GenerationConfig::default();
    let join = |file: &str| path.join(file).to_string_lossy().into_owned();
    if model.family == "Kokoro" {
        config.model.kokoro.model = Some(join(model.acoustic_file()));
        config.model.kokoro.voices = Some(join("voices.bin"));
        config.model.kokoro.tokens = Some(join("tokens.txt"));
        config.model.kokoro.lexicon = Some(format!(
            "{},{}",
            join("lexicon-us-en.txt"),
            join("lexicon-zh.txt")
        ));
        generation.sid = request.voice_id;
    } else {
        let (encoder, decoder) = model.zipvoice_files();
        let deps = root.join(tts_models::DIR);
        config.model.zipvoice.tokens = Some(join("tokens.txt"));
        config.model.zipvoice.encoder = Some(join(encoder));
        config.model.zipvoice.decoder = Some(join(decoder));
        config.model.zipvoice.vocoder =
            Some(deps.join("vocos_24khz.onnx").to_string_lossy().into_owned());
        config.model.zipvoice.lexicon = Some(format!(
            "{},{}",
            join("lexicon.txt"),
            deps.join("lexicon-en-us.txt").display()
        ));
        config.model.zipvoice.feat_scale = 0.1;
        config.model.zipvoice.t_shift = 0.5;
        config.model.zipvoice.target_rms = 0.1;
        config.model.zipvoice.guidance_scale = 1.0;
        generation.reference_text = Some(
            request
                .reference_text
                .take()
                .map(|text| text.trim().to_owned())
                .filter(|text| {
                    !text.is_empty() && text.chars().count() <= 1000 && !text.contains('\0')
                })
                .context("请填写与参考录音完全对应的参考文本")?,
        );
        let reference = request
            .reference_audio
            .as_deref()
            .context("ZipVoice 需要 3–10 秒参考音频")?;
        generation.reference_audio = Some(speech_audio::read_reference(reference)?);
        generation.reference_sample_rate = speech_audio::REFERENCE_RATE as i32;
        generation.num_steps = 4;
    }
    check_cancelled(token)?;
    let engine = OfflineTts::create(&config).context("加载 TTS 模型失败")?;
    check_cancelled(token)?;
    let sample_rate = u32::try_from(engine.sample_rate())
        .ok()
        .filter(|rate| (8_000..=65_535).contains(rate))
        .context("模型的采样率无效")?;
    if model.family == "Kokoro" && !(0..engine.num_speakers()).contains(&generation.sid) {
        bail!("所选 Kokoro 语音不可用");
    }
    let mut track = Vec::new();
    let total = request.segments.len();
    let _ = app.emit(
        "tts-synthesis-progress",
        SynthesisProgress {
            job_id,
            current: 0,
            total,
        },
    );
    for (index, segment) in request.segments.iter().enumerate() {
        check_cancelled(token)?;
        let cancel = token.clone();
        let audio = engine.generate_with_config(
            segment.text.trim(),
            &generation,
            Some(move |_: &[f32], _: f32| !cancel.is_cancelled()),
        );
        check_cancelled(token)?;
        let audio = audio
            .filter(|audio| {
                !audio.samples().is_empty() && audio.sample_rate() == sample_rate as i32
            })
            .with_context(|| format!("第 {} 段未生成有效语音", index + 1))?;
        let slot = segment
            .start_ms
            .zip(segment.end_ms)
            .map(|(start, end)| frames(end, sample_rate) - frames(start, sample_rate));
        let fitted;
        let samples = if let Some(slot) = slot.filter(|slot| *slot < audio.samples().len()) {
            fitted =
                speech_audio::fit_clip(audio.samples(), slot, sample_rate).with_context(|| {
                    format!("第 {} 段超过字幕时槽，请缩短文本或延长时槽", index + 1)
                })?;
            &fitted
        } else {
            audio.samples()
        };
        append_segment(&mut track, segment, samples, sample_rate)?;
        let _ = app.emit(
            "tts-synthesis-progress",
            SynthesisProgress {
                job_id,
                current: index + 1,
                total,
            },
        );
    }
    check_cancelled(token)?;
    let dir = result_dir(app)?;
    fs::create_dir_all(&dir)?;
    let name = format!("speech-{}.wav", uuid::Uuid::new_v4().simple());
    let staged = tempfile::Builder::new()
        .prefix("speech-part-")
        .suffix(".wav")
        .tempfile_in(&dir)?;
    crate::separation::write_wav16_slices(staged.path(), &[&track], sample_rate)?;
    check_cancelled(token)?;
    prune_results(
        &dir,
        staged.as_file().metadata()?.len(),
        CACHE_MAX_RESULTS,
        CACHE_MAX_BYTES,
    )?;
    check_cancelled(token)?;
    let path = dir.join(name);
    staged.persist(&path).context("发布合成音频失败")?;
    Ok(SynthesisResult {
        name: path
            .file_name()
            .context("无法读取音频文件名")?
            .to_string_lossy()
            .into_owned(),
        path: path.to_string_lossy().into_owned(),
        sample_rate,
        duration: track.len() as f64 / f64::from(sample_rate),
    })
}

fn frames(ms: u64, rate: u32) -> usize {
    ((ms * u64::from(rate) + 500) / 1000) as usize
}

fn append_segment(
    track: &mut Vec<f32>,
    segment: &TtsSegment,
    samples: &[f32],
    rate: u32,
) -> Result<()> {
    let start = segment.start_ms.map_or(track.len(), |ms| frames(ms, rate));
    let end = start.checked_add(samples.len()).context("音轨过长")?;
    let padded_end = segment.end_ms.map_or(end, |ms| end.max(frames(ms, rate)));
    if padded_end > frames(MAX_DURATION_MS, rate) {
        bail!("音轨不能超过 30 分钟");
    }
    track.resize(track.len().max(padded_end), 0.0);
    for (dst, src) in track[start..end].iter_mut().zip(samples) {
        *dst = (*dst + src).clamp(-1.0, 1.0);
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
    fn timed_track_preserves_initial_gaps_and_final_timestamp() {
        let mut track = Vec::new();
        for (start_ms, end_ms) in [(10, 20), (30, 50)] {
            append_segment(
                &mut track,
                &TtsSegment {
                    text: "a".into(),
                    start_ms: Some(start_ms),
                    end_ms: Some(end_ms),
                },
                &[0.5; 10],
                24_000,
            )
            .unwrap();
        }
        assert_eq!(track.len(), 1200);
        assert!(track[..240].iter().all(|&value| value == 0.0));
        assert_eq!(track[240], 0.5);
        assert!(track[250..720].iter().all(|&value| value == 0.0));
        assert_eq!(track[720], 0.5);
        assert!(track[730..].iter().all(|&value| value == 0.0));
    }
}
