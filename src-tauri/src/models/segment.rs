//! 识别结果清洗、能量切分与重采样。

use anyhow::{Result, bail};

use super::{
    ASR_SAMPLE_RATE, ENERGY_FRAME_SECS, RESAMPLE_TAPS, SEGMENT_MIN_SECS, SEGMENT_SEARCH_SECS,
    SEGMENT_TARGET_SECS,
};

/// 去掉 Qwen3-ASR 输出里可能夹带的 `<|...|>` 控制标记，并压平空白。
///
/// 官方 `generation_config.json` 把 `<|endoftext|>`(151643) 与 `<|im_end|>`(151645)
/// 都声明为终止符，但 sherpa-onnx 的 `QwenAsrTokenizer` 只把 `<|im_end|>` 当 EOS，
/// `<|endoftext|>` 仅作为 pad token。模型一旦先吐出 `<|endoftext|>`，解码不会停止，
/// 而会在没有音频约束的情况下自由续写，把预训练语料里的原文带进字幕（典型表现是
/// 歌词后面接一段与歌曲无关的百科式介绍）。因此正文之后第一次出现控制标记即视为
/// 本次识别的终止，丢弃其后的全部内容；开头连续的控制标记仍按脚手架跳过。
pub(super) fn clean_asr_text(raw: &str) -> String {
    let mut rest = raw;
    loop {
        let trimmed = rest.trim_start();
        if !trimmed.starts_with("<|") {
            rest = trimmed;
            break;
        }
        let Some(end) = trimmed.find("|>") else {
            rest = "";
            break;
        };
        rest = &trimmed[end + 2..];
    }
    let text = match rest.find("<|") {
        Some(position) => &rest[..position],
        None => rest,
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉 Qwen3-ASR 输出里偶发夹带的训练指令句（元指令幻觉）。
///
/// 模型在没有明确歌词的窗口（歌曲尾部、纯乐器段）上偶尔会把训练期的系统指令
/// 当作识别结果输出，例如「特别注意，不要把“system”翻译成“系统”。」。
/// 这类句子含「翻译 / 转写」且带引号或「不要 / 请」，不属于歌词；按句丢弃，
/// 同一段里正常识别出的歌词句子保留。
pub(super) fn strip_meta_instructions(text: &str) -> String {
    text.split_inclusive(['。', '！', '？', '；', '!', '?', ';', '.'])
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty() && !is_meta_instruction(sentence))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn is_meta_instruction(sentence: &str) -> bool {
    let mentions_translation = sentence.contains("翻译") || sentence.contains("转写");
    let quoted = sentence.contains('“')
        || sentence.contains('”')
        || sentence.contains('"')
        || sentence.contains('「');
    mentions_translation && (quoted || sentence.contains("不要") || sentence.contains("请"))
}

/// 规划字幕分片区间：按目标时长切分，并把每个切点吸附到附近能量最低的帧。
///
/// 返回首尾相接、覆盖整段音频的 `[start, end)` 秒区间；音频很短时只返回一段。
pub(super) fn plan_segments(
    samples: &[f32],
    rate: usize,
    total_secs: f64,
    window: f64,
) -> Vec<(f64, f64)> {
    if rate == 0 || samples.is_empty() || total_secs <= 0.0 {
        return Vec::new();
    }
    let window = if window.is_finite() && window > 0.0 {
        window
    } else {
        SEGMENT_TARGET_SECS
    };
    let min_len = SEGMENT_MIN_SECS.min(window / 2.0).max(ENERGY_FRAME_SECS);
    let energies = frame_energies(samples, rate);
    let mut ranges = Vec::new();
    let mut start = 0.0;
    // 剩余时长不足 1.5 个窗口时直接作为最后一段，避免切出过短的尾巴。
    while total_secs - start > window * 1.5 {
        let target = start + window;
        let lo = (start + min_len).max(target - SEGMENT_SEARCH_SECS);
        let hi = (total_secs - min_len).min(target + SEGMENT_SEARCH_SECS);
        let end = quietest_point(&energies, lo, hi, target).unwrap_or(target);
        if end > start + min_len {
            ranges.push((start, end));
            start = end;
            continue;
        }
        // 搜索区间不可用（接近结尾 / 全部越界）时退回固定切点。
        let fallback = (start + window).min(total_secs);
        if fallback <= start + min_len {
            break;
        }
        ranges.push((start, fallback));
        start = fallback;
    }
    if total_secs > start {
        ranges.push((start, total_secs));
    }
    ranges
}

/// 每 [`ENERGY_FRAME_SECS`] 一帧的 RMS 能量包络。
pub(super) fn frame_energies(samples: &[f32], rate: usize) -> Vec<f64> {
    let frame_len = ((rate as f64 * ENERGY_FRAME_SECS).round() as usize).max(1);
    samples
        .chunks(frame_len)
        .map(|frame| {
            let sum: f64 = frame
                .iter()
                .map(|value| (*value as f64) * (*value as f64))
                .sum();
            (sum / frame.len() as f64).sqrt()
        })
        .collect()
}

/// 在 `[lo, hi]` 秒内找能量最低的帧中心；能量相同时取离 `target` 最近的一个。
pub(super) fn quietest_point(energies: &[f64], lo: f64, hi: f64, target: f64) -> Option<f64> {
    // NaN 也要按「区间无效」处理，避免 f64 比较放进索引计算。
    if energies.is_empty() || hi.partial_cmp(&lo) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let first = (lo / ENERGY_FRAME_SECS).ceil().max(0.0) as usize;
    let last = ((hi / ENERGY_FRAME_SECS).floor() as usize).min(energies.len() - 1);
    if first > last {
        return None;
    }
    let mut best: Option<(f64, f64, f64)> = None;
    for (index, &energy) in energies.iter().enumerate().take(last + 1).skip(first) {
        let time = (index as f64 + 0.5) * ENERGY_FRAME_SECS;
        let distance = (time - target).abs();
        let better = match best {
            None => true,
            Some((best_energy, best_distance, _)) => {
                energy < best_energy - 1e-9
                    || ((energy - best_energy).abs() <= 1e-9 && distance < best_distance)
            }
        };
        if better {
            best = Some((energy, distance, time));
        }
    }
    best.map(|(_, _, time)| time)
}

/// 归一化 sinc：`sin(πx)/(πx)`，`x→0` 时取 1。
pub(super) fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

/// 把 sherpa-onnx 读出的单声道 PCM 重采样到 [`ASR_SAMPLE_RATE`]。
///
/// 采用带限 Lanczos 插值，核为 `sinc(cutoff·x) · sinc(x/A)`，其中
/// `cutoff = min(1, 16000/src_rate)`：
/// - 下采样时把截止频率收窄到目标 Nyquist（8 kHz），抑制会混叠进 fbank 的高频；
/// - 上采样时 `cutoff = 1`，仅做平滑插值，保留原带内容。
///
/// 逐输出点按核权重归一化，保证直流增益为 1，信号首尾不会因核被截断而衰减。
/// 已是 16 kHz 时直接返回副本（恒等路径），不改变音质也不增加耗时。
pub(super) fn resample_to_asr(samples: &[f32], src_rate: i32) -> Result<Vec<f32>> {
    if src_rate <= 0 {
        bail!("音频采样率无效：{src_rate}");
    }
    if samples.is_empty() || src_rate == ASR_SAMPLE_RATE {
        return Ok(samples.to_vec());
    }

    let ratio = ASR_SAMPLE_RATE as f64 / src_rate as f64;
    let cutoff = ratio.min(1.0);
    let a = RESAMPLE_TAPS as f64;
    let out_len = ((samples.len() as f64 * ratio).floor() as usize).max(1);
    let mut out = Vec::with_capacity(out_len);

    for n in 0..out_len {
        // n 为输出域样本序号，pos 为对应的源域浮点位置。
        let pos = n as f64 / ratio;
        let center = pos.floor();
        let mut acc = 0.0;
        let mut norm = 0.0;
        let from = (center - a).floor() as isize;
        let to = (center + a).ceil() as isize;
        for i in from..=to {
            if i < 0 || i as usize >= samples.len() {
                continue;
            }
            let x = pos - i as f64;
            let weight = sinc(x * cutoff) * sinc(x / a);
            acc += weight * samples[i as usize] as f64;
            norm += weight;
        }
        out.push(if norm.abs() > 1e-12 {
            (acc / norm) as f32
        } else {
            0.0
        });
    }
    Ok(out)
}
