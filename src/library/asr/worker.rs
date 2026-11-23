//! 识别子进程协议与模型会话。

mod engine;
mod pcm;

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, TryLockError, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{MODEL_FILES, model_id};
use crate::library::{SubtitleSegment, sherpa::runtime_library};

pub const WORKER_FLAG: &str = "--asr-worker";
const WORKER_EXITED: &str = "ASR 进程提前退出，请核对模型文件与推理库";
#[derive(Serialize, Deserialize)]
enum Request {
    Transcribe(PathBuf),
    TranscribeWithProgress(PathBuf),
}

#[derive(Serialize, Deserialize)]
enum Reply {
    Ready,
    Progress { current: usize, total: usize },
    Segments(Vec<SubtitleSegment>),
    Failed(String),
}

pub struct AsrSession {
    pub id: String,
    worker: Mutex<Worker>,
}

impl AsrSession {
    pub fn load(root: &Path, id: String) -> Result<Self, String> {
        let model = verified_model(root, &id)?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let runtime = runtime_library(&executable)?;
        let mut command = Command::new(executable);
        command.args([WORKER_FLAG]).arg(runtime).arg(model);
        let worker = Worker::start(&mut command)?;
        Ok(Self {
            id,
            worker: Mutex::new(worker),
        })
    }

    pub fn transcribe(&self, wav: PathBuf) -> Result<Vec<SubtitleSegment>, String> {
        self.worker
            .lock()
            .map_err(|_| "ASR 会话不可用")?
            .transcribe(wav)
    }

    pub fn transcribe_with_progress(
        &self,
        wav: PathBuf,
        progress: impl FnMut(usize, usize),
    ) -> Result<Vec<SubtitleSegment>, String> {
        self.worker
            .lock()
            .map_err(|_| "ASR 会话不可用")?
            .recognize(Request::TranscribeWithProgress(wav), progress)
    }

    pub fn is_running(&self) -> bool {
        match self.worker.try_lock() {
            Ok(mut worker) => worker.is_running(),
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(_)) => false,
        }
    }
}

fn verified_model(root: &Path, id: &str) -> Result<PathBuf, String> {
    let id = model_id(id).ok_or("模型标识无效")?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("无法访问模型目录：{error}"))?;
    let directory = root
        .join(id)
        .canonicalize()
        .map_err(|error| format!("无法访问所选模型：{error}"))?;
    if directory == root || !directory.starts_with(&root) || !directory.is_dir() {
        return Err("模型必须位于当前模型目录内".into());
    }
    for file in MODEL_FILES {
        if !fs::symlink_metadata(directory.join(file))
            .is_ok_and(|meta| meta.is_file() && meta.len() != 0)
        {
            return Err(format!("模型缺少有效文件 {file}"));
        }
    }
    if !fs::symlink_metadata(directory.join("tokenizer")).is_ok_and(|meta| meta.is_dir()) {
        return Err("模型缺少 tokenizer 目录".into());
    }
    Ok(directory)
}

struct Worker {
    child: Child,
    input: ChildStdin,
    messages: mpsc::Receiver<Result<Reply, String>>,
}

