use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TtsSegment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
}

pub fn parse_script(text: &str, format: &str) -> Result<Vec<TtsSegment>> {
    ensure!(text.len() <= 1_000_000, "文本不能超过 1 MB");
    let text = text
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut segments = match format {
        "txt" => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| TtsSegment {
                text: line.to_owned(),
                start_ms: None,
                end_ms: None,
            })
            .collect(),
        "lrc" => parse_lrc(&text),
        "srt" => parse_srt(&text)?,
        _ => bail!("仅支持 .txt、.lrc 与 .srt"),
    };
    validate_segments(&mut segments, 30 * 60 * 1000)?;
    Ok(segments)
}

pub fn validate_segments(segments: &mut [TtsSegment], max_ms: u64) -> Result<()> {
    ensure!(
        (1..=5000).contains(&segments.len()),
        "请输入 1–5000 段合成文本"
    );
    let timed = segments.iter().any(|cue| cue.start_ms.is_some());
    for (index, segment) in segments.iter().enumerate() {
        ensure!(
            !segment.text.trim().is_empty()
                && !segment.text.contains('\0')
                && segment.text.chars().count() <= 5000,
            "第 {} 段文本为空、包含无效字符或超过 5000 字，请拆分后重试",
            index + 1
        );
        if timed && segment.start_ms.is_none() {
            bail!("不能混用普通文本与带时间码的字幕");
        }
        match (segment.start_ms, segment.end_ms) {
            (Some(start), Some(end)) => ensure!(
                start < end && end <= max_ms,
                "第 {} 条字幕的时槽无效或超出 30 分钟",
                index + 1
            ),
            (Some(start), None) => {
                ensure!(start < max_ms, "第 {} 条字幕的起点超出 30 分钟", index + 1)
            }
            (None, Some(_)) => bail!("字幕缺少开始时间"),
            _ => (),
        }
    }
    if timed {
        segments.sort_by_key(|cue| cue.start_ms);
    }
    Ok(())
}

fn parse_lrc(text: &str) -> Vec<TtsSegment> {
    let mut segments = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut starts = Vec::new();
        while let Some(tag) = rest.strip_prefix('[').and_then(|s| s.split_once(']')) {
            if let Some(timestamp) = timestamp(tag.0, false) {
                starts.push(timestamp);
            } else if !matches!(
                tag.0
                    .split_once(':')
                    .map(|(key, _)| key.to_ascii_lowercase())
                    .as_deref(),
                Some("ti" | "ar" | "al" | "by" | "re" | "ve" | "length" | "offset" | "la" | "id")
            ) {
                break;
            }
            rest = tag.1.trim_start();
        }
        if !rest.is_empty() || !starts.is_empty() {
            segments.extend(starts.into_iter().map(|start_ms| TtsSegment {
                text: rest.to_owned(),
                start_ms: Some(start_ms),
                end_ms: None,
            }));
        }
    }
    segments.sort_by_key(|cue| cue.start_ms);
    let mut merged: Vec<TtsSegment> = Vec::with_capacity(segments.len());
    for cue in segments {
        if let Some(previous) = merged
            .last_mut()
            .filter(|previous| previous.start_ms == cue.start_ms)
        {
            if !cue.text.is_empty() {
                if !previous.text.is_empty() {
                    previous.text.push(' ');
                }
                previous.text.push_str(&cue.text);
            }
        } else {
            merged.push(cue);
        }
    }
    let mut segments = merged;
    let mut next_start = None;
    let mut end = None;
    for cue in segments.iter_mut().rev() {
        if cue.start_ms != next_start {
            end = next_start;
            next_start = cue.start_ms;
        }
        cue.end_ms = end;
    }
    segments.retain(|cue| !cue.text.is_empty());
    segments
}

fn parse_srt(text: &str) -> Result<Vec<TtsSegment>> {
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    lines
        .split(|line| line.is_empty())
        .filter(|block| !block.is_empty())
        .enumerate()
        .map(|(index, block)| {
            let parse = || -> Option<TtsSegment> {
                let block = if block[0].bytes().all(|char| char.is_ascii_digit()) {
                    &block[1..]
                } else {
                    block
                };
                let (timing, lines) = block.split_first()?;
                let (start, end) = timing.split_once("-->")?;
                let text = lines.join(" ");
                (!text.is_empty()).then_some(())?;
                Some(TtsSegment {
                    text,
                    start_ms: Some(timestamp(start.trim(), true)?),
                    end_ms: Some(timestamp(end.split_whitespace().next()?, true)?),
                })
            };
            parse()
                .ok_or_else(|| anyhow::anyhow!("第 {} 个 SRT 字幕块的时间码或文本无效", index + 1))
        })
        .collect()
}

fn timestamp(value: &str, srt: bool) -> Option<u64> {
    let delimiter = if srt { ',' } else { '.' };
    let (time, fraction) = match value.split_once(delimiter) {
        Some((time, fraction))
            if !fraction.is_empty() && fraction.bytes().all(|c| c.is_ascii_digit()) =>
        {
            (time, fraction)
        }
        None if !srt => (value, "0"),
        _ => return None,
    };
    if srt && fraction.len() != 3 {
        return None;
    }
    let millis = fraction
        .bytes()
        .take(3)
        .zip([100, 10, 1])
        .map(|(c, scale)| u64::from(c - b'0') * scale)
        .sum::<u64>();
    let mut parts = time.rsplit(':').map(|p| p.parse::<u64>());
    let seconds = parts.next()?.ok()?;
    let minutes = parts.next()?.ok()?;
    let hours = if srt { parts.next()?.ok()? } else { 0 };
    if parts.next().is_some() || seconds >= 60 || (srt && minutes >= 60) {
        return None;
    }
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds * 1_000)?
        .checked_add(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_text_lrc_and_multiline_srt() {
        let txt = parse_script("\u{feff}第一行\r\n\r\n第二行", "txt").unwrap();
        assert_eq!(txt.len(), 2);
        assert_eq!(txt[0].text, "第一行");
        assert_eq!(txt[0].start_ms, None);
        let lrc = parse_script("[ti:title]\n[ar:artist]\n[offset:-200]\nuntimed\n[00:02.5][00:03.50]second\n[00:01.25] first", "lrc").unwrap();
        assert_eq!(
            lrc.iter()
                .map(|cue| (cue.start_ms, cue.end_ms))
                .collect::<Vec<_>>(),
            vec![
                (Some(1250), Some(2500)),
                (Some(2500), Some(3500)),
                (Some(3500), None)
            ]
        );
        let srt = parse_script("1\r\n00:00:01,200 --> 00:00:03,400\r\nfirst\r\nsecond\r\n  \r\n2\r\n00:00:04,000 --> 00:00:05,000\r\nthird", "srt").unwrap();
        assert_eq!(srt[0].text, "first second");
        assert_eq!(srt[1].end_ms, Some(5000));
    }

    #[test]
    fn lrc_empty_cue_marks_the_end_without_being_spoken() {
        let cues = parse_script("[00:01]Hello\n[00:03]\n[00:06]World", "lrc").unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].end_ms, Some(3000));
        assert_eq!(cues[1].start_ms, Some(6000));
    }

    #[test]
    fn rejects_invalid_timestamps_without_panicking_on_unicode() {
        assert!(parse_script("1\n00:00:02,000 --> 00:00:01,000\nHey", "srt").is_err());
        assert!(parse_script("[00:01.你a]Hello", "lrc").is_err());
    }
}
