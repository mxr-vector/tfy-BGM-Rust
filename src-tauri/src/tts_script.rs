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
        "txt" => parse_plain_text(&text),
        "lrc" => parse_lrc(&text),
        "srt" => parse_srt(&text)?,
        _ => bail!("仅支持 .txt、.lrc 与 .srt"),
    };
    validate_segments(&mut segments, 30 * 60 * 1000)?;
    Ok(segments)
}

const MAX_PLAIN_CHARS: usize = 180;
const MAX_SENTENCES_PER_CHUNK: usize = 3;

fn sentence_count(text: &str) -> usize {
    text.chars().filter(|&c| is_sentence_mark(c)).count()
}

fn parse_plain_text(text: &str) -> Vec<TtsSegment> {
    let mut segments = Vec::new();
    let mut lines = text.lines().map(str::trim).peekable();
    while let Some(first) = lines.next() {
        if first.is_empty() {
            continue;
        }
        let mut paragraph = first.to_owned();
        while lines.peek().is_some_and(|line| !line.is_empty()) {
            let line = lines.next().unwrap();
            if paragraph.chars().last().is_some_and(|c| c.is_ascii())
                && line
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                paragraph.push(' ');
            }
            paragraph.push_str(line);
        }
        let mut paragraph_segments: Vec<TtsSegment> = Vec::new();
        let mut rest = paragraph.trim();
        while !rest.is_empty() {
            let (text, next) = rest.split_at(plain_chunk_end(rest));
            let text = text.trim_end();
            if let Some(previous) = paragraph_segments.last_mut().filter(|previous| {
                previous.text.chars().count() + text.chars().count() <= MAX_PLAIN_CHARS
                    && sentence_count(&previous.text) < MAX_SENTENCES_PER_CHUNK
            }) {
                if previous.text.chars().last().is_some_and(|c| c.is_ascii())
                    && text
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    previous.text.push(' ');
                }
                previous.text.push_str(text);
            } else {
                paragraph_segments.push(TtsSegment {
                    text: text.to_owned(),
                    start_ms: None,
                    end_ms: None,
                });
            }
            rest = next.trim_start();
        }
        segments.extend(paragraph_segments);
    }
    segments
}

fn plain_chunk_end(text: &str) -> usize {
    let chars: Vec<_> = text.char_indices().collect();
    if chars.len() <= MAX_PLAIN_CHARS {
        return chars
            .iter()
            .enumerate()
            .find(|(index, (_, c))| {
                is_sentence_mark(*c)
                    && chars
                        .get(index + 1)
                        .is_some_and(|(_, next)| !is_sentence_mark(*next))
            })
            .map_or(text.len(), |(index, _)| {
                chars.get(index + 1).map_or(text.len(), |(byte, _)| *byte)
            });
    }

    let minimum = MAX_PLAIN_CHARS / 3;
    let limit = chars[MAX_PLAIN_CHARS].0;
    let mut sentence = None;
    let mut phrase = None;
    let mut word = None;
    for (index, &(_, c)) in chars.iter().enumerate().take(MAX_PLAIN_CHARS) {
        let next = chars.get(index + 1).map_or(' ', |(_, next)| *next);
        let boundary = chars.get(index + 1).map_or(text.len(), |(byte, _)| *byte);
        if is_sentence_mark(c) && !(c == '.' && c.is_ascii() && next.is_ascii_alphanumeric()) {
            sentence = Some((index + 1, boundary));
        } else if matches!(c, '，' | ',' | '、' | '；' | ';' | '：' | ':')
            && !(c.is_ascii() && next.is_ascii_digit())
        {
            phrase = Some((index + 1, boundary));
        } else if c.is_whitespace() && !next.is_whitespace() {
            word = Some((index + 1, boundary));
        }
    }
    sentence
        .filter(|(count, _)| *count >= minimum)
        .or(phrase.filter(|(count, _)| *count >= minimum))
        .or(word.filter(|(count, _)| *count >= minimum))
        .map_or(limit, |(_, boundary)| boundary)
}

pub fn is_sentence_mark(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '.' | '!' | '?' | '…')
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
    fn plain_text_merges_wrapped_paragraphs_and_splits_at_sentence_marks() {
        let cues = parse_script(
            "这是第一句话。\n这是第二句话！\n这里继续\n下一行。\n\nEnglish line\ncontinued.",
            "txt",
        )
        .unwrap();
        assert_eq!(
            cues.iter().map(|cue| cue.text.as_str()).collect::<Vec<_>>(),
            [
                "这是第一句话。这是第二句话！这里继续下一行。",
                "English line continued."
            ]
        );
    }

    #[test]
    fn plain_text_groups_short_sentences_for_smoother_synthesis() {
        let cues = parse_script("第一句。\n第二句！\n第三句？\n第四句。", "txt").unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "第一句。第二句！第三句？");
        assert_eq!(cues[1].text, "第四句。");
    }

    #[test]
    fn plain_text_splits_long_input_without_breaking_utf8() {
        let text = format!("{}。{}", "前文".repeat(100), "后文".repeat(100));
        let cues = parse_script(&text, "txt").unwrap();
        assert!(cues.len() >= 2);
        assert!(
            cues.iter()
                .all(|cue| cue.text.chars().count() <= MAX_PLAIN_CHARS)
        );
        assert_eq!(
            cues.iter()
                .map(|cue| cue.text.as_str())
                .collect::<String>()
                .replace('。', ""),
            text.replace('。', "")
        );
        assert!(
            cues.iter()
                .all(|cue| cue.text.chars().count() <= MAX_PLAIN_CHARS)
        );
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
