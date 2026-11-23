//! 基于波形相似度重叠相加的保音高变速。

use super::PcmTrack;

#[derive(Default)]
pub(super) struct Blend {
    previous: Option<usize>,
    current: Option<usize>,
    mix: f32,
}

impl Blend {
    pub(super) fn sample(&self, track: &PcmTrack, channel: usize) -> f32 {
        let channels = usize::from(track.channels.get());
        let read = |frame: Option<usize>| {
            frame.map_or(0.0, |frame| track.samples[frame * channels + channel])
        };
        let current = read(self.current);
        if self.mix == 1.0 || self.current == self.previous {
            return current;
        }
        let previous = read(self.previous);
        previous + (current - previous) * self.mix
    }
}

pub(super) struct TimeStretch {
    hop: usize,
    radius: i64,
    search_stride: usize,
    analysis_stride: usize,
    reference: Vec<[f32; 2]>,
    grain: Option<i64>,
    previous: Option<i64>,
    offset: usize,
}

impl TimeStretch {
    pub(super) fn new(sample_rate: usize) -> Self {
        let hop = (sample_rate / 50).max(1);
        let analysis_stride = (sample_rate / 4_000).max(1);
        Self {
            hop,
            radius: (sample_rate / 80).max(1) as i64,
            search_stride: (sample_rate / 12_000).max(1),
            analysis_stride,
            reference: Vec::with_capacity(hop.div_ceil(analysis_stride)),
            grain: None,
            previous: None,
            offset: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.grain = None;
        self.previous = None;
        self.offset = 0;
    }

    pub(super) fn next(
        &mut self,
        track: &PcmTrack,
        nominal: i64,
        speed: f64,
        region: Option<(usize, usize)>,
    ) -> Blend {
        let range = SampleRange::new(track, region);
        if self.offset == self.hop || self.grain.is_none() {
            self.previous = self.grain.map(|start| start + self.hop as i64);
            self.grain = Some(match self.previous {
                Some(previous) if speed != 1.0 => self.align(&range, previous, nominal),
                _ => nominal,
            });
            self.offset = 0;
        }
        let offset = self.offset as i64;
        let result = Blend {
            previous: self.previous.and_then(|start| range.frame(start + offset)),
            current: self.grain.and_then(|start| range.frame(start + offset)),
            mix: self
                .previous
                .map_or(1.0, |_| self.offset as f32 / self.hop as f32),
        };
        self.offset += 1;
        result
    }

    fn align(&mut self, range: &SampleRange<'_>, previous: i64, nominal: i64) -> i64 {
        if range.frame(previous) == range.frame(nominal) {
            return nominal;
        }
        self.reference.clear();
        let mut reference_energy = 0.0;
        for offset in (0..self.hop).step_by(self.analysis_stride) {
            let [a, b] = range.stereo(previous + offset as i64);
            self.reference.push([a, b]);
            reference_energy += a * a + b * b;
        }
        if reference_energy < 1e-8 {
            return nominal;
        }

        let (first, last) = range.candidates(nominal, self.radius);
        let mut best = (nominal, f32::NEG_INFINITY);
        let compare = |best: &mut (i64, f32), candidate: i64| {
            let mut dot = 0.0;
            let mut energy = 0.0;
            for (index, [a, b]) in self.reference.iter().enumerate() {
                let [x, y] = range.stereo(candidate + (index * self.analysis_stride) as i64);
                dot += a * x + b * y;
                energy += x * x + y * y;
            }
            let distance = (candidate - nominal).abs() as f32 / self.radius as f32;
            let score = 2.0 * dot / (reference_energy + energy) - 0.015 * distance;
            if score > best.1 {
                *best = (candidate, score);
            }
        };
        compare(&mut best, nominal);
        for candidate in (first..=last).step_by(self.search_stride) {
            compare(&mut best, candidate);
        }
        let coarse = best.0;
        let fine_radius = self.search_stride as i64 - 1;
        for candidate in (coarse - fine_radius).max(first)..=(coarse + fine_radius).min(last) {
            compare(&mut best, candidate);
        }
        best.0
    }
}

struct SampleRange<'a> {
    samples: &'a [f32],
    channels: usize,
    end: i64,
    loop_start: Option<i64>,
}

impl<'a> SampleRange<'a> {
    fn new(track: &'a PcmTrack, region: Option<(usize, usize)>) -> Self {
        Self {
            samples: &track.samples,
            channels: usize::from(track.channels.get()),
            end: region.map_or_else(|| track.frames(), |(_, end)| end) as i64,
            loop_start: region.map(|(start, _)| start as i64),
        }
    }

    fn frame(&self, frame: i64) -> Option<usize> {
        if (self.loop_start.unwrap_or(0)..self.end).contains(&frame) {
            Some(frame as usize)
        } else {
            self.loop_start
                .map(|start| (start + (frame - start).rem_euclid(self.end - start)) as usize)
        }
    }

    fn stereo(&self, frame: i64) -> [f32; 2] {
        self.frame(frame).map_or([0.0, 0.0], |index| {
            let sample = index * self.channels;
            [
                self.samples[sample],
                self.samples[sample + usize::from(self.channels > 1)],
            ]
        })
    }

    fn candidates(&self, nominal: i64, radius: i64) -> (i64, i64) {
        if self.loop_start.is_some() {
            (nominal - radius, nominal + radius)
        } else {
            (
                (nominal - radius).max(0),
                (nominal + radius).min(self.end - 1),
            )
        }
    }
}
