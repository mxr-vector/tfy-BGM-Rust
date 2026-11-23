//! sherpa-onnx 动态库的离线识别接口。

mod ffi;

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr::NonNull;

use libloading::Library;

use crate::library::sherpa::{open, symbol};
use serde::Deserialize;

use super::pcm;
use crate::library::SubtitleSegment;

type Handle = *const c_void;

struct Api {
    destroy: unsafe extern "C" fn(Handle),
    stream: unsafe extern "C" fn(Handle) -> Handle,
    free_stream: unsafe extern "C" fn(Handle),
    accept: unsafe extern "C" fn(Handle, i32, *const f32, i32),
    decode: unsafe extern "C" fn(Handle, Handle),
    result: unsafe extern "C" fn(Handle) -> *const c_char,
    free_result: unsafe extern "C" fn(*const c_char),
    read_wave: unsafe extern "C" fn(*const c_char) -> *const ffi::Wave,
    free_wave: unsafe extern "C" fn(*const ffi::Wave),
}

pub struct Engine {
    recognizer: Handle,
    api: Api,
    _library: Library,
}

impl Engine {
    pub fn open(runtime: &Path, model: &Path) -> Result<Self, String> {
        let library = open(runtime)?;
        let api = unsafe {
            Api {
                destroy: symbol(&library, b"SherpaOnnxDestroyOfflineRecognizer\0")?,
                stream: symbol(&library, b"SherpaOnnxCreateOfflineStream\0")?,
                free_stream: symbol(&library, b"SherpaOnnxDestroyOfflineStream\0")?,
                accept: symbol(&library, b"SherpaOnnxAcceptWaveformOffline\0")?,
                decode: symbol(&library, b"SherpaOnnxDecodeOfflineStream\0")?,
                result: symbol(&library, b"SherpaOnnxGetOfflineStreamResultAsJson\0")?,
                free_result: symbol(&library, b"SherpaOnnxDestroyOfflineStreamResultJson\0")?,
                read_wave: symbol(&library, b"SherpaOnnxReadWave\0")?,
                free_wave: symbol(&library, b"SherpaOnnxFreeWave\0")?,
            }
        };
        let create: unsafe extern "C" fn(*const ffi::OfflineRecognizerConfig) -> Handle =
            unsafe { symbol(&library, b"SherpaOnnxCreateOfflineRecognizer\0")? };
        let frontend = path_string(&model.join("conv_frontend.onnx"))?;
        let encoder = path_string(&model.join("encoder.int8.onnx"))?;
        let decoder = path_string(&model.join("decoder.int8.onnx"))?;
        let tokenizer = path_string(&model.join("tokenizer"))?;
        // C 配置只含数值和指针，全零为有效初始化值。
        let mut config: ffi::OfflineRecognizerConfig = unsafe { std::mem::zeroed() };
        config.feat_config.sample_rate = pcm::SAMPLE_RATE as i32;
        config.feat_config.feature_dim = 80;
        config.model_config.tokens = c"".as_ptr();
        config.model_config.provider = c"cpu".as_ptr();
        config.model_config.num_threads =
            std::thread::available_parallelism().map_or(1, |count| count.get().min(4)) as i32;
        config.model_config.qwen3_asr.conv_frontend = frontend.as_ptr();
        config.model_config.qwen3_asr.encoder = encoder.as_ptr();
        config.model_config.qwen3_asr.decoder = decoder.as_ptr();
        config.model_config.qwen3_asr.tokenizer = tokenizer.as_ptr();
        config.decoding_method = c"greedy_search".as_ptr();
        let recognizer = unsafe { create(&config) };
        if recognizer.is_null() {
            return Err("sherpa-onnx 无法加载 Qwen3-ASR 模型".into());
        }
        Ok(Self {
            recognizer,
            api,
            _library: library,
        })
    }

    pub fn transcribe(
        &self,
        path: &Path,
        progress: impl FnMut(usize, usize) -> Result<(), String>,
    ) -> Result<Vec<SubtitleSegment>, String> {
        let path = path_string(path)?;
        let raw = unsafe { (self.api.read_wave)(path.as_ptr()) };
        let wave = NonNull::new(raw.cast_mut()).ok_or("无法读取音频，请选择单声道 WAV")?;
        let wave = WaveGuard {
            wave,
            release: self.api.free_wave,
        };
        let raw = unsafe { wave.wave.as_ref() };
        let frame_count: usize = raw.num_samples.try_into().map_err(|_| "WAV 采样数量无效")?;
        if frame_count == 0 || raw.samples.is_null() {
            return Err("WAV 音频没有有效采样".into());
        }
        let input = unsafe { std::slice::from_raw_parts(raw.samples, frame_count) };
        if raw.sample_rate == pcm::SAMPLE_RATE as i32 {
            self.decode(input, progress)
        } else {
            self.decode(&pcm::resample(input, raw.sample_rate)?, progress)
        }
    }

    fn decode(
        &self,
        pcm: &[f32],
        mut progress: impl FnMut(usize, usize) -> Result<(), String>,
    ) -> Result<Vec<SubtitleSegment>, String> {
        let mut segments = Vec::new();
        let windows = pcm::windows(pcm, 10.);
        let total = windows.len();
        for (index, range) in windows.into_iter().enumerate() {
            progress(index + 1, total)?;
            let start = range.start as f64 / pcm::SAMPLE_RATE as f64;
            let end = range.end as f64 / pcm::SAMPLE_RATE as f64;
            let text = self.decode_window(&pcm[range])?;
            if !text.is_empty() {
                segments.push(SubtitleSegment { start, end, text });
            }
        }
        if segments.is_empty() {
            return Err("ASR 未识别出文本，请确认音频包含人声".into());
        }
        Ok(segments)
    }

    fn decode_window(&self, pcm: &[f32]) -> Result<String, String> {
        let stream = unsafe { (self.api.stream)(self.recognizer) };
        if stream.is_null() {
            return Err("无法创建 ASR 识别流".into());
        }
        let result = unsafe {
            (self.api.accept)(
                stream,
                pcm::SAMPLE_RATE as i32,
                pcm.as_ptr(),
                pcm.len() as i32,
            );
            (self.api.decode)(self.recognizer, stream);
            let json = (self.api.result)(stream);
            let result = if json.is_null() {
                Err("sherpa-onnx 未返回识别结果".into())
            } else {
                let result = parse_result(CStr::from_ptr(json).to_bytes());
                (self.api.free_result)(json);
                result
            };
            (self.api.free_stream)(stream);
            result
        };
        result
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { (self.api.destroy)(self.recognizer) };
    }
}

struct WaveGuard {
    wave: NonNull<ffi::Wave>,
    release: unsafe extern "C" fn(*const ffi::Wave),
}

impl Drop for WaveGuard {
    fn drop(&mut self) {
        unsafe { (self.release)(self.wave.as_ptr()) };
    }
}

fn path_string(path: &Path) -> Result<CString, String> {
    CString::new(path.to_str().ok_or("ASR 文件路径不是有效文本")?)
        .map_err(|_| "ASR 文件路径含无效字符".into())
}

fn parse_result(bytes: &[u8]) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Text {
        text: String,
    }
    let result: Text =
        serde_json::from_slice(bytes).map_err(|error| format!("无法解析识别结果：{error}"))?;
    Ok(pcm::clean_text(&result.text))
}
