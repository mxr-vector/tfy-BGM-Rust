//! 播放位置同步和字幕逐词时间轴。

use std::time::{Duration, Instant};

use gpui::SharedString;
use unicode_segmentation::UnicodeSegmentation;

use crate::library::SubtitleSegment;

#[derive(Clone, Copy)]
pub struct PlaybackSample {
    pub position: f64,
    pub duration: f64,
    pub rate: f32,
    pub playing: bool,
}

pub struct Clock {
    anchor: PlaybackSample,
    sampled_at: Instant,
}

impl Clock {
    pub fn new(anchor: PlaybackSample, sampled_at: Instant) -> Self {
        Self { anchor, sampled_at }
    }

    pub fn sync(&mut self, sample: PlaybackSample, now: Instant) -> bool {
        let changed = sample.playing != self.anchor.playing
            || sample.rate != self.anchor.rate
            || sample.duration != self.anchor.duration
            || (self.position(now) - sample.position).abs() > 0.25
            || (!sample.playing && sample.position != self.anchor.position);
        if changed || now.saturating_duration_since(self.sampled_at) >= Duration::from_secs(1) {
            *self = Self::new(sample, now);
        }
        changed
    }

    pub fn position(&self, now: Instant) -> f64 {
        let mut position = self.anchor.position;
        if self.anchor.playing {
            position += now.saturating_duration_since(self.sampled_at).as_secs_f64()
                * f64::from(self.anchor.rate);
        }
        if self.anchor.duration > 0.0 {
            position.min(self.anchor.duration)
        } else {
            position
        }
    }
}

struct Cue {
    start: f64,
    end: f64,
    text: SharedString,
    word_ends: Vec<usize>,
}

impl Cue {
    fn new(segment: SubtitleSegment) -> Self {
        let text = segment
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut word_ends = text
            .unicode_word_indices()
            .map(|(offset, word)| offset + word.len())
            .collect::<Vec<_>>();
        if word_ends.is_empty() {
            word_ends.extend(
                text.grapheme_indices(true)
                    .filter(|(_, grapheme)| !grapheme.trim().is_empty())
                    .map(|(offset, grapheme)| offset + grapheme.len()),
            );
        }
        if let Some(last) = word_ends.last_mut() {
            *last = text.len();
        }
        Self {
            start: segment.start,
            end: segment.end,
            text: text.into(),
            word_ends,
        }
    }

    fn sung_bytes(&self, position: f64) -> usize {
        let progress = ((position - self.start) / (self.end - self.start)).clamp(0.0, 1.0);
        let word = (progress * self.word_ends.len() as f64).floor() as usize;
        self.word_ends.get(word).copied().unwrap_or(self.text.len())
    }
}

pub struct Timeline(Vec<Cue>);

pub struct Frame<'a> {
    pub previous: Option<&'a SharedString>,
    pub active: Option<(&'a SharedString, usize)>,
    pub next: Option<&'a SharedString>,
}

impl Timeline {
    pub fn new(segments: Vec<SubtitleSegment>) -> Self {
        let mut cues = segments.into_iter().map(Cue::new).collect::<Vec<_>>();
        cues.sort_by(|a, b| a.start.total_cmp(&b.start));
        Self(cues)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn line(&self, index: usize) -> (f64, SharedString) {
        let cue = &self.0[index];
        (cue.start, cue.text.clone())
    }

    fn locate(&self, position: f64) -> (usize, Option<usize>) {
        let started = self.0.partition_point(|cue| cue.start <= position);
        let active = self.0[..started].iter().rposition(|cue| cue.end > position);
        (started, active)
    }

    pub fn follow_index(&self, position: f64) -> usize {
        let (started, active) = self.locate(position);
        active.unwrap_or_else(|| started.saturating_sub(1))
    }

    pub fn active_line(&self, position: f64) -> Option<(usize, usize)> {
        let (_, active) = self.locate(position);
        active.map(|index| (index, self.0[index].sung_bytes(position)))
    }

    pub fn frame(&self, position: f64) -> Frame<'_> {
        let (started, active) = self.locate(position);
        let previous = active
            .unwrap_or(started)
            .checked_sub(1)
            .map(|i| &self.0[i].text);
        let next = self
            .0
            .get(active.map_or(started, |i| i + 1))
            .map(|cue| &cue.text);
        let active = active.map(|i| (&self.0[i].text, self.0[i].sung_bytes(position)));
        Frame {
            previous,
            active,
            next,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_clock_advances_at_the_sample_rate_and_resyncs_pause_and_seeks() {
        let start = Instant::now();
        let mut sample = PlaybackSample {
            position: 2.,
            duration: 30.,
            playing: true,
            rate: 2.,
        };
        let mut clock = Clock::new(sample, start);
        let now = start + Duration::from_millis(250);
        assert_eq!(clock.position(now), 2.5);
        sample.position = 2.5;
        sample.rate = 0.5;
        clock.sync(sample, now);
        let now = now + Duration::from_millis(500);
        assert_eq!(clock.position(now), 2.75);

        sample.position = 18.;
        clock.sync(sample, now);
        assert_eq!(clock.position(now), 18.);
        sample.playing = false;
        sample.position = 18.125;
        clock.sync(sample, now + Duration::from_millis(250));
        assert_eq!(clock.position(now + Duration::from_secs(3)), 18.125);
        sample.position = 18.2;
        assert!(clock.sync(sample, now + Duration::from_secs(4)));
        assert_eq!(clock.position(now + Duration::from_secs(5)), 18.2);
        assert!(!clock.sync(sample, now + Duration::from_secs(5)));
    }

    #[test]
    fn highlights_utf8_word_boundaries_and_handles_subtitle_gaps() {
        let segment = |start, end, text: &str| SubtitleSegment {
            start,
            end,
            text: text.into(),
        };
        let timeline = Timeline::new(vec![segment(7., 8., "结束"), segment(2., 6., "hello 世界")]);
        assert_eq!(timeline.frame(1.).next.unwrap().as_ref(), "hello 世界");
        let frame = timeline.frame(4.5);
        let (active, sung) = frame.active.unwrap();
        assert_eq!(&active[..sung], "hello 世");
        assert_eq!(frame.next.unwrap().as_ref(), "结束");
        let gap = timeline.frame(6.5);
        assert!(gap.active.is_none());
        assert_eq!(gap.previous.unwrap().as_ref(), "hello 世界");
        assert_eq!(gap.next.unwrap().as_ref(), "结束");
        assert!(timeline.frame(8.).active.is_none());
        assert_eq!(timeline.follow_index(1.), 0);
        assert_eq!(timeline.follow_index(6.5), 0);
        assert_eq!(timeline.follow_index(8.), 1);
        assert_eq!(timeline.active_line(7.5).map(|(index, _)| index), Some(1));
        assert_eq!(timeline.line(1).0, 7.0);
    }
}
