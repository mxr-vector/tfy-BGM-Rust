//! CosyVoice3 的 Lourdle/CosyVoice 运行时、Python 3.11 环境与独立 CPU 推理进程。

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::models::{COSYVOICE_FILES, verify_sha256};
use super::{Progress, models, script, wave};

const SAMPLE_RATE: u32 = 24_000;
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
const ENV_CHECK_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SETUP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const GENERATE_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const STDERR_LIMIT: usize = 16 * 1024;
const FRAME_LIMIT: usize = 16 * 1024;
const FRAME_PREFIX: &[u8] = b"TFY_COSYVOICE ";
const RUNTIME_COMMIT: &str = "24c9212";

const PYTHON_WORKER: &str = include_str!("cosyvoice_worker.py");

const COSYVOICE_COMMON_PACKAGES: &[&str] = &[
    "setuptools==80.9.0",
    "numpy==1.26.4",
    "transformers==4.51.3",
    "modelscope==1.20.0",
    "HyperPyYAML==1.2.3",
    "openai-whisper==20250625",
    "soundfile==0.12.1",
    "librosa==0.10.2",
    "conformer==0.3.2",
    "diffusers==0.29.0",
    "x-transformers==2.11.24",
    "hydra-core==1.3.2",
    "lightning==2.2.4",
    "rich==13.7.1",
    "gdown==5.1.0",
    "matplotlib==3.7.5",
    "wget==3.2",
    "pyarrow==18.1.0",
    "pyworld==0.3.5",
    "inflect",
    "pydantic",
];

fn cache_dir(models_root: &Path) -> PathBuf {
    models_root.join(models::DIR).join(".cosyvoice-cache")
}

fn runtime_home(cache: &Path) -> PathBuf {
    cache.join(format!("runtime/Lourdle-CosyVoice-{RUNTIME_COMMIT}"))
}

fn runtime_python(cache: &Path) -> PathBuf {
    let binary = if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python3"
    };
    cache.join("venv").join(binary)
}

fn marker_file(cache: &Path) -> PathBuf {
    cache.join("ready")
}

fn code_ready(home: &Path) -> bool {
    home.join("cosyvoice/cli/cosyvoice.py").is_file()
        && home
            .join("third_party/Matcha-TTS/matcha/utils/audio.py")
            .is_file()
}

pub fn runtime_ready(models_root: &Path) -> bool {
    let cache = cache_dir(models_root);
    let home = runtime_home(&cache);
    code_ready(&home)
        && runtime_python(&cache).is_file()
        && fs::read_to_string(marker_file(&cache)).is_ok_and(|value| value.trim() == RUNTIME_COMMIT)
}

fn uv_path() -> Option<PathBuf> {
    let shell_paths = env::var_os("PATH");
    let local = dirs::home_dir().map(|directory| directory.join(".local/bin/uv"));
    [
        PathBuf::from("/opt/homebrew/bin/uv"),
        PathBuf::from("/usr/local/bin/uv"),
    ]
    .into_iter()
    .chain(local)
    .chain(
        shell_paths
            .as_ref()
            .into_iter()
            .flat_map(env::split_paths)
            .map(|path| path.join("uv")),
    )
    .find(|path| path.is_absolute() && path.is_file())
}

