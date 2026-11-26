//! SRT / LRC 的格式化与解析。

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Result, anyhow, bail};

use super::SubtitleSegment;

pub fn srt_time(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = millis / 3_600_000;
    let minutes = (millis / 60_000) % 60;
    let secs = (millis / 1000) % 60;
    let ms = millis % 1000;
    format!("{hours:02}:{minutes:02}:{secs:02},{ms:03}")
}

/// LRC 时间：`MM:SS.cc`（百分秒）。
pub fn lrc_time(seconds: f64) -> String {
    let centis = (seconds.max(0.0) * 100.0).round() as u64;
    let minutes = centis / 6000;
    let secs = (centis / 100) % 60;
    let cs = centis % 100;
    format!("{minutes:02}:{secs:02}.{cs:02}")
}

pub fn format_srt(segments: &[SubtitleSegment]) -> String {
    let mut out = String::new();
    for (index, segment) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            srt_time(segment.start),
            srt_time(segment.end),
            segment.text
        ));
    }
    out
}

pub fn format_lrc(title: &str, segments: &[SubtitleSegment]) -> String {
    let safe_title = title.replace(['\r', '\n'], " ");
    let mut out = format!("[ar:]\n[ti:{safe_title}]\n");
    for segment in segments {
        out.push_str(&format!("[{}]{}\n", lrc_time(segment.start), segment.text));
    }
    out
}

/// 写出字幕文件；返回字节数。
pub fn write_subtitle(path: &Path, content: &str) -> Result<u64> {
    fs::write(path, content.as_bytes())
        .map_err(|error: io::Error| anyhow!("无法写入字幕文件 {}：{error}", path.display()))?;
    Ok(content.len() as u64)
}

// ---------------- 字幕编辑：校验、解析与写回 ----------------

/// 单句允许的最大字符数，避免异常粘贴撑爆导出文件与歌词窗口；超出时明确报错而非截断。
pub(super) const MAX_SEGMENT_TEXT_CHARS: usize = 2_000;
/// 一次编辑允许的最多分段数。
const MAX_SEGMENTS: usize = 5_000;

/// 规范化编辑后的分段：校验时间合法性、折叠文本换行、按开始时间排序。
///
/// 「分段编辑」与「原文编辑」两个入口共用此函数，保证落盘数据始终满足
/// `start >= 0`、`end > start`、文本为单行，且与桌面歌词 / 导出共用同一顺序。
pub fn normalize_segments(mut segments: Vec<SubtitleSegment>) -> Result<Vec<SubtitleSegment>> {
    if segments.is_empty() {
        bail!("字幕内容不能为空，至少需要保留一条");
    }
    if segments.len() > MAX_SEGMENTS {
        bail!("字幕条数过多（最多 {MAX_SEGMENTS} 条），请精简后重试");
    }
    for (index, segment) in segments.iter_mut().enumerate() {
        let label = index + 1;
        if !segment.start.is_finite() || !segment.end.is_finite() {
            bail!("第 {label} 条的开始 / 结束时间不是有效数字");
        }
        if segment.start < 0.0 || segment.end < 0.0 {
            bail!("第 {label} 条的时间不能为负数");
        }
        if segment.end <= segment.start {
            bail!("第 {label} 条的结束时间必须大于开始时间");
        }
        // SRT 以空行分块、文本单行；换行会破坏结构，统一折叠为空格。
        // 超长文本直接报错（不静默截断），避免用户内容悄悄丢失。
        let text = segment.text.replace(['\r', '\n'], " ");
        let text = text.trim();
        if text.chars().count() > MAX_SEGMENT_TEXT_CHARS {
            bail!("第 {label} 条文本过长（最多 {MAX_SEGMENT_TEXT_CHARS} 字），请精简后重试");
        }
        segment.text = text.to_owned();
    }
    segments.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.end.partial_cmp(&b.end).unwrap_or(Ordering::Equal))
    });
    Ok(segments)
}

pub fn parse_srt(text: &str) -> Result<Vec<SubtitleSegment>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut segments: Vec<SubtitleSegment> = Vec::new();
    let mut current: Option<(f64, f64, Vec<String>)> = None;

    fn flush(segments: &mut Vec<SubtitleSegment>, current: &mut Option<(f64, f64, Vec<String>)>) {
        if let Some((start, end, lines)) = current.take() {
            segments.push(SubtitleSegment {
                start,
                end,
                text: lines.join(" ").trim().to_owned(),
            });
        }
    }

    for (line_index, raw) in normalized.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            flush(&mut segments, &mut current);
            continue;
        }
        if let Some((start, end)) = parse_srt_range(line) {
            flush(&mut segments, &mut current);
            current = Some((start, end, Vec::new()));
            continue;
        }
        // 含 `-->` 却解析不出时间轴：多半是手误。直接报错而不是并入上一句，
        // 否则时间轴和随后的正文会被静默拼进上一条字幕。
        if line.contains("-->") {
            bail!("SRT 第 {} 行的时间轴无法解析：{line}", line_index + 1);
        }
        match current.as_mut() {
            Some((_, _, lines)) => lines.push(line.to_owned()),
            // 序号行；LRC 元数据等其它无头文本一律拒绝，避免静默丢内容。
            None if line.chars().all(|ch| ch.is_ascii_digit()) => {}
            None => bail!("SRT 第 {} 行既不是时间轴也不是序号：{line}", line_index + 1),
        }
    }
    flush(&mut segments, &mut current);
    normalize_segments(segments)
}

