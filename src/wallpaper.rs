//! 静态壁纸准备与视频解码。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{AnyElement, IntoElement, ObjectFit, RenderImage, Styled, StyledImage, img};
use image::{DynamicImage, Frame, GenericImageView, ImageReader, Limits, imageops::FilterType};

mod stream;
#[cfg(target_os = "macos")]
mod video;

enum VideoBackend {
    #[cfg(target_os = "macos")]
    Native(video::Video),
    Software(stream::VideoStream),
}

pub enum VideoFrame {
    #[cfg(target_os = "macos")]
    Surface(core_video::pixel_buffer::CVPixelBuffer),
    Image(Arc<RenderImage>),
}

impl VideoFrame {
    pub fn element(&self, fit: ObjectFit) -> AnyElement {
        match self {
            #[cfg(target_os = "macos")]
            Self::Surface(frame) => gpui::surface(frame.clone())
                .size_full()
                .object_fit(fit)
                .into_any_element(),
            Self::Image(frame) => img(frame.clone())
                .size_full()
                .object_fit(fit)
                .into_any_element(),
        }
    }

    pub fn release(self, cx: &mut gpui::App) {
        match self {
            #[cfg(target_os = "macos")]
            Self::Surface(_) => {}
            Self::Image(frame) => cx.drop_image(frame, None),
        }
    }
}

pub struct Video {
    path: PathBuf,
    backend: VideoBackend,
}

