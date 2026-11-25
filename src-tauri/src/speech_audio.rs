use std::path::Path;

use anyhow::{Context, Result, bail};
use bgm_core::{loudness::ffmpeg_path, process::quiet_command};

pub const REFERENCE_RATE: u32 = 16_000;

pub fn to_asr_wav(source: &Path, target: &Path) -> Result<()> {
    convert(source, target, None, None, REFERENCE_RATE)
}

fn convert(
    source: &Path,
    target: &Path,
    filter: Option<&str>,
    limit: Option<&str>,
    rate: u32,
) -> Result<()> {
    let ffmpeg = ffmpeg_path().context("未检测到 ffmpeg，请安装 ffmpeg 或设置 TFY_FFMPEG")?;
    let mut command = quiet_command(&ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(source)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            &rate.to_string(),
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ]);
    if let Some(filter) = filter {
        command.args(["-af", filter]);
    }
    if let Some(limit) = limit {
        command.args(["-t", limit]);
    }
    let output = command.arg(target).output().context("无法启动 ffmpeg")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message: String = stderr.trim().chars().take(400).collect();
        bail!("音频转换失败：{message}");
    }
    if std::fs::metadata(target)?.len() <= 44 {
        bail!("转换后的音频为空");
    }
    Ok(())
}

pub fn prepare_reference(bytes: &[u8]) -> Result<Vec<u8>> {
    // Decode slightly past the ceiling so validation rejects rather than truncates long clips.
    normalize_reference(bytes, "10.1")
}

pub fn prepare_builtin_reference(bytes: &[u8]) -> Result<Vec<u8>> {
    normalize_reference(bytes, "10")
}

fn normalize_reference(bytes: &[u8], limit: &str) -> Result<Vec<u8>> {
    let source = tempfile::NamedTempFile::new()?;
    std::fs::write(source.path(), bytes)?;
    let target = tempfile::NamedTempFile::new()?;
    convert(
        source.path(),
        target.path(),
        Some("loudnorm=I=-20:TP=-1.5:LRA=11"),
        Some(limit),
        REFERENCE_RATE,
    )?;
    let result = std::fs::read(target.path())?;
    read_reference(&result)?;
    Ok(result)
}

pub fn read_reference(bytes: &[u8]) -> Result<Vec<f32>> {
    if bytes.len() > 330_000 {
        bail!("参考音频时长必须在 3–10 秒之间");
    }
    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), bytes)?;
    let wave = read_wave(file.path())?;
    if wave.sample_rate() != REFERENCE_RATE as i32
        || !(48_000..=160_000).contains(&wave.samples().len())
    {
        bail!("参考音频须先转换为 16 kHz 单声道，时长须在 3–10 秒之间");
    }
    let peak = wave
        .samples()
        .iter()
        .fold(0.0_f32, |peak, &value| peak.max(value.abs()));
    if peak < 0.001 {
        bail!("参考音频中没有可用声音");
    }
    Ok(wave.samples().to_vec())
}

fn read_wave(path: &Path) -> Result<sherpa_onnx::Wave> {
    sherpa_onnx::Wave::read(path.to_str().context("无效音频路径")?)
        .context("无法解析单声道 PCM WAV")
}

pub fn fit_clip(samples: &[f32], slot: usize, rate: u32) -> Result<Vec<f32>> {
    if slot == 0 || samples.len() > slot.saturating_mul(3) {
        bail!("所需语速超过 3 倍");
    }
    let source = tempfile::NamedTempFile::new()?;
    let target = tempfile::NamedTempFile::new()?;
    crate::separation::write_wav16_slices(source.path(), &[samples], rate)?;
    let speed = samples.len() as f64 / slot as f64;
    for factor in [1.02, 1.10, 1.25] {
        let tempo = (speed * factor).sqrt();
        convert(
            source.path(),
            target.path(),
            Some(&format!("atempo={tempo:.8},atempo={tempo:.8}")),
            None,
            rate,
        )?;
        let wave = read_wave(target.path())?;
        if wave.samples().len() <= slot {
            return Ok(wave.samples().to_vec());
        }
    }
    bail!("保音高变速后仍超出字幕时槽")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_references_fit_the_supported_window() {
        if ffmpeg_path().is_none() {
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../models/voices");
        for name in ["keai", "lulu", "manbo", "qingnian", "roucheng", "weiketuo"] {
            let bytes = std::fs::read(root.join(format!("{name}.wav"))).unwrap();
            let normalized =
                prepare_builtin_reference(&bytes).unwrap_or_else(|err| panic!("{name}: {err:#}"));
            assert!((48_000..=160_000).contains(&read_reference(&normalized).unwrap().len()));
        }
    }

    #[test]
    fn reference_validates_duration_and_converts_rate() {
        if ffmpeg_path().is_none() {
            return;
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        for seconds in [2, 4, 11] {
            let samples: Vec<_> = (0..24_000 * seconds)
                .map(|i| (i as f32 * 0.11).sin() * 0.2)
                .collect();
            crate::separation::write_wav16_slices(file.path(), &[&samples], 24_000).unwrap();
            let result = prepare_reference(&std::fs::read(file.path()).unwrap());
            assert_eq!(result.is_ok(), seconds == 4, "{result:?}");
            if let Ok(bytes) = result {
                assert_eq!(read_reference(&bytes).unwrap().len(), 64_000);
            }
        }
    }
}
