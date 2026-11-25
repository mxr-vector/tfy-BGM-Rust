use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use bgm_core::{
    loudness::{ExportPreset, ffmpeg_path, normalize_wav},
    process::quiet_command,
};

pub const REFERENCE_RATE: u32 = 16_000;
const MIN_REFERENCE_SAMPLES: usize = 48_000;
const MIN_RECORDED_REFERENCE_SAMPLES: usize = 80_000;
const MAX_REFERENCE_SAMPLES: usize = 240_000;
const MAX_REFERENCE_BYTES: usize = 500_000;
const EDGE_FADE_MS: u32 = 5;
const EDGE_PAD_MS: u32 = 12;
const OUTPUT_PRESET: ExportPreset = ExportPreset {
    id: "tts",
    label: "文生语音",
    description: "单人声优先，保持自然动态并限制真峰值。",
    target_lufs: -18.0,
    true_peak: -1.5,
    loudness_range: 7.0,
};

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
    let normalized = normalize_reference(bytes, "15.1")?;
    read_recorded_reference(&normalized)?;
    Ok(normalized)
}

pub fn builtin_voice_files(root: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("无法打开内置参考音色目录"),
    };
    let mut voices = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && let Ok(name) = entry.file_name().into_string()
            && Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        {
            voices.push(name);
        }
    }
    voices.sort_unstable();
    Ok(voices)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuiltinPrompt {
    end_seconds: f64,
    text: String,
}

