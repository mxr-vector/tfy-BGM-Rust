//! Kokoro 103 音色清单、内置参考音色、录音规范化与音频后处理。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use bgm_core::loudness::{ExportPreset, ffmpeg_path, normalize_wav};
use bgm_core::process::quiet_command;
use serde::Deserialize;

use super::wave;

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VoiceCategory {
    All,
    ZhFemale,
    ZhMale,
    English,
}

impl VoiceCategory {
    pub const ALL: [Self; 4] = [Self::All, Self::ZhFemale, Self::ZhMale, Self::English];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "全部",
            Self::ZhFemale => "中文女声",
            Self::ZhMale => "中文男声",
            Self::English => "英语",
        }
    }
}

#[derive(Clone, Copy)]
pub struct KokoroVoice {
    pub id: i32,
    pub code: &'static str,
    pub category: VoiceCategory,
}

const KOKORO_CODES: [&str; 103] = [
    "af_maple", "af_sol", "bf_vale", "zf_001", "zf_002", "zf_003", "zf_004", "zf_005", "zf_006",
    "zf_007", "zf_008", "zf_017", "zf_018", "zf_019", "zf_021", "zf_022", "zf_023", "zf_024",
    "zf_026", "zf_027", "zf_028", "zf_032", "zf_036", "zf_038", "zf_039", "zf_040", "zf_042",
    "zf_043", "zf_044", "zf_046", "zf_047", "zf_048", "zf_049", "zf_051", "zf_059", "zf_060",
    "zf_067", "zf_070", "zf_071", "zf_072", "zf_073", "zf_074", "zf_075", "zf_076", "zf_077",
    "zf_078", "zf_079", "zf_083", "zf_084", "zf_085", "zf_086", "zf_087", "zf_088", "zf_090",
    "zf_092", "zf_093", "zf_094", "zf_099", "zm_009", "zm_010", "zm_011", "zm_012", "zm_013",
    "zm_014", "zm_015", "zm_016", "zm_020", "zm_025", "zm_029", "zm_030", "zm_031", "zm_033",
    "zm_034", "zm_035", "zm_037", "zm_041", "zm_045", "zm_050", "zm_052", "zm_053", "zm_054",
    "zm_055", "zm_056", "zm_057", "zm_058", "zm_061", "zm_062", "zm_063", "zm_064", "zm_065",
    "zm_066", "zm_068", "zm_069", "zm_080", "zm_081", "zm_082", "zm_089", "zm_091", "zm_095",
    "zm_096", "zm_097", "zm_098", "zm_100",
];

pub fn kokoro_voices() -> Vec<KokoroVoice> {
    KOKORO_CODES
        .iter()
        .enumerate()
        .map(|(index, &code)| {
            let category = if code.starts_with("zf_") {
                VoiceCategory::ZhFemale
            } else if code.starts_with("zm_") {
                VoiceCategory::ZhMale
            } else {
                VoiceCategory::English
            };
            KokoroVoice {
                id: index as i32,
                code,
                category,
            }
        })
        .collect()
}

pub const DEFAULT_KOKORO_SPEAKER: i32 = 3;

pub fn kokoro_voice_label(voice: KokoroVoice) -> String {
    match voice.code {
        "af_maple" => "美式英语 · Maple".into(),
        "af_sol" => "美式英语 · Sol".into(),
        "bf_vale" => "英式英语 · Vale".into(),
        code if code.starts_with("zf_") => {
            format!("中文女声 · {}", code.trim_start_matches("zf_"))
        }
        code if code.starts_with("zm_") => {
            format!("中文男声 · {}", code.trim_start_matches("zm_"))
        }
        code => code.to_owned(),
    }
}