fn prepare_code(home: &Path, progress: &Progress) -> Result<(), String> {
    let root = home.parent().ok_or("CosyVoice 运行目录无效")?;
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    if home.exists() {
        fs::remove_dir_all(home).map_err(|error| format!("无法清理旧运行代码：{error}"))?;
    }
    let staging = tempfile::tempdir_in(root).map_err(|error| error.to_string())?;
    let sources = [
        (
            "CosyVoice",
            "https://codeload.github.com/Lourdle/CosyVoice/tar.gz/24c9212eb54e832af4d9b03bcd9157d88acbf6db",
            "f2dbab21a5b539e56cc7de226ef1ed344d649fe3dff769d247970ac21e4394fb",
            1_484_017u64,
            staging.path().join("CosyVoice-root"),
        ),
        (
            "Matcha-TTS",
            "https://codeload.github.com/shivammehta25/Matcha-TTS/tar.gz/dd9105b34bf2be2230f4aa1e4769fb586a3c824e",
            "59c43afae79110e4057eaa43f45b5558b556435da8fc85eab83f0c819ba3358d",
            694_198u64,
            staging.path().join("CosyVoice-root/third_party/Matcha-TTS"),
        ),
    ];
    for (name, url, hash, _, destination_root) in sources {
        progress.ensure_running()?;
        progress.set(format!("正在下载 {name} 运行代码…"));
        let archive = staging.path().join(format!("{name}.tar.gz"));
        models::download(url, &archive, progress)?;
        let actual_len = fs::metadata(&archive).map_or(0, |meta| meta.len());
        verify_sha256(&archive, hash, actual_len)
            .map_err(|error| format!("{name} 运行代码校验失败：{error}"))?;
        progress.set(format!("正在解压 {name} 运行代码…"));
        let extract = staging.path().join(name);
        fs::create_dir_all(&extract).map_err(|error| error.to_string())?;
        let mut tar = Command::new("/usr/bin/tar");
        tar.arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&extract)
            .args(["--no-same-owner", "--no-same-permissions"]);
        models::run(tar, None, 0, 0, progress, Duration::from_secs(120))?;
        let top = fs::read_dir(&extract)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.is_dir())
            .ok_or_else(|| format!("{name} 压缩包内容无效"))?;
        if destination_root.exists() {
            fs::remove_dir_all(&destination_root).map_err(|error| error.to_string())?;
        }
        fs::create_dir_all(destination_root.parent().unwrap())
            .map_err(|error| error.to_string())?;
        fs::rename(top, &destination_root).map_err(|error| error.to_string())?;
    }
    let staged_home = staging.path().join("CosyVoice-root");
    if !code_ready(&staged_home) {
        return Err("CosyVoice 运行代码缺少必需文件".into());
    }
    fs::rename(&staged_home, home).map_err(|error| format!("无法保存 CosyVoice 运行代码：{error}"))
}

