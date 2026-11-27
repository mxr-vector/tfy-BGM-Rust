//! 合成脚本分段（TXT/SRT/LRC）与原始采样率时间轴。

use serde::{Deserialize, Serialize};

use super::voices;

const MAX_BYTES: usize = 256 * 1024;
const MAX_SEGMENTS: usize = 5000;
const MAX_PLAIN_CHARS: usize = 120;
const MAX_SENTENCES_PER_CHUNK: usize = 3;
pub(super) const MAX_SECONDS: f64 = 30.0 * 60.0;
const MAX_MS: u64 = 30 * 60 * 1000;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Txt,
    Srt,
    Lrc,
}

impl Format {
    pub const ALL: [Self; 3] = [Self::Txt, Self::Srt, Self::Lrc];

    pub fn label(self) -> &'static str {
        match self {
            Self::Txt => "TXT",
            Self::Srt => "SRT",
            Self::Lrc => "LRC",
        }
    }

    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_uppercase();
        Self::ALL
            .into_iter()
            .find(|format| format.label() == extension)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub text: String,
    pub start: Option<f64>,
    pub end: Option<f64>,
}

pub fn parse(format: Format, text: &str) -> Result<Vec<Segment>, String> {
    if text.len() > MAX_BYTES {
        return Err("合成文本最多 256 KiB".into());
    }
    let text = text.trim_start_matches('\u{feff}').trim();
    if text.is_empty() {
        return Err("请输入要合成的文本".into());
    }
    if text.contains('\0') {
        return Err("合成文本含有无效字符".into());
    }
    let mut segments = match format {
        Format::Txt => parse_plain(text),
        Format::Lrc => parse_lrc(text),
        Format::Srt => parse_srt(text)?,
    };
    validate_segments(&mut segments)?;
    Ok(segments)
}

fn sentence_count(text: &str) -> usize {
    let mut chars = text.chars().peekable();
    let mut count = 0;
    while let Some(c) = chars.next() {
        if is_sentence_mark(c) && chars.peek().is_none_or(|next| !is_sentence_mark(*next)) {
            count += 1;
        }
    }
    count
}

