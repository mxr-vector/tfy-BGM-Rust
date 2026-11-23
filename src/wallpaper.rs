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

pub fn builtin_video() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let resources = if let Some(contents) = executable
        .parent()?
        .parent()
        .filter(|path| path.ends_with("Contents"))
    {
        contents.join("Resources")
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets")
    };
    let path = resources.join("bg/1.mp4");
    path.is_file().then_some(path)
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
            assert_eq!(image.size(0), gpui::size(1920.into(), 1012.into()));
            assert_eq!(image.as_bytes(0).unwrap().len(), 1920 * 1012 * 4);
        }
    }
}
