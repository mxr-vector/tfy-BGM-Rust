//! 单声道 PCM WAV 读写。

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::num::{NonZeroU16, NonZeroU32};
use std::path::Path;

use rodio::{Decoder, Source, source::UniformSourceIterator};

pub fn reference(path: &Path) -> Result<(u32, Vec<f32>), String> {
    let (rate, audio) = open_mono(path, "参考音频")?;
    if rate > 192_000 {
        return Err("参考音频采样率不受支持".into());
    }
    let limit = rate as usize * 15;
    let mono: Vec<_> = audio.take(limit + 1).collect();
    if mono.is_empty() {
        return Err("参考音频没有可读取的采样，请更换音频或重新录制".into());
    }
    if mono.len() > limit {
        return Err("参考音频超过 15 秒，请缩短音频或重新录制".into());
    }
    Ok((rate, mono))
}

pub fn read_generated(path: &Path, rate: u32, max_frames: usize) -> Result<Vec<f32>, String> {
    let (actual_rate, audio) = open_mono(path, "生成音频")?;
    if actual_rate != rate {
        return Err(format!(
            "生成音频的采样率为 {actual_rate} Hz，预期为 {rate} Hz"
        ));
    }
    let mono: Vec<_> = audio.take(max_frames.saturating_add(1)).collect();
    if mono.is_empty() {
        return Err("生成音频没有可读取的采样".into());
    }
    if mono.len() > max_frames {
        return Err("生成音频超过预期的时长".into());
    }
    Ok(mono)
}

fn open_mono(path: &Path, label: &str) -> Result<(u32, impl Iterator<Item = f32>), String> {
    let file = File::open(path).map_err(|error| format!("无法打开{label}：{error}"))?;
    let audio = Decoder::try_from(file).map_err(|error| format!("无法读取{label}：{error}"))?;
    let rate = audio.sample_rate();
    Ok((
        rate.get(),
        UniformSourceIterator::new(audio, NonZeroU16::MIN, rate),
    ))
}

pub fn write(path: &Path, rate: u32, mono: &[f32], normalize: bool) -> Result<(), String> {
    let rate = NonZeroU32::new(rate).ok_or("采样率无效")?;
    let samples = u32::try_from(mono.len()).map_err(|_| "WAV 音频体积超限")?;
    let bytes = samples.checked_mul(2).ok_or("WAV 音频体积超限")?;
    let riff = bytes.checked_add(36).ok_or("WAV 音频体积超限")?;
    let mut writer = BufWriter::new(File::create(path).map_err(|error| error.to_string())?);
    write_header(&mut writer, rate.get(), riff, bytes).map_err(|error| error.to_string())?;
    let gain = if normalize {
        crate::diagnostics::speech_gain(mono, rate.get())
    } else {
        1.0
    };
    for chunk in mono.chunks(4096) {
        let mut buffer = [0u8; 8192];
        for (index, &sample) in chunk.iter().enumerate() {
            let value = (sample * gain).clamp(-1.0, 1.0);
            let bytes = ((value * f32::from(i16::MAX)).round() as i16).to_le_bytes();
            buffer[index * 2..index * 2 + 2].copy_from_slice(&bytes);
        }
        writer
            .write_all(&buffer[..chunk.len() * 2])
            .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn write_header(output: &mut impl Write, rate: u32, riff: u32, data: u32) -> std::io::Result<()> {
    output.write_all(b"RIFF")?;
    output.write_all(&riff.to_le_bytes())?;
    output.write_all(b"WAVEfmt ")?;
    output.write_all(&16u32.to_le_bytes())?;
    output.write_all(&1u16.to_le_bytes())?;
    output.write_all(&1u16.to_le_bytes())?;
    output.write_all(&rate.to_le_bytes())?;
    output.write_all(&(rate * 2).to_le_bytes())?;
    output.write_all(&2u16.to_le_bytes())?;
    output.write_all(&16u16.to_le_bytes())?;
    output.write_all(b"data")?;
    output.write_all(&data.to_le_bytes())
}

pub fn export(source: &Path, destination: &Path) -> Result<(), String> {
    let directory = destination.parent().ok_or("无效的保存目录")?;
    let temporary =
        tempfile::NamedTempFile::new_in(directory).map_err(|error| error.to_string())?;
    fs::copy(source, temporary.path()).map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(destination)
        .map_err(|error| error.to_string())?;
    Ok(())
}
