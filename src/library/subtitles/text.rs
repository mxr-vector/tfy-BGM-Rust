//! SRT 和 LRC 的时间戳、原文解析与输出。

use super::SubtitleSegment;

pub const MAX_SEGMENTS: usize = 5000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SubtitleFormat {
    Srt,
    Lrc,
}

impl SubtitleFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Lrc => "lrc",
        }
    }

    pub fn parse(
        self,
        text: &str,
        durations: [Option<f64>; 2],
    ) -> Result<Vec<SubtitleSegment>, String> {
        let text = text.trim_start_matches('\u{feff}');
        let mut segments = match self {
            Self::Srt => parse_srt(text)?,
            Self::Lrc => parse_lrc(text, durations)?,
        };
        validate(&segments)?;
        segments.sort_by(|a, b| a.start.total_cmp(&b.start));
        Ok(segments)
    }

    pub fn render_named(self, title: &str, segments: &[SubtitleSegment]) -> String {
        let text = self.render(segments);
        if self == Self::Lrc {
            let title = title.replace(['\r', '\n'], " ");
            format!("[ar:]\n[ti:{title}]\n{text}")
        } else {
            text
        }
    }

    pub fn render(self, segments: &[SubtitleSegment]) -> String {
        use std::fmt::Write;
        let mut output = String::new();
        for (index, line) in segments.iter().enumerate() {
            match self {
                Self::Srt => {
                    let start = millis(line.start);
                    let end = millis(line.end).max(start.saturating_add(1));
                    let _ = writeln!(
                        output,
                        "{}\n{} --> {}\n{}\n",
                        index + 1,
                        timestamp(start, true),
                        timestamp(end, true),
                        line.text
                    );
                }
                Self::Lrc => {
                    let _ = writeln!(
                        output,
                        "[{}]{}",
                        timestamp(millis(line.start), false),
                        line.text
                    );
                }
            }
        }
        output
    }
}

pub(super) fn validate(segments: &[SubtitleSegment]) -> Result<(), String> {
    if segments.is_empty() {
        return Err("字幕至少需要一个分段".into());
    }
    if segments.len() > MAX_SEGMENTS {
        return Err(format!("字幕最多包含 {MAX_SEGMENTS} 个分段"));
    }
    for (index, line) in segments.iter().enumerate() {
        if !line.start.is_finite()
            || !line.end.is_finite()
            || line.start < 0.0
            || line.end <= line.start
            || line.end * 1000.0 >= u64::MAX as f64
        {
            return Err(format!("第 {} 个分段的时间无效", index + 1));
        }
        if line.text.trim().is_empty() || line.text.chars().count() > 2000 {
            return Err(format!("第 {} 个分段的文本须为 1 至 2000 字", index + 1));
        }
    }
    Ok(())
}

fn parse_srt(text: &str) -> Result<Vec<SubtitleSegment>, String> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut segments = Vec::new();
    let mut line = 0;
    while line < lines.len() {
        let value = lines[line].trim();
        if value.is_empty()
            || (srt_counter(value) && lines.get(line + 1).is_some_and(|next| next.contains("-->")))
        {
            line += 1;
            continue;
        }
        let (start, end) = value
            .split_once("-->")
            .and_then(|(start, end)| {
                Some((
                    parse_time(start.trim(), true)?,
                    parse_time(end.split_whitespace().next()?, true)?,
                ))
            })
            .filter(|(start, end)| end > start)
            .ok_or_else(|| format!("SRT 第 {} 行的时间轴无效", line + 1))?;
        let timestamp_line = line + 1;
        line += 1;
        let begin = line;
        while let Some(value) = lines.get(line).map(|line| line.trim()) {
            if value.is_empty()
                || value.contains("-->")
                || (srt_counter(value)
                    && lines.get(line + 1).is_some_and(|next| next.contains("-->")))
            {
                break;
            }
            line += 1;
        }
        if line == begin {
            return Err(format!("SRT 第 {timestamp_line} 行之后缺少字幕文本"));
        }
        let text = lines[begin..line]
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ");
        segments.push(SubtitleSegment { start, end, text });
        if segments.len() > MAX_SEGMENTS {
            break;
        }
    }
    Ok(segments)
}

