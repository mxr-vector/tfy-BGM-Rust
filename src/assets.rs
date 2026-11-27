//! 内嵌图标、封套与静态壁纸资源。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

use crate::icons;

pub const BRAND_MARK: &[u8] = include_bytes!("../assets/branding/brand-mark.png");
pub const BUILTIN_WALLPAPER: &[u8] = include_bytes!("../assets/bg/雪山云海.jpg");

#[cfg(target_os = "macos")]
pub const MENU_BAR_TEMPLATE: &[u8] = include_bytes!("../assets/branding/menu-bar-template.pdf");

const SLEEVES: &[(&str, &str, &[u8])] = &[
    (
        "cheerful",
        "sleeves/cheerful.jpg",
        include_bytes!("../assets/sleeves/cheerful.jpg"),
    ),
    (
        "solitude",
        "sleeves/solitude.jpg",
        include_bytes!("../assets/sleeves/solitude.jpg"),
    ),
    (
        "memories",
        "sleeves/memories.jpg",
        include_bytes!("../assets/sleeves/memories.jpg"),
    ),
    (
        "epic",
        "sleeves/epic.jpg",
        include_bytes!("../assets/sleeves/epic.jpg"),
    ),
    (
        "gufeng",
        "sleeves/gufeng.jpg",
        include_bytes!("../assets/sleeves/gufeng.jpg"),
    ),
    (
        "country",
        "sleeves/country.jpg",
        include_bytes!("../assets/sleeves/country.jpg"),
    ),
];

pub fn sleeve_path(category: &str) -> Option<&'static str> {
    SLEEVES
        .iter()
        .find(|(id, _, _)| *id == category)
        .map(|(_, path, _)| *path)
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == "brand/mark.png" {
            return Ok(Some(Cow::Borrowed(BRAND_MARK)));
        }
        if let Some((_, _, bytes)) = SLEEVES.iter().find(|(_, asset, _)| *asset == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
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
