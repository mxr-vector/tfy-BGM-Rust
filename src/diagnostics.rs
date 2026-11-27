//! 从已解码 PCM 计算波形、响度、削波与相位。指标取自源音频。

use std::f64::consts::TAU;

use crate::audio::PcmTrack;

pub const WAVE_BARS: usize = 192;
pub const SPECTRUM_BARS: usize = 72;
const FFT_SIZE: usize = 2048;
const SPECTRUM_SMOOTHING: f64 = 0.72;

#[derive(Default)]
pub struct Analysis {
    pub waveform: Vec<f32>,
    pub integrated_lufs: Option<f64>,
    pub peak_db: Option<f64>,
    pub rms_db: Option<f64>,
    pub clipped: usize,
    pub clip_runs: usize,
    pub pops: usize,
    pub max_delta: f64,
    pub silence: Vec<(f64, f64)>,
    pub min_correlation: Option<f64>,
    pub average_correlation: Option<f64>,
    pub issues: Vec<Issue>,
}

pub struct Issue {
    pub message: String,
    severity: Verdict,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
pub enum Verdict {
    Good,
    Warning,
    Bad,
}

impl Analysis {
    pub fn verdict(&self) -> Verdict {
        self.issues
            .iter()
            .map(|issue| issue.severity)
            .max()
            .unwrap_or(Verdict::Good)
    }

    fn collect_issues(&self) -> Vec<Issue> {
        let mut issues = Vec::new();
        let mut add = |severity, message| issues.push(Issue { severity, message });
        if self.clipped > 0 {
            let peak = self
                .peak_db
                .map_or_else(|| "—".to_owned(), |peak| format!("{peak:.1}"));
            add(
                Verdict::Bad,
                format!("{} 个采样达到满幅，峰值 {peak} dBFS", self.clipped),
            );
        }
        if let Some(phase) = self.min_correlation.filter(|value| *value < -0.25) {
            add(
                Verdict::Bad,
                format!("最低声道相关 {phase:.2}，单声道播放可能抵消"),
            );
        }
        if !self.silence.is_empty() {
            let seconds: f64 = self.silence.iter().map(|(start, end)| end - start).sum();
            let severity = if seconds > 5.0 {
                Verdict::Bad
            } else {
                Verdict::Warning
            };
            add(
                severity,
                format!("{} 段静音，共 {seconds:.1} 秒", self.silence.len()),
            );
        }
        if let Some(loudness) = self
            .integrated_lufs
            .filter(|value| !(-24.0..=-9.0).contains(value))
        {
            add(
                Verdict::Warning,
                format!("综合响度 {loudness:.1} LUFS，偏离常见 BGM 区间（−24 至 −9 LUFS）"),
            );
        }
        if self.pops > 40 {
            add(
                Verdict::Warning,
                format!("检测到 {} 次疑似瞬态爆音", self.pops),
            );
        }
        issues
    }
}

#[derive(Clone, Copy)]
struct Filter {
    b: [f64; 3],
    a: [f64; 2],
    delay: [f64; 2],
}

impl Filter {
    fn new(b: [f64; 3], a: [f64; 3]) -> Self {
        Self {
            b: b.map(|value| value / a[0]),
            a: [a[1] / a[0], a[2] / a[0]],
            delay: [0.0; 2],
        }
    }

