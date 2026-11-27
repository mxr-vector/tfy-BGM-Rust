//! 系统麦克风权限、限时单声道录音与取消。

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

const POLL: Duration = Duration::from_millis(40);

pub struct Capture {
    pub rate: NonZeroU32,
    pub mono: Vec<f32>,
}

pub struct Recording {
    pub elapsed: Arc<AtomicUsize>,
    signal: Arc<AtomicU8>,
    pub result: mpsc::Receiver<Result<Capture, String>>,
}

impl Recording {
    pub fn start(minimum: u32, maximum: u32) -> Result<Self, String> {
        let (tx, result) = mpsc::sync_channel(1);
        let elapsed = Arc::new(AtomicUsize::new(0));
        let signal = Arc::new(AtomicU8::new(0));
        let worker_elapsed = elapsed.clone();
        let worker_signal = signal.clone();
        std::thread::Builder::new()
            .name("tfy-microphone".into())
            .spawn(move || {
                let capture = record(minimum, maximum, &worker_elapsed, &worker_signal);
                let _ = tx.send(capture);
            })
            .map_err(|error| format!("无法启动录音：{error}"))?;
        Ok(Self {
            elapsed,
            signal,
            result,
        })
    }

    pub fn finish(&self) {
        let _ = self
            .signal
            .compare_exchange(0, 1, Ordering::Relaxed, Ordering::Relaxed);
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        self.signal.store(2, Ordering::Relaxed);
    }
}

fn record(
    minimum: u32,
    maximum: u32,
    elapsed: &AtomicUsize,
    signal: &AtomicU8,
) -> Result<Capture, String> {
    let pause_started = std::time::Instant::now();
    request_access(signal)?;
    if signal.load(Ordering::Relaxed) != 0 {
        return Err("已取消录音".into());
    }
    let device = cpal::default_host()
        .default_input_device()
        .ok_or("找不到系统默认麦克风")?;
    let config = device
        .default_input_config()
        .map_err(|error| format!("无法读取麦克风设置：{error}"))?;
    let rate = NonZeroU32::new(config.sample_rate()).ok_or("麦克风采样率无效")?;
    let channels = usize::from(config.channels());
    if channels == 0 || rate.get() > 192_000 {
        return Err("麦克风声道或采样率不受支持".into());
    }
    let max_samples = (rate.get() * maximum) as usize;
    let samples = Arc::new(Mutex::new(Vec::with_capacity(max_samples)));
    let audio = samples.clone();
    let (tx, errors) = mpsc::sync_channel(1);
    let err = move |error| {
        let _ = tx.try_send(format!("录音中断：{error}"));
    };
    let stream_config = config.config();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            stream_config,
            move |data: &[f32], _| {
                append(data, channels, max_samples, &audio, |value| value);
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            stream_config,
            move |data: &[i16], _| {
                append(data, channels, max_samples, &audio, |value| {
                    f32::from(value) / 32768.0
                });
            },
            err,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            stream_config,
            move |data: &[u16], _| {
                append(data, channels, max_samples, &audio, |value| {
                    f32::from(value) / 32768.0 - 1.0
                });
            },
            err,
            None,
        ),
        format => return Err(format!("不支持麦克风采样格式 {format}")),
    }
    .map_err(|error| format!("无法开启麦克风：{error}"))?;
    std::thread::sleep(Duration::from_millis(120).saturating_sub(pause_started.elapsed()));
    if signal.load(Ordering::Relaxed) != 0 {
        return Err("已取消录音".into());
    }
    stream
        .play()
        .map_err(|error| format!("无法开始录音：{error}"))?;
    let start = std::time::Instant::now();
    loop {
        let recorded = samples.lock().map_err(|_| "录音状态不可用")?.len();
        elapsed.store(recorded * 1000 / rate.get() as usize, Ordering::Relaxed);
        if let Ok(error) = errors.try_recv() {
            return Err(error);
        }
        if signal.load(Ordering::Relaxed) != 0 || recorded >= max_samples {
            break;
        }
        if start.elapsed() > Duration::from_secs(u64::from(maximum) + 3) {
            return Err("未收到完整的麦克风输入，请检查系统输入设备".into());
        }
        std::thread::sleep(POLL);
    }
    drop(stream);
    if signal.load(Ordering::Relaxed) == 2 {
        return Err("已取消录音".into());
    }
    let mono = std::mem::take(&mut *samples.lock().map_err(|_| "录音状态不可用")?);
    if mono.len() < (rate.get() * minimum) as usize {
        return Err(format!("录音至少需要 {minimum} 秒，请重新录制"));
    }
    Ok(Capture { rate, mono })
}

fn append<T: Copy>(
    input: &[T],
    channels: usize,
    limit: usize,
    samples: &Mutex<Vec<f32>>,
    convert: impl Fn(T) -> f32,
) {
    if let Ok(mut samples) = samples.lock() {
        let remaining = limit.saturating_sub(samples.len());
        samples.extend(
            input.chunks_exact(channels).take(remaining).map(|frame| {
                frame.iter().map(|&value| convert(value)).sum::<f32>() / channels as f32
            }),
        );
    }
}

#[cfg(target_os = "macos")]
fn request_access(signal: &AtomicU8) -> Result<(), String> {
    use block2::RcBlock;
    use objc2::runtime::{AnyClass, Bool};
    use objc2::{class, msg_send};
    use objc2_foundation::NSString;

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {
        static AVMediaTypeAudio: &'static NSString;
    }

    let device: &AnyClass = class!(AVCaptureDevice);
    let media_type = unsafe { AVMediaTypeAudio };
    let status: isize = unsafe { msg_send![device, authorizationStatusForMediaType: media_type] };
    match status {
        3 => Ok(()),
        0 => {
            let (tx, rx) = mpsc::sync_channel(1);
            let completion = RcBlock::new(move |granted: Bool| {
                let _ = tx.try_send(granted.as_bool());
            });
            unsafe {
                let _: () = msg_send![device, requestAccessForMediaType: media_type, completionHandler: &*completion];
            }
            while signal.load(Ordering::Relaxed) == 0 {
                match rx.recv_timeout(POLL) {
                    Ok(true) => return Ok(()),
                    Ok(false) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                }
            }
            Err(if signal.load(Ordering::Relaxed) == 0 {
                "麦克风权限未开启，请在系统设置中允许访问".into()
            } else {
                "已取消录音".into()
            })
        }
        _ => Err("麦克风权限未开启，请在系统设置中允许访问".into()),
    }
}

#[cfg(not(target_os = "macos"))]
fn request_access(_: &AtomicU8) -> Result<(), String> {
    Ok(())
}
