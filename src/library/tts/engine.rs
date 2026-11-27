//! sherpa-onnx 2.0.2 的本地合成会话和 C API 资源。

use std::ffi::{CString, c_char, c_void};
use std::path::Path;
use std::ptr::{self, NonNull};

use libloading::Library;

use crate::library::sherpa;

use super::Model;
use super::models::Family;

type CStr = *const c_char;

#[repr(C)]
#[derive(Default)]
struct Vits {
    model: CStr,
    lexicon: CStr,
    tokens: CStr,
    data: CStr,
    noise_scale: f32,
    noise_scale_w: f32,
    length_scale: f32,
    dict: CStr,
}

#[repr(C)]
#[derive(Default)]
struct Matcha {
    model: CStr,
    vocoder: CStr,
    lexicon: CStr,
    tokens: CStr,
    data: CStr,
    noise_scale: f32,
    length_scale: f32,
    dict: CStr,
}

#[repr(C)]
#[derive(Default)]
struct Kokoro {
    model: CStr,
    voices: CStr,
    tokens: CStr,
    data: CStr,
    length_scale: f32,
    dict: CStr,
    lexicon: CStr,
    lang: CStr,
}

#[repr(C)]
#[derive(Default)]
struct Kitten {
    model: CStr,
    voices: CStr,
    tokens: CStr,
    data: CStr,
    length_scale: f32,
    lexicon: CStr,
}

#[repr(C)]
#[derive(Default)]
struct ZipVoice {
    tokens: CStr,
    encoder: CStr,
    decoder: CStr,
    vocoder: CStr,
    data: CStr,
    lexicon: CStr,
    feat_scale: f32,
    t_shift: f32,
    target_rms: f32,
    guidance_scale: f32,
}

#[repr(C)]
#[derive(Default)]
struct Pocket {
    lm_flow: CStr,
    lm_main: CStr,
    encoder: CStr,
    decoder: CStr,
    conditioner: CStr,
    vocab: CStr,
    scores: CStr,
    capacity: i32,
}

#[repr(C)]
#[derive(Default)]
struct Supertonic {
    duration: CStr,
    encoder: CStr,
    estimator: CStr,
    vocoder: CStr,
    config: CStr,
    unicode: CStr,
    style: CStr,
}

#[repr(C)]
#[derive(Default)]
struct TtsModel {
    vits: Vits,
    threads: i32,
    debug: i32,
    provider: CStr,
    matcha: Matcha,
    kokoro: Kokoro,
    kitten: Kitten,
    zipvoice: ZipVoice,
    pocket: Pocket,
    supertonic: Supertonic,
}

#[repr(C)]
#[derive(Default)]
struct Config {
    model: TtsModel,
    rule_fsts: CStr,
    max_sentences: i32,
    rule_fars: CStr,
    silence_scale: f32,
}

#[repr(C)]
#[derive(Default)]
struct Generation {
    silence_scale: f32,
    speed: f32,
    speaker: i32,
    reference_audio: *const f32,
    reference_len: i32,
    reference_rate: i32,
    reference_text: CStr,
    steps: i32,
    extra: CStr,
    phonemes: *const i32,
    phoneme_lens: *const i32,
    phoneme_count: i32,
    tokens: *const CStr,
    token_lens: *const i32,
    token_count: i32,
}

#[repr(C)]
struct Audio {
    samples: *const f32,
    count: i32,
    rate: i32,
}

type Opaque = *const c_void;
type DestroyAudio = unsafe extern "C" fn(*const Audio);
type Callback = Option<unsafe extern "C" fn(*const f32, i32, f32, *mut c_void) -> i32>;

pub struct Engine {
    handle: NonNull<c_void>,
    destroy: unsafe extern "C" fn(Opaque),
    generate: unsafe extern "C" fn(
        Opaque,
        CStr,
        *const Generation,
        Callback,
        *mut c_void,
    ) -> *const Audio,
    release_audio: DestroyAudio,
    _library: Library,
    pub rate: u32,
    pub speakers: i32,
}

