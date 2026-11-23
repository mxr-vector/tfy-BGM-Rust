//! 16 kHz 带限重采样、低能量切分和识别结果清洗。

use std::f64::consts::PI;
use std::ops::Range;

pub const SAMPLE_RATE: usize = 16_000;
const FRAME_SAMPLES: usize = 320;
const SEARCH_SAMPLES: usize = SAMPLE_RATE * 5 / 2;

pub fn resample(samples: &[f32], source_rate: i32) -> Result<Vec<f32>, String> {
    if source_rate <= 0 {
        return Err("WAV 音频的采样率无效".into());
    }
    let step = f64::from(source_rate) / SAMPLE_RATE as f64;
    let cutoff = (1. / step).min(1.);
    let count = (samples.len() as f64 / step).round() as usize;
    let mut output = Vec::new();
    output
        .try_reserve_exact(count)
        .map_err(|_| "WAV 重采样所需的内存不足")?;
    for index in 0..count {
        let position = index as f64 * step;
        let center = position.floor() as usize;
        let from = center.saturating_sub(7);
        let to = center.saturating_add(9).min(samples.len());
        let mut signal = 0.;
        let mut normalization = 0.;
        for (offset, &sample) in samples[from..to].iter().enumerate() {
            let distance = (from + offset) as f64 - position;
            if distance.abs() < 8. {
                let weight = sinc(distance * cutoff) * sinc(distance / 8.);
                signal += f64::from(sample) * weight;
                normalization += weight;
            }
        }
        output.push(if normalization.abs() > f64::EPSILON {
            (signal / normalization) as f32
        } else {
            0.
        });
    }
    Ok(output)
}

fn sinc(value: f64) -> f64 {
    let radians = PI * value;
    if radians.abs() < f64::EPSILON {
        1.
    } else {
        radians.sin() / radians
    }
}

pub fn windows(samples: &[f32], seconds: f64) -> Vec<Range<usize>> {
    let width = (seconds.clamp(2., 60.) * SAMPLE_RATE as f64) as usize;
    let min_width = (SAMPLE_RATE * 3).min(width / 2);
    let powers: Vec<_> = samples
        .chunks(FRAME_SAMPLES)
        .map(|chunk| {
            chunk
                .iter()
                .map(|sample| f64::from(*sample).powi(2))
                .sum::<f64>()
                / chunk.len() as f64
        })
        .collect();
    let mut windows = Vec::new();
    let mut start = 0;
    while samples.len() - start > width * 3 / 2 {
        let target = start + width;
        let left = target.saturating_sub(SEARCH_SAMPLES).max(start + min_width);
        let right = target
            .saturating_add(SEARCH_SAMPLES)
            .min(samples.len() - min_width);
        let first = left.div_ceil(FRAME_SAMPLES);
        let last = (right / FRAME_SAMPLES).min(powers.len().saturating_sub(1));
        let end = (first..=last)
            .min_by(|&a, &b| {
                powers[a].total_cmp(&powers[b]).then_with(|| {
                    let distance =
                        |index: usize| (index * FRAME_SAMPLES + FRAME_SAMPLES / 2).abs_diff(target);
                    distance(a).cmp(&distance(b))
                })
            })
            .map_or(target, |frame| {
                (frame * FRAME_SAMPLES + FRAME_SAMPLES / 2).min(right)
            });
        windows.push(start..end);
        start = end;
    }
    if start < samples.len() {
        windows.push(start..samples.len());
    }
    windows
}

pub fn clean_text(raw: &str) -> String {
    let mut rest = raw.trim_start();
    while let Some(token) = rest.strip_prefix("<|") {
        let Some((_, next)) = token.split_once("|>") else {
            return String::new();
        };
        rest = next.trim_start();
    }
    let text = rest.split_once("<|").map_or(rest, |(text, _)| text);
    text.split_inclusive(['。', '！', '？', '；', '!', '?', ';', '.'])
        .map(str::trim)
        .filter(|sentence| {
            let translation = sentence.contains("翻译") || sentence.contains("转写");
            let instruction = sentence.contains(['“', '”', '"', '「'])
                || sentence.contains("不要")
                || sentence.contains("请");
            !sentence.is_empty() && !(translation && instruction)
        })
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_preserving_dc_and_rejects_invalid_rates() {
        let pcm = resample(&vec![0.25; 48_000], 48_000).unwrap();
        assert_eq!(pcm.len(), 16_000);
        assert!(pcm.iter().all(|sample| (*sample - 0.25).abs() < 0.001));
        assert!(resample(&[0.], 0).is_err());
    }

    #[test]
    fn splits_at_quiet_frames_and_cleans_model_control_text() {
        let mut audio = vec![1.; SAMPLE_RATE * 21];
        audio[SAMPLE_RATE * 11..SAMPLE_RATE * 11 + FRAME_SAMPLES].fill(0.);
        let chunks = windows(&audio, 10.);
        assert_eq!(
            chunks,
            [
                0..SAMPLE_RATE * 11 + 160,
                SAMPLE_RATE * 11 + 160..audio.len()
            ]
        );
        assert_eq!(
            clean_text("<|asr|> 山海。 请转写歌词。 清风 <|endoftext|> 补写"),
            "山海。 清风"
        );
    }
}