pub fn prepare_runtime(
    models_root: &Path,
    model_dir: Option<&Path>,
    progress: &Progress,
) -> Result<(), String> {
    let cache = cache_dir(models_root);
    fs::create_dir_all(&cache).map_err(|error| error.to_string())?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(cache.join(".runtime.lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock()
        .map_err(|_| "有另一个实例正在准备 CosyVoice3 环境")?;

    let home = runtime_home(&cache);
    if !code_ready(&home) {
        prepare_code(&home, progress)?;
    }

    let uv = uv_path().ok_or("找不到 uv。请安装 uv（如 brew install uv）后重试环境准备")?;
    let python = runtime_python(&cache);
    let venv_dir = cache.join("venv");
    let uv_cache = cache.join("uv-cache");
    let python_install_dir = cache.join("python");

    if !python.is_file() {
        let _ = fs::remove_dir_all(&venv_dir);
        progress.set("正在准备 Python 3.11 虚拟环境…");
        let mut create = bgm_core::process::command(&uv);
        create
            .args(["venv", "--python", "3.11", "--no-python-downloads"])
            .arg(&venv_dir)
            .env("UV_CACHE_DIR", &uv_cache);
        if models::run(create, None, 0, 0, progress, Duration::from_secs(45)).is_err() {
            progress.ensure_running()?;
            let mut install_py = bgm_core::process::command(&uv);
            install_py
                .args(["python", "install", "3.11.14"])
                .env("UV_PYTHON_INSTALL_DIR", &python_install_dir)
                .env("UV_CACHE_DIR", &uv_cache);
            models::run(install_py, None, 0, 0, progress, SETUP_COMMAND_TIMEOUT)?;

            let mut create = bgm_core::process::command(&uv);
            create
                .args(["venv", "--python", "3.11.14", "--managed-python"])
                .arg(&venv_dir)
                .env("UV_PYTHON_INSTALL_DIR", &python_install_dir)
                .env("UV_CACHE_DIR", &uv_cache);
            models::run(create, None, 0, 0, progress, Duration::from_secs(120))?;
        }
    }

    progress.set("正在安装 CosyVoice3 CPU 推理依赖（可能需要数分钟）…");
    let mut install = bgm_core::process::command(&uv);
    install
        .args(["pip", "install", "--python"])
        .arg(&python)
        .args(["torch==2.3.1", "torchaudio==2.3.1", "onnxruntime==1.23.2"])
        .args(COSYVOICE_COMMON_PACKAGES)
        .env("UV_CACHE_DIR", &uv_cache);
    models::run(install, None, 0, 0, progress, SETUP_COMMAND_TIMEOUT)
        .map_err(|error| format!("安装 CosyVoice3 CPU 依赖失败：{error}"))?;

    if let Some(model) = model_dir.filter(|dir| super::Model::CosyVoice3.check(dir).is_ok()) {
        progress.set("正在验证 CosyVoice3 环境及 ONNX 模型…");
        let mut self_test = worker_command(&python, &home, model, &cache);
        self_test.arg("--self-test");
        models::run(self_test, None, 0, 0, progress, ENV_CHECK_TIMEOUT)
            .map_err(|error| format!("CosyVoice3 环境自检失败：{error}"))?;
    } else {
        progress.set("正在验证 CosyVoice3 Python 依赖…");
        let mut inspect = bgm_core::process::command(&python);
        inspect
            .args([
                "-u",
                "-c",
                "import torch, torchaudio, onnxruntime, numpy, transformers, hyperpyyaml, soundfile, librosa, scipy; assert 'CPUExecutionProvider' in onnxruntime.get_available_providers()",
            ])
            .env("PYTHONPATH", home.join("third_party/Matcha-TTS"))
            .env("PYTHONUTF8", "1");
        models::run(inspect, None, 0, 0, progress, Duration::from_secs(120))?;
    }

    progress.ensure_running()?;
    fs::write(marker_file(&cache), format!("{RUNTIME_COMMIT}\n"))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn worker_command(python: &Path, home: &Path, model: &Path, cache: &Path) -> Command {
    let mut command = bgm_core::process::command(python);
    command
        .args(["-u", "-c", PYTHON_WORKER])
        .arg(home)
        .arg(model)
        .env("HF_HUB_OFFLINE", "1")
        .env("MODELSCOPE_OFFLINE", "1")
        .env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONPATH", home.join("third_party/Matcha-TTS"))
        .env("MODELSCOPE_CACHE", cache.join("modelscope"))
        .env("TFY_COSYVOICE_USE_CUDA", "0")
        .env("CUDA_VISIBLE_DEVICES", "")
        .env("TFY_COSYVOICE_MANAGED_CUDA", "0");
    command
}

#[derive(Serialize)]
struct GenerateRequest<'a> {
    text: &'a str,
    prompt_text: &'a str,
    prompt_path: &'a Path,
    output_path: &'a Path,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Reply {
    Ready { sample_rate: u32 },
    Generated,
    Error { message: String },
}

pub struct Engine {
    child: Option<Child>,
    requests: Option<SyncSender<Vec<u8>>>,
    replies: Receiver<Result<Reply, String>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
}

impl Engine {
    pub fn load(models_root: &Path, model_dir: &Path, progress: &Progress) -> Result<Self, String> {
        progress.ensure_running()?;
        let model =
            std::path::absolute(model_dir).map_err(|_| "CosyVoice3 模型路径无效".to_owned())?;
        for file in COSYVOICE_FILES {
            progress.ensure_running()?;
            let path = model.join(file.path);
            if !fs::metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() > 0) {
                return Err(format!("CosyVoice3 模型文件缺失或为空：{}", path.display()));
            }
        }
        if !runtime_ready(models_root) {
            prepare_runtime(models_root, Some(&model), progress)?;
        }
        progress.ensure_running()?;
        progress.set("正在启动 CosyVoice3 CPU 推理进程…");
        let cache = cache_dir(models_root);
        let python = runtime_python(&cache);
        let home = runtime_home(&cache);
        let mut engine = Self::spawn(&python, &home, &model, &cache)?;
        let reply = engine.wait_for_reply(LOAD_TIMEOUT, progress);
        match reply {
            Ok(Reply::Ready {
                sample_rate: SAMPLE_RATE,
            }) => Ok(engine),
            Ok(Reply::Ready { sample_rate }) => {
                engine.stop();
                Err(format!(
                    "CosyVoice3 模型采样率应为 24000 Hz，实际为 {sample_rate} Hz"
                ))
            }
            Ok(Reply::Error { message }) => {
                engine.stop();
                Err(format!("CosyVoice3 加载失败：{message}"))
            }
            Ok(Reply::Generated) => {
                engine.stop();
                Err("CosyVoice3 Python 进程返回了无效的启动响应".into())
            }
            Err(error) => {
                let tail = engine.stderr_string();
                engine.stop();
                if tail.is_empty() {
                    Err(error)
                } else {
                    Err(format!("{error}：{tail}"))
                }
            }
        }
    }

    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }

    pub fn generate(
        &mut self,
        text: &str,
        prompt_text: &str,
        prompt_path: &Path,
        output_path: &Path,
        progress: &Progress,
    ) -> Result<Vec<f32>, String> {
        if let Err(error) = progress.ensure_running() {
            self.stop();
            return Err(error);
        }
        let prompt_path =
            std::path::absolute(prompt_path).map_err(|_| "参考音频路径无效".to_owned())?;
        let output_path =
            std::path::absolute(output_path).map_err(|_| "语音输出路径无效".to_owned())?;
        let mut line = serde_json::to_vec(&GenerateRequest {
            text,
            prompt_text,
            prompt_path: &prompt_path,
            output_path: &output_path,
        })
        .map_err(|error| error.to_string())?;
        line.push(b'\n');
        self.requests
            .as_ref()
            .ok_or("CosyVoice3 Python 进程已关闭")?
            .try_send(line)
            .map_err(|_| "无法向 CosyVoice3 Python 进程提交请求".to_owned())?;
        match self.wait_for_reply(GENERATE_TIMEOUT, progress)? {
            Reply::Generated => {}
            Reply::Error { message } => return Err(format!("CosyVoice3 合成失败：{message}")),
            Reply::Ready { .. } => {
                return Err("CosyVoice3 Python 进程返回了无效的推理响应".into());
            }
        }
        let max_frames = SAMPLE_RATE as usize * script::MAX_SECONDS as usize;
        let samples = wave::read_generated(&output_path, SAMPLE_RATE, max_frames)?;
        if !samples.iter().all(|sample| sample.is_finite()) {
            return Err("CosyVoice3 未生成有效音频采样".into());
        }
        Ok(samples)
    }

    fn spawn(python: &Path, home: &Path, model: &Path, cache: &Path) -> Result<Self, String> {
        let mut command = worker_command(python, home, model, cache);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| {
            format!(
                "无法启动 CosyVoice3 Python 推理进程（{}）：{error}",
                python.display()
            )
        })?;
        let stdin = child.stdin.take().ok_or("missing stdin")?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let stderr = child.stderr.take().ok_or("missing stderr")?;

        let (req_tx, req_rx) = mpsc::sync_channel::<Vec<u8>>(4);
        thread::Builder::new()
            .name("cosyvoice-stdin".into())
            .spawn(move || {
                let mut stdin = stdin;
                for line in req_rx {
                    if stdin.write_all(&line).is_err() || stdin.flush().is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let (rep_tx, rep_rx) = mpsc::channel();
        thread::Builder::new()
            .name("cosyvoice-stdout".into())
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    let read = read_bounded_line(&mut reader, &mut buf, FRAME_LIMIT);
                    match read {
                        Ok(0) => break,
                        Ok(_) => {
                            let line = buf
                                .strip_suffix(b"\n")
                                .unwrap_or(&buf)
                                .strip_suffix(b"\r")
                                .unwrap_or(&buf);
                            if let Some(json) = line.strip_prefix(FRAME_PREFIX) {
                                let reply = serde_json::from_slice::<Reply>(json)
                                    .map_err(|error| format!("CosyVoice3 响应解析失败：{error}"));
                                if rep_tx.send(reply).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = rep_tx.send(Err(error));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        let stderr_tail = Arc::new(Mutex::new(Vec::new()));
        let tail_sink = stderr_tail.clone();
        thread::Builder::new()
            .name("cosyvoice-stderr".into())
            .spawn(move || {
                let mut stderr = stderr;
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut buf) = tail_sink.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > STDERR_LIMIT {
                            let excess = buf.len() - STDERR_LIMIT;
                            buf.drain(..excess);
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;

        Ok(Self {
            child: Some(child),
            requests: Some(req_tx),
            replies: rep_rx,
            stderr_tail,
        })
    }

    fn wait_for_reply(&mut self, timeout: Duration, progress: &Progress) -> Result<Reply, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Err(error) = progress.ensure_running() {
                self.stop();
                return Err(error);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.stop();
                return Err("CosyVoice3 推理超时".into());
            }
            match self.replies.recv_timeout(remaining.min(POLL_INTERVAL)) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) => {
                    if !self.is_alive() {
                        return Err("CosyVoice3 Python 进程已意外退出".into());
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("CosyVoice3 Python 进程已关闭输出".into());
                }
            }
        }
    }

    fn stderr_string(&self) -> String {
        self.stderr_tail
            .lock()
            .map(|buf| String::from_utf8_lossy(&buf).trim().to_owned())
            .unwrap_or_default()
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

fn read_bounded_line(
    reader: &mut impl BufRead,
    buf: &mut Vec<u8>,
    limit: usize,
) -> Result<usize, String> {
    let mut total = 0;
    loop {
        let available = reader.fill_buf().map_err(|error| error.to_string())?;
        if available.is_empty() {
            return Ok(total);
        }
        let newline = available.iter().position(|&b| b == b'\n');
        let take = newline.map_or(available.len(), |pos| pos + 1);
        if total + take > limit {
            return Err("CosyVoice3 协议帧超过长度上限".into());
        }
        buf.extend_from_slice(&available[..take]);
        reader.consume(take);
        total += take;
        if newline.is_some() {
            return Ok(total);
        }
    }
}
