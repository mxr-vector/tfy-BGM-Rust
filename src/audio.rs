//! 本地 MP3 播放。GPUI 不管音频，这里用 rodio 0.23 走系统输出设备。
//!
//! 倍速沿用 rodio 的采样率缩放，音高会一起变化。浏览器版默认保音高，这里做不到同等效果。

use std::fs::File;
use std::path::Path;
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};

pub struct AudioEngine {
    sink: Option<MixerDeviceSink>,
    player: Option<Player>,
    speed: f32,
    volume: f32,
    duration: Duration,
    open_error: Option<String>,
}

impl AudioEngine {
    pub fn open() -> Self {
        match DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => Self {
                sink: Some(sink),
                player: None,
                speed: 1.0,
                volume: 1.0,
                duration: Duration::ZERO,
                open_error: None,
            },
            Err(error) => Self {
                sink: None,
                player: None,
                speed: 1.0,
                volume: 1.0,
                duration: Duration::ZERO,
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

    /// 文件内的播放位置。rodio 的 `get_pos` 是变速后的时间轴，要乘回倍速。
    pub fn file_position(&self) -> Duration {
        let Some(player) = &self.player else {
            return Duration::ZERO;
        };
        player.get_pos().mul_f32(self.speed.max(0.25))
    }

    pub fn load(&mut self, path: &Path) -> Result<Duration, String> {
        let sink = self.sink.as_ref().ok_or_else(|| {
            self.open_error
                .clone()
                .unwrap_or_else(|| "音频设备不可用".into())
        })?;
        let file = File::open(path).map_err(|error| format!("无法读取音频：{error}"))?;
        let decoder = Decoder::try_from(file).map_err(|error| format!("无法解码 MP3：{error}"))?;
        let duration = decoder.total_duration().unwrap_or_default();

        // 换一个新的 Player，旧实例 drop 时会停掉上一首，避免 clear() 在界面线程上等待。
        let player = Player::connect_new(sink.mixer());
        player.set_speed(self.speed);
        player.set_volume(self.volume);
        player.append(decoder);
        player.play();

        self.player = Some(player);
        self.duration = duration;
        Ok(duration)
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

    /// `file_pos` 是文件时间轴上的位置，不是变速后的听感时间。
    pub fn seek_file(&self, file_pos: Duration) -> Result<(), String> {
        let Some(player) = &self.player else {
            return Err("当前没有在播放".into());
        };
        let timeline = file_pos.div_f32(self.speed.max(0.25));
        player
            .try_seek(timeline)
            .map_err(|error| format!("无法跳转：{error}"))
    }

    pub fn stop(&mut self) {
        self.player = None;
        self.duration = Duration::ZERO;
    }
}
