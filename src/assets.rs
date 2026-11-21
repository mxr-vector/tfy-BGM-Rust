//! 编译期内嵌 SVG 与内置壁纸资源，运行时无需依赖仓库中的前端目录。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

use crate::icons;

/// 内置静态壁纸（雪山云海），与主线 `assets/bg/2.jpg` 同源；动态壁纸为 mp4，GPUI 暂不支持。
const BUILTIN_WALLPAPER: &[u8] = include_bytes!("../assets/bg/2.jpg");

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "bg/2.jpg" {
            return Ok(Some(Cow::Borrowed(BUILTIN_WALLPAPER)));
        }
        let name = path
            .strip_prefix("icons/")
            .and_then(|path| path.strip_suffix(".svg"));
        let Some(name) = name else {
            return Ok(None);
        };
        Ok(Some(Cow::Owned(icons::source(name).into_bytes())))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