/// 尝试把一行解析为 SRT 时间轴；不是时间轴时返回 `None`（当作普通文本）。
pub(super) fn parse_srt_range(line: &str) -> Option<(f64, f64)> {
    let (start, end) = line.split_once("-->")?;
    Some((parse_srt_time(start)?, parse_srt_time(end)?))
}

/// 支持 `HH:MM:SS,mmm` / `HH:MM:SS.mmm` / `MM:SS,mmm` / `SS.mmm`。
pub(super) fn parse_srt_time(raw: &str) -> Option<f64> {
    let cleaned = raw.trim().replace(',', ".");
    let mut parts = cleaned.split(':');
    let first = parts.next()?;
    let second = parts.next();
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let value = match (second, third) {
        (Some(minutes), Some(rest)) => {
            let hours: f64 = first.parse().ok()?;
            let minutes: f64 = minutes.parse().ok()?;
            let seconds: f64 = rest.parse().ok()?;
            hours * 3600.0 + minutes * 60.0 + seconds
        }
        (Some(rest), None) => {
            let minutes: f64 = first.parse().ok()?;
            let seconds: f64 = rest.parse().ok()?;
            minutes * 60.0 + seconds
        }
        (None, _) => first.parse().ok()?,
    };
    value.is_finite().then_some(value)
}

/// 解析 LRC 原文为分段；元数据标签忽略，同一行的多个时间标签展开为多条。
///
/// LRC 只有起点没有终点：每条的 `end` 取下一条起点，末条取音频时长
/// （`duration_secs` 可用且晚于起点时），否则取起点后 5 秒。解析结果同样经
/// `normalize_segments` 校验与排序，确保 SRT / LRC / 桌面歌词三处时间轴一致。
pub fn parse_lrc(text: &str, duration_secs: Option<f64>) -> Result<Vec<SubtitleSegment>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut timed: Vec<(f64, String)> = Vec::new();
    for (line_index, raw) in normalized.lines().enumerate() {
        let mut rest = raw.trim();
        if rest.is_empty() {
            continue;
        }
        let mut tags = 0usize;
        let mut times = Vec::new();
        // 只解析行首连续的时间标签；出现时间标签之后的 `[...]`（如 `[笑]`）属于正文。
        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else { break };
            let tag = &rest[1..close];
            if let Some(seconds) = parse_lrc_time(tag) {
                times.push(seconds);
                rest = rest[close + 1..].trim_start();
                tags += 1;
                continue;
            }
            if times.is_empty() && is_lrc_metadata_tag(tag) {
                rest = rest[close + 1..].trim_start();
                tags += 1;
                continue;
            }
            if times.is_empty() {
                // 形如 `[00:01,500]` 的手误标签若被当作元数据跳过，会连带丢掉整行歌词。
                bail!("LRC 第 {} 行的标签无法识别：{tag}", line_index + 1);
            }
            break;
        }
        if times.is_empty() {
            // 只有「纯元数据行」（[ar:] / [ti:] / [offset:] 等）可以忽略；
            // 元数据之后还有正文却没有时间标签时按解析失败处理，避免静默丢内容。
            if tags > 0 && rest.is_empty() {
                continue;
            }
            bail!("LRC 第 {} 行缺少时间标签：[mm:ss.cc]", line_index + 1);
        }
        for seconds in times {
            timed.push((seconds, rest.trim().to_owned()));
        }
    }
    if timed.is_empty() {
        bail!("LRC 文本中没有可识别的歌词行");
    }
    timed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let mut segments = Vec::with_capacity(timed.len());
    for (index, (start, text)) in timed.iter().enumerate() {
        let next = timed.get(index + 1).map(|(start, _)| *start);
        let mut end = next
            .or_else(|| duration_secs.filter(|duration| *duration > *start))
            .unwrap_or(start + 5.0);
        if end <= *start {
            end = start + 0.1;
        }
        segments.push(SubtitleSegment {
            start: *start,
            end,
            text: text.clone(),
        });
    }
    normalize_segments(segments)
}

/// LRC 元数据标签键（不区分大小写）：这些标签的值不是时间，解析失败时按元数据忽略。
///
/// 只有白名单键会被当作元数据；其它无法解析为时间的标签一律报错，避免 `[00:01,500]`
/// 这类手误让整行歌词被静默丢弃。`[offset:]` 仅作为元数据忽略，不参与时间轴调整。
pub(super) fn is_lrc_metadata_tag(tag: &str) -> bool {
    let key = tag
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        key.as_str(),
        "ar" | "al"
            | "ti"
            | "au"
            | "by"
            | "offset"
            | "re"
            | "ve"
            | "length"
            | "tool"
            | "kana"
            | "language"
    )
}

/// 支持 `[mm:ss.cc]` / `[mm:ss:cc]` / `[mm:ss.xxx]`（百分秒或毫秒）。
pub(super) fn parse_lrc_time(tag: &str) -> Option<f64> {
    let parts: Vec<&str> = tag.split(':').collect();
    let minutes: f64;
    let seconds: f64;
    match parts.as_slice() {
        [m, s] => {
            minutes = m.parse().ok()?;
            seconds = s.parse().ok()?;
        }
        [m, s, frac] => {
            minutes = m.parse().ok()?;
            let whole: f64 = s.parse().ok()?;
            let frac_value: f64 = frac.parse().ok()?;
            // 三位小数按毫秒，两位按百分秒。
            let divisor = if frac.len() == 3 { 1000.0 } else { 100.0 };
            seconds = whole + frac_value / divisor;
        }
        _ => return None,
    }
    let value = minutes * 60.0 + seconds;
    value.is_finite().then_some(value)
}