fn srt_counter(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_lrc(text: &str, durations: [Option<f64>; 2]) -> Result<Vec<SubtitleSegment>, String> {
    let metadata = [
        "ar", "al", "ti", "au", "by", "offset", "re", "ve", "length", "tool", "kana", "language",
    ];
    let mut starts = Vec::<(f64, &str)>::new();
    for (index, line) in text.lines().enumerate() {
        let mut rest = line.trim();
        if rest.is_empty() {
            continue;
        }
        let error = || format!("LRC 第 {} 行的时间标签无效", index + 1);
        let mut times = Vec::new();
        while let Some(tag) = rest.strip_prefix('[') {
            let (value, following) = tag.split_once(']').ok_or_else(error)?;
            if let Some(start) = parse_time(value, false) {
                times.push(start);
                if starts.len() + times.len() > MAX_SEGMENTS {
                    return Err(format!("字幕最多包含 {MAX_SEGMENTS} 个分段"));
                }
            } else if times.is_empty()
                && value
                    .split_once(':')
                    .is_some_and(|(key, _)| metadata.contains(&key.to_ascii_lowercase().as_str()))
                && following.trim().is_empty()
            {
                rest = "";
                break;
            } else if times.is_empty() || value.as_bytes().first().is_some_and(u8::is_ascii_digit) {
                return Err(error());
            } else {
                break;
            }
            rest = following;
        }
        if times.is_empty() && !rest.is_empty() {
            return Err(error());
        }
        let lyric = rest.trim();
        if !times.is_empty() && lyric.is_empty() {
            return Err(format!("LRC 第 {} 行缺少字幕文本", index + 1));
        }
        starts.extend(times.into_iter().map(|start| (start, lyric)));
    }
    starts.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(starts
        .iter()
        .map(|&(start, text)| {
            let next = starts.partition_point(|&(candidate, _)| candidate <= start);
            let end = starts
                .get(next)
                .map(|&(end, _)| end)
                .or_else(|| {
                    durations
                        .into_iter()
                        .flatten()
                        .find(|end| end.is_finite() && *end > start)
                })
                .unwrap_or(start + 5.0);
            SubtitleSegment {
                start,
                end,
                text: text.into(),
            }
        })
        .collect())
}

fn parse_time(value: &str, hours_allowed: bool) -> Option<f64> {
    let (leading, secs) = value.rsplit_once(':')?;
    let (hours, mins) = match leading.split_once(':') {
        Some((hours, mins)) if hours_allowed => (number(hours)?, number(mins)?),
        Some(_) => return None,
        None => (0, number(leading)?),
    };
    let (seconds, fraction) = match secs.split_once([',', '.']) {
        Some((seconds, fraction)) if (1..=3).contains(&fraction.len()) => (
            number(seconds)?,
            number(fraction)? as f64 / 10u32.pow(fraction.len() as u32) as f64,
        ),
        Some(_) => return None,
        None => (number(secs)?, 0.),
    };
    (seconds < 60 && (!leading.contains(':') || mins < 60))
        .then_some(hours as f64 * 3600. + mins as f64 * 60. + seconds as f64 + fraction)
}

fn number(text: &str) -> Option<u64> {
    if !srt_counter(text) {
        return None;
    }
    text.parse().ok()
}

fn millis(seconds: f64) -> u64 {
    (seconds * 1000.).round() as u64
}

fn timestamp(ms: u64, srt: bool) -> String {
    let seconds = ms / 1000 % 60;
    let millis = ms % 1000;
    if srt {
        format!(
            "{:02}:{:02}:{seconds:02},{millis:03}",
            ms / 3_600_000,
            ms / 60_000 % 60
        )
    } else {
        format!("{:02}:{seconds:02}.{millis:03}", ms / 60_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_supports_old_and_unindexed_times_and_reports_the_original_line_number() {
        let raw = "\u{feff}12\r\n00:01,250 --> 00:00:02,500\r\n海风\r\n吹过\r\n\r\n00:03,000 --> 00:04,000\r\nEnd";
        let lines = SubtitleFormat::Srt.parse(raw, [None; 2]).unwrap();
        assert_eq!(lines[0].text, "海风 吹过");
        assert_eq!(lines[0].start, 1.25);
        assert_eq!(lines[1].end, 4.0);
        let bad = raw.replace("00:04,000", "00:01,000");
        assert!(
            SubtitleFormat::Srt
                .parse(&bad, [None; 2])
                .unwrap_err()
                .contains("第 6 行")
        );
    }

    #[test]
    fn lrc_expands_multiple_times_ignores_offset_and_resolves_the_final_end() {
        let text = "[ar:曲目]\n[offset:400]\n[00:01.120][00:02.40]行一\n[00:03.03]行二";
        let format = SubtitleFormat::Lrc;
        let lines = format.parse(text, [Some(6.), Some(7.)]).unwrap();
        assert_eq!(
            lines.iter().map(|c| c.end).collect::<Vec<_>>(),
            [2.4, 3.03, 6.]
        );
        assert_eq!(lines[0].start, 1.12);
        assert_eq!(format.parse(text, [None, Some(7.)]).unwrap()[2].end, 7.0);
        assert_eq!(format.parse(text, [None, None]).unwrap()[2].end, 8.03);
        assert!(format.parse("[unknown:value]", [None; 2]).is_err());
        assert!(format.parse("[00:01.00]x", [None; 2]).is_ok());
        assert!(
            format
                .parse(&format!("[00:01.00]{}", "字".repeat(2001)), [None; 2])
                .is_err()
        );
    }
}