pub struct BuiltinReference {
    pub file: tempfile::TempPath,
    pub text: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuiltinPrompt {
    end_seconds: f64,
    text: String,
}

pub fn builtin_voices_root() -> PathBuf {
    if let Ok(executable) = std::env::current_exe()
        && let Some(contents) = executable
            .parent()
            .and_then(|dir| dir.parent())
            .filter(|path| path.ends_with("Contents"))
    {
        let bundled = contents.join("Resources/voices");
        if bundled.is_dir() {
            return bundled;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("models/voices")
}

pub fn list_builtin_voices(root: &Path) -> Result<Vec<String>, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("无法打开内置参考音色目录：{error}")),
    };
    let mut voices = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
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

pub fn load_builtin_reference(root: &Path, voice: &str) -> Result<BuiltinReference, String> {
    let available = list_builtin_voices(root)?;
    if !available.iter().any(|item| item == voice) {
        return Err("未找到所选内置参考音频".into());
    }
    let path = root.join(voice);
    let manifest_path = root.join("prompts.json");
    let prompt = match fs::read(&manifest_path) {
        Ok(bytes) => {
            let mut prompts: HashMap<String, BuiltinPrompt> = serde_json::from_slice(&bytes)
                .map_err(|error| format!("无法解析内置音色逐字稿：{error}"))?;
            prompts.remove(voice)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("无法读取内置音色逐字稿：{error}")),
    };
    let (limit, text) = if let Some(prompt) = prompt {
        let text = prompt.text.trim().to_owned();
        if !(3.0..=15.0).contains(&prompt.end_seconds) || !valid_reference_text(&text) {
            return Err("内置音色逐字稿或参考片段时长无效".into());
        }
        (Some(prompt.end_seconds), Some(text))
    } else {
        (Some(10.0), None)
    };
    let wav = normalize_reference_file(&path, limit, MIN_REFERENCE_SAMPLES)?;
    Ok(BuiltinReference { file: wav, text })
}

pub fn prepare_recorded_reference(rate: u32, mono: &[f32]) -> Result<tempfile::TempPath, String> {
    let raw = tempfile::Builder::new()
        .prefix("tfy-voice-raw-")
        .suffix(".wav")
        .tempfile()
        .map_err(|error| error.to_string())?;
    wave::write(raw.path(), rate, mono, false)?;
    normalize_reference_file(raw.path(), Some(15.1), MIN_RECORDED_REFERENCE_SAMPLES)
}

pub fn valid_reference_text(text: &str) -> bool {
    !text.is_empty() && text.chars().count() <= 1000 && !text.contains('\0')
}

fn normalize_reference_file(
    source: &Path,
    limit_seconds: Option<f64>,
    min_samples: usize,
) -> Result<tempfile::TempPath, String> {
    let target = tempfile::Builder::new()
        .prefix("tfy-voice-ref-")
        .suffix(".wav")
        .tempfile()
        .map_err(|error| error.to_string())?
        .into_temp_path();
    if let Some(ffmpeg) = ffmpeg_path() {
        let limit = limit_seconds.map(|value| format!("{value}"));
        convert(
            &ffmpeg,
            source,
            &target,
            Some("loudnorm=I=-20:TP=-1.5:LRA=11"),
            limit.as_deref(),
            REFERENCE_RATE,
        )?;
    } else {
        let (rate, mut samples) = wave::reference(source)?;
        if rate != REFERENCE_RATE {
            samples = resample_linear(&samples, rate, REFERENCE_RATE);
        }
        if let Some(limit) = limit_seconds {
            samples.truncate((limit * f64::from(REFERENCE_RATE)).round() as usize);
        }
        wave::write(&target, REFERENCE_RATE, &samples, true)?;
    }
    validate_reference_wav(&target, min_samples)?;
    Ok(target)
}

fn validate_reference_wav(path: &Path, min_samples: usize) -> Result<Vec<f32>, String> {
    let meta = fs::metadata(path).map_err(|error| error.to_string())?;
    if meta.len() as usize > MAX_REFERENCE_BYTES {
        return Err("参考音频时长不能超过 15 秒".into());
    }
    let (rate, samples) = wave::reference(path)?;
    if rate != REFERENCE_RATE {
        return Err("参考音频须先转换为 16 kHz 单声道".into());
    }
    if !(min_samples..=MAX_REFERENCE_SAMPLES).contains(&samples.len()) {
        let min_seconds = min_samples / REFERENCE_RATE as usize;
        return Err(format!("参考音频时长须在 {min_seconds}–15 秒之间"));
    }
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, &value| peak.max(value.abs()));
    if peak < 0.001 {
        return Err("参考音频中没有可用声音".into());
    }
    Ok(samples)
}

