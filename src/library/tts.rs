//! 文生语音模型进程、合成任务和离线 WAV 交付。

mod cosyvoice;
mod engine;
mod models;
mod script;
mod voices;
mod wave;

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub use cosyvoice::{
    prepare_runtime as prepare_cosyvoice_runtime, runtime_ready as cosyvoice_runtime_ready,
};
pub use models::{Family, Model, install, remove, validate_source};
pub use script::{Format, Segment, parse};
pub use voices::{
    BuiltinReference, DEFAULT_KOKORO_SPEAKER, KokoroVoice, VoiceCategory, builtin_voices_root,
    kokoro_voice_label, kokoro_voices, list_builtin_voices, load_builtin_reference,
    prepare_recorded_reference, valid_reference_text,
};
pub use wave::export;
#[cfg(test)]
pub use wave::write as write_wave;

pub const WORKER_FLAG: &str = "--tts-worker";

#[derive(Default)]
pub struct Progress {
    cancelled: AtomicBool,
    pub received: AtomicU64,
    pub total: AtomicU64,
    text: Mutex<String>,
}

impl Progress {
    pub fn set(&self, text: impl Into<String>) {
        if let Ok(mut current) = self.text.lock() {
            *current = text.into();
        }
        self.received.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
    }

    pub fn set_download(&self, text: impl Into<String>, received: u64, total: u64) {
        if let Ok(mut current) = self.text.lock() {
            *current = text.into();
        }
        self.set_bytes(received, total);
    }