impl Engine {
    pub fn open(runtime: &Path, model: Model, directory: &Path) -> Result<Self, String> {
        model.check(directory)?;
        let lib = sherpa::open(runtime)?;
        let create: unsafe extern "C" fn(*const Config) -> Opaque =
            unsafe { sherpa::symbol(&lib, b"SherpaOnnxCreateOfflineTts\0")? };
        let destroy: unsafe extern "C" fn(Opaque) =
            unsafe { sherpa::symbol(&lib, b"SherpaOnnxDestroyOfflineTts\0")? };
        let rate: unsafe extern "C" fn(Opaque) -> i32 =
            unsafe { sherpa::symbol(&lib, b"SherpaOnnxOfflineTtsSampleRate\0")? };
        let speakers: unsafe extern "C" fn(Opaque) -> i32 =
            unsafe { sherpa::symbol(&lib, b"SherpaOnnxOfflineTtsNumSpeakers\0")? };
        let generate =
            unsafe { sherpa::symbol(&lib, b"SherpaOnnxOfflineTtsGenerateWithConfig\0")? };
        let release_audio =
            unsafe { sherpa::symbol(&lib, b"SherpaOnnxDestroyOfflineTtsGeneratedAudio\0")? };

        let join_paths = |paths: &[std::path::PathBuf]| -> Result<CString, String> {
            let mut names = Vec::with_capacity(paths.len());
            for path in paths {
                let name = path
                    .to_str()
                    .filter(|name| !name.contains(','))
                    .ok_or("模型目录不支持逗号或无效 UTF-8 字符")?;
                names.push(name);
            }
            CString::new(names.join(",")).map_err(|_| "模型路径无效".into())
        };
        let in_dir = |files: &[&str]| -> Result<CString, String> {
            let paths: Vec<_> = files.iter().map(|file| directory.join(file)).collect();
            join_paths(&paths)
        };
        let threads =
            std::thread::available_parallelism().map_or(2, |count| count.get().min(4)) as i32;
        let fst = ["date-zh.fst", "phone-zh.fst", "number-zh.fst"]
            .into_iter()
            .filter(|name| directory.join(name).is_file())
            .collect::<Vec<_>>();
        let rules = in_dir(&fst)?;
        let mut cfg = Config {
            model: TtsModel {
                threads,
                provider: c"cpu".as_ptr(),
                ..TtsModel::default()
            },
            rule_fsts: rules.as_ptr(),
            max_sentences: 3,
            silence_scale: 0.3,
            ..Config::default()
        };
        let primary;
        let secondary;
        let tokens;
        let lexicon;
        let vocoder;
        match model.family() {
            Family::ZipVoice => {
                primary = in_dir(&[model.encoder()])?;
                secondary = in_dir(&[model.decoder()])?;
                tokens = in_dir(&["tokens.txt"])?;
                lexicon = join_paths(&[
                    directory.join("lexicon.txt"),
                    Model::aux_path(directory, "lexicon-en-us.txt"),
                ])?;
                vocoder = join_paths(&[Model::aux_path(directory, "vocos_24khz.onnx")])?;
                cfg.model.zipvoice = ZipVoice {
                    tokens: tokens.as_ptr(),
                    encoder: primary.as_ptr(),
                    decoder: secondary.as_ptr(),
                    vocoder: vocoder.as_ptr(),
                    lexicon: lexicon.as_ptr(),
                    feat_scale: 0.1,
                    t_shift: 0.5,
                    target_rms: 0.1,
                    guidance_scale: 1.0,
                    ..ZipVoice::default()
                };
            }
            Family::Kokoro => {
                primary = in_dir(&[model.model_file(directory)])?;
                secondary = in_dir(&["voices.bin"])?;
                tokens = in_dir(&["tokens.txt"])?;
                lexicon = in_dir(&["lexicon-us-en.txt", "lexicon-zh.txt"])?;
                cfg.model.kokoro = Kokoro {
                    model: primary.as_ptr(),
                    voices: secondary.as_ptr(),
                    tokens: tokens.as_ptr(),
                    lexicon: lexicon.as_ptr(),
                    length_scale: 1.0,
                    ..Kokoro::default()
                };
            }
            Family::CosyVoice3 => {
                return Err("CosyVoice3 使用独立 Python ONNX 进程".into());
            }
        }
        let handle =
            NonNull::new(unsafe { create(&cfg) }.cast_mut()).ok_or("未能初始化语音模型")?;
        let info = unsafe { (rate(handle.as_ptr()), speakers(handle.as_ptr())) };
        Ok(Self {
            handle,
            destroy,
            generate,
            release_audio,
            _library: lib,
            rate: info.0.try_into().unwrap_or(0),
            speakers: info.1,
        })
    }

    pub fn synthesize(
        &self,
        text: &str,
        speaker: i32,
        speed: f32,
        reference: Option<(&str, u32, &[f32])>,
    ) -> Result<Vec<f32>, String> {
        let text = CString::new(text).map_err(|_| "文本包含无效字符")?;
        let mut config = Generation {
            speaker,
            speed,
            silence_scale: 0.3,
            steps: 4,
            ..Generation::default()
        };
        let prompt;
        if let Some((transcript, rate, samples)) = reference {
            prompt = CString::new(transcript).map_err(|_| "参考文本无效")?;
            config.reference_text = prompt.as_ptr();
            config.reference_audio = samples.as_ptr();
            config.reference_len = samples.len() as i32;
            config.reference_rate = rate as i32;
        } else if !(0..self.speakers).contains(&speaker) {
            return Err("所选 Kokoro 语音不可用".into());
        }
        let audio = unsafe {
            (self.generate)(
                self.handle.as_ptr(),
                text.as_ptr(),
                &config,
                None,
                ptr::null_mut(),
            )
        };
        let audio = NonNull::new(audio.cast_mut())
            .map(|ptr| AudioBuffer(ptr, self.release_audio))
            .ok_or("模型没有返回音频，请检查文本语言与音色")?;
        let info = unsafe { audio.0.as_ref() };
        if info.samples.is_null()
            || info.count <= 0
            || info.rate != self.rate as i32
            || info.count as u32 > self.rate * 30 * 60
        {
            return Err("模型返回的音频长度或采样率无效".into());
        }
        Ok(unsafe { std::slice::from_raw_parts(info.samples, info.count as usize) }.to_vec())
    }
}

struct AudioBuffer(NonNull<Audio>, DestroyAudio);

impl Drop for AudioBuffer {
    fn drop(&mut self) {
        unsafe { (self.1)(self.0.as_ptr()) };
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { (self.destroy)(self.handle.as_ptr()) };
    }
}
