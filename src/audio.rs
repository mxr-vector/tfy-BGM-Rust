//! 本地音频解码、采样级 A/B 循环与双声部切歌。

use std::collections::HashMap;
use std::fs::{File, Metadata};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

use cpal::traits::{DeviceTrait, HostTrait};
use rodio::mixer::Mixer;
use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source};
use serde::Serialize;

use crate::settings::OutputRoute;

mod stretch;
use stretch::{Blend, TimeStretch};

const MAX_DECODE_DURATION: Duration = Duration::from_secs(30 * 60);
const MAX_SAMPLES: usize = 64 * 1024 * 1024;
const MAX_CACHED_SAMPLES: usize = 8 * 1024 * 1024;
const TRANSPORT_FADE_SECONDS: f32 = 0.08;
const MIN_CROSSFADE_SECONDS: f32 = 0.05;
const DECODE_CANCELLED: &str = "音频加载已取消";
const DECODE_WORKER_STOPPED: &str = "音频解码线程已停止";
const TRACK_TOO_LONG: &str = "时长超过 30 分钟，整曲解码内存占用过高";

pub struct PcmTrack {
    pub samples: Vec<f32>,
    pub channels: rodio::ChannelCount,
    pub sample_rate: rodio::SampleRate,
}

impl PcmTrack {
    fn frames(&self) -> usize {
        self.samples.len() / usize::from(self.channels.get())
    }

    pub fn duration(&self) -> Duration {
        self.time_at(self.frames() as f64)
    }

    fn time_at(&self, frame: f64) -> Duration {
        Duration::from_secs_f64(frame / f64::from(self.sample_rate.get()))
    }

    fn frame_at(&self, seconds: f64) -> usize {
        ((seconds * f64::from(self.sample_rate.get())).round() as usize).min(self.frames())
    }

    fn fade_frames(&self, seconds: f32) -> usize {
        (seconds * self.sample_rate.get() as f32).round() as usize
    }
}

pub fn stretch_mono(samples: Vec<f32>, rate: u32, speed: f64) -> Vec<f32> {
    let track = PcmTrack {
        samples,
        channels: rodio::ChannelCount::MIN,
        sample_rate: rodio::SampleRate::new(rate).expect("有效采样率"),
    };
    let mut stretch = TimeStretch::new(rate as usize);
    let len = (track.frames() as f64 / speed).floor() as usize;
    (0..len)
        .map(|index| {
            stretch
                .next(&track, (index as f64 * speed) as i64, speed, None)
                .sample(&track, 0)
        })
        .collect()
}

#[derive(Clone)]
struct Gain {
    current: f32,
    target: f32,
    frames_left: usize,
}

impl Gain {
    fn new(value: f32) -> Self {
        Self {
            current: value,
            target: value,
            frames_left: 0,
        }
    }

    fn ramp_to(&mut self, target: f32, frames: usize) {
        self.target = target;
        self.frames_left = frames;
        if frames == 0 {
            self.current = target;
        }
    }

    fn next_frame(&mut self) -> f32 {
        let value = self.current;
        if self.frames_left > 0 {
            self.current += (self.target - self.current) / self.frames_left as f32;
            self.frames_left -= 1;
        }
        value
    }
}

#[derive(Clone)]
struct Cursor {
    frame: f64,
    speed: f64,
    loop_region: Option<(usize, usize)>,
    revision: u64,
    envelope: Gain,
    transport: Gain,
    retiring: bool,
}

impl Cursor {
    fn start(
        track: &PcmTrack,
        region: Option<(f64, f64)>,
        fade: f32,
        speed: f32,
        playing: bool,
    ) -> Self {
        let loop_region = loop_frames(track, region);
        let mut envelope = Gain::new(1.0);
        let mut transport = Gain::new(0.0);
        if playing && fade > 0.0 {
            envelope = Gain::new(0.0);
            envelope.ramp_to(1.0, track.fade_frames(fade));
            transport = Gain::new(1.0);
        } else if playing {
            transport.ramp_to(1.0, track.fade_frames(TRANSPORT_FADE_SECONDS));
        }
        Self {
            frame: loop_region.map_or(0.0, |(start, _)| start as f64),
            speed: f64::from(speed),
            loop_region,
            revision: 0,
            envelope,
            transport,
            retiring: false,
        }
    }

    fn seek(&mut self, track: &PcmTrack, position: Duration) {
        let frame = track.frame_at(position.as_secs_f64());
        self.frame = self
            .loop_region
            .map_or(frame, |(start, end)| frame.clamp(start, end - 1)) as f64;
        self.revision = self.revision.wrapping_add(1);
    }
}

struct TrackSource {
    track: Arc<PcmTrack>,
    cursor: Arc<Mutex<Cursor>>,
    stretch: TimeStretch,
    revision: u64,
    channel: usize,
    blend: Blend,
    gain: f32,
}

impl TrackSource {
    fn new(track: Arc<PcmTrack>, cursor: Arc<Mutex<Cursor>>) -> Self {
        Self {
            stretch: TimeStretch::new(track.sample_rate.get() as usize),
            track,
            cursor,
            revision: 0,
            channel: 0,
            blend: Blend::default(),
            gain: 0.0,
        }
    }
}

impl Iterator for TrackSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.channel == 0 {
            let mut cursor = self.cursor.lock().ok()?;
            if cursor.retiring && cursor.envelope.frames_left == 0 {
                return None;
            }
            let end = self.track.frames() as f64;
            let frame = cursor.frame;
            if frame >= end {
                return None;
            }
            let transport = cursor.transport.next_frame();
            self.gain = cursor.envelope.next_frame() * transport;
            if transport > 0.0 || cursor.transport.target > 0.0 {
                let speed = cursor.speed;
                let region = cursor.loop_region;
                let revision = cursor.revision;
                let next = frame + speed;
                cursor.frame = match region {
                    Some((start, stop)) if next >= stop as f64 => {
                        start as f64 + (next - stop as f64) % (stop - start) as f64
                    }
                    _ => next.min(end),
                };
                drop(cursor);
                if revision != self.revision {
                    self.stretch.reset();
                    self.revision = revision;
                }
                self.blend = self.stretch.next(&self.track, frame as i64, speed, region);
            }
        }
        let channels = usize::from(self.track.channels.get());
        let sample = if self.gain == 0.0 {
            0.0
        } else {
            self.blend.sample(&self.track, self.channel) * self.gain
        };
        self.channel = (self.channel + 1) % channels;
        Some(sample)
    }
}