fn convert(
    ffmpeg: &Path,
    source: &Path,
    target: &Path,
    filter: Option<&str>,
    limit: Option<&str>,
    rate: u32,
) -> Result<(), String> {
    let mut command = quiet_command(ffmpeg);
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
    let output = command
        .arg(target)
        .output()
        .map_err(|error| format!("无法启动 ffmpeg：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message: String = stderr.trim().chars().take(400).collect();
        return Err(format!("音频转换失败：{message}"));
    }
    if fs::metadata(target).map_or(0, |meta| meta.len()) <= 44 {
        return Err("转换后的音频为空".into());
    }
    Ok(())
}

fn resample_linear(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let out_len = ((samples.len() as f64) * f64::from(to) / f64::from(from)).round() as usize;
    (0..out_len.max(1))
        .map(|i| {
            let pos = i as f64 * f64::from(from) / f64::from(to);
            let left = (pos.floor() as usize).min(samples.len() - 1);
            let right = (left + 1).min(samples.len() - 1);
            let frac = (pos - left as f64) as f32;
            samples[left] * (1.0 - frac) + samples[right] * frac
        })
        .collect()
}

pub fn polish_segment(samples: &[f32], rate: u32) -> Result<Vec<f32>, String> {
    if samples.is_empty() || rate == 0 {
        return Err("语音片段或采样率无效".into());
    }
    if !samples.iter().all(|sample| sample.is_finite()) {
        return Err("语音片段包含无效采样".into());
    }
    let peak = samples
        .iter()
        .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
    if peak <= 1e-6 {
        return Err("语音片段接近静音".into());
    }
    let threshold = peak * 0.015;
    let first = samples
        .iter()
        .position(|sample| sample.abs() >= threshold)
        .ok_or("语音片段没有有效声音")?;
    let last = samples
        .iter()
        .rposition(|sample| sample.abs() >= threshold)
        .ok_or("语音片段没有有效声音")?;
    let pad = rate.saturating_mul(EDGE_PAD_MS) as usize / 1000;
    let start = first.saturating_sub(pad);
    let end = last
        .saturating_add(1)
        .saturating_add(pad)
        .min(samples.len());
    let mut polished = samples[start..end].to_vec();
    if polished.len() < 2 {
        return Err("语音片段过短".into());
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

pub fn fit_subtitle_audio(
    samples: &[f32],
    max_frames: usize,
    rate: u32,
) -> Result<Vec<f32>, String> {
    if max_frames == 0 {
        return Err("字幕时槽太短，无法清晰读完整句".into());
    }
    let tempo = (samples.len() as f64 / max_frames as f64) * 1.05;
    if tempo > 2.0 {
        return Err("字幕时槽太短，无法清晰读完整句".into());
    }
    if let Some(ffmpeg) = ffmpeg_path() {
        let source = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        let target = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
        wave::write(source.path(), rate, samples, false)?;
        convert(
            &ffmpeg,
            source.path(),
            target.path(),
            Some(&format!("atempo={tempo:.6}")),
            None,
            rate,
        )?;
        let stretched = wave::read_generated(target.path(), rate, samples.len())?;
        let fitted = polish_segment(&stretched, rate)?;
        if fitted.len() > max_frames {
            return Err("字幕时槽太短，无法清晰读完整句".into());
        }
        return Ok(fitted);
    }
    let stretched = crate::audio::stretch_mono(samples.to_vec(), rate, tempo);
    let fitted = polish_segment(&stretched, rate)?;
    if fitted.len() > max_frames {
        return Err("字幕时槽太短，无法清晰读完整句".into());
    }
    Ok(fitted)
}

pub fn normalize_output(
    source: &Path,
    target: &Path,
    rate: u32,
    frames: usize,
) -> Result<(), String> {
    if let Some(ffmpeg) = ffmpeg_path()
        && normalize_wav(&ffmpeg, source, target, rate, OUTPUT_PRESET).is_ok()
        && let Ok(actual_samples) = wave::read_generated(target, rate, frames)
        && actual_samples.len() == frames
    {
        return Ok(());
    }
    let _ = fs::remove_file(target);
    let mut samples = wave::read_generated(source, rate, frames)?;
    if samples.len() != frames {
        return Err("生成音频的帧数与脚本时轴不一致".into());
    }
    normalize_samples(&mut samples, rate);
    wave::write(target, rate, &samples, false)
}

fn normalize_samples(samples: &mut [f32], rate: u32) {
    if samples.is_empty() || rate == 0 || samples.iter().any(|sample| !sample.is_finite()) {
        return;
    }
    let mut sum = 0.0;
    let mut active_samples = 0;
    for &sample in samples.iter().filter(|&&sample| sample != 0.0) {
        sum += f64::from(sample);
        active_samples += 1;
    }
    if active_samples == 0 {
        return;
    }
    let mean = sum / f64::from(active_samples);
    for sample in samples.iter_mut().filter(|sample| **sample != 0.0) {
        *sample = (f64::from(*sample) - mean) as f32;
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
    fn kokoro_voice_catalog_covers_all_103_speakers() {
        let voices = kokoro_voices();
        assert_eq!(voices.len(), 103);
        assert_eq!(voices[0].code, "af_maple");
        assert_eq!(voices[DEFAULT_KOKORO_SPEAKER as usize].code, "zf_001");
        assert_eq!(voices[102].code, "zm_100");
    }

    #[test]
    fn bundled_references_all_have_transcripts_and_fit_window() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("models/voices");
        let files = list_builtin_voices(&root).unwrap();
        assert_eq!(files.len(), 11);
        for name in files {
            let loaded = load_builtin_reference(&root, &name)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(loaded.text.is_some(), "missing transcript for {name}");
            let (rate, samples) = wave::reference(&loaded.file).unwrap();
            assert_eq!(rate, REFERENCE_RATE);
            assert!((MIN_REFERENCE_SAMPLES..=MAX_REFERENCE_SAMPLES).contains(&samples.len()));
        }
    }

    #[test]
    fn missing_transcript_for_builtin_voice_returns_none_text() {
        let temp = tempfile::tempdir().unwrap();
        let samples: Vec<_> = (0..16_000 * 5)
            .map(|i| (i as f32 * 0.11).sin() * 0.2)
            .collect();
        wave::write(&temp.path().join("custom.wav"), 16_000, &samples, false).unwrap();
        fs::write(temp.path().join("prompts.json"), b"{}").unwrap();
        let loaded = load_builtin_reference(temp.path(), "custom.wav").unwrap();
        assert!(loaded.text.is_none());
    }

    #[test]
    fn pause_after_punctuation_matches_upstream_bounds() {
        assert_eq!(pause_after("一句话。", 24_000), 3_840);
        assert_eq!(pause_after("“你好。”", 24_000), 3_840);
        assert_eq!(pause_after("前半句，", 24_000), 1_440);
        assert_eq!(pause_after("跨段继续", 24_000), 0);
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
    }
}