    fn sample(&mut self, value: f64) -> f64 {
        let result = self.b[0] * value + self.delay[0];
        self.delay[0] = self.b[1] * value - self.a[0] * result + self.delay[1];
        self.delay[1] = self.b[2] * value - self.a[1] * result;
        result
    }
}

fn weighting(rate: f64) -> (Filter, Filter) {
    let (sin, cos) = (TAU * 1681.974450955533 / rate).sin_cos();
    let gain = 10_f64.powf(3.999843853973347 / 40.0);
    let alpha = gain.sqrt() * sin / 0.7071752369554196;
    let plus = gain + 1.0;
    let minus = gain - 1.0;
    let shelf = Filter::new(
        [
            gain * (plus + minus * cos + alpha),
            -2.0 * gain * (minus + plus * cos),
            gain * (plus + minus * cos - alpha),
        ],
        [
            plus - minus * cos + alpha,
            2.0 * (minus - plus * cos),
            plus - minus * cos - alpha,
        ],
    );
    let (sin, cos) = (TAU * 38.13547087602444 / rate).sin_cos();
    let alpha = sin / (2.0 * 0.5003270373238773);
    let hp = Filter::new(
        [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
        [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
    );
    (shelf, hp)
}

#[derive(Clone, Copy)]
struct Channel {
    shelf: Filter,
    hp: Filter,
    previous: f64,
    previous_delta: f64,
    average_delta: f64,
    clipped_in_row: usize,
}

fn lufs(energy: f64) -> Option<f64> {
    (energy.is_finite() && energy > 0.0).then(|| -0.691 + 10.0 * energy.log10())
}

fn db(energy: f64) -> Option<f64> {
    (energy.is_finite() && energy > 0.0).then(|| 10.0 * energy.log10())
}

fn integrated(blocks: &[f64], hop: usize, frames: usize) -> Option<f64> {
    let absolute = 10_f64.powf((-70.0 + 0.691) / 10.0);
    let windows: Vec<_> = blocks[..frames / hop]
        .windows(4)
        .map(|window| window.iter().sum::<f64>() / (4 * hop) as f64)
        .filter(|energy| *energy > absolute)
        .collect();
    if windows.is_empty() {
        return None;
    }
    let relative = windows.iter().sum::<f64>() / windows.len() as f64 / 10.0;
    let (sum, count) = windows
        .into_iter()
        .filter(|energy| *energy > relative)
        .fold((0.0, 0), |(sum, count), energy| (sum + energy, count + 1));
    lufs(sum / f64::from(count))
}

pub fn speech_gain(samples: &[f32], rate: u32) -> f32 {
    let (mut shelf, mut high_pass) = weighting(f64::from(rate));
    let hop = (rate as usize / 10).max(1);
    let mut peak = 0.0_f32;
    let mut energy = Vec::with_capacity(samples.len().div_ceil(hop));
    for block in samples.chunks(hop) {
        let mut sum = 0.0;
        for &sample in block {
            peak = peak.max(sample.abs());
            let weighted = high_pass.sample(shelf.sample(f64::from(sample)));
            sum += weighted * weighted;
        }
        energy.push(sum);
    }
    let loudness = integrated(&energy, hop, samples.len())
        .or_else(|| lufs(energy.iter().sum::<f64>() / samples.len().max(1) as f64));
    let loudness_gain = loudness.map_or(1.0, |level| 10_f64.powf((-16.0 - level) / 20.0) as f32);
    loudness_gain.min(0.89125 / peak.max(f32::EPSILON))
}

pub fn analyse(track: &PcmTrack) -> Analysis {
    let channels = usize::from(track.channels.get());
    let rate = track.sample_rate.get() as usize;
    let frames = track.samples.len() / channels;
    let hop = ((rate as f64 * 0.1).round() as usize).max(1);
    let mut report = Analysis {
        waveform: vec![0.0; WAVE_BARS],
        ..Analysis::default()
    };
    let mut energy = vec![0.0; frames.div_ceil(hop)];
    let mut raw = vec![0.0; energy.len()];
    let (shelf, hp) = weighting(rate as f64);
    let mut states = vec![
        Channel {
            shelf,
            hp,
            previous: 0.0,
            previous_delta: 0.0,
            average_delta: 0.0,
            clipped_in_row: 0,
        };
        channels
    ];
    let mut peak = 0.0_f64;
    for (index, frame) in track.samples.chunks_exact(channels).enumerate() {
        let bucket = index * WAVE_BARS / frames;
        let block = index / hop;
        for (&sample, state) in frame.iter().zip(&mut states) {
            let sample = f64::from(sample);
            let magnitude = sample.abs();
            peak = peak.max(magnitude);
            report.waveform[bucket] = report.waveform[bucket].max(magnitude as f32);
            if magnitude >= 0.999 {
                report.clipped += 1;
                state.clipped_in_row += 1;
                report.clip_runs += usize::from(state.clipped_in_row == 2);
            } else {
                state.clipped_in_row = 0;
            }
            let delta = (sample - state.previous).abs();
            let threshold = 0.65_f64.max(state.average_delta * 6.0);
            if delta > threshold && state.previous_delta <= threshold && magnitude < 0.9 {
                report.pops += 1;
            }
            report.max_delta = report.max_delta.max(delta);
            state.average_delta += (delta - state.average_delta) * 0.01;
            state.previous_delta = delta;
            state.previous = sample;
            raw[block] += sample * sample;
            let weighted = state.hp.sample(state.shelf.sample(sample));
            energy[block] += weighted * weighted;
        }
    }
    report.peak_db = db(peak * peak);
    report.rms_db = db(raw.iter().sum::<f64>() / (frames * channels) as f64);
    report.integrated_lufs = integrated(&energy, hop, frames);
    let mut quiet_start = None;
    for index in 0..=raw.len() {
        let frame = (index * hop).min(frames);
        let block = hop.min(frames - frame);
        let is_quiet = raw
            .get(index)
            .is_some_and(|sum| sum / ((block * channels) as f64) < 1e-5);
        if is_quiet {
            quiet_start.get_or_insert(frame);
        } else if let Some(start) = quiet_start.take()
            && (frame - start) as f64 >= rate as f64 * 0.5
        {
            report
                .silence
                .push((start as f64 / rate as f64, frame as f64 / rate as f64));
        }
    }
    if channels >= 2 {
        let mut sum = 0.0;
        let mut count = 0;
        for block in track.samples.chunks(rate * channels) {
            let size = (block.len() / channels) as f64;
            if size < rate as f64 * 0.5 {
                continue;
            }
            let mut moments = [0.0; 5];
            for frame in block.chunks_exact(channels) {
                let (left, right) = (f64::from(frame[0]), f64::from(frame[1]));
                for (total, value) in
                    moments
                        .iter_mut()
                        .zip([left, right, left * left, right * right, left * right])
                {
                    *total += value;
                }
            }
            let [x, y, xx, yy, xy] = moments.map(|value| value / size);
            let (var_x, var_y) = (xx - x * x, yy - y * y);
            if var_x >= 1e-8 && var_y >= 1e-8 {
                let value = ((xy - x * y) / (var_x * var_y).sqrt()).clamp(-1.0, 1.0);
                report.min_correlation = Some(
                    report
                        .min_correlation
                        .map_or(value, |prior| prior.min(value)),
                );
                count += 1;
                sum += value;
            }
        }
        if count > 0 {
            report.average_correlation = Some(sum / f64::from(count));
        }
    }
    report.issues = report.collect_issues();
    report
}

pub struct Live {
    pub bands: [f32; SPECTRUM_BARS],
    pub lufs: Option<f64>,
    pub peak_db: Option<f64>,
    magnitudes: [f64; SPECTRUM_BARS],
}

pub fn live(track: &PcmTrack, position: f64, previous: Option<&Live>) -> Live {
    let channels = usize::from(track.channels.get());
    let rate = f64::from(track.sample_rate.get());
    let end = ((position * rate) as usize).min(track.samples.len() / channels);
    let window = ((rate * 0.4).round() as usize).max(1);
    let first = end.saturating_sub(window);
    let samples = &track.samples[first * channels..end * channels];
    let (shelf, hp) = weighting(rate);
    let mut filters = vec![(shelf, hp); channels];
    let mut energy = 0.0;
    for frame in samples.chunks_exact(channels) {
        for (&sample, (shelf, hp)) in frame.iter().zip(&mut filters) {
            let sample = f64::from(sample);
            let value = hp.sample(shelf.sample(sample));
            energy += value * value;
        }
    }
    let spectrum_start = end.saturating_sub(FFT_SIZE);
    let mut peak = 0.0_f64;
    let mut bins = vec![(0.0, 0.0); FFT_SIZE];
    for (index, frame) in track.samples[spectrum_start * channels..end * channels]
        .chunks_exact(channels)
        .enumerate()
    {
        let offset = index + FFT_SIZE - (end - spectrum_start);
        let reversed = offset.reverse_bits() >> (usize::BITS - FFT_SIZE.ilog2());
        let mono = frame.iter().map(|value| f64::from(*value)).sum::<f64>() / channels as f64;
        peak = peak.max(mono.abs());
        bins[reversed].0 = mono * (0.5 - 0.5 * (TAU * offset as f64 / FFT_SIZE as f64).cos());
    }
    let mut span = 2;
    while span <= FFT_SIZE {
        for chunk in bins.chunks_exact_mut(span) {
            let (left, right) = chunk.split_at_mut(span / 2);
            for (k, (a, b)) in left.iter_mut().zip(right).enumerate() {
                let (sin, cos) = (-TAU * k as f64 / span as f64).sin_cos();
                let (real, imag) = (b.0 * cos - b.1 * sin, b.0 * sin + b.1 * cos);
                *b = (a.0 - real, a.1 - imag);
                *a = (a.0 + real, a.1 + imag);
            }
        }
        span *= 2;
    }
    let magnitudes = std::array::from_fn(|index| {
        let bin =
            ((index as f64 / SPECTRUM_BARS as f64).powf(1.7) * (FFT_SIZE / 2) as f64) as usize;
        let (real, imag) = bins[bin];
        let amplitude = real.hypot(imag) / (FFT_SIZE as f64 / 4.0);
        let prior = previous.map_or(0.0, |previous| previous.magnitudes[index]);
        SPECTRUM_SMOOTHING * prior + (1.0 - SPECTRUM_SMOOTHING) * amplitude
    });
    let bands = magnitudes.map(|magnitude| {
        let level = db(magnitude * magnitude).unwrap_or(-90.0);
        ((level + 90.0) / 80.0).clamp(0.0, 1.0) as f32
    });
    Live {
        bands,
        lufs: lufs(energy / window as f64),
        peak_db: db(peak * peak),
        magnitudes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_flags_clipping_and_inverted_stereo() {
        let track = PcmTrack {
            samples: (0..48000)
                .flat_map(|i| if i % 50 == 0 { [1., -1.] } else { [0., 0.] })
                .collect(),
            channels: rodio::ChannelCount::new(2).unwrap(),
            sample_rate: rodio::SampleRate::new(48000).unwrap(),
        };
        let report = analyse(&track);
        assert_eq!(report.clipped, 1920);
        assert_eq!(report.min_correlation, Some(-1.));
        assert!(report.verdict() == Verdict::Bad);
        assert!(report.integrated_lufs.is_some());
        assert!(report.waveform.iter().all(|value| *value == 1.));
        assert_eq!(report.peak_db, Some(0.));
        assert_eq!(report.issues.len(), 2);
        let realtime = live(&track, 0.7, None);
        assert_eq!(realtime.peak_db, None);
        assert!(realtime.lufs.is_some());
    }

    #[test]
    fn verdict_respects_inclusive_loudness_and_silence_boundaries() {
        let rate = 48_000;
        let mut track = PcmTrack {
            samples: vec![0.0; rate * 5],
            channels: rodio::ChannelCount::new(1).unwrap(),
            sample_rate: rodio::SampleRate::new(rate as u32).unwrap(),
        };
        let exactly_five = analyse(&track);
        assert_eq!(exactly_five.silence, [(0.0, 5.0)]);
        assert_eq!(exactly_five.verdict(), Verdict::Warning);
        assert_eq!(exactly_five.integrated_lufs, None);
        assert_eq!(exactly_five.min_correlation, None);
        track.samples.push(0.0);
        assert_eq!(analyse(&track).verdict(), Verdict::Bad);

        let mut report = Analysis {
            pops: 40,
            min_correlation: Some(-0.25),
            ..Default::default()
        };
        for (loudness, expected) in [
            (-24.01, Verdict::Warning),
            (-24.0, Verdict::Good),
            (-9.0, Verdict::Good),
            (-8.99, Verdict::Warning),
        ] {
            report.integrated_lufs = Some(loudness);
            report.issues = report.collect_issues();
            assert_eq!(report.verdict(), expected);
        }
        report.integrated_lufs = Some(-16.0);
        report.pops = 41;
        report.issues = report.collect_issues();
        assert_eq!(report.verdict(), Verdict::Warning);
        assert!(report.issues[0].message.contains("41 次"));
    }

    #[test]
    fn live_spectrum_smooths_seventy_two_bands_and_keeps_meter_windows_separate() {
        let rate = 48_000;
        let track = PcmTrack {
            samples: (0..rate * 2)
                .map(|index| {
                    if index < rate {
                        (TAU * 997.0 * index as f64 / rate as f64).sin() as f32 * 0.2
                    } else {
                        0.0
                    }
                })
                .collect(),
            channels: rodio::ChannelCount::new(1).unwrap(),
            sample_rate: rodio::SampleRate::new(rate).unwrap(),
        };
        let signal = live(&track, 0.9, None);
        assert_eq!(signal.bands.len(), 72);
        assert!(signal.bands.iter().any(|&band| band > 0.0));
        assert!(signal.bands.iter().all(|band| (0.0..=1.0).contains(band)));
        assert!(signal.peak_db.is_some());

        let tail = live(&track, 1.1, Some(&signal));
        assert!(tail.lufs.is_some());
        assert_eq!(tail.peak_db, None);
        for (before, after) in signal.magnitudes.iter().zip(tail.magnitudes) {
            assert_eq!(after, before * SPECTRUM_SMOOTHING);
        }
        let silent = live(&track, 1.5, None);
        assert_eq!(silent.lufs, None);
        assert_eq!(silent.bands, [0.0; SPECTRUM_BARS]);
    }

    #[test]
    fn integrated_loudness_applies_both_gates_to_complete_windows() {
        let absolute = 10_f64.powf((-70.0 + 0.691) / 10.0);
        assert_eq!(integrated(&[absolute; 4], 1, 4), None);
        assert!(integrated(&[absolute * 1.01; 4], 1, 4).is_some());
        assert_eq!(integrated(&[1.0; 4], 4, 15), None);

        let mut blocks = [0.0001; 12];
        blocks[..4].fill(1.0);
        let measured = integrated(&blocks, 1, blocks.len()).unwrap();
        let expected = lufs(0.6250375).unwrap();
        assert!((measured - expected).abs() < 1e-10);
    }

    #[test]
    fn gated_stereo_loudness_matches_ffmpeg_reference() {
        let Some(ffmpeg) = bgm_core::loudness::ffmpeg_path() else {
            return;
        };
        let rate = 48_000;
        let track = PcmTrack {
            samples: (0..rate * 9)
                .flat_map(|index| {
                    let amplitude = match index / rate {
                        0..=3 => 0.2,
                        4..=5 => 0.00001,
                        _ => 0.009,
                    };
                    let sample =
                        (TAU * 997.0 * index as f64 / rate as f64).sin() as f32 * amplitude;
                    [sample, sample * 0.8]
                })
                .collect(),
            channels: rodio::ChannelCount::new(2).unwrap(),
            sample_rate: rodio::SampleRate::new(rate).unwrap(),
        };
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("stereo.f32le");
        let bytes: Vec<_> = track
            .samples
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        std::fs::write(&source, bytes).unwrap();
        let measured = std::process::Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-nostats",
                "-f",
                "f32le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
            ])
            .arg(&source)
            .args(["-af", "loudnorm=print_format=json", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(measured.status.success());
        let stderr = String::from_utf8_lossy(&measured.stderr);
        let json = &stderr[stderr.rfind('{').unwrap()..=stderr.rfind('}').unwrap()];
        let stats: serde_json::Value = serde_json::from_str(json).unwrap();
        let expected: f64 = stats["input_i"].as_str().unwrap().parse().unwrap();
        let actual = analyse(&track).integrated_lufs.unwrap();
        assert!(
            (actual - expected).abs() < 0.3,
            "GPUI {actual:.2} LUFS, FFmpeg {expected:.2} LUFS"
        );
    }
}
