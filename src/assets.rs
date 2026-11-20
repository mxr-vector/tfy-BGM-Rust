//! 编译期内嵌 SVG 资源，运行时无需依赖仓库中的前端目录。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

use crate::icons;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
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
