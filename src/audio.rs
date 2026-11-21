//! 本地 MP3 播放。GPUI 不管音频，这里用 rodio 0.23 走系统输出设备。
//!
//! 与主线的 Web Audio 引擎对齐：整曲解码成 f32 PCM 再播放，A/B 循环区间才能在
//! 采样级无缝回绕，播放位置也直接由帧序号给出，不依赖 rodio 的变速时间轴。
//! 倍速沿用 rodio 的采样率缩放，音高会一起变化。浏览器版默认保音高，这里做不到同等效果。

use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::conversions::SampleTypeConverter;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

/// 整曲解码的安全上限（30 分钟约 690MB），超出直接拒绝，避免内存占用过高。
const MAX_DECODE_SECONDS: u64 = 30 * 60;

/// 共享播放内核：界面线程写位置与循环区间，音频线程在 `next()` 里逐帧消费。
#[derive(Default)]
struct Core {
    /// 下一帧的序号（按声道交织后计）。
    frame: u64,
    /// 循环区间（帧序号，左闭右开）；终点到达后回绕到起点。
    loop_region: Option<(u64, u64)>,
}

/// 解码完成的整曲 PCM 数据，在切歌 / 改倍速 / 改循环点之间共享，不重复解码。
struct PcmTrack {
    samples: Arc<Vec<f32>>,
    channels: u16,
    sample_rate: u32,
    frames: u64,
}

/// 喂给 rodio 的音源；循环回绕与位置推进都在这里发生，天然采样级精确。
struct TrackSource {
    track: Arc<PcmTrack>,
    core: Arc<Mutex<Core>>,
}

impl Iterator for TrackSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let mut core = self.core.lock().ok()?;
        let (start, end) = core
            .loop_region
            .map(|(start, end)| (start.min(self.track.frames), end.min(self.track.frames)))
            .filter(|(start, end)| end > start)
            .unwrap_or((0, self.track.frames));
        if core.frame >= end {
            if core.loop_region.is_some() {
                core.frame = start;
            } else {
                // 无循环区间时走完整曲，交给上层的队列计数判断「播完」。
                return None;
            }
        }
        let sample = self.track.samples.get(core.frame as usize).copied()?;
        core.frame += 1;
        Some(sample)
    }
}

impl Source for TrackSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        rodio::SampleRate::new(self.track.sample_rate).unwrap_or(rodio::SampleRate::MAX)
    }

    fn channels(&self) -> rodio::ChannelCount {
        rodio::ChannelCount::new(self.track.channels).unwrap_or(rodio::ChannelCount::MAX)
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(duration_of_frames(
            self.track.frames,
            self.track.sample_rate,
        ))
    }
}

pub struct AudioEngine {
    sink: Option<MixerDeviceSink>,
    player: Option<Player>,
    track: Option<Arc<PcmTrack>>,
    core: Option<Arc<Mutex<Core>>>,
    speed: f32,
    volume: f32,
    open_error: Option<String>,
}

