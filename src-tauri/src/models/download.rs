//! 默认模型下载：镜像解析、断点续传与进度事件。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tauri::{AppHandle, Emitter};

use super::catalog::{ensure_root, missing_components, total_bytes};
use super::{
    DEFAULT_DOWNLOAD_URL, DEFAULT_MODEL_NAME, DOWNLOAD_EVENT, DownloadProgress, PROGRESS_INTERVAL,
    QWEN3_FILES, RemoteFile,
};

/// 下载地址的两种形态：ModelScope 仓库入口，或可直接拼接文件名的目录地址。
pub(super) enum DownloadSource {
    ModelScope {
        owner: String,
        repo: String,
        revision: String,
    },
    Base(String),
}

impl DownloadSource {
    pub(super) fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim().trim_end_matches('/');
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            bail!("下载地址需以 http:// 或 https:// 开头");
        }

        // 参考地址形如 https://modelscope.cn/models/{owner}/{repo}/tree/{revision}/{path}
        // 只取仓库信息；文件清单固定为 0.6B-int8，不使用地址里的路径。
        let lower = trimmed.to_ascii_lowercase();
        if let Some(position) = lower.find("modelscope.cn/models/") {
            let tail = &trimmed[position + "modelscope.cn/models/".len()..];
            let mut parts = tail.split('/');
            let owner = parts.next().unwrap_or_default().trim().to_owned();
            let repo = parts
                .next()
                .unwrap_or_default()
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            if !owner.is_empty() && !repo.is_empty() {
                let rest: Vec<&str> = parts.collect();
                let revision = match rest.as_slice() {
                    ["tree", revision, ..] if !revision.is_empty() => (*revision).to_owned(),
                    _ => "master".to_owned(),
                };
                return Ok(Self::ModelScope {
                    owner,
                    repo,
                    revision,
                });
            }
        }

        Ok(Self::Base(trimmed.to_owned()))
    }

    pub(super) fn file_url(&self, file: &RemoteFile) -> String {
        match self {
            Self::ModelScope {
                owner,
                repo,
                revision,
            } => format!(
                "https://modelscope.cn/api/v1/models/{owner}/{repo}/repo?Revision={revision}&FilePath={}",
                file.remote
            ),
            Self::Base(base) => format!("{base}/{}", file.local),
        }
    }
}

/// 逐文件下载到 `models/<默认模型名>/`；已完整存在的文件直接跳过，便于失败后续传。
///
/// 进度与完整性以响应的 `Content-Length` 为准（缺失时回退清单字节数），
/// 避免上游文件更新后因大小不一致而永久失败；取消或读写失败时清理 `.part`。
pub(super) fn download_files(
    app: &AppHandle,
    root: &Path,
    url: &str,
    cancelled: &AtomicBool,
) -> Result<()> {
    let url = if url.trim().is_empty() {
        DEFAULT_DOWNLOAD_URL
    } else {
        url.trim()
    };
    let source = DownloadSource::parse(url)?;
    ensure_root(root)?;

    let target = root.join(DEFAULT_MODEL_NAME);
    fs::create_dir_all(&target)
        .with_context(|| format!("无法创建模型目录：{}", target.display()))?;

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60 * 60))
        .user_agent(concat!("tfy-bgm/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("初始化下载客户端失败")?;

    let expected = total_bytes();
    let mut overall = 0u64;
    for file in &QWEN3_FILES {
        if is_complete(&target.join(file.local), file) {
            overall += file.bytes;
        }
    }

    for (index, file) in QWEN3_FILES.iter().enumerate() {
        if cancelled.load(Ordering::SeqCst) {
            bail!("下载已取消");
        }
        let path = target.join(file.local);
        if is_complete(&path, file) {
            emit_progress(
                app,
                DownloadProgress {
                    file: file.local.to_owned(),
                    index,
                    total_files: QWEN3_FILES.len(),
                    received: file.bytes,
                    file_bytes: file.bytes,
                    overall_received: overall,
                    overall_bytes: expected,
                    done: true,
                    error: None,
                },
            );
            continue;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建目录：{}", parent.display()))?;
        }
        let file_url = source.file_url(file);
        let mut response = client
            .get(&file_url)
            .send()
            .with_context(|| format!("请求下载地址失败：{file_url}"))?;
        if !response.status().is_success() {
            bail!(
                "下载 {} 失败：HTTP {}（{file_url}）",
                file.local,
                response.status()
            );
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        {
            bail!("下载地址返回的是网页而不是模型文件，请填写模型文件目录或可直连的地址");
        }

        let expected_len = response.content_length().unwrap_or(file.bytes);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.local.to_owned());
        let part = path.with_file_name(format!("{file_name}.part"));
        let mut writer =
            File::create(&part).with_context(|| format!("无法写入临时文件：{}", part.display()))?;

        let mut received = 0u64;
        let mut last_emit = Instant::now()
            .checked_sub(PROGRESS_INTERVAL)
            .unwrap_or_else(Instant::now);
        let mut buffer = vec![0u8; 256 * 1024];
        let mut copy_error: Option<anyhow::Error> = None;
        loop {
            if cancelled.load(Ordering::SeqCst) {
                copy_error = Some(anyhow!("下载已取消"));
                break;
            }
            let read = match response.read(&mut buffer) {
                Ok(read) => read,
                Err(error) => {
                    copy_error = Some(anyhow!("下载 {} 失败：{error}", file.local));
                    break;
                }
            };
            if read == 0 {
                break;
            }
            if let Err(error) = writer.write_all(&buffer[..read]) {
                copy_error = Some(anyhow!("写入 {} 失败：{error}", part.display()));
                break;
            }
            received += read as u64;
            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                emit_progress(
                    app,
                    DownloadProgress {
                        file: file.local.to_owned(),
                        index,
                        total_files: QWEN3_FILES.len(),
                        received,
                        file_bytes: expected_len,
                        overall_received: overall + received,
                        overall_bytes: expected,
                        done: false,
                        error: None,
                    },
                );
                last_emit = Instant::now();
            }
        }
        writer.flush().ok();
        drop(writer);

        if let Some(error) = copy_error {
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        if received == 0 {
            let _ = fs::remove_file(&part);
            bail!("下载 {} 失败：服务器返回空文件", file.local);
        }
        if expected_len > 0 && received < expected_len {
            let _ = fs::remove_file(&part);
            bail!(
                "下载 {} 不完整：已收到 {} 字节，期望 {} 字节",
                file.local,
                received,
                expected_len
            );
        }
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        fs::rename(&part, &path)
            .with_context(|| format!("无法保存模型文件：{}", path.display()))?;

        overall += received;
        emit_progress(
            app,
            DownloadProgress {
                file: file.local.to_owned(),
                index,
                total_files: QWEN3_FILES.len(),
                received,
                file_bytes: expected_len,
                overall_received: overall,
                overall_bytes: expected,
                done: true,
                error: None,
            },
        );
    }

    let missing = missing_components(&target);
    if !missing.is_empty() {
        bail!("下载完成后仍缺少构件：{}", missing.join("、"));
    }
    Ok(())
}

pub(super) fn emit_progress(app: &AppHandle, progress: DownloadProgress) {
    let _ = app.emit(DOWNLOAD_EVENT, progress);
}

pub(super) fn is_complete(path: &Path, file: &RemoteFile) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() == file.bytes)
        .unwrap_or(false)
}
