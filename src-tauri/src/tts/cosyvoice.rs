use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::check_cancelled;
use crate::speech_audio;

const SAMPLE_RATE: u32 = 24_000;
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);
const GENERATE_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const STDERR_LIMIT: usize = 16 * 1024;
const FRAME_LIMIT: u64 = 16 * 1024;
const FRAME_PREFIX: &[u8] = b"TFY_COSYVOICE ";

// Lourdle/CosyVoice 24c9212eb54e832af4d9b03bcd9157d88acbf6db.
const PYTHON_WORKER: &str = r#"
import os
import sys

# Reserve the original stdout for protocol replies. Redirect both Python and
# native-library stdout before importing the runtime.
protocol = os.fdopen(os.dup(sys.stdout.fileno()), 'w', encoding='ascii', newline='\n')
os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
sys.stdout = sys.stderr

import json
import traceback


def reply(kind, **fields):
    protocol.write('TFY_COSYVOICE ' + json.dumps(dict(kind=kind, **fields)) + '\n')
    protocol.flush()


def main():
    home, model_dir = sys.argv[1:3]
    sys.path[:0] = [home, os.path.join(home, 'third_party', 'Matcha-TTS')]
    import numpy as np
    import soundfile as sf
    from cosyvoice.cli.cosyvoice import AutoModel

    model = AutoModel(model_dir=model_dir, load_onnx=True, fp16=False)
    rate = int(model.sample_rate)
    if rate != 24000:
        raise ValueError(f'expected a 24 kHz CosyVoice3 model, got {rate} Hz')
    reply('ready', sample_rate=rate)

    for line in sys.stdin:
        request = json.loads(line)
        prompt_text = 'You are a helpful assistant.<|endofprompt|>' + request['prompt_text']
        frames = 0
        with sf.SoundFile(request['output_path'], mode='w', samplerate=rate,
                          channels=1, format='WAV', subtype='PCM_16') as output:
            for chunk in model.inference_zero_shot(
                request['text'], prompt_text, request['prompt_path'],
                stream=False, speed=request['speed'],
            ):
                audio = chunk['tts_speech'].detach().cpu().numpy()
                if audio.ndim == 2 and audio.shape[0] == 1:
                    audio = audio[0]
                if audio.ndim != 1 or not np.isfinite(audio).all():
                    raise ValueError('expected finite mono speech samples')
                output.write(audio)
                frames += len(audio)
        if frames == 0:
            raise ValueError('inference returned no speech samples')
        info = sf.info(request['output_path'])
        if (info.channels, info.samplerate, info.subtype, info.frames) != (1, rate, 'PCM_16', frames):
            raise ValueError('expected a complete 24 kHz mono PCM16 WAV')
        reply('generated')


try:
    main()
except Exception as exc:
    traceback.print_exc()
    reply('error', message=f'{type(exc).__name__}: {exc}'[:1024])
    sys.exit(1)
"#;

#[derive(Default, Deserialize)]
struct RuntimeConfig {
    cosyvoice_home: Option<PathBuf>,
    python: Option<PathBuf>,
}