impl Source for TrackSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.track.sample_rate
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.track.channels
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

struct Voice {
    player: Player,
    track: Arc<PcmTrack>,
    cursor: Arc<Mutex<Cursor>>,
    route_gain: f32,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputDevice {
    pub device_id: String,
    pub label: String,
}

struct Output {
    route_id: String,
    hardware_id: String,
    sink: OutputSink,
    gain: f32,
    current: Option<Voice>,
    outgoing: Option<Voice>,
}

struct OutputSink {
    mixer: Mixer,
    unavailable: Arc<AtomicBool>,
    _stream: Option<MixerDeviceSink>,
}

impl OutputSink {
    fn open(device: cpal::Device) -> Result<Self, rodio::DeviceSinkError> {
        let unavailable = Arc::new(AtomicBool::new(false));
        let signal = unavailable.clone();
        let stream = DeviceSinkBuilder::from_device(device).and_then(|builder| {
            builder
                .with_error_callback(move |error| {
                    use cpal::ErrorKind::{DeviceNotAvailable, HostUnavailable, StreamInvalidated};
                    if matches!(
                        error.kind(),
                        DeviceNotAvailable | HostUnavailable | StreamInvalidated
                    ) {
                        signal.store(true, Ordering::Relaxed);
                    }
                })
                .open_sink_or_fallback()
        })?;
        Ok(Self {
            mixer: stream.mixer().clone(),
            unavailable,
            _stream: Some(stream),
        })
    }

    fn is_available(&self) -> bool {
        !self.unavailable.load(Ordering::Relaxed)
    }
}

pub struct AudioPreview {
    player: Player,
    sink: OutputSink,
}

impl AudioPreview {
    pub fn prepare(path: &Path, volume: f32) -> Result<Self, String> {
        let file = File::open(path).map_err(|error| format!("无法读取音频：{error}"))?;
        let decoder = Decoder::try_from(file).map_err(|error| format!("无法解码音频：{error}"))?;
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("未找到系统音频输出。请在系统设置的「声音 → 输出」选择设备。")?;
        let label = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| "系统默认输出".into());
        let sink = OutputSink::open(device).map_err(|error| {
            eprintln!("试听无法打开音频输出「{label}」：{error:?}");
            format!("无法打开「{label}」。请在系统设置的「声音 → 输出」检查或更换设备后重试。")
        })?;
        let player = Player::connect_new(&sink.mixer);
        player.pause();
        player.set_volume(volume);
        player.append(decoder);
        Ok(Self { player, sink })
    }

    pub fn play(&self) {
        self.player.play();
    }

    pub fn is_finished(&self) -> bool {
        self.player.empty() || !self.sink.is_available()
    }
}

type DecodeResult = Result<Arc<PcmTrack>, String>;

enum DecodeRequest {
    Track(PathBuf),
    Clear,
    Shutdown,
}

#[derive(Default)]
struct DecodeSlots {
    request: Option<DecodeRequest>,
    result: Option<DecodeResult>,
    clear_cache: bool,
}

#[derive(Default)]
struct DecodeMailbox {
    slots: Mutex<DecodeSlots>,
    ready: Condvar,
    generation: AtomicU64,
}

impl DecodeMailbox {
    fn submit(&self, request: DecodeRequest) {
        let mut slots = self.slots.lock().unwrap();
        self.generation.fetch_add(1, Ordering::Relaxed);
        slots.clear_cache |= matches!(request, DecodeRequest::Clear);
        slots.request = Some(request);
        slots.result = None;
        self.ready.notify_one();
    }

    fn take_request(&self) -> (u64, DecodeRequest, bool) {
        let mut slots = self
            .ready
            .wait_while(self.slots.lock().unwrap(), |slots| slots.request.is_none())
            .unwrap();
        (
            self.generation.load(Ordering::Relaxed),
            slots.request.take().unwrap(),
            std::mem::take(&mut slots.clear_cache),
        )
    }

    fn is_current(&self, ticket: u64) -> bool {
        self.generation.load(Ordering::Relaxed) == ticket
    }

    fn publish(&self, ticket: u64, result: DecodeResult) {
        let mut slots = self.slots.lock().unwrap();
        if self.is_current(ticket) {
            slots.result = Some(result);
        }
    }

    fn take_result(&self) -> Option<DecodeResult> {
        self.slots.lock().unwrap().result.take()
    }
}