pub fn prepare_builtin_reference(path: &Path, bytes: &[u8]) -> Result<(Vec<u8>, Option<String>)> {
    let manifest_path = path.with_file_name("prompts.json");
    let prompt = match std::fs::read(&manifest_path) {
        Ok(bytes) => {
            let mut prompts: std::collections::HashMap<String, BuiltinPrompt> =
                serde_json::from_slice(&bytes).context("无法解析内置音色逐字稿")?;
            prompts.remove(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("无法读取内置音色逐字稿"),
    };
    if let Some(prompt) = prompt {
        let text = prompt.text.trim();
        if !(3.0..=15.0).contains(&prompt.end_seconds) || !valid_reference_text(text) {
            bail!("内置音色逐字稿或参考片段时长无效");
        }
        let audio = normalize_reference(bytes, &prompt.end_seconds.to_string())?;
        Ok((audio, Some(text.to_owned())))
    } else {
        Ok((normalize_reference(bytes, "10")?, None))
    }
}

pub fn valid_reference_text(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= 1000 && !text.contains('\0')
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
    read_reference_with_minimum(bytes, MIN_REFERENCE_SAMPLES)
}

pub fn read_recorded_reference(bytes: &[u8]) -> Result<Vec<f32>> {
    read_reference_with_minimum(bytes, MIN_RECORDED_REFERENCE_SAMPLES)
}

fn read_reference_with_minimum(bytes: &[u8], minimum_samples: usize) -> Result<Vec<f32>> {
    if bytes.len() > MAX_REFERENCE_BYTES {
        bail!("参考音频时长不能超过 15 秒");
    }
    let file = tempfile::NamedTempFile::new()?;
    std::fs::write(file.path(), bytes)?;
    let wave = read_wave(file.path())?;
    if wave.sample_rate() != REFERENCE_RATE as i32 {
        bail!("参考音频须先转换为 16 kHz 单声道");
    }
    if !(minimum_samples..=MAX_REFERENCE_SAMPLES).contains(&wave.samples().len()) {
        let minimum_seconds = minimum_samples / REFERENCE_RATE as usize;
        bail!("参考音频时长须在 {minimum_seconds}–15 秒之间");
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

pub(crate) fn read_wave(path: &Path) -> Result<sherpa_onnx::Wave> {
    sherpa_onnx::Wave::read(path.to_str().context("无效音频路径")?)
        .context("无法解析单声道 PCM WAV")
}

/// 去掉模型首尾过长的静音，并在片段边缘加短淡入淡出，避免拼接爆音。
pub fn polish_segment(samples: &[f32], rate: u32) -> Result<Vec<f32>> {
    ensure!(!samples.is_empty() && rate > 0, "语音片段或采样率无效");
    ensure!(
        samples.iter().all(|sample| sample.is_finite()),
        "语音片段包含无效采样"
    );
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    ensure!(peak > 1e-6, "语音片段接近静音");

    let threshold = peak * 0.015;
    let first = samples
        .iter()
        .position(|sample| sample.abs() >= threshold)
        .context("语音片段没有有效声音")?;
    let last = samples
        .iter()
        .rposition(|sample| sample.abs() >= threshold)
        .context("语音片段没有有效声音")?;
    let pad = rate.saturating_mul(EDGE_PAD_MS) as usize / 1000;
    let start = first.saturating_sub(pad);
    let end = last
        .saturating_add(1)
        .saturating_add(pad)
        .min(samples.len());
    let mut polished = samples[start..end].to_vec();
    if polished.len() < 2 {
        bail!("语音片段过短");
    }
    let fade = (rate.saturating_mul(EDGE_FADE_MS) as usize / 1000).min(polished.len() / 2);
    apply_edge_fades(&mut polished, fade);
    Ok(polished)
}

fn apply_edge_fades(samples: &mut [f32], fade: usize) {
    if fade == 0 {
        return;
    }
    let last = samples.len() - 1;
    for index in 0..fade {
        let phase = (index + 1) as f32 / fade as f32;
        let gain = 0.5 - 0.5 * (std::f32::consts::PI * phase).cos();
        samples[index] *= gain;
        samples[last - index] *= gain;
    }
    samples[0] = 0.0;
    samples[last] = 0.0;
}

/// 句末停顿只用于自然衔接，不再读取或压缩字幕时槽。
pub fn pause_after(text: &str, rate: u32) -> usize {
    let text = text.trim_end().trim_end_matches(|character| {
        matches!(
            character,
            '”' | '’' | '」' | '』' | '》' | '）' | '】' | ')' | ']' | '}'
        )
    });
    let milliseconds = match text.chars().last() {
        Some('。' | '！' | '？' | '.' | '!' | '?' | '…') => 160,
        Some('，' | '、' | '；' | ':' | ',' | ';') => 60,
        _ => 0,
    };
    rate as usize * milliseconds / 1000
}

/// 优先用 ffmpeg 做两遍响度归一化；不可用时退回不削峰的线性增益。
pub fn normalize_output(source: &Path, target: &Path, rate: u32) -> Result<()> {
    if let Some(ffmpeg) = ffmpeg_path()
        && normalize_wav(&ffmpeg, source, target, rate, OUTPUT_PRESET).is_ok()
        && let Ok(wave) = read_wave(target)
        && wave.sample_rate() == rate as i32
        && !wave.samples().is_empty()
    {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);

    let wave = read_wave(source)?;
    let mut samples = wave.samples().to_vec();
    normalize_samples(&mut samples, rate);
    crate::separation::write_wav16_slices(target, &[&samples], rate)?;
    Ok(())
}

fn normalize_samples(samples: &mut [f32], rate: u32) {
    if samples.is_empty() || rate == 0 || samples.iter().any(|sample| !sample.is_finite()) {
        return;
    }
    let mean = samples.iter().map(|sample| *sample as f64).sum::<f64>() / samples.len() as f64;
    for sample in samples.iter_mut() {
        *sample = (*sample as f64 - mean) as f32;
    }
    let peak = samples
        .iter()
        .fold(0.0_f64, |peak, sample| peak.max(sample.abs() as f64));
    if peak <= 1e-6 {
        return;
    }

    let frame = (rate as usize / 50).max(1);
    let levels: Vec<f64> = samples
        .chunks(frame)
        .map(|chunk| {
            (chunk
                .iter()
                .map(|sample| (*sample as f64).powi(2))
                .sum::<f64>()
                / chunk.len() as f64)
                .sqrt()
        })
        .collect();
    let loudest = levels.iter().copied().fold(0.0_f64, f64::max);
    let active: Vec<f64> = levels
        .into_iter()
        .filter(|level| *level >= loudest * 0.08)
        .collect();
    if active.is_empty() {
        return;
    }
    let mean_square = active.iter().map(|level| level.powi(2)).sum::<f64>() / active.len() as f64;
    let rms = mean_square.sqrt();
    if rms <= 1e-6 {
        return;
    }
    let target_rms = 10.0_f64.powf(-18.5 / 20.0);
    let peak_limit = 10.0_f64.powf(f64::from(OUTPUT_PRESET.true_peak) / 20.0);
    let gain = (target_rms / rms).min(peak_limit / peak).min(4.0);
    for sample in samples.iter_mut() {
        *sample = (*sample as f64 * gain) as f32;
    }
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
        for name in builtin_voice_files(&root).unwrap() {
            let path = root.join(&name);
            let bytes = std::fs::read(&path).unwrap();
            let (normalized, transcript) = prepare_builtin_reference(&path, &bytes)
                .unwrap_or_else(|err| panic!("{name}: {err:#}"));
            assert!(transcript.is_some(), "missing transcript for {name}");
            assert!((MIN_REFERENCE_SAMPLES..=MAX_REFERENCE_SAMPLES).contains(&read_reference(&normalized).unwrap().len()));
        }
    }

    #[test]
    fn reference_validates_duration_and_converts_rate() {
        if ffmpeg_path().is_none() {
            return;
        }
        let file = tempfile::NamedTempFile::new().unwrap();
        for seconds in [4, 5, 15, 16] {
            let samples: Vec<_> = (0..24_000 * seconds)
                .map(|i| (i as f32 * 0.11).sin() * 0.2)
                .collect();
            crate::separation::write_wav16_slices(file.path(), &[&samples], 24_000).unwrap();
            let result = prepare_reference(&std::fs::read(file.path()).unwrap());
            assert_eq!(result.is_ok(), (5..=15).contains(&seconds), "{result:?}");
            if let Ok(bytes) = result {
                assert_eq!(read_reference(&bytes).unwrap().len(), 16_000 * seconds);
            }
        }
    }

    #[test]
    fn recorded_reference_requires_at_least_five_seconds() {
        let file = tempfile::NamedTempFile::new().unwrap();
        for seconds in [4, 5] {
            let samples: Vec<_> = (0..16_000 * seconds)
                .map(|i| (i as f32 * 0.11).sin() * 0.2)
                .collect();
            crate::separation::write_wav16_slices(file.path(), &[&samples], 16_000).unwrap();
            let result = read_recorded_reference(&std::fs::read(file.path()).unwrap());
            assert_eq!(result.is_ok(), seconds == 5, "{result:?}");
        }
    }

    #[test]
    fn polishing_trims_silence_and_zeroes_both_edges() {
        let mut samples = vec![0.0; 4_800];
        for sample in &mut samples[1_200..3_600] {
            *sample = 0.4;
        }
        let polished = polish_segment(&samples, 24_000).unwrap();
        assert!(polished.len() < samples.len());
        assert_eq!(polished[0], 0.0);
        assert_eq!(*polished.last().unwrap(), 0.0);
        assert!(polished.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn fallback_normalization_raises_level_without_crossing_peak_limit() {
        let mut samples: Vec<f32> = (0..24_000)
            .map(|index| {
                if index < 12_000 {
                    (index as f32 * 0.11).sin() * 0.02
                } else {
                    0.0
                }
            })
            .collect();
        let before = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        normalize_samples(&mut samples, 24_000);
        let after = samples
            .iter()
            .map(|sample| sample.abs())
            .fold(0.0_f32, f32::max);
        let limit = 10.0_f32.powf(OUTPUT_PRESET.true_peak / 20.0);
        assert!(after > before * 2.0);
        assert!(after <= limit + 1e-5);
        assert!(samples.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn output_normalization_keeps_native_rate_and_headroom() {
        let source = tempfile::NamedTempFile::new().unwrap();
        let target = tempfile::NamedTempFile::new().unwrap();
        let samples: Vec<f32> = (0..72_000)
            .map(|index| (index as f32 * 0.11).sin() * 0.05)
            .collect();
        crate::separation::write_wav16_slices(source.path(), &[&samples], 24_000).unwrap();
        normalize_output(source.path(), target.path(), 24_000).unwrap();
        let wave = read_wave(target.path()).unwrap();
        assert_eq!(wave.sample_rate(), 24_000);
        assert!(!wave.samples().is_empty());
        let peak = wave
            .samples()
            .iter()
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak <= 0.86, "normalized peak is too high: {peak}");
    }
}