    pub fn set_bytes(&self, received: u64, total: u64) {
        self.received.store(received, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    pub fn text(&self) -> String {
        self.text
            .lock()
            .map_or_else(|_| "正在处理…".into(), |value| value.clone())
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn ensure_running(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err("语音任务已取消".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Reference {
    pub text: String,
    pub path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Request {
    output: PathBuf,
    segments: Vec<Segment>,
    speaker: i32,
    reference: Option<Reference>,
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Ready { sample_rate: u32, speakers: i32 },
    Working { current: usize, total: usize },
    Done { samples: usize, rate: u32 },
    Failed(String),
}

pub struct Output {
    pub file: tempfile::TempPath,
    pub rate: u32,
    pub duration: Duration,
}

enum LoadedBackend {
    Sherpa(Worker),
    CosyVoice(cosyvoice::Engine),
}

pub struct Session {
    backend: Mutex<LoadedBackend>,
    pub model: Model,
    pub rate: u32,
    pub speakers: i32,
}

impl Session {
    pub fn load(model: Model, models_root: &Path, progress: &Progress) -> Result<Self, String> {
        let directory = model.dir(models_root);
        model.check(&directory)?;
        if model == Model::CosyVoice3 {
            let engine = cosyvoice::Engine::load(models_root, &directory, progress)?;
            let rate = engine.sample_rate();
            return Ok(Self {
                backend: Mutex::new(LoadedBackend::CosyVoice(engine)),
                model,
                rate,
                speakers: 0,
            });
        }

        let stage = tempfile::tempdir().map_err(|error| error.to_string())?;
        progress.set("正在加载模型到 CPU");
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let runtime = super::sherpa::runtime_library(&exe)?;
        let mut command = Command::new(exe);
        command
            .arg(WORKER_FLAG)
            .arg(runtime)
            .arg(model.key())
            .arg(&directory)
            .arg(stage.path());
        let mut worker = Worker::start(command, stage)?;
        match worker.receive(progress, Duration::from_secs(5 * 60), false)? {
            Reply::Ready {
                sample_rate,
                speakers,
            } => Ok(Self {
                backend: Mutex::new(LoadedBackend::Sherpa(worker)),
                model,
                rate: sample_rate,
                speakers,
            }),
            Reply::Failed(error) => Err(error),
            _ => Err("语音进程返回意外的加载状态".into()),
        }
    }

    pub fn running(&self) -> bool {
        match self.backend.try_lock() {
            Ok(mut backend) => match &mut *backend {
                LoadedBackend::Sherpa(worker) => worker.alive(),
                LoadedBackend::CosyVoice(engine) => engine.is_alive(),
            },
            Err(std::sync::TryLockError::WouldBlock) => true,
            Err(std::sync::TryLockError::Poisoned(_)) => false,
        }
    }

    pub fn synthesize(
        &self,
        segments: Vec<Segment>,
        speaker: i32,
        reference: Option<Reference>,
        progress: &Progress,
    ) -> Result<Output, String> {
        let mut backend = self.backend.lock().map_err(|_| "语音模型进程已失效")?;
        let alive = match &mut *backend {
            LoadedBackend::Sherpa(worker) => worker.alive(),
            LoadedBackend::CosyVoice(engine) => engine.is_alive(),
        };
        if !alive {
            return Err("语音模型进程已退出，请重新加载".into());
        }
        progress.ensure_running()?;
        let file = tempfile::Builder::new()
            .prefix("tfy-speech-")
            .suffix(".wav")
            .tempfile()
            .map_err(|error| error.to_string())?
            .into_temp_path();
        let request = Request {
            output: file.to_path_buf(),
            segments,
            speaker,
            reference,
        };

        match &mut *backend {
            LoadedBackend::CosyVoice(engine) => {
                let reference = request
                    .reference
                    .as_ref()
                    .ok_or("CosyVoice3 需要参考录音和逐字稿")?;
                let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
                let clip_wav = scratch.path().join("clip.wav");
                let status = |current, total| {
                    progress.ensure_running()?;
                    progress.set(if current <= total {
                        format!("CosyVoice3 CPU 合成 {current} / {total}")
                    } else {
                        "正在归一化并输出 WAV".into()
                    });
                    Ok(())
                };
                let samples = render_script(
                    &request,
                    self.rate,
                    status,
                    |text, _| {
                        engine.generate(text, &reference.text, &reference.path, &clip_wav, progress)
                    },
                    false,
                )?;
                progress.ensure_running()?;
                Ok(Output {
                    file,
                    rate: self.rate,
                    duration: Duration::from_secs_f64(samples as f64 / f64::from(self.rate)),
                })
            }
            LoadedBackend::Sherpa(worker) => {
                worker.send(&request)?;
                match worker.receive(progress, Duration::from_secs(60 * 60), true)? {
                    Reply::Done { samples, rate } => {
                        progress.ensure_running()?;
                        Ok(Output {
                            file,
                            rate,
                            duration: Duration::from_secs_f64(samples as f64 / f64::from(rate)),
                        })
                    }
                    Reply::Failed(error) => Err(error),
                    _ => Err("语音进程返回意外的合成状态".into()),
                }
            }
        }
    }
}

struct Worker {
    child: Child,
    input: ChildStdin,
    stage: tempfile::TempDir,
}

impl Worker {
    fn start(mut command: Command, stage: tempfile::TempDir) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法启动语音推理进程：{error}"))?;
        let input = child.stdin.take().expect("TTS stdin");
        Ok(Self {
            child,
            input,
            stage,
        })
    }

    fn send(&mut self, request: &impl Serialize) -> Result<(), String> {
        let previous = self.stage.path().join("state.json");
        fs::remove_file(previous).map_err(|error| error.to_string())?;
        bgm_core::persist::write_json(&self.stage.path().join("request.json"), request)
            .map_err(|error| error.to_string())?;
        self.input
            .write_all(b"generate\n")
            .and_then(|()| self.input.flush())
            .map_err(|error| error.to_string())
    }

    fn receive(
        &mut self,
        progress: &Progress,
        timeout: Duration,
        generating: bool,
    ) -> Result<Reply, String> {
        let state = self.stage.path().join("state.json");
        let deadline = Instant::now() + timeout;
        let mut last_progress = None;
        loop {
            if let Err(error) = progress.ensure_running() {
                self.stop();
                return Err(error);
            }
            if let Ok(text) = fs::read(&state)
                && let Ok(reply) = serde_json::from_slice(&text)
            {
                match reply {
                    Reply::Working { current, total } if generating => {
                        if last_progress != Some((current, total)) {
                            progress.set(if current <= total {
                                format!("正在合成第 {current} / {total} 段")
                            } else {
                                "正在归一化并输出 WAV".into()
                            });
                            last_progress = Some((current, total));
                        }
                    }
                    other => return Ok(other),
                }
            }
            if !self.alive() {
                return Err("语音推理进程已退出，请检查模型文件和推理环境".into());
            }
            if Instant::now() >= deadline {
                self.stop();
                return Err("语音任务超时".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        if self.alive() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn run_worker() -> Result<(), String> {
    let mut args = std::env::args_os().skip(2);
    let runtime = args.next().map(PathBuf::from).ok_or("缺少语音运行库")?;
    let name = args.next().ok_or("缺少语音模型")?;
    let model = Model::from_key(&name.to_string_lossy()).ok_or("未知语音模型")?;
    if model == Model::CosyVoice3 {
        return Err("CosyVoice3 使用独立的 ONNX 推理进程".into());
    }
    let directory = args.next().map(PathBuf::from).ok_or("缺少语音模型目录")?;
    let stage = args.next().map(PathBuf::from).ok_or("缺少语音工作目录")?;
    let state = stage.join("state.json");
    let respond = |reply: Reply| {
        bgm_core::persist::write_json(&state, &reply).map_err(|error| error.to_string())
    };
    let engine = match engine::Engine::open(&runtime, model, &directory) {
        Ok(engine) => engine,
        Err(error) => return respond(Reply::Failed(error)),
    };
    script::Timeline::new(engine.rate)?;
    respond(Reply::Ready {
        sample_rate: engine.rate,
        speakers: engine.speakers,
    })?;
    for line in io::stdin().lock().lines() {
        if line.map_err(|error| error.to_string())?.trim() != "generate" {
            continue;
        }
        let request = stage.join("request.json");
        let result = fs::read(&request)
            .and_then(|bytes| {
                fs::remove_file(&request)?;
                Ok(bytes)
            })
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                serde_json::from_slice::<Request>(&bytes).map_err(|error| error.to_string())
            })
            .and_then(|request| generate(request, &engine, &respond));
        respond(match result {
            Ok(reply) => reply,
            Err(error) => Reply::Failed(error),
        })?;
    }
    Ok(())
}

fn generate(
    request: Request,
    engine: &engine::Engine,
    respond: &impl Fn(Reply) -> Result<(), String>,
) -> Result<Reply, String> {
    let reference = request
        .reference
        .as_ref()
        .map(|reference| {
            let (rate, samples) = wave::reference(&reference.path)?;
            Ok::<_, String>((reference.text.as_str(), rate, samples))
        })
        .transpose()?;
    let reference = reference
        .as_ref()
        .map(|(text, rate, mono)| (*text, *rate, mono.as_slice()));
    let samples = render_script(
        &request,
        engine.rate,
        |current, total| respond(Reply::Working { current, total }),
        |text, speed| engine.synthesize(text, request.speaker, speed, reference),
        true,
    )?;
    Ok(Reply::Done {
        samples,
        rate: engine.rate,
    })
}

fn render_script(
    request: &Request,
    rate: u32,
    status: impl Fn(usize, usize) -> Result<(), String>,
    mut synthesize: impl FnMut(&str, f32) -> Result<Vec<f32>, String>,
    supports_model_speed: bool,
) -> Result<usize, String> {
    let mut timeline = script::Timeline::new(rate)?;
    let total = request.segments.len();
    for (index, segment) in request.segments.iter().enumerate() {
        let current = index + 1;
        status(current, total)?;
        let mut run = |speed: f32| {
            let raw = synthesize(segment.text.trim(), speed)
                .map_err(|error| format!("第 {current} 段：{error}"))?;
            voices::polish_segment(&raw, rate).map_err(|error| format!("第 {current} 段：{error}"))
        };
        let mut voice = run(1.0)?;
        if let Some(capacity) = timeline.slot(segment)
            && voice.len() > capacity
        {
            if supports_model_speed
                && let Some(speed) = timeline.fit_speed(segment, voice.len())
                && let Ok(faster) = run(speed)
                && faster.len() < voice.len()
            {
                voice = faster;
            }
            if voice.len() > capacity {
                voice = voices::fit_subtitle_audio(&voice, capacity, rate)
                    .map_err(|error| format!("第 {current} 段：{error}"))?;
            }
        }
        timeline.append(current, segment, &voice)?;
    }
    status(total + 1, total)?;
    let raw = tempfile::Builder::new()
        .prefix("tfy-tts-raw-")
        .suffix(".wav")
        .tempfile()
        .map_err(|error| error.to_string())?;
    let frames = timeline.samples.len();
    wave::write(raw.path(), rate, &timeline.samples, false)
        .and_then(|()| voices::normalize_output(raw.path(), &request.output, rate, frames))
        .map_err(|error| format!("无法导出 WAV：{error}"))?;
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_script_longer_than_reference_audio_limit() {
        let directory = tempfile::tempdir().unwrap();
        let request = Request {
            output: directory.path().join("speech.wav"),
            segments: parse(Format::Txt, "第一段。\n\n第二段。").unwrap(),
            speaker: DEFAULT_KOKORO_SPEAKER,
            reference: None,
        };
        let rate = 24_000;
        let voice: Vec<_> = (0..rate * 8)
            .map(|frame| (frame as f32 * 0.11).sin() * 0.2)
            .collect();

        let frames = render_script(
            &request,
            rate,
            |_, _| Ok(()),
            |_, _| Ok(voice.clone()),
            true,
        )
        .unwrap();

        assert_eq!(
            frames,
            voice.len() * 2 + voices::pause_after("第一段。", rate)
        );
        let output = wave::read_generated(&request.output, rate, frames).unwrap();
        assert_eq!(output.len(), frames);
        assert!(
            output[frames - rate as usize..]
                .iter()
                .any(|sample| sample.abs() > 0.01)
        );
        assert!(
            wave::reference(&request.output)
                .unwrap_err()
                .contains("参考音频超过 15 秒")
        );
    }
}