fn parse_plain(text: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.lines().map(str::trim).peekable();
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
        let mut paragraph_segments: Vec<Segment> = Vec::new();
        let mut rest = paragraph.trim();
        while !rest.is_empty() {
            let (chunk, next) = rest.split_at(plain_chunk_end(rest));
            let chunk = chunk.trim_end();
            if let Some(previous) = paragraph_segments.last_mut().filter(|previous| {
                previous.text.chars().count() + chunk.chars().count() <= MAX_PLAIN_CHARS
                    && sentence_count(&previous.text) < MAX_SENTENCES_PER_CHUNK
            }) {
                if previous.text.chars().last().is_some_and(|c| c.is_ascii())
                    && chunk
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    previous.text.push(' ');
                }
                previous.text.push_str(chunk);
            } else {
                paragraph_segments.push(Segment {
                    text: chunk.to_owned(),
                    start: None,
                    end: None,
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

fn is_sentence_mark(c: char) -> bool {
    matches!(c, '。' | '！' | '？' | '.' | '!' | '?' | '…')
}

fn parse_lrc(text: &str) -> Vec<Segment> {
    let mut raw = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut starts = Vec::new();
        while let Some(tag) = rest.strip_prefix('[').and_then(|s| s.split_once(']')) {
            if let Some(ms) = timestamp(tag.0, false) {
                starts.push(ms);
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
            for start_ms in starts {
                raw.push((Some(start_ms), None::<u64>, rest.to_owned()));
            }
        }
    }
    raw.sort_by_key(|(start, _, _)| *start);
    let mut merged: Vec<(Option<u64>, Option<u64>, String)> = Vec::with_capacity(raw.len());
    for cue in raw {
        if let Some(previous) = merged.last_mut().filter(|previous| previous.0 == cue.0) {
            if !cue.2.is_empty() {
                if !previous.2.is_empty() {
                    previous.2.push(' ');
                }
                previous.2.push_str(&cue.2);
            }
        } else {
            merged.push(cue);
        }
    }
    let mut next_start = None;
    let mut end = None;
    for cue in merged.iter_mut().rev() {
        if cue.0 != next_start {
            end = next_start;
            next_start = cue.0;
        }
        cue.1 = end;
    }
    merged
        .into_iter()
        .filter(|(_, _, text)| !text.is_empty())
        .map(|(start_ms, end_ms, text)| Segment {
            text,
            start: start_ms.map(|ms| ms as f64 / 1000.0),
            end: end_ms.map(|ms| ms as f64 / 1000.0),
        })
        .collect()
}

fn parse_srt(text: &str) -> Result<Vec<Segment>, String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<_> = normalized.lines().map(str::trim).collect();
    lines
        .split(|line| line.is_empty())
        .filter(|block| !block.is_empty())
        .enumerate()
        .map(|(index, block)| {
            let parse = || -> Option<Segment> {
                let block = if block[0].bytes().all(|char| char.is_ascii_digit()) {
                    &block[1..]
                } else {
                    block
                };
                let (timing, lines) = block.split_first()?;
                let (start, end) = timing.split_once("-->")?;
                let text = lines.join(" ");
                (!text.is_empty()).then_some(())?;
                let start_ms = timestamp(start.trim(), true)?;
                let end_ms = timestamp(end.split_whitespace().next()?, true)?;
                Some(Segment {
                    text,
                    start: Some(start_ms as f64 / 1000.0),
                    end: Some(end_ms as f64 / 1000.0),
                })
            };
            parse().ok_or_else(|| format!("第 {} 个 SRT 字幕块的时间码或文本无效", index + 1))
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

fn validate_segments(segments: &mut [Segment]) -> Result<(), String> {
    if !(1..=MAX_SEGMENTS).contains(&segments.len()) {
        return Err(format!("请输入 1–{MAX_SEGMENTS} 段合成文本"));
    }
    let timed = segments.iter().any(|cue| cue.start.is_some());
    for (index, segment) in segments.iter().enumerate() {
        if segment.text.trim().is_empty()
            || segment.text.contains('\0')
            || segment.text.chars().count() > 5000
        {
            return Err(format!(
                "第 {} 段文本为空、包含无效字符或超过 5000 字，请拆分后重试",
                index + 1
            ));
        }
        if timed && segment.start.is_none() {
            return Err("不能混用普通文本与带时间码的字幕".into());
        }
        match (segment.start, segment.end) {
            (Some(start), Some(end)) => {
                if !(start < end && end <= MAX_SECONDS) {
                    return Err(format!("第 {} 条字幕的时槽无效或超出 30 分钟", index + 1));
                }
            }
            (Some(start), None) => {
                if !(start < MAX_SECONDS) {
                    return Err(format!("第 {} 条字幕的起点超出 30 分钟", index + 1));
                }
            }
            (None, Some(_)) => return Err("字幕缺少开始时间".into()),
            (None, None) => (),
        }
    }
    if timed {
        segments.sort_by(|left, right| {
            left.start
                .unwrap_or(0.0)
                .total_cmp(&right.start.unwrap_or(0.0))
        });
    }
    Ok(())
}

pub struct Timeline {
    pub samples: Vec<f32>,
    pub rate: u32,
    cursor: usize,
}

impl Timeline {
    pub fn new(rate: u32) -> Result<Self, String> {
        if !(8_000..=96_000).contains(&rate) {
            return Err("模型返回了无效采样率".into());
        }
        Ok(Self {
            samples: Vec::new(),
            rate,
            cursor: 0,
        })
    }

    pub fn fit_speed(&self, segment: &Segment, len: usize) -> Option<f32> {
        let capacity = self.slot(segment)?;
        (len > capacity && capacity > 0)
            .then(|| (len as f32 / capacity as f32 * 1.05).clamp(1.0, 2.0))
    }

    pub fn slot(&self, segment: &Segment) -> Option<usize> {
        let end = self.frames(segment.end?);
        Some(end.saturating_sub(self.frames(segment.start?)))
    }

    pub fn append(
        &mut self,
        number: usize,
        segment: &Segment,
        voice: &[f32],
    ) -> Result<(), String> {
        if voice.is_empty() || voice.iter().any(|sample| !sample.is_finite()) {
            return Err(format!("第 {number} 段没有生成有效音频"));
        }
        if self
            .slot(segment)
            .is_some_and(|capacity| voice.len() > capacity)
        {
            return Err(format!(
                "第 {number} 段语音超过可用时间，请缩短文本或延长字幕时槽"
            ));
        }
        let start = segment.start.map_or(self.cursor, |time| self.frames(time));
        let end = start + voice.len();
        if end > self.frames(MAX_SECONDS) || end * 2 > usize::MAX / 2 {
            return Err(format!("第 {number} 段超过 30 分钟输出上限"));
        }
        let length = end.max(segment.end.map_or(0, |time| self.frames(time)));
        if length > self.samples.len() {
            self.samples.resize(length, 0.0);
        }
        for (output, &sample) in self.samples[start..end].iter_mut().zip(voice) {
            *output = (*output + sample).clamp(-1.0, 1.0);
        }
        if segment.start.is_none() {
            self.cursor = end + voices::pause_after(&segment.text, self.rate);
        } else {
            self.cursor = self.cursor.max(end);
        }
        let _ = MAX_MS;
        Ok(())
    }

    fn frames(&self, seconds: f64) -> usize {
        (seconds * f64::from(self.rate)).round() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_text_breaks_and_unbounded_final_lrc_line() {
        let txt = parse(
            Format::Txt,
            "第一句。\n第二句！\n第三句？\n第四句。\n\nEnglish line\ncontinued.",
        )
        .unwrap();
        assert_eq!(
            txt.iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            [
                "第一句。第二句！第三句？",
                "第四句。",
                "English line continued."
            ]
        );
        let lrc = parse(
            Format::Lrc,
            "[ti:title]\n[offset:-200]\nuntimed\n[00:02.50]乙\n[00:01.25]甲",
        )
        .unwrap();
        assert_eq!(lrc[0].text, "甲");
        assert_eq!(lrc[0].start, Some(1.25));
        assert_eq!(lrc[0].end, Some(2.5));
        assert_eq!(lrc[1].text, "乙");
        assert_eq!(lrc[1].end, None);
    }

    #[test]
    fn places_audio_without_filling_gaps_and_rejects_long_segments() {
        let lines = parse(
            Format::Srt,
            "1\n00:00:01,000 --> 00:00:02,000\n你好\n\n2\n00:00:03,000 --> 00:00:03,100\n短句",
        )
        .unwrap();
        let mut audio = Timeline::new(24_000).unwrap();
        audio.append(1, &lines[0], &[0.5; 8_000]).unwrap();
        assert!(audio.samples[..24_000].iter().all(|&sample| sample == 0.0));
        assert_eq!(audio.samples.len(), 48_000);
        assert!(audio.fit_speed(&lines[1], 3_000).unwrap() > 1.0);
        assert!(
            audio
                .append(2, &lines[1], &[0.5; 3_000])
                .unwrap_err()
                .contains("第 2 段")
        );
        audio.append(2, &lines[1], &[0.25; 2_000]).unwrap();
        assert_eq!(audio.samples[72_000], 0.25);
    }
}
