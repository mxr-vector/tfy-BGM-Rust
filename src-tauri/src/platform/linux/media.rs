//! Linux WebKitGTK 的视频壁纸媒体服务。
//!
//! WebKitGTK 无法通过 `asset://` 自定义协议打开 `<video>`，因此只在本机临时端口上
//! 提供带 Range 的 HTTP 流。地址中包含随机令牌与文件 id，不暴露磁盘路径，也不依赖
//! 「开放接口」开关或其 token 配置。

use std::collections::HashMap;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use bgm_core::prefs::wallpaper_kind;
use parking_lot::Mutex;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

#[derive(Clone)]
pub struct WallpaperMediaServer {
    inner: Arc<MediaInner>,
}

struct MediaInner {
    token: String,
    base_url: Option<String>,
    files: Mutex<MediaFiles>,
}

#[derive(Default)]
struct MediaFiles {
    by_id: HashMap<String, PathBuf>,
    by_path: HashMap<PathBuf, String>,
}

impl WallpaperMediaServer {
    pub fn start() -> Self {
        let token = match random_hex(24) {
            Ok(token) => token,
            Err(error) => {
                eprintln!("[wallpaper] 无法生成媒体令牌：{error:#}");
                return Self::offline();
            }
        };
        let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("[wallpaper] 无法启动视频媒体服务：{error}");
                return Self::offline();
            }
        };
        if let Err(error) = listener.set_nonblocking(true) {
            eprintln!("[wallpaper] 无法设置视频媒体套接字：{error}");
            return Self::offline();
        }
        let port = match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(error) => {
                eprintln!("[wallpaper] 无法读取视频媒体端口：{error}");
                return Self::offline();
            }
        };
        let server = Self {
            inner: Arc::new(MediaInner {
                token,
                base_url: Some(format!("http://127.0.0.1:{port}")),
                files: Mutex::new(MediaFiles::default()),
            }),
        };
        let router = Router::new()
            .route("/wallpaper/{token}/{id}", get(wallpaper_media))
            .with_state(server.clone());
        tauri::async_runtime::spawn(async move {
            match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => {
                    if let Err(error) = axum::serve(listener, router).await {
                        eprintln!("[wallpaper] 视频媒体服务退出：{error}");
                    }
                }
                Err(error) => eprintln!("[wallpaper] 无法接管视频媒体套接字：{error}"),
            }
        });
        server
    }

    fn offline() -> Self {
        Self {
            inner: Arc::new(MediaInner {
                token: String::new(),
                base_url: None,
                files: Mutex::new(MediaFiles::default()),
            }),
        }
    }

    /// 登记已授权的壁纸文件；同一文件重复登记时复用地址。
    pub fn register(&self, path: &Path) -> Result<()> {
        if self.inner.base_url.is_none() {
            return Ok(());
        }
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("无法解析壁纸文件：{}", path.display()))?;
        if !canonical.is_file() {
            bail!("壁纸文件不存在：{}", path.display());
        }
        let mut files = self.inner.files.lock();
        if files.by_path.contains_key(&canonical) {
            return Ok(());
        }
        let id = random_hex(16)?;
        files.by_id.insert(id.clone(), canonical.clone());
        files.by_path.insert(canonical, id);
        Ok(())
    }

    /// 返回已登记文件的 HTTP 地址；服务不可用或文件尚未授权时返回错误。
    pub fn url_for(&self, path: &Path) -> Result<Option<String>> {
        let base_url = self
            .inner
            .base_url
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("视频壁纸服务不可用"))?;
        let canonical = std::fs::canonicalize(path)
            .with_context(|| format!("无法解析壁纸文件：{}", path.display()))?;
        let id = self
            .inner
            .files
            .lock()
            .by_path
            .get(&canonical)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("视频壁纸文件尚未授权"))?;
        Ok(Some(format!(
            "{base_url}/wallpaper/{}/{id}",
            self.inner.token
        )))
    }
}

fn random_hex(len: usize) -> Result<String> {
    let mut bytes = vec![0_u8; len];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("随机数生成失败：{error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

async fn wallpaper_media(
    State(server): State<WallpaperMediaServer>,
    AxumPath((supplied_token, id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if supplied_token != server.inner.token {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = server.inner.files.lock().by_id.get(&id).cloned();
    let Some(path) = path.filter(|path| wallpaper_kind(path) == "video") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) => {
            eprintln!("[wallpaper] 无法读取 {}：{error}", path.display());
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let len = match file.metadata().await {
        Ok(metadata) => metadata.len(),
        Err(error) => {
            eprintln!("[wallpaper] 无法读取壁纸元数据：{error}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let (start, length, partial) = match range {
        Some(value) => match parse_byte_range(value, len) {
            Ok((start, length)) => (start, length, true),
            Err(()) => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .body(Body::empty())
                    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
            }
        },
        None => (0, len, false),
    };
    if let Err(error) = file.seek(std::io::SeekFrom::Start(start)).await {
        eprintln!("[wallpaper] 无法定位壁纸数据：{error}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, "video/mp4")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Range")
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Content-Range, Content-Length, Accept-Ranges",
        );
    if partial {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{len}", start + length - 1),
        );
    }
    builder
        .body(Body::from_stream(ReaderStream::new(file.take(length))))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// 解析单段 `Range: bytes=start-end`；支持开放区间与后缀区间。
fn parse_byte_range(value: &str, len: u64) -> std::result::Result<(u64, u64), ()> {
    if len == 0 {
        return Err(());
    }
    let (start, end) = value
        .strip_prefix("bytes=")
        .ok_or(())?
        .split_once('-')
        .ok_or(())?;
    let (start, end) = if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        (len.saturating_sub(suffix), len - 1)
    } else {
        let start = start.parse::<u64>().map_err(|_| ())?;
        if start >= len {
            return Err(());
        }
        let end = if end.is_empty() {
            len - 1
        } else {
            end.parse::<u64>().map_err(|_| ())?
        };
        (start, end.min(len - 1))
    };
    if end < start {
        return Err(());
    }
    Ok((start, end - start + 1))
}

#[cfg(test)]
mod tests {
    use super::parse_byte_range;

    #[test]
    fn parses_closed_open_and_suffix_ranges() {
        assert_eq!(parse_byte_range("bytes=0-1445", 10_000), Ok((0, 1_446)));
        assert_eq!(parse_byte_range("bytes=9000-", 10_000), Ok((9_000, 1_000)));
        assert_eq!(parse_byte_range("bytes=-500", 10_000), Ok((9_500, 500)));
        assert_eq!(parse_byte_range("bytes=0-", 10_000), Ok((0, 10_000)));
    }

    #[test]
    fn rejects_invalid_or_empty_ranges() {
        for value in [
            "items=0-10",
            "bytes=20-10",
            "bytes=10000-",
            "bytes=-0",
            "bytes=0-1,4-5",
        ] {
            assert_eq!(parse_byte_range(value, 10_000), Err(()), "{value}");
        }
        assert_eq!(parse_byte_range("bytes=0-1", 0), Err(()));
    }
}
