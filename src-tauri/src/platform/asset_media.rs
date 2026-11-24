//! 未提供视频流服务的平台共用实现：直接使用 Tauri 的 asset 协议。
//!
//! Windows（WebView2）与 macOS（WKWebView）都能通过 `asset://` 播放 `<video>`，
//! 因此不需要本地 HTTP 服务；壁纸文件只需经 `asset_protocol_scope().allow_file`
//! 授权即可。

use std::path::Path;

use anyhow::Result;

#[derive(Clone, Copy, Debug, Default)]
pub struct WallpaperMediaServer;

impl WallpaperMediaServer {
    pub fn start() -> Self {
        Self
    }

    /// 无需登记媒体地址：asset 协议授权由调用方完成。
    pub fn register(&self, _path: &Path) -> Result<()> {
        Ok(())
    }

    /// 返回 `Ok(None)` 表示本平台不需要媒体服务，调用方应退回 asset 协议。
    pub fn url_for(&self, _path: &Path) -> Result<Option<String>> {
        Ok(None)
    }
}
