//! 用 ffmpeg 把任意音频解码为分离引擎需要的 44.1 kHz 双声道 PCM。

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::loudness::ffmpeg_path;

use super::INPUT_CHANNELS;

/// 用 ffmpeg 把任意源音频解码为 44.1 kHz 双声道浮点 PCM。
pub(super) fn decode_input(source: &Path) -> Result<Vec<Vec<f32>>> {
    let ffmpeg = ffmpeg_path().ok_or_else(|| {
        anyhow!("未检测到 ffmpeg，无法执行人声分离。请安装 ffmpeg 或设置 TFY_FFMPEG 环境变量后重试")
    })?;
    let raw = std::env::temp_dir().join(format!(
        "tfy-bgm-separation-{}-{}.f32",
        std::process::id(),
        now_ms()
    ));
    let output = bgm_core::process::quiet_command(&ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(source)
        .args([
            "-vn",
            "-ac",
            "2",
            "-ar",
            "44100",
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
        ])
        .arg(&raw)
        .output()
        .with_context(|| format!("无法启动 ffmpeg 解码：{}", ffmpeg.display()))?;
    if !output.status.success() {
        let _ = fs::remove_file(&raw);
        bail!("音频解码失败：{}", stderr_tail(&output.stderr));
    }

    // 分块读取解码结果，避免「整份原始字节 + 左右声道」三份音频同时驻留。
    let decoded = read_decoded(&raw);
    let _ = fs::remove_file(&raw);
    decoded
}

pub(super) fn read_decoded(path: &Path) -> Result<Vec<Vec<f32>>> {
    const FRAME_BYTES: usize = std::mem::size_of::<f32>() * INPUT_CHANNELS;
    let file = File::open(path).with_context(|| format!("无法读取解码结果：{}", path.display()))?;
    let total = file
        .metadata()
        .map(|meta| meta.len())
        .with_context(|| format!("无法读取解码结果大小：{}", path.display()))?;
    if total == 0 || !total.is_multiple_of(FRAME_BYTES as u64) {
        bail!("音频解码失败：解码结果为空或长度异常");
    }
    let frames = usize::try_from(total / FRAME_BYTES as u64)
        .map_err(|_| anyhow!("音频过长，解码结果超出可处理范围"))?;
    if frames > i32::MAX as usize {
        bail!("音频过长（{frames} 帧），超出分离引擎的可处理范围");
    }

    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut chunk = vec![0u8; (1 << 20) / FRAME_BYTES * FRAME_BYTES];
    loop {
        let read = reader
            .read(&mut chunk)
            .with_context(|| format!("无法读取解码结果：{}", path.display()))?;
        if read == 0 {
            break;
        }
        if !read.is_multiple_of(FRAME_BYTES) {
            bail!("音频解码失败：解码结果长度异常");
        }
        let (frames, _) = chunk[..read].as_chunks::<FRAME_BYTES>();
        for frame in frames {
            left.push(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]));
            right.push(f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]));
        }
    }
    if left.len() != frames {
        bail!("音频解码失败：解码结果长度与文件大小不一致");
    }
    Ok(vec![left, right])
}

fn stderr_tail(bytes: &[u8]) -> String {
    tail_text(bytes, "（ffmpeg 未输出错误信息）")
}

/// 截取输出末尾 400 个字符；为空时返回调用方给的占位文本。
pub(super) fn tail_text(bytes: &[u8], empty: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return empty.to_owned();
    }
    trimmed
        .chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