impl Worker {
    fn start(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法启动 ASR 进程：{error}"))?;
        let input = child.stdin.take().expect("ASR stdin");
        let output = child.stdout.take().expect("ASR stdout");
        let (sender, messages) = mpsc::sync_channel(1);
        let mut worker = Self {
            child,
            input,
            messages,
        };
        std::thread::Builder::new()
            .name("asr-responses".into())
            .spawn(move || {
                let mut stream = BufReader::new(output);
                loop {
                    match read_message::<Reply>(&mut stream) {
                        Ok(Some(message)) => {
                            if sender.send(Ok(message)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    }
                }
            })
            .map_err(|error| format!("无法读取 ASR 进程状态：{error}"))?;
        match worker.receive(Duration::from_secs(90))? {
            Reply::Ready => Ok(worker),
            Reply::Failed(error) => Err(error),
            _ => Err("ASR 进程返回了意外的加载结果".into()),
        }
    }

    fn receive(&mut self, timeout: Duration) -> Result<Reply, String> {
        match self.messages.recv_timeout(timeout) {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => {
                self.stop();
                Err(error)
            }
            Err(reason) => {
                self.stop();
                Err(match reason {
                    mpsc::RecvTimeoutError::Timeout => "ASR 进程响应超时".into(),
                    mpsc::RecvTimeoutError::Disconnected => WORKER_EXITED.into(),
                })
            }
        }
    }

    fn transcribe(&mut self, path: PathBuf) -> Result<Vec<SubtitleSegment>, String> {
        self.recognize(Request::Transcribe(path), |_, _| {})
    }

    fn recognize(
        &mut self,
        request: Request,
        mut progress: impl FnMut(usize, usize),
    ) -> Result<Vec<SubtitleSegment>, String> {
        if !self.is_running() {
            return Err(WORKER_EXITED.into());
        }
        if let Err(error) = write_message(&mut self.input, &request) {
            self.stop();
            return Err(error);
        }
        loop {
            match self.receive(Duration::from_secs(30 * 60))? {
                Reply::Segments(segments) => return Ok(segments),
                Reply::Failed(error) => return Err(error),
                Reply::Progress { current, total } => progress(current, total),
                Reply::Ready => {
                    self.stop();
                    return Err("ASR 进程返回了意外的识别结果".into());
                }
            }
        }
    }

    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn read_message<T: DeserializeOwned>(stream: &mut impl BufRead) -> Result<Option<T>, String> {
    let mut line = String::new();
    let count = stream
        .take(64 * 1024 * 1024)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if count == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(|error| format!("ASR 消息格式无效：{error}"))
}

fn write_message(stream: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(&mut *stream, value)
        .map_err(|error| format!("无法发送 ASR 消息：{error}"))?;
    stream
        .write_all(b"\n")
        .and_then(|()| stream.flush())
        .map_err(|error| format!("无法发送 ASR 消息：{error}"))
}

pub fn run_asr_worker() -> Result<(), String> {
    let mut args = std::env::args_os().skip(2);
    let runtime = args.next().map(PathBuf::from).ok_or("缺少 ASR 推理库")?;
    let model = args.next().map(PathBuf::from).ok_or("缺少 ASR 模型目录")?;
    let mut output = io::stdout().lock();
    let engine = match engine::Engine::open(&runtime, &model) {
        Ok(engine) => engine,
        Err(error) => return write_message(&mut output, &Reply::Failed(error)),
    };
    write_message(&mut output, &Reply::Ready)?;
    let mut input = io::stdin().lock();
    while let Some(request) = read_message::<Request>(&mut input)? {
        let (path, reports_progress) = match request {
            Request::Transcribe(path) => (path, false),
            Request::TranscribeWithProgress(path) => (path, true),
        };
        let reply = match engine.transcribe(&path, |current, total| {
            if reports_progress {
                write_message(&mut output, &Reply::Progress { current, total })?;
            }
            Ok(())
        }) {
            Ok(segments) => Reply::Segments(segments),
            Err(error) => Reply::Failed(error),
        };
        write_message(&mut output, &reply)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_loading_rejects_parent_paths_empty_components_and_invalid_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("root");
        let model = root.join("nested/model");
        fs::create_dir_all(model.join("tokenizer")).unwrap();
        for component in MODEL_FILES {
            fs::write(model.join(component), b"component").unwrap();
        }
        assert_eq!(
            verified_model(&root, "nested/model").unwrap(),
            model.canonicalize().unwrap()
        );
        assert!(verified_model(&root, "../root/nested/model").is_err());
        assert!(verified_model(&root, "nested//model").is_err());
        fs::write(model.join(MODEL_FILES[0]), []).unwrap();
        assert!(verified_model(&root, "nested/model").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn recognizer_process_exit_returns_an_error_without_stopping_the_host() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '\"Ready\"\\n'; IFS= read -r request; exit 1"]);
        let mut worker = Worker::start(&mut command).unwrap();
        assert!(
            worker
                .transcribe("fixture.wav".into())
                .unwrap_err()
                .contains("提前退出")
        );
        assert!(!worker.is_running());
        assert_eq!(
            worker.transcribe("retry.wav".into()).unwrap_err(),
            WORKER_EXITED
        );
    }

    #[cfg(unix)]
    #[test]
    fn ended_idle_worker_is_no_longer_reported_as_loaded() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '\"Ready\"\\n'; exit 1"]);
        let mut worker = Worker::start(&mut command).unwrap();
        assert!(!worker.child.wait().unwrap().success());
        let session = AsrSession {
            id: "fixture".into(),
            worker: Mutex::new(worker),
        };
        assert!(!session.is_running());
    }

    #[cfg(unix)]
    #[test]
    fn unexpected_recognition_reply_invalidates_the_session() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"printf '"Ready"\n'; while IFS= read -r request; do printf '"Ready"\n'; done"#,
        ]);
        let session = AsrSession {
            id: "fixture".into(),
            worker: Mutex::new(Worker::start(&mut command).unwrap()),
        };
        assert!(session.is_running());
        assert!(
            session
                .transcribe("fixture.wav".into())
                .unwrap_err()
                .contains("意外")
        );
        assert!(!session.is_running());
        assert_eq!(
            session.transcribe("retry.wav".into()).unwrap_err(),
            WORKER_EXITED
        );
    }

    #[cfg(unix)]
    #[test]
    fn worker_stays_loaded_across_requests_and_returns_structured_segments() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", r#"printf '"Ready"\n'; while IFS= read -r request; do printf '%s\n' '{"Segments":[{"start":0.0,"end":1.0,"text":"测试"}]}'; done"#]);
        let mut worker = Worker::start(&mut command).unwrap();
        for path in ["first.wav", "second.wav"] {
            let segments = worker.transcribe(path.into()).unwrap();
            assert_eq!(segments[0].start, 0.);
            assert_eq!(segments[0].end, 1.);
            assert_eq!(segments[0].text, "测试");
        }
        assert!(worker.child.try_wait().unwrap().is_none());
    }
}
