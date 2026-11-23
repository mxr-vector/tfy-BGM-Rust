//! 逐帧读取的软件视频壁纸后备路径。

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use gpui::RenderImage;
use image::{Frame, RgbaImage, imageops};

pub struct VideoStream {
    child: Child,
    request: SyncSender<u32>,
    frames: Receiver<Result<Arc<RenderImage>, String>>,
    pending: bool,
    waiting_since: Option<Instant>,
}

impl VideoStream {
    pub fn open(path: &Path) -> Result<Self, String> {
        let ffmpeg = bgm_core::loudness::ffmpeg_path().ok_or("视频解码回退需要 ffmpeg")?;
        let mut child = Command::new(ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                "2",
                "-protocol_whitelist",
                "file,pipe",
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(path)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-sn",
                "-filter_threads",
                "1",
                "-vf",
                "fps=25,scale='min(960,iw)':'min(600,ih)':force_original_aspect_ratio=decrease",
                "-c:v",
                "ppm",
                "-f",
                "image2pipe",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("无法启动视频解码：{error}"))?;
        let output = child.stdout.take().expect("视频解码管道启用 stdout");
        let (request, commands) = mpsc::sync_channel(1);
        let (frames_tx, frames) = mpsc::sync_channel(1);
        let stream = Self {
            child,
            request,
            frames,
            pending: false,
            waiting_since: None,
        };
        std::thread::Builder::new()
            .name("wallpaper-decode".into())
            .spawn(move || {
                let mut reader = BufReader::new(output);
                while let Ok(blur) = commands.recv() {
                    let frame = read_frame(&mut reader, blur);
                    let failed = frame.is_err();
                    if frames_tx.send(frame).is_err() || failed {
                        break;
                    }
                }
            })
            .map_err(|error| format!("无法创建视频解码线程：{error}"))?;
        Ok(stream)
    }

    pub fn poll(&mut self, active: bool, blur: u32) -> Result<Option<Arc<RenderImage>>, String> {
        if !active {
            self.waiting_since = None;
            return Ok(None);
        }
        let image = match self.frames.try_recv() {
            Ok(frame) => {
                self.pending = false;
                Some(frame?)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => return Err("无法读取 MP4 视频画面".into()),
        };
        if !self.pending {
            self.request
                .try_send(blur)
                .map_err(|_| "视频解码线程已停止")?;
            self.pending = true;
            self.waiting_since = Some(Instant::now());
        }
        if self
            .waiting_since
            .get_or_insert_with(Instant::now)
            .elapsed()
            >= Duration::from_secs(5)
        {
            return Err("读取 MP4 视频画面超时".into());
        }
        Ok(image)
    }
}

impl Drop for VideoStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_frame(reader: &mut impl BufRead, blur: u32) -> Result<Arc<RenderImage>, String> {
    let mut magic = String::new();
    let mut size = String::new();
    let mut range = String::new();
    for line in [&mut magic, &mut size, &mut range] {
        if reader
            .take(64)
            .read_line(line)
            .map_err(|error| error.to_string())?
            == 0
        {
            return Err("无法读取 MP4 视频画面".into());
        }
    }
    let mut dimensions = size.split_ascii_whitespace();
    let width: u32 = dimensions
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let height: u32 = dimensions
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if magic.trim() != "P6"
        || range.trim() != "255"
        || !(1..=960).contains(&width)
        || !(1..=600).contains(&height)
    {
        return Err("视频帧格式无效".into());
    }
    let mut rgb = vec![0; (width * height * 3) as usize];
    reader
        .read_exact(&mut rgb)
        .map_err(|error| format!("视频帧不完整：{error}"))?;
    let bgra = rgb
        .chunks_exact(3)
        .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255])
        .collect();
    let mut frame = RgbaImage::from_raw(width, height, bgra).ok_or("视频帧尺寸无效")?;
    if blur > 0 {
        frame = imageops::fast_blur(&frame, blur as f32);
    }
    Ok(Arc::new(RenderImage::new([Frame::new(frame)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_frames_loop_and_hidden_state_does_not_advance_them() {
        let Some(ffmpeg) = bgm_core::loudness::ffmpeg_path() else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("wallpaper.mp4");
        let status = Command::new(ffmpeg)
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x96:rate=25:duration=0.32",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());

        let mut video = VideoStream::open(&source).unwrap();
        let mut next = |blur| {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                if let Some(frame) = video.poll(true, blur).unwrap() {
                    break frame;
                }
                assert!(Instant::now() < deadline, "视频帧读取超时");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let first = next(0);
        let second = next(0);
        assert_ne!(first.as_bytes(0), second.as_bytes(0));
        for _ in 2..8 {
            next(0);
        }
        assert_eq!(next(0).as_bytes(0), first.as_bytes(0));
        for _ in 0..3 {
            assert!(video.poll(false, 0).unwrap().is_none());
            std::thread::sleep(Duration::from_millis(20));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(frame) = video.poll(true, 3).unwrap() {
                assert_eq!(frame.as_bytes(0), second.as_bytes(0));
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn rejects_incomplete_frames_and_keeps_bgra_order() {
        let mut valid = &b"P6\n1 1\n255\n\xff\x80\x01"[..];
        assert_eq!(
            read_frame(&mut valid, 0).unwrap().as_bytes(0),
            Some([1, 128, 255, 255].as_slice())
        );
        let mut truncated = &b"P6\n1 1\n255\n\x10"[..];
        assert!(read_frame(&mut truncated, 0).is_err());
        let mut oversized = &b"P6\n2000 1000\n255\n"[..];
        assert!(read_frame(&mut oversized, 0).is_err());
    }
}
