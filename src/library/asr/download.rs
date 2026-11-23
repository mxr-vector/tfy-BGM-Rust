//! 固定 ASR 模型下载清单和逐文件传输。

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::{Builder, tempfile};
use url::Url;

use super::{ModelPreferences, valid_download_url};

pub const DEFAULT_MODEL_NAME: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";

struct ModelFile {
    remote: &'static str,
    local: &'static str,
    bytes: u64,
}

const MANIFEST: [ModelFile; 9] = [
    ModelFile {
        remote: "model_0.6B/conv_frontend.onnx",
        local: "conv_frontend.onnx",
        bytes: 44_148_281,
    },
    ModelFile {
        remote: "model_0.6B/encoder.int8.onnx",
        local: "encoder.int8.onnx",
        bytes: 182_491_662,
    },
    ModelFile {
        remote: "model_0.6B/decoder.int8.onnx",
        local: "decoder.int8.onnx",
        bytes: 755_914_231,
    },
    ModelFile {
        remote: "tokenizer/merges.txt",
        local: "tokenizer/merges.txt",
        bytes: 1_671_853,
    },
    ModelFile {
        remote: "tokenizer/vocab.json",
        local: "tokenizer/vocab.json",
        bytes: 2_776_833,
    },
    ModelFile {
        remote: "tokenizer/tokenizer_config.json",
        local: "tokenizer/tokenizer_config.json",
        bytes: 12_487,
    },
    ModelFile {
        remote: "tokenizer/config.json",
        local: "tokenizer/config.json",
        bytes: 6_193,
    },
    ModelFile {
        remote: "tokenizer/chat_template.json",
        local: "tokenizer/chat_template.json",
        bytes: 1_161,
    },
    ModelFile {
        remote: "tokenizer/preprocessor_config.json",
        local: "tokenizer/preprocessor_config.json",
        bytes: 330,
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DownloadMode {
    Resume,
    Replace,
}

pub struct DefaultModelFiles {
    pub complete: usize,
    pub count: usize,
    pub expected_bytes: u64,
    pub missing: Vec<&'static str>,
}

impl DefaultModelFiles {
    pub(super) fn inspect(root: &Path) -> Self {
        let target = root.join(DEFAULT_MODEL_NAME);
        let exists = ordinary_directory(&target);
        let missing: Vec<_> = MANIFEST
            .iter()
            .filter(|file| !exists || !file_complete(&target, file))
            .map(|file| file.local)
            .collect();
        Self {
            complete: MANIFEST.len() - missing.len(),
            count: MANIFEST.len(),
            expected_bytes: total_size(&MANIFEST),
            missing,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DownloadProgress {
    pub file: &'static str,
    pub index: usize,
    pub count: usize,
    pub received: u64,
    pub total: u64,
}

#[derive(Default)]
struct DownloadState {
    cancelled: AtomicBool,
    progress: Mutex<Option<DownloadProgress>>,
}

#[derive(Default)]
pub struct ModelDownload(Arc<DownloadState>);

impl ModelDownload {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
    }

    pub fn progress(&self) -> Option<DownloadProgress> {
        self.0.progress.lock().ok()?.clone()
    }

    pub fn job(
        &self,
        preferences: ModelPreferences,
        mode: DownloadMode,
    ) -> impl FnOnce() -> Result<(), String> + use<> {
        let state = self.0.clone();
        move || {
            download(
                &preferences,
                &state,
                &MANIFEST,
                Path::new("/usr/bin/curl"),
                mode,
            )
        }
    }
}

impl Drop for ModelDownload {
    fn drop(&mut self) {
        self.cancel();
    }
}

enum DownloadSource {
    ModelScope { api: Url, revision: String },
    Mirror(Url),
}

impl DownloadSource {
    fn parse(address: &str) -> Result<Self, String> {
        if !valid_download_url(address) {
            return Err("请输入有效的 HTTP(S) 下载地址".into());
        }
        let url = Url::parse(address).map_err(|error| error.to_string())?;
        let parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
        if matches!(url.host_str(), Some("modelscope.cn" | "www.modelscope.cn"))
            && let ["models", owner, repo, rest @ ..] = parts.as_slice()
            && !owner.is_empty()
            && !repo.is_empty()
        {
            let revision = match rest {
                ["tree", revision, ..] if !revision.is_empty() => (*revision).into(),
                _ => "master".into(),
            };
            let api = Url::parse(&format!(
                "https://modelscope.cn/api/v1/models/{owner}/{repo}/repo"
            ))
            .map_err(|error| error.to_string())?;
            return Ok(Self::ModelScope { api, revision });
        }
        Ok(Self::Mirror(url))
    }

    fn file_url(&self, file: &ModelFile) -> Url {
        match self {
            Self::ModelScope { api, revision } => {
                let mut url = api.clone();
                url.query_pairs_mut()
                    .append_pair("Revision", revision)
                    .append_pair("FilePath", file.remote);
                url
            }
            Self::Mirror(base) => {
                let mut url = base.clone();
                url.set_fragment(None);
                if let Ok(mut path) = url.path_segments_mut() {
                    path.pop_if_empty().extend(file.local.split('/'));
                }
                url
            }
        }
    }
}

struct DownloadLock(fs::File);

impl Drop for DownloadLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn download(
    preferences: &ModelPreferences,
    state: &DownloadState,
    files: &[ModelFile],
    curl: &Path,
    mode: DownloadMode,
) -> Result<(), String> {
    let source = DownloadSource::parse(&preferences.download_url)?;
    let root = preferences.root();
    fs::create_dir_all(&root).map_err(|error| format!("无法创建模型目录：{error}"))?;
    let target = ensure_child_directory(&root, DEFAULT_MODEL_NAME)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(target.join(".download.lock"))
        .map_err(|error| format!("无法打开模型下载锁：{error}"))?;
    lock.try_lock()
        .map_err(|error| format!("无法取得模型下载锁，可能有其他实例正在下载：{error}"))?;
    let _lock = DownloadLock(lock);
    let total = total_size(files);
    let reuse = |file: &ModelFile| mode == DownloadMode::Resume && file_complete(&target, file);
    let mut complete = files
        .iter()
        .filter(|file| reuse(file))
        .map(|file| file.bytes)
        .sum::<u64>();
    for (index, file) in files.iter().enumerate() {
        check_cancelled(state)?;
        if reuse(file) {
            continue;
        }
        let output = target.join(file.local);
        let parent = if file.local.starts_with("tokenizer/") {
            ensure_child_directory(&target, "tokenizer")?
        } else {
            target.clone()
        };
        match fs::symlink_metadata(&output) {
            Ok(meta) if !meta.is_file() => {
                return Err(format!("模型文件路径被占用：{}", file.local));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法检查模型文件：{error}")),
        }
        let progress = |partial: u64| {
            if let Ok(mut progress) = state.progress.lock() {
                *progress = Some(DownloadProgress {
                    file: file.local,
                    index: index + 1,
                    count: files.len(),
                    received: complete.saturating_add(partial),
                    total,
                });
            }
        };
        progress(0);
        complete = complete.saturating_add(transfer(
            curl,
            source.file_url(file),
            &parent,
            &output,
            file,
            state,
            progress,
        )?);
    }
    Ok(())
}

fn total_size(files: &[ModelFile]) -> u64 {
    files.iter().map(|file| file.bytes).sum()
}

fn ordinary_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn ensure_child_directory(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists && ordinary_directory(&path) => {}
        Err(error) => return Err(format!("无法创建目录 {}：{error}", path.display())),
    }
    Ok(path)
}

fn file_complete(target: &Path, file: &ModelFile) -> bool {
    (!file.local.starts_with("tokenizer/") || ordinary_directory(&target.join("tokenizer")))
        && fs::symlink_metadata(target.join(file.local))
            .is_ok_and(|meta| meta.is_file() && meta.len() == file.bytes)
}

fn check_cancelled(state: &DownloadState) -> Result<(), String> {
    if state.cancelled.load(Ordering::Relaxed) {
        Err("下载已取消".into())
    } else {
        Ok(())
    }
}

struct Transfer(Child);

impl Drop for Transfer {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn transfer(
    curl: &Path,
    url: Url,
    parent: &Path,
    destination: &Path,
    file: &ModelFile,
    state: &DownloadState,
    report: impl Fn(u64),
) -> Result<u64, String> {
    let part = Builder::new()
        .prefix(".asr-")
        .suffix(".part")
        .tempfile_in(parent)
        .map_err(|error| format!("无法创建下载暂存文件：{error}"))?;
    let mut headers = tempfile().map_err(|error| error.to_string())?;
    let stdout = headers.try_clone().map_err(|error| error.to_string())?;
    let mut child = Transfer(
        Command::new(curl)
            .args([
                "--disable",
                "--fail",
                "--silent",
                "--location",
                "--max-redirs",
                "8",
                "--proto",
                "=http,https",
                "--proto-redir",
                "=http,https",
                "--connect-timeout",
                "20",
                "--max-time",
                "3600",
                "--globoff",
                "--write-out",
                "%{http_code}\n%{content_type}\n%header{content-length}\n",
                "--output",
            ])
            .arg(part.path())
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(stdout)
            .spawn()
            .map_err(|error| format!("无法启动模型下载：{error}"))?,
    );
    let status = loop {
        check_cancelled(state)?;
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|error| format!("无法读取下载状态：{error}"))?
        {
            break status;
        }
        report(
            part.as_file()
                .metadata()
                .map_err(|error| error.to_string())?
                .len(),
        );
        std::thread::sleep(Duration::from_millis(240));
    };
    let mut response = String::new();
    headers.rewind().map_err(|error| error.to_string())?;
    headers
        .take(16_384)
        .read_to_string(&mut response)
        .map_err(|error| error.to_string())?;
    let mut lines = response.lines();
    let code = lines
        .next()
        .and_then(|line| line.parse::<u16>().ok())
        .unwrap_or(0);
    if !status.success() || code != 200 {
        let detail = if code == 0 {
            status.to_string()
        } else {
            format!("HTTP {code}，{status}")
        };
        return Err(format!("下载 {} 失败（{detail}）", file.local));
    }
    if lines
        .next()
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
    {
        return Err("下载地址返回网页，请填写模型文件目录或可直连的地址".into());
    }
    let expected = lines
        .next()
        .and_then(|line| line.trim().parse::<u64>().ok())
        .unwrap_or(file.bytes);
    let received = part
        .as_file()
        .metadata()
        .map_err(|error| error.to_string())?
        .len();
    if received == 0 || received != expected {
        return Err(format!(
            "{} 下载不完整，已收到 {received} 字节，预期 {expected} 字节",
            file.local
        ));
    }
    check_cancelled(state)?;
    part.persist(destination)
        .map_err(|error| format!("无法保存 {}：{}", file.local, error.error))?;
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    const CURL: &str = "/usr/bin/curl";
    const SMALL_FILE: ModelFile = ModelFile {
        remote: "sample",
        local: "sample.onnx",
        bytes: 8,
    };

    fn serve(
        response: Vec<u8>,
        keep_open: Option<mpsc::Receiver<()>>,
    ) -> (Url, thread::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = Url::parse(&format!(
            "http://{}/mirror/",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let task = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "未收到本地下载请求");
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("接收下载请求失败：{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut line = String::new();
            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                    break;
                }
            }
            stream.write_all(&response).unwrap();
            if let Some(keep) = keep_open {
                let _ = keep.recv_timeout(Duration::from_secs(5));
            }
            request
        });
        (address, task)
    }

    #[test]
    fn fixed_manifest_and_sources_match_the_int8_model() {
        assert_eq!(MANIFEST.len(), 9);
        assert_eq!(total_size(&MANIFEST), 987_023_031);
        let source = DownloadSource::parse(super::super::DEFAULT_ASR_DOWNLOAD_URL).unwrap();
        let model = source.file_url(&MANIFEST[2]);
        assert_eq!(
            model.path(),
            "/api/v1/models/zengshuishui/Qwen3-ASR-onnx/repo"
        );
        assert!(
            model
                .query_pairs()
                .any(|(key, value)| key == "FilePath" && value == "model_0.6B/decoder.int8.onnx")
        );
        assert!(
            model
                .query_pairs()
                .any(|(key, value)| key == "Revision" && value == "master")
        );
        let mirror =
            DownloadSource::parse("https://local.example/models/?tag=test#ignore").unwrap();
        assert_eq!(
            mirror.file_url(&MANIFEST[8]).as_str(),
            "https://local.example/models/tokenizer/preprocessor_config.json?tag=test"
        );
    }

    #[test]
    fn real_http_transfer_reuses_complete_files_and_replaces_incomplete_files() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(DEFAULT_MODEL_NAME);
        fs::create_dir(&target).unwrap();
        fs::write(target.join("already.onnx"), b"reuse").unwrap();
        fs::write(target.join(SMALL_FILE.local), b"old").unwrap();
        let files = [
            ModelFile {
                remote: "already",
                local: "already.onnx",
                bytes: 5,
            },
            SMALL_FILE,
        ];
        let (mirror, server) = serve(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\ncomplete".to_vec(),
            None,
        );
        let preferences = ModelPreferences {
            last_model: None,
            directory: Some(directory.path().into()),
            download_url: mirror.to_string(),
        };
        download(
            &preferences,
            &DownloadState::default(),
            &files,
            Path::new(CURL),
            DownloadMode::Resume,
        )
        .unwrap();
        assert_eq!(
            server.join().unwrap(),
            "GET /mirror/sample.onnx HTTP/1.1\r\n"
        );
        assert_eq!(fs::read(target.join(files[0].local)).unwrap(), b"reuse");
        assert_eq!(fs::read(target.join(files[1].local)).unwrap(), b"complete");
        download(
            &preferences,
            &DownloadState::default(),
            &files,
            Path::new(CURL),
            DownloadMode::Resume,
        )
        .unwrap();
    }

    #[test]
    fn redownload_requests_a_complete_file_and_replaces_it_only_on_success() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join(DEFAULT_MODEL_NAME);
        fs::create_dir(&target).unwrap();
        let path = target.join(SMALL_FILE.local);
        fs::write(&path, b"original").unwrap();
        assert!(file_complete(&target, &SMALL_FILE));

        for (response, expected_file, succeeds) in [
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nshort"
                    .as_slice(),
                b"original",
                false,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nreplaced"
                    .as_slice(),
                b"replaced",
                true,
            ),
        ] {
            let (mirror, server) = serve(response.to_vec(), None);
            let preferences = ModelPreferences {
                last_model: None,
                directory: Some(directory.path().into()),
                download_url: mirror.to_string(),
            };
            let result = download(
                &preferences,
                &DownloadState::default(),
                &[SMALL_FILE],
                Path::new(CURL),
                DownloadMode::Replace,
            );
            assert_eq!(
                server.join().unwrap(),
                "GET /mirror/sample.onnx HTTP/1.1\r\n"
            );
            assert_eq!(result.is_ok(), succeeds, "{result:?}");
            assert_eq!(fs::read(&path).unwrap(), expected_file);
            assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
        }
    }

    #[test]
    fn invalid_responses_keep_existing_files_and_remove_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(SMALL_FILE.local);
        fs::write(&path, b"old").unwrap();
        for response in [
            b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 8\r\nConnection: close\r\n\r\nwebpage!".as_slice(),
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nshort",
            b"HTTP/1.1 206 Partial Content\r\nContent-Length: 8\r\nContent-Range: bytes 0-7/16\r\nConnection: close\r\n\r\npartial!",
        ] {
            let (url, server) = serve(response.to_vec(), None);
            assert!(transfer(Path::new(CURL), url, directory.path(), &path, &SMALL_FILE, &DownloadState::default(), |_| {}).is_err());
            server.join().unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"old");
            assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn cancelling_active_http_transfer_reaps_curl_and_cleans_up_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        let (release, keep) = mpsc::channel();
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 262144\r\nConnection: close\r\n\r\n".to_vec();
        response.extend([42; 16_384]);
        let (url, server) = serve(response, Some(keep));
        let state = Arc::new(DownloadState::default());
        let job_state = state.clone();
        let location = directory.path().to_path_buf();
        let (first_progress, progress) = mpsc::channel();
        let task = thread::spawn(move || {
            transfer(
                Path::new(CURL),
                url,
                &location,
                &location.join(SMALL_FILE.local),
                &SMALL_FILE,
                &job_state,
                |bytes| {
                    if bytes != 0 {
                        let _ = first_progress.send(());
                    }
                },
            )
        });
        let observed = progress.recv_timeout(Duration::from_secs(4));
        state.cancelled.store(true, Ordering::Relaxed);
        let result = task.join().unwrap();
        let _ = release.send(());
        server.join().unwrap();
        observed.unwrap();
        assert_eq!(result.unwrap_err(), "下载已取消");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}