impl Video {
    pub fn open(path: PathBuf, _blur: u32) -> Result<Self, String> {
        if !path.is_file() {
            return Err("找不到视频文件".into());
        }
        #[cfg(target_os = "macos")]
        if let Ok(native) = video::Video::open(&path, _blur) {
            return Ok(Self {
                path,
                backend: VideoBackend::Native(native),
            });
        }
        let stream = stream::VideoStream::open(&path)?;
        Ok(Self {
            path,
            backend: VideoBackend::Software(stream),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn poll(&mut self, active: bool, blur: u32) -> Result<Option<VideoFrame>, String> {
        match &mut self.backend {
            #[cfg(target_os = "macos")]
            VideoBackend::Native(native) => match native.poll(active, blur) {
                Ok(frame) => Ok(frame.map(VideoFrame::Surface)),
                Err(native_error) => {
                    let stream = stream::VideoStream::open(&self.path)
                        .map_err(|error| format!("{native_error}，{error}"))?;
                    self.backend = VideoBackend::Software(stream);
                    Ok(None)
                }
            },
            VideoBackend::Software(stream) => stream
                .poll(active, blur)
                .map(|frame| frame.map(VideoFrame::Image)),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum Asset {
    Embedded,
    Local(PathBuf),
}

#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    pub asset: Asset,
    pub blur: u32,
}

impl Request {
    pub fn prepare(&self) -> Result<Arc<RenderImage>, String> {
        let source = match &self.asset {
            Asset::Embedded => decode_bytes(crate::assets::BUILTIN_WALLPAPER)?,
            Asset::Local(path) => {
                let reader = ImageReader::open(path).map_err(|error| error.to_string())?;
                decode(reader)?
            }
        };
        let (width, height) = source.dimensions();
        let mut pixels = if width > 1920 || height > 1200 {
            source.resize(1920, 1200, FilterType::Triangle)
        } else {
            source
        };
        if self.blur > 0 {
            pixels = pixels.blur(self.blur as f32);
        }
        let mut rgba = pixels.into_rgba8();
        for pixel in rgba.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        Ok(Arc::new(RenderImage::new([Frame::new(rgba)])))
    }
}

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinWallpaper {
    pub id: String,
    pub label: String,
    pub path: PathBuf,
    pub video: bool,
}

pub fn discover_builtin() -> Vec<BuiltinWallpaper> {
    let mut dirs = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(contents) = executable
            .parent()
            .and_then(|dir| dir.parent())
            .filter(|path| path.ends_with("Contents"))
    {
        dirs.push(contents.join("Resources/assets/bg"));
    }
    dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/bg"));
    discover_in_dirs(&dirs)
}

fn discover_in_dirs(dirs: &[PathBuf]) -> Vec<BuiltinWallpaper> {
    let mut assets = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for dir in dirs {
        for item in discover_in_dir(dir) {
            if seen_ids.insert(item.id.clone()) {
                assets.push(item);
            }
        }
    }
    assets
}

fn discover_in_dir(dir: &Path) -> Vec<BuiltinWallpaper> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    let mut stem_counts = std::collections::HashMap::<&str, usize>::new();
    for path in &files {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !supported_builtin(path, name) {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            *stem_counts.entry(stem).or_insert(0) += 1;
        }
    }

    let mut found = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    for path in &files {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !supported_builtin(path, name) {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let base = builtin_id(stem);
        let mut id = base.clone();
        let mut suffix = 2;
        while !seen_ids.insert(id.clone()) {
            id = format!("{base}-{suffix}");
            suffix += 1;
        }
        let label = if stem_counts.get(stem).copied().unwrap_or(0) > 1 {
            name.to_owned()
        } else {
            stem.to_owned()
        };
        found.push(BuiltinWallpaper {
            id,
            label,
            video: is_video(path),
            path: path.clone(),
        });
    }
    found
}

fn supported_builtin(path: &Path, name: &str) -> bool {
    !name.starts_with('.')
        && !name.to_ascii_lowercase().contains(".thumb.")
        && path
            .to_str()
            .is_some_and(bgm_core::prefs::is_wallpaper_path)
}

fn builtin_id(stem: &str) -> String {
    let mut suffix = String::new();
    for ch in stem.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            suffix.push(ch);
        } else if !suffix.ends_with('-') {
            suffix.push('-');
        }
    }
    let suffix = suffix.trim_matches('-');
    if suffix.is_empty() {
        format!("builtin-{:016x}", fnv1a(stem.as_bytes()))
    } else {
        format!("builtin-{suffix}")
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn decode_bytes(bytes: &[u8]) -> Result<DynamicImage, String> {
    let reader = ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    decode(reader)
}

fn decode<R: std::io::BufRead + std::io::Seek>(
    mut reader: ImageReader<R>,
) -> Result<DynamicImage, String> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("无法读取壁纸：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_local_wallpaper_fits_the_display_bitmap_budget() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallpaper.png");
        image::RgbImage::new(2400, 40).save(&path).unwrap();

        let image = Request {
            asset: Asset::Local(path),
            blur: 0,
        }
        .prepare()
        .unwrap();
        assert_eq!(image.size(0), gpui::size(1920.into(), 32.into()));
    }

    #[test]
    fn embedded_wallpaper_fits_the_display_bitmap_budget_with_or_without_blur() {
        for blur in [0, 4] {
            let image = Request {
                asset: Asset::Embedded,
                blur,
            }
            .prepare()
            .unwrap();
            assert_eq!(image.size(0), gpui::size(1920.into(), 1013.into()));
            assert_eq!(image.as_bytes(0).unwrap().len(), 1920 * 1013 * 4);
        }
    }

    #[test]
    fn discovers_builtin_wallpapers_skips_thumbnails_and_disambiguates_stems() {
        let temp = tempfile::tempdir().unwrap();
        let primary = temp.path().join("bundle");
        let fallback = temp.path().join("dev");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&fallback).unwrap();
        std::fs::write(primary.join("幻彩流光.mp4"), b"v").unwrap();
        std::fs::write(primary.join("幻彩流光.thumb.jpg"), b"t").unwrap();
        std::fs::write(primary.join(".hidden.jpg"), b"h").unwrap();
        std::fs::write(primary.join("scene.jpg"), b"a").unwrap();
        std::fs::write(primary.join("scene.png"), b"b").unwrap();
        std::fs::write(fallback.join("scene.jpg"), b"dup").unwrap();
        std::fs::write(fallback.join("雪山云海.jpg"), b"s").unwrap();

        let items = discover_in_dirs(&[primary.clone(), fallback]);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].label, "scene.jpg");
        assert_eq!(items[0].id, "builtin-scene");
        assert_eq!(items[0].path, primary.join("scene.jpg"));
        assert_eq!(items[1].label, "scene.png");
        assert_eq!(items[1].id, "builtin-scene-2");
        assert!(items[2].video);
        assert_eq!(items[2].label, "幻彩流光");
        assert!(items[2].id.starts_with("builtin-"));
        assert_eq!(items[3].label, "雪山云海");
        assert_eq!(
            builtin_id("雪山云海"),
            format!("builtin-{:016x}", fnv1a("雪山云海".as_bytes()))
        );
    }
}