impl AudioEngine {
    pub fn open() -> Self {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => Self {
                sink: Some(sink),
                player: None,
                track: None,
                core: None,
                speed: 1.0,
                volume: 1.0,
                open_error: None,
            },
            Err(error) => Self {
                sink: None,
                player: None,
                track: None,
                core: None,
                speed: 1.0,
                volume: 1.0,
                open_error: Some(format!("无法打开音频设备：{error}")),
            },
        }
    }

    pub fn open_error(&self) -> Option<&str> {
        self.open_error.as_deref()
    }

    /// 队列里还有没有正在解码的音频。刚 append 后音频线程可能晚一拍才计数。
    pub fn queued(&self) -> usize {
        self.player.as_ref().map(Player::len).unwrap_or(0)
    }

    pub fn is_paused(&self) -> bool {
        self.player.as_ref().is_some_and(Player::is_paused)
    }

    /// 文件内的播放位置，由帧序号换算，与倍速无关。
    pub fn file_position(&self) -> Duration {
        let Some((track, core)) = self.track.as_ref().zip(self.core.as_ref()) else {
            return Duration::ZERO;
        };
        let frame = core.lock().map(|core| core.frame).unwrap_or(0);
        duration_of_frames(frame, track.sample_rate)
    }

    /// 整曲解码并开始播放。太长的曲目直接拒绝，避免界面线程长时间阻塞。
    pub fn load(&mut self, path: &Path) -> Result<Duration, String> {
        let sink = self.sink.as_ref().ok_or_else(|| {
            self.open_error
                .clone()
                .unwrap_or_else(|| "音频设备不可用".into())
        })?;
        let file = File::open(path).map_err(|error| format!("无法读取音频：{error}"))?;
        let decoder = Decoder::try_from(file).map_err(|error| format!("无法解码 MP3：{error}"))?;
        let sample_rate = decoder.sample_rate().get();
        let channels = decoder.channels().get();
        if let Some(duration) = decoder.total_duration()
            && duration.as_secs() > MAX_DECODE_SECONDS
        {
            return Err(format!(
                "时长 {} 超过 {} 分钟，整曲解码内存占用过高",
                format_time(duration),
                MAX_DECODE_SECONDS / 60
            ));
        }

        let samples: Arc<Vec<f32>> =
            Arc::new(SampleTypeConverter::<_, f32>::new(decoder).collect());
        let frames = (samples.len() as u64) / u64::from(channels);
        if frames == 0 {
            return Err("音频文件为空或无法解码".into());
        }
        let duration = duration_of_frames(frames, sample_rate);
        if duration.as_secs() > MAX_DECODE_SECONDS {
            return Err(format!(
                "时长 {} 超过 {} 分钟，整曲解码内存占用过高",
                format_time(duration),
                MAX_DECODE_SECONDS / 60
            ));
        }

        let track = Arc::new(PcmTrack {
            samples,
            channels,
            sample_rate,
            frames,
        });
        let core = Arc::new(Mutex::new(Core::default()));

        // 换一个新的 Player，旧实例 drop 时会停掉上一首，避免 clear() 在界面线程上等待。
        let player = Player::connect_new(sink.mixer());
        player.set_speed(self.speed);
        player.set_volume(self.volume);
        player.append(TrackSource {
            track: track.clone(),
            core: core.clone(),
        });
        player.play();

        self.player = Some(player);
        self.track = Some(track);
        self.core = Some(core);
        Ok(duration)
    }

    /// 设置 A/B 循环区间（秒）；区间不足 0.1 秒视为无效，等价于关闭循环。
    /// 播放位置落在区间外时直接跳到区间起点，与主线的 setLoopRegion 一致。
    pub fn set_loop_region(&mut self, region: Option<(f64, f64)>) {
        let Some((track, core)) = self.track.as_ref().zip(self.core.as_ref()) else {
            return;
        };
        let Ok(mut core) = core.lock() else {
            return;
        };
        let region = region.and_then(|(start, end)| {
            let start = seconds_to_frames(start, track.sample_rate).min(track.frames);
            let end = seconds_to_frames(end, track.sample_rate).min(track.frames);
            (end.saturating_sub(start) >= track.sample_rate as u64 / 10).then_some((start, end))
        });
        match region {
            Some((start, end)) => {
                if core.frame < start || core.frame >= end {
                    core.frame = start;
                }
                core.loop_region = Some((start, end));
            }
            None => core.loop_region = None,
        }
    }

    /// `file_pos` 是文件时间轴上的位置；暂停时也立即生效。
    pub fn seek_file(&mut self, file_pos: Duration) -> Result<(), String> {
        let Some((track, core)) = self.track.as_ref().zip(self.core.as_ref()) else {
            return Err("当前没有在播放".into());
        };
        let target = seconds_to_frames(file_pos.as_secs_f64(), track.sample_rate).min(track.frames);
        if let Ok(mut core) = core.lock() {
            core.frame = target;
        }
        Ok(())
    }

    pub fn play(&self) {
        if let Some(player) = &self.player {
            player.play();
        }
    }

    pub fn pause(&self) {
        if let Some(player) = &self.player {
            player.pause();
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(player) = &self.player {
            player.set_volume(self.volume);
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(0.25, 4.0);
        if let Some(player) = &self.player {
            player.set_speed(self.speed);
        }
    }

    pub fn stop(&mut self) {
        self.player = None;
        self.track = None;
        self.core = None;
    }
}

fn seconds_to_frames(seconds: f64, sample_rate: u32) -> u64 {
    (seconds.max(0.0) * f64::from(sample_rate)).round() as u64
}

fn duration_of_frames(frames: u64, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(frames as f64 / f64::from(sample_rate))
}

fn format_time(duration: Duration) -> String {
    let total = duration.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}