fn runtime_paths(model: &Path) -> Result<(PathBuf, PathBuf)> {
    let config_path = model.join("runtime.json");
    let config: RuntimeConfig = match fs::read(&config_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("CosyVoice3 配置格式无效：{}", config_path.display()))?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => RuntimeConfig::default(),
        Err(error) => return Err(error).context("无法读取 CosyVoice3 runtime.json"),
    };

    let home = match config.cosyvoice_home {
        Some(home) => {
            ensure!(
                !home.as_os_str().is_empty(),
                "runtime.json 的 cosyvoice_home 路径为空"
            );
            model.join(home)
        }
        None => env::var_os("COSYVOICE_HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .context("请设置 runtime.json 的 cosyvoice_home 或 COSYVOICE_HOME，指向 Lourdle CosyVoice 仓库")?,
    };
    let home = std::path::absolute(home).context("CosyVoice3 仓库路径无效")?;
    ensure!(
        home.join("cosyvoice/cli/cosyvoice.py").is_file(),
        "CosyVoice3 仓库缺少 cosyvoice/cli/cosyvoice.py：{}",
        home.display()
    );
    ensure!(
        home.join("third_party/Matcha-TTS/matcha").is_dir(),
        "CosyVoice3 仓库缺少 Matcha-TTS，请在 {} 中运行 git submodule update --init --recursive",
        home.display()
    );

    let python = config.python.unwrap_or_else(|| {
        PathBuf::from(if cfg!(windows) {
            "python.exe"
        } else {
            "python3"
        })
    });
    ensure!(
        !python.as_os_str().is_empty(),
        "runtime.json 的 python 路径为空"
    );
    // A bare executable name uses PATH. Explicit relative paths use the model directory.
    let python = if python.file_name() == Some(python.as_os_str()) {
        python
    } else {
        let python = model.join(python);
        ensure!(
            python.is_file(),
            "找不到 runtime.json 指定的 Python：{}",
            python.display()
        );
        python
    };
    Ok((home, python))
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    text: &'a str,
    prompt_text: &'a str,
    prompt_path: &'a Path,
    output_path: &'a Path,
    speed: f32,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Reply {
    Ready { sample_rate: u32 },
    Generated,
    Error { message: String },
}

pub(super) struct Engine {
    child: Option<Child>,
    requests: Option<SyncSender<Vec<u8>>>,
    replies: Receiver<Result<Reply>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
}

impl Engine {
    pub(super) fn load(path: &Path, token: &CancellationToken) -> Result<Self> {
        check_cancelled(token)?;
        let model = std::path::absolute(path).context("CosyVoice3 模型路径无效")?;
        for name in [
            "cosyvoice3.yaml",
            "CosyVoice-BlankEN/model.safetensors",
            "llm.pt",
            "flow.pt",
            "hift.pt",
            "flow_fp32.onnx",
            "hift.onnx",
            "campplus.onnx",
            "speech_tokenizer_v3.onnx",
        ] {
            check_cancelled(token)?;
            let file = model.join(name);
            ensure!(
                fs::metadata(&file).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0),
                "CosyVoice3 模型文件缺失或为空：{}",
                file.display()
            );
        }
        let (home, python) = runtime_paths(&model)?;
        check_cancelled(token)?;
        let mut engine = Self::spawn(&python, &home, &model)?;
        let result = engine
            .wait_for_reply(LOAD_TIMEOUT, token)
            .and_then(|reply| match reply {
                Reply::Ready {
                    sample_rate: SAMPLE_RATE,
                } => Ok(()),
                Reply::Ready { sample_rate } => {
                    bail!("CosyVoice3 模型采样率应为 24000 Hz，实际为 {sample_rate} Hz")
                }
                _ => bail!("CosyVoice3 Python 进程返回了无效的启动响应"),
            });
        engine.finish(
            result,
            "CosyVoice3 加载失败，请检查 Lourdle CosyVoice 运行环境及依赖",
            token,
        )?;
        Ok(engine)
    }

    pub(super) fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub(super) fn is_alive(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }

    pub(super) fn generate(
        &mut self,
        text: &str,
        prompt_text: &str,
        prompt_path: &Path,
        output_path: &Path,
        speed: f32,
        token: &CancellationToken,
    ) -> Result<Vec<f32>> {
        let result = (|| {
            check_cancelled(token)?;
            ensure!(
                speed.is_finite() && speed > 0.0,
                "CosyVoice3 语速必须为有限正数"
            );
            let prompt_path = std::path::absolute(prompt_path).context("参考音频路径无效")?;
            let output_path = std::path::absolute(output_path).context("语音输出路径无效")?;
            let mut line = serde_json::to_vec(&GenerateRequest {
                text,
                prompt_text,
                prompt_path: &prompt_path,
                output_path: &output_path,
                speed,
            })
            .context("无法编码 CosyVoice3 请求")?;
            line.push(b'\n');
            self.requests
                .as_ref()
                .context("CosyVoice3 Python 进程已关闭")?
                .try_send(line)
                .context("无法向 CosyVoice3 Python 进程提交请求")?;
            ensure!(
                matches!(
                    self.wait_for_reply(GENERATE_TIMEOUT, token)?,
                    Reply::Generated
                ),
                "CosyVoice3 Python 进程返回了无效的推理响应"
            );
            let wave = speech_audio::read_wave(&output_path)
                .context("CosyVoice3 未生成有效的单声道 PCM WAV")?;
            ensure!(
                wave.sample_rate() == self.sample_rate() as i32,
                "CosyVoice3 输出采样率须为 24000 Hz"
            );
            let samples = wave.samples();
            ensure!(
                !samples.is_empty() && samples.iter().all(|sample| sample.is_finite()),
                "CosyVoice3 未生成有效音频采样"
            );
            Ok(samples.to_vec())
        })();
        self.finish(result, "CosyVoice3 推理失败", token)
    }

    fn spawn(python: &Path, home: &Path, model: &Path) -> Result<Self> {
        let child = bgm_core::process::command(python)
            .args(["-u", "-c", PYTHON_WORKER])
            .arg(home)
            .arg(model)
            .env("CUDA_VISIBLE_DEVICES", "")
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "无法启动 CosyVoice3 Python（{}），请检查 PATH 或 runtime.json 的 python",
                    python.display()
                )
            })?;

        let (requests, pending) = mpsc::sync_channel::<Vec<u8>>(1);
        let (events, replies) = mpsc::channel();
        let mut engine = Self {
            child: Some(child),
            requests: Some(requests),
            replies,
            stderr_tail: Arc::default(),
        };
        // Each thread owns its pipe. In particular, writing a large request to
        // a stalled worker must never block the caller's cancellation polling.
        let child = engine.child.as_mut().unwrap();
        let mut stdin = child
            .stdin
            .take()
            .context("CosyVoice3 Python stdin 不可用")?;
        let stdout = child
            .stdout
            .take()
            .context("CosyVoice3 Python stdout 不可用")?;
        let stderr = child
            .stderr
            .take()
            .context("CosyVoice3 Python stderr 不可用")?;
        let tail = Arc::clone(&engine.stderr_tail);
        thread::Builder::new()
            .name("cosyvoice-stderr".into())
            .spawn(move || drain_stderr(stderr, &tail))?;
        let write_events = events.clone();
        thread::Builder::new()
            .name("cosyvoice-stdin".into())
            .spawn(move || {
                while let Ok(line) = pending.recv() {
                    if let Err(error) = stdin.write_all(&line) {
                        let _ = write_events
                            .send(Err(error).context("写入 CosyVoice3 Python 请求失败"));
                        return;
                    }
                }
            })?;
        thread::Builder::new()
            .name("cosyvoice-stdout".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let reply = read_reply(&mut stdout);
                    let terminal = reply.is_err() || matches!(reply, Ok(Reply::Error { .. }));
                    if events.send(reply).is_err() || terminal {
                        return;
                    }
                }
            })?;
        Ok(engine)
    }

    fn wait_for_reply(&mut self, timeout: Duration, token: &CancellationToken) -> Result<Reply> {
        let deadline = Instant::now() + timeout;
        loop {
            check_cancelled(token)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "等待 CosyVoice3 Python 响应超时（{} 秒）",
                timeout.as_secs()
            );
            match self.replies.recv_timeout(remaining.min(POLL_INTERVAL)) {
                Ok(reply) => {
                    check_cancelled(token)?;
                    return match reply? {
                        Reply::Error { message } => bail!("{message}"),
                        reply => Ok(reply),
                    };
                }
                Err(RecvTimeoutError::Disconnected) => bail!("CosyVoice3 Python 通信已中断"),
                Err(RecvTimeoutError::Timeout) => {
                    let child = self
                        .child
                        .as_mut()
                        .context("CosyVoice3 Python 进程已关闭")?;
                    if let Some(status) = child
                        .try_wait()
                        .context("无法查询 CosyVoice3 Python 进程状态")?
                    {
                        bail!("CosyVoice3 Python 进程已退出（{status}）");
                    }
                }
            }
        }
    }

    fn finish<T>(
        &mut self,
        result: Result<T>,
        stage: &str,
        token: &CancellationToken,
    ) -> Result<T> {
        if token.is_cancelled() {
            self.stop();
            check_cancelled(token)?;
        }
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                self.stop();
                let tail = self
                    .stderr_tail
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                let tail = String::from_utf8_lossy(&tail);
                if tail.trim().is_empty() {
                    Err(error).context(stage.to_owned())
                } else {
                    Err(error).with_context(|| {
                        format!("{stage}\nPython stderr（末尾）：\n{}", tail.trim())
                    })
                }
            }
        }
    }

    fn stop(&mut self) {
        self.requests.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_reply(stdout: &mut impl BufRead) -> Result<Reply> {
    let mut line = Vec::new();
    loop {
        line.clear();
        stdout
            .take(FRAME_LIMIT + 1)
            .read_until(b'\n', &mut line)
            .context("读取 CosyVoice3 Python 响应失败")?;
        ensure!(!line.is_empty(), "CosyVoice3 Python 已关闭响应管道");
        ensure!(
            line.len() as u64 <= FRAME_LIMIT && line.ends_with(b"\n"),
            "CosyVoice3 Python 响应帧不完整或过长"
        );
        if let Some(payload) = line.strip_prefix(FRAME_PREFIX) {
            return serde_json::from_slice(payload).context("无法解析 CosyVoice3 Python 响应");
        }
    }
}

fn drain_stderr(mut stderr: impl Read, tail: &Mutex<Vec<u8>>) {
    let mut buffer = [0; 4096];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => return,
            Ok(len) => {
                let mut tail = tail.lock().unwrap_or_else(|error| error.into_inner());
                let excess = (tail.len() + len).saturating_sub(STDERR_LIMIT);
                tail.drain(..excess);
                tail.extend_from_slice(&buffer[..len]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}
