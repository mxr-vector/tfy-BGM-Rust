use std::ffi::{CString, c_char, c_void};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr::{self, NonNull};

use anyhow::{Context, Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use super::{ACCOMPANIMENT_WAV, VOCALS_WAV, models};
use crate::library::sherpa;

const SAMPLE_RATE: u32 = 44_100;
pub(super) const JOB_FILE: &str = "job.json";
pub(super) const RESULT_FILE: &str = "result.json";
const LONG_AUDIO_FRAMES: usize = 10 * 60 * SAMPLE_RATE as usize;

#[derive(Deserialize, Serialize)]
pub(super) struct Job {
    pub source: PathBuf,
    pub runtime: PathBuf,
    pub vocals: models::Engine,
    pub target: models::Target,
    pub accompaniment: models::Engine,
}

pub fn run(job_path: &Path) -> Result<()> {
    let directory = job_path.parent().context("分离任务缺少工作目录")?;
    let job: Job = serde_json::from_reader(BufReader::new(File::open(job_path)?))?;
    let result = execute(&job, directory);
    let payload: Result<(), String> = result
        .as_ref()
        .map(|_| ())
        .map_err(|error| format!("{error:#}"));
    serde_json::to_writer(File::create(directory.join(RESULT_FILE))?, &payload)?;
    result
}

fn execute(job: &Job, directory: &Path) -> Result<()> {
    job.vocals.validate()?;
    job.accompaniment.validate()?;
    let input = decode(&job.source, &directory.join("decoded.f32"))?;
    let frames = input[0].len();
    let library = sherpa::open(&job.runtime).map_err(anyhow::Error::msg)?;
    let api = Api::load(&library)?;
    let cores = std::thread::available_parallelism().map_or(2, |cores| cores.get());
    let parallel = frames <= LONG_AUDIO_FRAMES;
    let threads = (cores / if parallel { 2 } else { 1 }).clamp(1, 4) as i32;

    let vocals = || {
        api.generate(
            &job.vocals,
            [&input[0], &input[1]],
            job.target.vocal_index(),
            threads,
            &directory.join(VOCALS_WAV),
        )
        .context("人声分离失败")
    };
    let accompaniment = || {
        api.generate(
            &job.accompaniment,
            [&input[0], &input[1]],
            0,
            threads,
            &directory.join(ACCOMPANIMENT_WAV),
        )
        .context("伴奏分离失败")
    };
    if parallel {
        std::thread::scope(|scope| {
            let vocals = scope.spawn(vocals);
            let accompaniment = scope.spawn(accompaniment);
            let vocals = vocals.join();
            let accompaniment = accompaniment.join();
            vocals.map_err(|_| anyhow!("人声分离线程异常退出"))??;
            accompaniment.map_err(|_| anyhow!("伴奏分离线程异常退出"))?
        })
    } else {
        vocals()?;
        accompaniment()
    }
}

fn decode(source: &Path, pcm_path: &Path) -> Result<[Vec<f32>; 2]> {
    let ffmpeg = bgm_core::loudness::ffmpeg_path().context("人声分离需要 FFmpeg")?;
    let result = Command::new(ffmpeg)
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(source)
        .args([
            "-map", "0:a:0", "-ac", "2", "-ar", "44100", "-f", "f32le", "pipe:1",
        ])
        .stdout(File::create(pcm_path)?)
        .stderr(Stdio::piped())
        .output()
        .context("无法运行 FFmpeg")?;
    ensure!(
        result.status.success(),
        "FFmpeg 解码失败：{}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    let input = File::open(pcm_path)?;
    let size = input.metadata()?.len();
    ensure!(size > 0 && size % 8 == 0, "FFmpeg 输出不是有效的双声道 PCM");
    let frames = usize::try_from(size / 8)?;
    ensure!(
        u32::try_from(size / 2 + 36).is_ok(),
        "音频长度超出 WAV 格式限制"
    );
    let mut channels = [Vec::new(), Vec::new()];
    for channel in &mut channels {
        channel
            .try_reserve_exact(frames)
            .context("没有足够内存读取音频")?;
    }
    let mut input = BufReader::new(input);
    let mut chunk = [0; 32768];
    let mut remaining = size;
    while remaining > 0 {
        let count = remaining.min(chunk.len() as u64) as usize;
        input.read_exact(&mut chunk[..count])?;
        for frame in chunk[..count].chunks_exact(8) {
            let (left, right) = frame.split_first_chunk::<4>().unwrap();
            let right: &[u8; 4] = right.try_into().unwrap();
            for (channel, sample) in channels.iter_mut().zip([left, right]) {
                let sample = f32::from_le_bytes(*sample);
                channel.push(if sample.is_finite() {
                    sample.clamp(-1.0, 1.0)
                } else {
                    0.0
                });
            }
        }
        remaining -= count as u64;
    }
    drop(input);
    fs::remove_file(pcm_path)?;
    Ok(channels)
}

#[repr(C)]
struct SpleeterConfig {
    vocals: *const c_char,
    accompaniment: *const c_char,
}

#[repr(C)]
struct UvrConfig {
    model: *const c_char,
}

#[repr(C)]
struct Config {
    spleeter: SpleeterConfig,
    uvr: UvrConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
struct Stem {
    samples: *const *const f32,
    num_channels: i32,
    n: i32,
}

#[repr(C)]
struct Output {
    stems: *const Stem,
    num_stems: i32,
    sample_rate: i32,
}

type Handle = *const c_void;

struct Api {
    create: unsafe extern "C" fn(*const Config) -> Handle,
    destroy: unsafe extern "C" fn(Handle),
    rate: unsafe extern "C" fn(Handle) -> i32,
    count: unsafe extern "C" fn(Handle) -> i32,
    process: unsafe extern "C" fn(Handle, *const *const f32, i32, i32, i32) -> *const Output,
    free: unsafe extern "C" fn(*const Output),
}

impl Api {
    fn load(library: &libloading::Library) -> Result<Self> {
        use sherpa::symbol;
        let load = || -> Result<Self, String> {
            unsafe {
                Ok(Self {
                    create: symbol(library, b"SherpaOnnxCreateOfflineSourceSeparation\0")?,
                    destroy: symbol(library, b"SherpaOnnxDestroyOfflineSourceSeparation\0")?,
                    rate: symbol(
                        library,
                        b"SherpaOnnxOfflineSourceSeparationGetOutputSampleRate\0",
                    )?,
                    count: symbol(
                        library,
                        b"SherpaOnnxOfflineSourceSeparationGetNumberOfStems\0",
                    )?,
                    process: symbol(library, b"SherpaOnnxOfflineSourceSeparationProcess\0")?,
                    free: symbol(library, b"SherpaOnnxDestroySourceSeparationOutput\0")?,
                })
            }
        };
        load().map_err(anyhow::Error::msg)
    }

    fn generate(
        &self,
        model: &models::Engine,
        channels: [&[f32]; 2],
        stem: usize,
        threads: i32,
        path: &Path,
    ) -> Result<()> {
        let mut config = Config {
            spleeter: SpleeterConfig {
                vocals: ptr::null(),
                accompaniment: ptr::null(),
            },
            uvr: UvrConfig { model: ptr::null() },
            num_threads: threads,
            debug: 0,
            provider: c"cpu".as_ptr(),
        };
        let (first, second);
        match model {
            models::Engine::Spleeter {
                vocals,
                accompaniment,
            } => {
                first = model_path(vocals)?;
                second = model_path(accompaniment)?;
                config.spleeter = SpleeterConfig {
                    vocals: first.as_ptr(),
                    accompaniment: second.as_ptr(),
                };
            }
            models::Engine::Uvr(path) => {
                first = model_path(path)?;
                config.uvr.model = first.as_ptr();
            }
        }
        let raw = unsafe { (self.create)(&config) };
        let handle = NonNull::new(raw.cast_mut()).context("无法初始化分离引擎")?;
        let engine = Native {
            pointer: handle,
            release: self.destroy,
        };
        let engine = engine.borrowed();
        let rate = unsafe { (self.rate)(engine) };
        ensure!(
            (1..=u16::MAX as i32).contains(&rate) && unsafe { (self.count)(engine) } == 2,
            "分离模型须输出采样率有效的双轨"
        );
        let input = channels.map(|channel| channel.as_ptr());
        let frames = i32::try_from(channels[0].len())?;
        let raw = unsafe { (self.process)(engine, input.as_ptr(), 2, frames, SAMPLE_RATE as i32) };
        let pointer = NonNull::new(raw.cast_mut()).context("分离引擎未返回音轨")?;
        let output = Native {
            pointer,
            release: self.free,
        };
        let output = unsafe { &*output.borrowed() };
        ensure!(
            output.num_stems == 2 && output.sample_rate == rate && !output.stems.is_null(),
            "分离引擎返回的音轨格式无效"
        );
        let stem = unsafe { &*output.stems.add(stem) };
        ensure!(
            stem.num_channels == 2 && stem.n > 0 && !stem.samples.is_null(),
            "分离引擎未返回完整的双声道音轨"
        );
        let samples = unsafe { std::slice::from_raw_parts(stem.samples, 2) };
        ensure!(
            samples.iter().all(|sample| !sample.is_null()),
            "分离引擎返回了空声道"
        );
        let left = unsafe { std::slice::from_raw_parts(samples[0], stem.n as usize) };
        let right = unsafe { std::slice::from_raw_parts(samples[1], stem.n as usize) };
        write_wav(path, rate as u32, left, right)
    }
}

struct Native<T> {
    pointer: NonNull<T>,
    release: unsafe extern "C" fn(*const T),
}

impl<T> Native<T> {
    fn borrowed(&self) -> *const T {
        self.pointer.as_ptr()
    }
}

impl<T> Drop for Native<T> {
    fn drop(&mut self) {
        unsafe { (self.release)(self.borrowed()) };
    }
}

fn model_path(path: &Path) -> Result<CString> {
    CString::new(path.to_str().context("分离模型路径必须为有效文本")?)
        .context("分离模型路径含无效字符")
}

fn wav_header(rate: u32, data: u32) -> Result<[u8; 44]> {
    let mut header = *b"RIFF\x00\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x00\x10\x00data\x00\x00\x00\x00";
    ensure!((1..=u16::MAX as u32).contains(&rate), "WAV 采样率无效");
    let riff_size = data.checked_add(36).context("音轨长度超出 WAV 格式限制")?;
    header[4..8].copy_from_slice(&riff_size.to_le_bytes());
    header[24..28].copy_from_slice(&rate.to_le_bytes());
    header[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
    header[40..44].copy_from_slice(&data.to_le_bytes());
    Ok(header)
}

fn write_wav(path: &Path, rate: u32, left: &[f32], right: &[f32]) -> Result<()> {
    let data = u32::try_from(left.len() as u64 * 4)?;
    let mut output = BufWriter::new(File::create(path)?);
    output.write_all(&wav_header(rate, data)?)?;
    for (&left, &right) in left.iter().zip(right) {
        ensure!(
            left.is_finite() && right.is_finite(),
            "分离引擎返回非有限采样值"
        );
        for sample in [left, right] {
            let sample = (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16;
            output.write_all(&sample.to_le_bytes())?;
        }
    }
    output.flush()?;
    output.get_ref().sync_all()?;
    Ok(())
}

pub(super) fn verify_wave(path: &Path) -> Result<u32> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let data = size.checked_sub(44).context("生成的 WAV 文件缺少文件头")?;
    ensure!(
        data > 0 && data % 4 == 0,
        "生成的 WAV 文件没有完整的双声道采样"
    );
    let mut header = [0; 44];
    file.read_exact(&mut header)?;
    let rate = u32::from_le_bytes(header[24..28].try_into().unwrap());
    ensure!(
        header == wav_header(rate, u32::try_from(data)?)?,
        "生成的 WAV 文件头无效"
    );
    Ok(rate)
}