pub enum DecodeError {
    Track(String),
    Unavailable(&'static str),
}

#[derive(PartialEq, Eq)]
struct FileVersion {
    modified: SystemTime,
    size: u64,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileVersion {
    fn read(metadata: Metadata) -> Option<Self> {
        Some(Self {
            modified: metadata.modified().ok()?,
            size: metadata.len(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        })
    }
}

#[derive(Default)]
struct DecodeCache {
    last: Option<(PathBuf, FileVersion, Arc<PcmTrack>)>,
}

impl DecodeCache {
    fn load(
        &mut self,
        path: PathBuf,
        is_current: impl Fn() -> bool,
    ) -> Result<Arc<PcmTrack>, String> {
        if !is_current() {
            return Err(DECODE_CANCELLED.into());
        }
        let file = File::open(&path).map_err(|error| format!("无法读取音频：{error}"))?;
        let version = file.metadata().ok().and_then(FileVersion::read);
        if let Some((cached_path, cached_version, track)) = &self.last
            && *cached_path == path
            && Some(cached_version) == version.as_ref()
        {
            return Ok(track.clone());
        }
        self.last = None;
        let decoder = Decoder::try_from(file).map_err(|error| format!("无法解码音频：{error}"))?;
        let track = Arc::new(decode_pcm(decoder, is_current)?);
        if track.samples.capacity() <= MAX_CACHED_SAMPLES
            && let Some(version) = version
            && path.metadata().ok().and_then(FileVersion::read).as_ref() == Some(&version)
        {
            self.last = Some((path, version, track.clone()));
        }
        Ok(track)
    }
}

fn decode_requests(mailbox: &DecodeMailbox) {
    let mut cache = DecodeCache::default();
    loop {
        let (ticket, request, clear_cache) = mailbox.take_request();
        if clear_cache {
            cache.last = None;
        }
        match request {
            DecodeRequest::Track(path) => {
                let result = cache.load(path, || mailbox.is_current(ticket));
                mailbox.publish(ticket, result);
            }
            DecodeRequest::Clear => {}
            DecodeRequest::Shutdown => break,
        }
    }
}

pub struct AudioEngine {
    outputs: Vec<Output>,
    decode_mailbox: Arc<DecodeMailbox>,
    decode_worker: JoinHandle<()>,
    source_path: Option<PathBuf>,
    speed: f32,
    volume: f32,
    paused: bool,
    open_error: Option<String>,
}

impl AudioEngine {
    pub fn open() -> Self {
        let decode_mailbox = Arc::new(DecodeMailbox::default());
        let mailbox = decode_mailbox.clone();
        let decode_worker = std::thread::spawn(move || decode_requests(&mailbox));
        Self {
            outputs: Vec::new(),
            decode_mailbox,
            decode_worker,
            source_path: None,
            speed: 1.0,
            volume: 1.0,
            paused: false,
            open_error: Some("没有可用的音频输出设备".into()),
        }
    }

    pub fn open_error(&self) -> Option<&str> {
        self.open_error.as_deref()
    }

    pub fn queued(&self) -> usize {
        self.current_voices()
            .next()
            .map_or(0, |voice| voice.player.len())
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn track(&self) -> Option<Arc<PcmTrack>> {
        self.current_voices()
            .next()
            .map(|voice| voice.track.clone())
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    pub fn relocate_source(&mut self, path: PathBuf) {
        if self.source_path.is_some() {
            self.source_path = Some(path);
        }
    }

    pub fn output_interrupted(&self) -> bool {
        self.outputs
            .iter()
            .any(|output| !output.sink.is_available())
    }

    pub fn refresh_outputs(&mut self, routes: &[OutputRoute]) -> (Vec<OutputDevice>, Vec<String>) {
        let host = cpal::default_host();
        let enumerated = host.output_devices();
        let enumeration_error = enumerated
            .as_ref()
            .err()
            .map(|error| format!("无法刷新输出设备列表：{error}"));
        let mut native: HashMap<_, _> = enumerated
            .into_iter()
            .flatten()
            .filter_map(|device| Some((device.id().ok()?.to_string(), device)))
            .collect();
        let system_default = host.default_output_device().and_then(|device| {
            let id = device.id().ok()?.to_string();
            native.entry(id.clone()).or_insert(device);
            Some(id)
        });
        let mut devices: Vec<_> = native
            .iter()
            .map(|(id, device)| OutputDevice {
                device_id: id.clone(),
                label: device
                    .description()
                    .map(|description| description.name().to_owned())
                    .unwrap_or_default(),
            })
            .collect();
        devices.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        for (index, device) in devices.iter_mut().enumerate() {
            if device.label.trim().is_empty() {
                device.label = format!("输出设备 {}", index + 1);
            }
        }
        devices.insert(
            0,
            OutputDevice {
                device_id: "default".into(),
                label: "系统默认输出".into(),
            },
        );
        let retained: Vec<_> = self
            .outputs
            .iter()
            .filter(|output| enumeration_error.is_some() && output.sink.is_available())
            .map(|output| (output.route_id.clone(), output.hardware_id.clone()))
            .collect();
        let mut hardware: HashMap<_, _> = native
            .keys()
            .map(|id| (id.as_str(), id.as_str()))
            .chain(system_default.as_deref().map(|id| ("default", id)))
            .collect();
        for (route, id) in &retained {
            hardware.entry(route.as_str()).or_insert(id.as_str());
        }
        let mut warnings = self.apply_inventory(routes, &hardware, |id| {
            let device = native.get(id).ok_or("未能重新枚举该设备")?;
            OutputSink::open(device.clone()).map_err(|error| error.to_string())
        });
        warnings.extend(enumeration_error);
        (devices, warnings)
    }

    fn apply_inventory(
        &mut self,
        routes: &[OutputRoute],
        hardware: &HashMap<&str, &str>,
        mut open: impl FnMut(&str) -> Result<OutputSink, String>,
    ) -> Vec<String> {
        let mut messages = Vec::new();
        let mut previous = std::mem::take(&mut self.outputs);
        let clock = previous
            .iter()
            .find(|output| output.current.is_some() && output.sink.is_available())
            .or_else(|| previous.iter().find(|output| output.current.is_some()));
        let snapshot = |voice: Option<&Voice>| {
            let voice = voice.filter(|voice| !voice.player.empty())?;
            let cursor = voice.cursor.lock().ok()?.clone();
            Some((voice.track.clone(), cursor))
        };
        let current = snapshot(clock.and_then(|output| output.current.as_ref()));
        let outgoing = snapshot(clock.and_then(|output| output.outgoing.as_ref()));
        let silent_default = routes.iter().all(|route| !route.enabled);
        let configured = routes
            .iter()
            .filter(|route| route.enabled || silent_default && route.device_id == "default");
        previous.retain(|output| {
            output.sink.is_available()
                && configured.clone().any(|route| {
                    output.route_id == route.device_id
                        && hardware
                            .get(route.device_id.as_str())
                            .is_some_and(|&id| output.hardware_id == id)
                })
        });
        for route in configured {
            let gain = if route.enabled && !route.muted {
                route.volume
            } else {
                0.0
            };
            let Some(&hardware_id) = hardware.get(route.device_id.as_str()) else {
                messages.push(format!("输出设备「{}」不可用", route.label));
                continue;
            };
            if let Some(index) = previous
                .iter()
                .position(|output| output.route_id == route.device_id)
            {
                let mut output = previous.swap_remove(index);
                output.gain = gain;
                for voice in output.current.iter_mut().chain(&mut output.outgoing) {
                    voice.route_gain = gain;
                }
                self.outputs.push(output);
                continue;
            }
            let sink = match open(hardware_id) {
                Ok(sink) => sink,
                Err(error) => {
                    messages.push(format!("无法打开输出设备「{}」：{error}", route.label));
                    continue;
                }
            };
            let restore = |source: &Option<(Arc<PcmTrack>, Cursor)>| {
                source.as_ref().map(|(track, cursor)| {
                    start_voice(
                        &sink.mixer,
                        track.clone(),
                        cursor.clone(),
                        self.volume,
                        gain,
                    )
                })
            };
            self.outputs.push(Output {
                route_id: route.device_id.clone(),
                hardware_id: hardware_id.into(),
                current: restore(&current),
                outgoing: restore(&outgoing),
                sink,
                gain,
            });
        }
        self.set_volume(self.volume);
        self.open_error = self.outputs.is_empty().then(|| {
            messages
                .first()
                .cloned()
                .unwrap_or_else(|| "没有可用的音频输出设备".into())
        });
        messages
    }

    pub fn file_position(&self) -> Duration {
        self.current_voices()
            .next()
            .and_then(|voice| {
                let frame = voice.cursor.lock().ok()?.frame;
                Some(voice.track.time_at(frame))
            })
            .unwrap_or_default()
    }

    pub fn crossfade_for_load(&self, seconds: f32, playing: bool) -> f32 {
        if seconds > MIN_CROSSFADE_SECONDS
            && playing
            && !self.paused
            && (self.current_voices().any(|voice| !voice.player.empty())
                || self.pending_fade() > 0.0)
        {
            seconds
        } else {
            0.0
        }
    }

    pub fn request_track(&mut self, path: PathBuf, crossfade: f32) -> Result<(), String> {
        if let Some(error) = self.open_error() {
            return Err(error.to_owned());
        }
        if self.decode_worker.is_finished() {
            return Err(DECODE_WORKER_STOPPED.into());
        }
        self.decode_mailbox
            .submit(DecodeRequest::Track(path.clone()));
        self.source_path = Some(path);
        if crossfade == 0.0 {
            self.clear_voices();
        } else {
            for output in &mut self.outputs {
                if let Some(current) = output.current.take() {
                    output.outgoing = retire(Some(current), crossfade);
                }
            }
        }
        Ok(())
    }

    pub fn pending_fade(&self) -> f32 {
        if self.paused {
            return 0.0;
        }
        self.outputs
            .iter()
            .filter_map(|output| {
                let voice = output
                    .outgoing
                    .as_ref()
                    .filter(|voice| !voice.player.empty())?;
                let cursor = voice.cursor.lock().ok()?;
                Some(cursor.envelope.frames_left as f32 / voice.track.sample_rate.get() as f32)
            })
            .fold(0.0, f32::max)
    }

    pub fn decoded_track(&self) -> Option<Result<Arc<PcmTrack>, DecodeError>> {
        self.decode_mailbox
            .take_result()
            .map(|result| result.map_err(DecodeError::Track))
            .or_else(|| {
                self.decode_worker
                    .is_finished()
                    .then_some(Err(DecodeError::Unavailable(DECODE_WORKER_STOPPED)))
            })
    }

    pub fn start(
        &mut self,
        track: Arc<PcmTrack>,
        region: Option<(f64, f64)>,
        crossfade: f32,
        playing: bool,
        position: Option<Duration>,
    ) {
        let fade = if playing { crossfade } else { 0.0 };
        let retire_fade = if self.paused { 0.0 } else { fade };
        let mut cursor = Cursor::start(&track, region, fade, self.speed, playing);
        if let Some(position) = position {
            cursor.seek(&track, position);
        }
        for output in &mut self.outputs {
            if let Some(current) = output.current.take() {
                output.outgoing = retire(Some(current), retire_fade);
            } else if retire_fade == 0.0 {
                output.outgoing = None;
            }
            output.current = Some(start_voice(
                &output.sink.mixer,
                track.clone(),
                cursor.clone(),
                self.volume,
                output.gain,
            ));
        }
        self.paused = !playing;
    }

    fn current_voices(&self) -> impl Iterator<Item = &Voice> {
        self.outputs
            .iter()
            .filter_map(|output| output.current.as_ref())
    }

    fn voices(&self) -> impl Iterator<Item = &Voice> {
        self.outputs
            .iter()
            .flat_map(|output| output.current.iter().chain(&output.outgoing))
    }

    pub fn set_loop_region(&self, region: Option<(f64, f64)>) {
        for voice in self.current_voices() {
            let Ok(mut cursor) = voice.cursor.lock() else {
                continue;
            };
            let region = loop_frames(&voice.track, region);
            if cursor.loop_region == region {
                continue;
            }
            cursor.loop_region = region;
            cursor.revision = cursor.revision.wrapping_add(1);
            if let Some((start, end)) = cursor.loop_region
                && !(start as f64..end as f64).contains(&cursor.frame)
            {
                cursor.frame = start as f64;
            }
        }
    }

    pub fn seek_file(&self, position: Duration) -> Result<(), String> {
        if self.current_voices().next().is_none() {
            return Err("当前没有在播放".into());
        }
        for voice in self.current_voices() {
            let mut cursor = voice.cursor.lock().map_err(|_| "无法更新播放位置")?;
            cursor.seek(&voice.track, position);
        }
        Ok(())
    }

    pub fn play(&mut self) {
        self.set_paused(false);
    }

    pub fn pause(&mut self) {
        self.set_paused(true);
    }

    fn set_paused(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        for voice in self.voices() {
            let Ok(mut cursor) = voice.cursor.lock() else {
                continue;
            };
            let target = if paused { 0.0 } else { 1.0 };
            let frames = voice.track.fade_frames(TRANSPORT_FADE_SECONDS);
            cursor.transport.ramp_to(target, frames);
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        for voice in self.voices() {
            voice.player.set_volume(self.volume * voice.route_gain);
        }
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed.clamp(0.25, 4.0);
        for voice in self.voices() {
            if let Ok(mut cursor) = voice.cursor.lock() {
                cursor.speed = f64::from(self.speed);
            }
        }
    }

    pub fn stop(&mut self) {
        self.decode_mailbox.submit(DecodeRequest::Clear);
        self.source_path = None;
        self.clear_voices();
    }

    fn clear_voices(&mut self) {
        for output in &mut self.outputs {
            output.current = None;
            output.outgoing = None;
        }
        self.paused = false;
    }

    pub fn release_finished_fade(&mut self) {
        for output in &mut self.outputs {
            if output
                .outgoing
                .as_ref()
                .is_some_and(|voice| voice.player.empty())
            {
                output.outgoing = None;
            }
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.decode_mailbox.submit(DecodeRequest::Shutdown);
    }
}

fn retire(previous: Option<Voice>, fade: f32) -> Option<Voice> {
    previous.filter(|voice| {
        if fade <= 0.0 || voice.player.empty() {
            return false;
        }
        if let Ok(mut cursor) = voice.cursor.lock() {
            cursor.retiring = true;
            cursor.envelope.ramp_to(0.0, voice.track.fade_frames(fade));
        }
        true
    })
}

fn start_voice(
    mixer: &Mixer,
    track: Arc<PcmTrack>,
    cursor: Cursor,
    volume: f32,
    route_gain: f32,
) -> Voice {
    let cursor = Arc::new(Mutex::new(cursor));
    let (player, source) = Player::new();
    player.set_volume(volume * route_gain);
    player.append(TrackSource::new(track.clone(), cursor.clone()));
    mixer.add(source);
    Voice {
        player,
        track,
        cursor,
        route_gain,
    }
}

fn loop_frames(track: &PcmTrack, region: Option<(f64, f64)>) -> Option<(usize, usize)> {
    let (start, end) = region?;
    let (start, end) = (track.frame_at(start), track.frame_at(end));
    (end.saturating_sub(start) >= (track.sample_rate.get() as usize).div_ceil(5))
        .then_some((start, end))
}

fn decode_pcm(decoder: impl Source, is_current: impl Fn() -> bool) -> Result<PcmTrack, String> {
    if decoder
        .total_duration()
        .is_some_and(|duration| duration > MAX_DECODE_DURATION)
    {
        return Err(TRACK_TOO_LONG.into());
    }
    let sample_rate = decoder.sample_rate();
    let channels = decoder.channels();
    let duration_limit =
        MAX_DECODE_DURATION.as_secs() * u64::from(sample_rate.get()) * u64::from(channels.get());
    let mut samples = Vec::new();
    for (index, sample) in decoder.enumerate() {
        if index % 4096 == 0 && !is_current() {
            return Err(DECODE_CANCELLED.into());
        }
        if index as u64 == duration_limit {
            return Err(TRACK_TOO_LONG.into());
        }
        if index == MAX_SAMPLES {
            return Err("整曲解码超出 256 MiB 内存上限".into());
        }
        samples.push(sample);
    }
    if !is_current() {
        return Err(DECODE_CANCELLED.into());
    }
    let track = PcmTrack {
        samples,
        channels,
        sample_rate,
    };
    if track.frames() == 0 {
        return Err("音频文件为空或无法解码".into());
    }
    Ok(track)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::buffer::SamplesBuffer;
    use rodio::source::{Function, SignalGenerator};
    use std::cell::Cell;
    use std::path::Path;

    fn test_output() -> (OutputSink, rodio::mixer::MixerSource) {
        let (mixer, samples) = rodio::mixer::mixer(
            rodio::ChannelCount::new(1).unwrap(),
            rodio::SampleRate::new(1_000).unwrap(),
        );
        (
            OutputSink {
                mixer,
                unavailable: Arc::new(AtomicBool::new(false)),
                _stream: None,
            },
            samples,
        )
    }

    fn test_track() -> Arc<PcmTrack> {
        Arc::new(PcmTrack {
            samples: vec![0.8; 1_000],
            channels: rodio::ChannelCount::new(1).unwrap(),
            sample_rate: rodio::SampleRate::new(1_000).unwrap(),
        })
    }

    fn test_route(id: &str, volume: f32) -> OutputRoute {
        OutputRoute {
            device_id: id.into(),
            label: id.into(),
            volume,
            ..OutputRoute::default()
        }
    }

    #[test]
    fn reloaded_source_restores_cursor_loop_and_paused_transport_before_audio() {
        let mut engine = AudioEngine::open();
        let (sink, mut rendered) = test_output();
        let mut sink = Some(sink);
        engine.apply_inventory(
            &[test_route("desk", 1.0)],
            &HashMap::from([("desk", "desk")]),
            |_| Ok(sink.take().unwrap()),
        );
        let track = test_track();
        engine.start(
            track,
            Some((0.2, 0.8)),
            0.0,
            false,
            Some(Duration::from_millis(500)),
        );
        assert_eq!(engine.file_position(), Duration::from_millis(500));
        assert!(engine.is_paused());
        assert_eq!(rendered.nth(20), Some(0.0));
        assert_eq!(engine.file_position(), Duration::from_millis(500));
        engine.play();
        assert_eq!(rendered.nth(90), Some(0.8));
        assert_eq!(engine.file_position(), Duration::from_millis(591));
    }

    #[test]
    fn explicit_outputs_play_without_default_and_keep_independent_volume() {
        let mut engine = AudioEngine::open();
        let mut routes = [
            OutputRoute::default(),
            test_route("desk", 0.5),
            test_route("headset", 1.0),
        ];
        let inventory = HashMap::from([("desk", "desk"), ("headset", "headset")]);
        let mut rendered = HashMap::new();
        let warnings = engine.apply_inventory(&routes, &inventory, |id| {
            let (sink, samples) = test_output();
            rendered.insert(id.to_owned(), samples);
            Ok(sink)
        });
        assert!(warnings[0].contains("系统默认输出"));
        assert!(engine.open_error().is_none());

        engine.set_volume(0.5);
        engine.start(test_track(), None, 0.0, true, None);
        assert_eq!(rendered.get_mut("desk").unwrap().nth(90), Some(0.2));
        assert_eq!(rendered.get_mut("headset").unwrap().nth(90), Some(0.4));
        routes[1].muted = true;
        engine.apply_inventory(&routes, &inventory, |_| panic!("existing output reopened"));
        assert_eq!(rendered.get_mut("desk").unwrap().nth(20), Some(0.0));
        assert_eq!(rendered.get_mut("headset").unwrap().nth(20), Some(0.4));
        assert_eq!(engine.file_position(), Duration::from_millis(112));
    }

    #[test]
    fn replacing_default_output_continues_from_surviving_device_position() {
        let mut engine = AudioEngine::open();
        let routes = [OutputRoute::default(), test_route("headset", 1.0)];
        let mut inventory = HashMap::from([("default", "speaker"), ("headset", "headset")]);
        let mut rendered = HashMap::new();
        let mut open = |id: &str| {
            let (sink, samples) = test_output();
            rendered.insert(id.to_owned(), samples);
            Ok(sink)
        };
        assert!(
            engine
                .apply_inventory(&routes, &inventory, &mut open)
                .is_empty()
        );
        let track = test_track();
        engine.start(track.clone(), None, 0.0, true, None);
        assert_eq!(rendered.get_mut("speaker").unwrap().nth(90), Some(0.8));
        assert_eq!(rendered.get_mut("headset").unwrap().nth(90), Some(0.8));

        inventory.remove("default");
        let warnings =
            engine.apply_inventory(&routes, &inventory, |_| panic!("surviving output reopened"));
        assert_eq!(warnings.len(), 1);
        assert_eq!(engine.outputs[0].route_id, "headset");
        assert_eq!(rendered.get_mut("headset").unwrap().nth(9), Some(0.8));
        assert_eq!(engine.file_position(), Duration::from_millis(101));

        inventory.insert("default", "dock");
        let mut dock = None;
        let warnings = engine.apply_inventory(&routes, &inventory, |id| {
            assert_eq!(id, "dock");
            let (sink, samples) = test_output();
            dock = Some(samples);
            Ok(sink)
        });
        assert!(warnings.is_empty());
        assert_eq!(engine.outputs[0].route_id, "default");
        assert!(Arc::ptr_eq(&track, &engine.track().unwrap()));
        assert_eq!(engine.file_position(), Duration::from_millis(101));
        assert_eq!(dock.unwrap().next(), Some(0.8));
    }

    #[test]
    fn disabled_default_is_only_used_as_a_silent_clock_with_no_enabled_routes() {
        let mut engine = AudioEngine::open();
        let mut routes = [
            OutputRoute {
                enabled: false,
                ..OutputRoute::default()
            },
            test_route("missing", 1.0),
        ];
        let inventory = HashMap::from([("default", "speaker")]);
        let messages =
            engine.apply_inventory(&routes, &inventory, |_| panic!("disabled default opened"));
        assert_eq!(messages.len(), 1);
        assert!(engine.open_error().is_some());
        routes[1].enabled = false;
        let mut render = None;
        let messages = engine.apply_inventory(&routes, &inventory, |_| {
            let (sink, samples) = test_output();
            render = Some(samples);
            Ok(sink)
        });
        assert!(messages.is_empty());
        engine.start(test_track(), None, 0.0, true, None);
        assert_eq!(render.unwrap().nth(90), Some(0.0));
        assert_eq!(engine.file_position(), Duration::from_millis(91));
    }

    #[test]
    fn invalidated_stream_reopens_the_same_device_at_its_current_position() {
        let mut engine = AudioEngine::open();
        let routes = [OutputRoute::default()];
        let inventory = HashMap::from([("default", "speaker")]);
        let (sink, mut before) = test_output();
        let mut sink = Some(sink);
        engine.apply_inventory(&routes, &inventory, |_| Ok(sink.take().unwrap()));
        engine.start(test_track(), None, 0.0, true, None);
        assert_eq!(before.nth(91), Some(0.8));
        engine.outputs[0]
            .sink
            .unavailable
            .store(true, Ordering::Relaxed);
        assert!(engine.output_interrupted());

        let mut after = None;
        let warnings = engine.apply_inventory(&routes, &inventory, |id| {
            assert_eq!(id, "speaker");
            let (sink, samples) = test_output();
            after = Some(samples);
            Ok(sink)
        });
        assert!(warnings.is_empty());
        assert!(!engine.output_interrupted());
        assert_eq!(engine.file_position(), Duration::from_millis(92));
        assert_eq!(after.unwrap().next(), Some(0.8));
    }

    fn write_wav(path: &Path, samples: &[i16]) {
        let data_size = (samples.len() * 2) as u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((36 + data_size).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(8000_u32.to_le_bytes());
        wav.extend(16000_u32.to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_size.to_le_bytes());
        wav.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
        std::fs::write(path, wav).unwrap();
    }

    #[test]
    fn decodes_all_upstream_library_formats() {
        let Some(ffmpeg) = bgm_core::loudness::ffmpeg_path() else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let mut cache = DecodeCache::default();
        for (extension, codec) in [
            ("flac", "flac"),
            ("wav", "pcm_s16le"),
            ("m4a", "aac"),
            ("mp3", "mp3"),
            ("ogg", "vorbis"),
        ] {
            let path = directory.path().join(format!("tone.{extension}"));
            let encoded = std::process::Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=997:sample_rate=44100:duration=0.5",
                    "-ac",
                    "2",
                    "-c:a",
                    codec,
                    "-strict",
                    "experimental",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                encoded.status.success(),
                "{extension}: {}",
                String::from_utf8_lossy(&encoded.stderr)
            );

            let track = cache
                .load(path, || true)
                .unwrap_or_else(|error| panic!("{extension}: {error}"));
            assert_eq!(track.channels.get(), 2, "{extension}");
            assert_eq!(track.sample_rate.get(), 44_100, "{extension}");
            assert!(track.duration() > Duration::from_millis(400), "{extension}");
            assert!(
                track.samples.iter().any(|&sample| sample != 0.),
                "{extension}"
            );
        }
    }

    #[test]
    fn decode_cache_reuses_unchanged_file_and_invalidates_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tone.wav");
        write_wav(&path, &[1024; 128]);
        let mut cache = DecodeCache::default();
        let first = cache.load(path.clone(), || true).unwrap();
        let same = cache.load(path.clone(), || true).unwrap();
        assert!(Arc::ptr_eq(&first, &same));

        let replacement = directory.path().join("replacement.wav");
        write_wav(&replacement, &[-1024; 128]);
        std::fs::rename(replacement, &path).unwrap();
        let changed = cache.load(path.clone(), || true).unwrap();
        assert_eq!(first.samples.len(), changed.samples.len());
        assert!(first.samples[0] > 0.0 && changed.samples[0] < 0.0);
        assert!(!Arc::ptr_eq(&first, &changed));
        assert_eq!(cache.load(path, || false).err().unwrap(), DECODE_CANCELLED);
    }

    #[test]
    fn decode_mailbox_releases_unread_pcm_and_rejects_stale_results() {
        let mailbox = DecodeMailbox::default();
        mailbox.submit(DecodeRequest::Track("first.wav".into()));
        let (first, _, _) = mailbox.take_request();
        let track = test_track();
        let unread = Arc::downgrade(&track);
        mailbox.publish(first, Ok(track));

        mailbox.submit(DecodeRequest::Track("second.wav".into()));
        assert!(unread.upgrade().is_none());
        mailbox.publish(first, Ok(test_track()));
        assert!(mailbox.take_result().is_none());

        mailbox.submit(DecodeRequest::Clear);
        let latest_path = PathBuf::from("latest.wav");
        mailbox.submit(DecodeRequest::Track(latest_path.clone()));
        let (latest, request, clear_cache) = mailbox.take_request();
        assert!(matches!(request, DecodeRequest::Track(path) if path == latest_path));
        assert!(clear_cache);
        mailbox.publish(latest, Ok(test_track()));
        assert!(mailbox.take_result().unwrap().is_ok());
        assert!(mailbox.take_result().is_none());
    }

    #[test]
    fn real_decode_errors_stop_at_the_skip_limit_or_recover_on_a_valid_track() {
        use crate::playback_queue::{FailedTracks, next_available_index};

        let directory = tempfile::tempdir().unwrap();
        let queue = ["missing.wav", "corrupt.wav", "valid.wav"].map(str::to_owned);
        std::fs::write(directory.path().join(&queue[1]), b"invalid WAV").unwrap();
        write_wav(&directory.path().join(&queue[2]), &[1024; 128]);

        for (max_skips, expected) in [(0, [0].as_slice()), (1, &[0, 1]), (2, &[0, 1, 2])] {
            let mut cache = DecodeCache::default();
            let mut failed = FailedTracks::default();
            let mut attempts = Vec::new();
            let mut index = 0;
            loop {
                attempts.push(index);
                match cache.load(directory.path().join(&queue[index]), || true) {
                    Ok(track) => {
                        assert_eq!(index, 2);
                        assert!(track.samples[0] > 0.0);
                        failed.clear();
                        assert!(failed.record(queue[0].clone(), max_skips));
                        break;
                    }
                    Err(error) => {
                        let expected_reason = if index == 0 {
                            "无法读取音频"
                        } else {
                            "无法解码音频"
                        };
                        assert!(error.starts_with(expected_reason), "{error}");
                        if !failed.record(queue[index].clone(), max_skips) {
                            break;
                        }
                    }
                }
                let Some(next) = next_available_index(&queue, index, |key| !failed.contains(key))
                else {
                    break;
                };
                index = next;
            }
            assert_eq!(attempts, expected);
        }
    }

    #[test]
    fn decode_checks_duration_and_cancels_during_sample_read() {
        let mono = rodio::ChannelCount::new(1).unwrap();
        let one_hz = rodio::SampleRate::new(1).unwrap();
        let limit = MAX_DECODE_DURATION.as_secs() as usize;
        let exact =
            decode_pcm(SamplesBuffer::new(mono, one_hz, vec![0.0; limit]), || true).unwrap();
        assert_eq!(exact.duration(), MAX_DECODE_DURATION);
        let over = SamplesBuffer::new(mono, one_hz, vec![0.0; limit + 1]);
        assert_eq!(
            decode_pcm(over, || panic!("时长预检应先于读取采样"))
                .err()
                .unwrap(),
            TRACK_TOO_LONG
        );
        let unknown_duration = SignalGenerator::new(one_hz, 0.25, Function::Sine);
        assert_eq!(
            decode_pcm(unknown_duration, || true).err().unwrap(),
            TRACK_TOO_LONG
        );

        let rate = rodio::SampleRate::new(8000).unwrap();
        let checks = Cell::new(0);
        let source = SamplesBuffer::new(mono, rate, vec![0.0; 8192]);
        let cancelled = decode_pcm(source, || {
            checks.set(checks.get() + 1);
            checks.get() == 1
        });
        assert_eq!(cancelled.err().unwrap(), DECODE_CANCELLED);
        assert_eq!(checks.get(), 2);
    }

    #[test]
    fn play_and_resume_use_the_same_transport_ramp() {
        let track = PcmTrack {
            samples: vec![1.0; 1000],
            channels: rodio::ChannelCount::new(1).unwrap(),
            sample_rate: rodio::SampleRate::new(1000).unwrap(),
        };
        let mut playing = Cursor::start(&track, None, 0.0, 2.0, true);
        let mut paused = Cursor::start(&track, None, 2.0, 2.0, false);
        assert_eq!(playing.transport.frames_left, 80);
        assert_eq!(paused.transport.next_frame(), 0.0);
        paused.transport.ramp_to(1.0, 80);
        for _ in 0..80 {
            assert_eq!(
                playing.transport.next_frame(),
                paused.transport.next_frame()
            );
            assert_eq!(playing.envelope.next_frame(), 1.0);
            assert_eq!(paused.envelope.next_frame(), 1.0);
        }
        assert_eq!(playing.transport.next_frame(), 1.0);

        let fade = Cursor::start(&track, None, 2.0, 1.0, true);
        assert_eq!(fade.envelope.frames_left, 2000);
        assert_eq!(fade.transport.current, 1.0);
    }

    #[test]
    fn stereo_position_and_loop_are_measured_in_frames() {
        let track = Arc::new(PcmTrack {
            samples: vec![0., 1., 2., 3., 4., 5., 6., 7.],
            channels: rodio::ChannelCount::new(2).unwrap(),
            sample_rate: rodio::SampleRate::new(10).unwrap(),
        });
        let cursor = Arc::new(Mutex::new(Cursor {
            frame: 1.0,
            speed: 1.0,
            loop_region: loop_frames(&track, Some((0.1, 0.3))),
            revision: 0,
            envelope: Gain::new(1.),
            transport: Gain::new(1.),
            retiring: false,
        }));
        let mut source = TrackSource::new(track.clone(), cursor.clone());
        assert_eq!(track.duration(), Duration::from_millis(400));
        assert_eq!(
            source.by_ref().take(8).collect::<Vec<_>>(),
            [2., 3., 4., 5., 2., 3., 4., 5.]
        );
        cursor.lock().unwrap().transport.ramp_to(0., 1);
        assert_eq!(
            source.by_ref().take(6).collect::<Vec<_>>(),
            [2., 3., 0., 0., 0., 0.]
        );
        assert_eq!(cursor.lock().unwrap().frame, 2.0);
    }

    #[test]
    fn stale_audio_finishes_fading_while_latest_track_is_still_decoding() {
        let mut engine = AudioEngine::open();
        let (sink, mut rendered) = test_output();
        let mut sink = Some(sink);
        engine.apply_inventory(
            &[test_route("desk", 1.0)],
            &HashMap::from([("desk", "desk")]),
            |_| Ok(sink.take().unwrap()),
        );
        engine.start(test_track(), None, 0.0, true, None);
        assert_eq!(rendered.nth(90), Some(0.8));

        let fade = engine.crossfade_for_load(0.2, true);
        engine
            .request_track(PathBuf::from("first.wav"), fade)
            .unwrap();
        assert!(engine.track().is_none());
        assert_eq!(engine.pending_fade(), 0.2);
        let level = rendered.nth(99).unwrap();
        assert!((level - 0.4).abs() < 0.01);

        let fade = engine.crossfade_for_load(0.2, true);
        engine
            .request_track(PathBuf::from("latest.wav"), fade)
            .unwrap();
        assert_eq!(engine.source_path(), Some(Path::new("latest.wav")));
        assert_eq!(engine.pending_fade(), 0.1);
        assert_eq!(rendered.nth(100), Some(0.0));
        assert_eq!(engine.pending_fade(), 0.0);
        engine.release_finished_fade();
        assert!(engine.outputs[0].outgoing.is_none());

        engine.start(test_track(), None, engine.pending_fade(), true, None);
        assert_eq!(rendered.nth(90), Some(0.8));
    }

    #[test]
    fn crossfade_envelopes_have_complementary_gain() {
        let channels = rodio::ChannelCount::new(1).unwrap();
        let sample_rate = rodio::SampleRate::new(1000).unwrap();
        let (mixer, mut output) = rodio::mixer::mixer(channels, sample_rate);
        let tone = |value: f32| {
            Arc::new(PcmTrack {
                samples: vec![value; 1000],
                channels,
                sample_rate,
            })
        };
        let start = |value, fade| {
            let track = tone(value);
            let cursor = Cursor::start(&track, None, fade, 1.0, true);
            start_voice(&mixer, track, cursor, 1.0, 1.0)
        };

        let old = start(0.25, 0.0);
        assert_eq!(output.nth(80), Some(0.25));
        let old = retire(Some(old), 0.2).unwrap();
        let next = start(0.75, 0.2);
        for frame in 0..=200 {
            let expected = 0.25 + 0.5 * frame as f32 / 200.0;
            let sample = output.next().unwrap();
            assert!(
                (sample - expected).abs() < 0.00001,
                "frame {frame}: {sample} != {expected}"
            );
        }
        assert!(old.player.empty());
        drop(old);
        assert_eq!(output.nth(798), Some(0.75));
        assert_eq!(output.next(), Some(0.0));
        assert!(next.player.empty());
        assert!(retire(Some(next), 0.2).is_none());

        let after_eof = start(0.5, 0.2);
        assert_eq!(output.next(), Some(0.0));
        assert!((output.nth(99).unwrap() - 0.25).abs() < 0.00001);
        assert!(retire(Some(after_eof), 0.0).is_none());
        assert!(
            output
                .by_ref()
                .take(1024)
                .skip(5)
                .all(|sample| sample == 0.0)
        );
        assert_eq!(output.next(), None);
    }
    #[test]
    fn three_second_loop_keeps_stereo_alignment_for_two_minutes() {
        let track = Arc::new(PcmTrack {
            samples: (0..3500)
                .flat_map(|frame| [frame as f32, -(frame as f32)])
                .collect(),
            channels: rodio::ChannelCount::new(2).unwrap(),
            sample_rate: rodio::SampleRate::new(1000).unwrap(),
        });
        let cursor = Arc::new(Mutex::new(Cursor {
            frame: 500.0,
            speed: 1.0,
            loop_region: loop_frames(&track, Some((0.5, 3.5))),
            revision: 0,
            envelope: Gain::new(1.0),
            transport: Gain::new(1.0),
            retiring: false,
        }));
        let mut source = TrackSource::new(track, cursor);
        for frame in 0..120_000 {
            let value = (500 + frame % 3000) as f32;
            assert_eq!(source.next(), Some(value));
            assert_eq!(source.next(), Some(-value));
        }
        assert_eq!(source.next(), Some(500.0));
    }

    #[test]
    fn stretch_preserves_pitch_stereo_and_duration_at_supported_rates() {
        const RATE: u32 = 48_000;
        const FREQUENCY: f32 = 443.0;
        let phase = |frame: usize| std::f32::consts::TAU * FREQUENCY * frame as f32 / RATE as f32;
        let track = Arc::new(PcmTrack {
            samples: (0..RATE)
                .flat_map(|frame| {
                    let sample = phase(frame as usize).sin();
                    [sample, -sample * 0.5]
                })
                .collect(),
            channels: rodio::ChannelCount::new(2).unwrap(),
            sample_rate: rodio::SampleRate::new(RATE).unwrap(),
        });
        for speed in [0.25, 4.0]
            .into_iter()
            .chain(bgm_core::prefs::PLAYBACK_RATES)
        {
            let cursor = Arc::new(Mutex::new(Cursor::start(&track, None, 0.0, speed, true)));
            let samples: Vec<f32> = TrackSource::new(track.clone(), cursor.clone()).collect();
            let expected = (RATE as f64 / f64::from(speed)).ceil() as usize;
            assert_eq!(samples.len(), expected * 2, "{speed}x");
            assert_eq!(cursor.lock().unwrap().frame, RATE as f64);
            assert!(
                samples
                    .chunks_exact(2)
                    .all(|pair| (pair[0] * 0.5 + pair[1]).abs() < 1e-6)
            );

            let window = samples.chunks_exact(2).skip(4800).take(6000);
            let count = window.len() as f32;
            let (sin, cos) = window
                .enumerate()
                .fold((0.0, 0.0), |(sin, cos), (i, pair)| {
                    let angle = phase(i);
                    (sin + pair[0] * angle.sin(), cos + pair[0] * angle.cos())
                });
            let fundamental = sin.hypot(cos) * 2.0 / count;
            assert!(
                fundamental > 0.8,
                "{speed}x: 443 Hz amplitude = {fundamental}"
            );
        }
    }

    #[test]
    fn changing_speed_keeps_loop_position_and_uses_output_time_for_pausing() {
        let track = Arc::new(PcmTrack {
            samples: (0..1000)
                .map(|frame| {
                    if (200..500).contains(&frame) {
                        0.5
                    } else {
                        -1.0
                    }
                })
                .collect(),
            channels: rodio::ChannelCount::new(1).unwrap(),
            sample_rate: rodio::SampleRate::new(1000).unwrap(),
        });
        let cursor = Arc::new(Mutex::new(Cursor::start(
            &track,
            Some((0.2, 0.5)),
            0.0,
            0.25,
            true,
        )));
        let mut source = TrackSource::new(track.clone(), cursor.clone());
        assert!(source.by_ref().take(100).all(|sample| sample >= 0.0));
        assert_eq!(cursor.lock().unwrap().frame, 225.0);

        cursor.lock().unwrap().speed = 4.0;
        assert!(source.by_ref().take(80).all(|sample| sample == 0.5));
        assert_eq!(cursor.lock().unwrap().frame, 245.0);

        let frames = track.fade_frames(TRANSPORT_FADE_SECONDS);
        cursor.lock().unwrap().transport.ramp_to(0.0, frames);
        assert!(source.by_ref().take(frames).all(|sample| sample > 0.0));
        assert_eq!(cursor.lock().unwrap().frame, 265.0);
        assert!(source.by_ref().take(frames).all(|sample| sample == 0.0));
        assert_eq!(cursor.lock().unwrap().frame, 265.0);
    }
}
