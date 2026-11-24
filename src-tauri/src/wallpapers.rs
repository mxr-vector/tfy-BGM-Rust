//! 界面壁纸：内置资源定位、自定义文件校验与 asset 协议授权。
//!
//! 打包后壁纸位于 `$RESOURCE/assets/bg/`，开发模式由 `tauri-build` 复制到
//! `target/<profile>/assets/bg/`，两者都通过 `resource_dir` 命中；仓库路径作为
//! 最后兜底，方便直接 `cargo run` 调试。
//! 图片使用 asset 协议；Linux 视频改用本机 HTTP Range 流，以规避 WebKitGTK
//! 无法通过 `asset://` 打开 `<video>` 的限制。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bgm_core::prefs::is_wallpaper_path;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Manager};

#[cfg(target_os = "linux")]
use std::net::{Ipv4Addr, TcpListener};
#[cfg(target_os = "linux")]
use tokio::fs::File;
#[cfg(target_os = "linux")]
use tokio::io::{AsyncReadExt, AsyncSeekExt};
#[cfg(target_os = "linux")]
use tokio_util::io::ReaderStream;

/// (id, 名称, 类型, 壁纸文件, 缩略图文件)
pub const BUILTIN_WALLPAPERS: [(&str, &str, &str, &str, &str); 2] = [
    ("builtin-1", "幻彩流光", "video", "1.mp4", "1.thumb.jpg"),
    ("builtin-2", "雪山云海", "image", "2.jpg", "2.jpg"),
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperAsset {
    pub id: String,
    pub label: String,
    pub kind: String,
    /// 传给前端 `convertFileSrc` 的本地路径。
    pub path: String,
    pub thumb: String,
}

/// 自定义壁纸选择结果；`label` 只用于界面提示，不参与持久化。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WallpaperPick {
    pub path: String,
    pub kind: String,
    pub label: String,
}

/// 按扩展名区分静态图片与视频；不支持的格式统一按图片处理（由调用方过滤）。
pub fn kind_of(path: &Path) -> &'static str {
    let is_video = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"));
    if is_video { "video" } else { "image" }
}

/// 自定义壁纸只接受 jpg / jpeg / png / webp / mp4。
pub fn is_supported(path: &Path) -> bool {
    path.to_str().is_some_and(is_wallpaper_path)
}

pub fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Linux WebKitGTK 的视频媒体服务。
///
/// WebKitGTK 当前无法通过 `asset://` 自定义协议打开 `<video>`，因此只在本机临时端口上
/// 提供带 Range 的 HTTP 流。地址中包含随机令牌与文件 id，不暴露磁盘路径，也不依赖
/// 「开放接口」开关或其 token 配置。
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
        #[cfg(target_os = "linux")]
        {
            Self::start_linux()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::offline()
        }
    }

    #[cfg(target_os = "linux")]
    fn start_linux() -> Self {
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
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        {
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
    }

    /// 返回已登记文件的 HTTP 地址；未登记或服务不可用时返回 None。
    pub fn url_for(&self, path: &Path) -> Option<String> {
        let base_url = self.inner.base_url.as_ref()?;
        let canonical = std::fs::canonicalize(path).ok()?;
        let id = self.inner.files.lock().by_path.get(&canonical).cloned()?;
        Some(format!("{base_url}/wallpaper/{}/{id}", self.inner.token))
    }
}

#[cfg(target_os = "linux")]
fn random_hex(len: usize) -> Result<String> {
    let mut bytes = vec![0_u8; len];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("随机数生成失败：{error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(target_os = "linux")]
async fn wallpaper_media(
    State(server): State<WallpaperMediaServer>,
    AxumPath((supplied_token, id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if supplied_token != server.inner.token {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = server.inner.files.lock().by_id.get(&id).cloned();
    let Some(path) = path.filter(|path| kind_of(path) == "video") else {
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
#[cfg(target_os = "linux")]
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

pub fn allow_wallpaper_file(app: &AppHandle, path: &Path) -> Result<()> {
    app.asset_protocol_scope()
        .allow_file(path)
        .with_context(|| format!("无法授权壁纸文件：{}", path.display()))?;
    if let Some(media) = app.try_state::<WallpaperMediaServer>() {
        media.register(path)?;
    }
    Ok(())
}

fn candidates(app: &AppHandle, file_name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        paths.push(resource_dir.join("assets").join("bg").join(file_name));
    }
    // 开发模式兜底：资源尚未复制进 target 时直接读仓库文件。
    paths.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("assets")
            .join("bg")
            .join(file_name),
    );
    paths
}

pub fn resolve_asset(app: &AppHandle, file_name: &str) -> Option<PathBuf> {
    candidates(app, file_name)
        .into_iter()
        .find(|path| path.is_file())
}

/// 返回仓库内置的图片 / 视频壁纸，并顺带授权资源文件访问。
pub fn builtin_catalog(app: &AppHandle) -> Vec<WallpaperAsset> {
    BUILTIN_WALLPAPERS
        .iter()
        .filter_map(|(id, label, kind, file, thumb_file)| {
            let path = resolve_asset(app, file)?;
            // 缩略图缺失时留空，前端会退回类型图标而不是显示破图。
            let thumb = resolve_asset(app, thumb_file);
            allow_wallpaper_file(app, &path).ok()?;
            if let Some(thumb) = &thumb {
                allow_wallpaper_file(app, thumb).ok()?;
            }
            Some(WallpaperAsset {
                id: (*id).to_owned(),
                label: (*label).to_owned(),
                kind: (*kind).to_owned(),
                path: path.to_string_lossy().into_owned(),
                thumb: thumb
                    .map(|thumb| thumb.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// 启动时授权上次保存的自定义壁纸，避免首屏加载被 asset 协议拦下。
/// 文件缺失或格式不再受支持时静默跳过，由前端提示用户重新选择。
pub fn prime_custom(app: &AppHandle, path: Option<&str>) {
    let Some(path) = path else { return };
    let path = Path::new(path);
    if path.is_file() && is_supported(path) {
        let _ = allow_wallpaper_file(app, path);
    }
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
