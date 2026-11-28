//! Native library loading and inference for the isolated speech worker only.

use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};
use std::slice;

use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use sherpa_onnx_sys as sys;

use super::{Init, TtsRequest};

const ASR_RATE: i32 = 16_000;
type Destroy<T> = unsafe extern "C" fn(*const T);
type Version = unsafe extern "C" fn() -> *const c_char;

pub(super) struct Native {
    api: SherpaApi,
    // Drop Sherpa before its ONNX Runtime dependency.
    _sherpa: Library,
    _ort: Library,
}

struct SherpaApi {
    create_asr:
        unsafe extern "C" fn(*const sys::OfflineRecognizerConfig) -> *const sys::OfflineRecognizer,
    destroy_asr: Destroy<sys::OfflineRecognizer>,
    create_stream: unsafe extern "C" fn(*const sys::OfflineRecognizer) -> *const sys::OfflineStream,
    destroy_stream: Destroy<sys::OfflineStream>,
    accept_waveform: unsafe extern "C" fn(*const sys::OfflineStream, i32, *const f32, i32),
    decode: unsafe extern "C" fn(*const sys::OfflineRecognizer, *const sys::OfflineStream),
    result_json: unsafe extern "C" fn(*const sys::OfflineStream) -> *const c_char,
    destroy_json: Destroy<c_char>,
    create_tts:
        unsafe extern "C" fn(*const sys::OfflineTtsConfig) -> *const sys::SherpaOnnxOfflineTts,
    destroy_tts: Destroy<sys::SherpaOnnxOfflineTts>,
    sample_rate: unsafe extern "C" fn(*const sys::SherpaOnnxOfflineTts) -> i32,
    speakers: unsafe extern "C" fn(*const sys::SherpaOnnxOfflineTts) -> i32,
    generate: unsafe extern "C" fn(
        *const sys::SherpaOnnxOfflineTts,
        *const c_char,
        *const sys::SherpaOnnxGenerationConfig,
        sys::SherpaOnnxGeneratedAudioProgressCallbackWithArg,
        *mut c_void,
    ) -> *const sys::SherpaOnnxGeneratedAudio,
    destroy_audio: Destroy<sys::SherpaOnnxGeneratedAudio>,
}

impl Native {
    pub(super) fn load(dir: &Path) -> Result<Self> {
        ensure!(
            dir.is_absolute(),
            "Sherpa CUDA runtime directory must be absolute"
        );
        ensure!(cfg!(target_arch = "x86_64"), "Sherpa CUDA requires x86-64");
        let (sherpa_name, ort_names): (&str, &[&str]) = if cfg!(windows) {
            ("sherpa-onnx-c-api.dll", &["onnxruntime.dll"])
        } else if cfg!(target_os = "linux") {
            (
                "libsherpa-onnx-c-api.so",
                &["libonnxruntime.so.1", "libonnxruntime.so"],
            )
        } else {
            bail!("Sherpa CUDA is only supported on Windows and Linux");
        };
        let dir = dir
            .canonicalize()
            .context("locating Sherpa CUDA runtime directory")?;
        let sherpa_path = find_library(&dir, &[sherpa_name])?;
        let sibling = sherpa_path.parent().unwrap_or(&dir);
        let ort_path =
            find_library(sibling, ort_names).or_else(|_| find_library(&dir, ort_names))?;
        // Preload the package's ORT by absolute path, not the CPU runtime of the app.
        let ort = open_library(&ort_path)?;
        let sherpa = open_library(&sherpa_path)?;

        // SAFETY: These version queries are part of both supported C ABIs. The
        // returned strings are static and their libraries are open throughout.
        unsafe {
            let version: Version = symbol(&sherpa, c"SherpaOnnxGetVersionStr")?;
            let version = version();
            ensure!(!version.is_null(), "Sherpa did not report a version");
            let version = CStr::from_ptr(version);
            ensure!(
                matches!(version.to_bytes(), b"2.0.1" | b"2.0.3"),
                "unsupported Sherpa CUDA version {:?}; expected 2.0.1 or 2.0.3",
                version
            );

            let get_ort: GetOrtApiBase = symbol(&ort, c"OrtGetApiBase")?;
            let sherpa_ort: Version = symbol(&sherpa, c"SherpaOnnxGetOnnxruntimeVersionStr")?;
            let base = get_ort()
                .as_ref()
                .context("ONNX Runtime did not return its API base")?;
            // Both supported Sherpa releases return ORT's static version pointer
            // directly. Comparing addresses also detects a same-version CPU copy.
            let expected = (base.version)();
            ensure!(
                !expected.is_null() && ptr::eq(sherpa_ort(), expected),
                "Sherpa did not bind to the ONNX Runtime loaded from {}",
                ort_path.display()
            );
            let api = (base.get_api)(12)
                .as_ref()
                .context("ONNX Runtime C API v12 is unavailable")?;
            probe_cuda(api)?;
        }

        // SAFETY: The version gate above admits only the two C ABIs with these
        // ASR/TTS signatures and layouts. No sys extern function is linked here.
        let api = unsafe { SherpaApi::load(&sherpa)? };
        Ok(Self {
            api,
            _sherpa: sherpa,
            _ort: ort,
        })
    }

    pub(super) fn start(&self, init: Init) -> Result<Engine<'_>> {
        if let Init::Asr { dir, threads } = init {
            return self.start_asr(&dir, threads);
        }

        let mut strings = ConfigStrings::default();
        // SAFETY: This repr(C) config and its nested fields contain only raw
        // pointers, integers and floats; zero is valid for each of them.
        let mut config: sys::OfflineTtsConfig = unsafe { std::mem::zeroed() };
        config.model.provider = c"cuda".as_ptr();
        config.max_num_sentences = 3;
        match init {
            Init::ZipVoice {
                tokens,
                encoder,
                decoder,
                vocoder,
                lexicon,
                threads,
            } => {
                config.model.num_threads = threads;
                config.model.zipvoice = sys::OfflineTtsZipvoiceModelConfig {
                    tokens: strings.push(tokens, "ZipVoice tokens")?,
                    encoder: strings.push(encoder, "ZipVoice encoder")?,
                    decoder: strings.push(decoder, "ZipVoice decoder")?,
                    vocoder: strings.push(vocoder, "ZipVoice vocoder")?,
                    lexicon: strings.push(lexicon, "ZipVoice lexicon")?,
                    data_dir: ptr::null(),
                    feat_scale: 0.1,
                    t_shift: 0.5,
                    target_rms: 0.1,
                    guidance_scale: 1.0,
                };
            }
            Init::Asr { .. } => unreachable!(),
        }
        // SAFETY: All config strings live through creation; Sherpa copies them.
        let tts = unsafe { (self.api.create_tts)(&config) };
        let tts = self
            .own(tts, self.api.destroy_tts)
            .context("Sherpa could not create the CUDA TTS engine")?;
        // SAFETY: tts is a live handle from this API.
        let (rate, speakers) = unsafe {
            (
                (self.api.sample_rate)(tts.as_ptr()),
                (self.api.speakers)(tts.as_ptr()),
            )
        };
        ensure!(
            (8000..=65535).contains(&rate) && speakers >= 0,
            "invalid Sherpa TTS metadata: {rate} Hz, {speakers} speakers"
        );
        Ok(Engine {
            native: self,
            kind: EngineKind::Tts(tts),
            metadata: (rate as u32, speakers),
        })
    }

    fn start_asr(&self, dir: &Path, threads: u32) -> Result<Engine<'_>> {
        let mut strings = ConfigStrings::default();
        let mut path = |name| {
            let path = dir.join(name);
            let utf8 = path
                .to_str()
                .with_context(|| format!("non-UTF-8 ASR path: {}", path.display()))?;
            strings.push(utf8, "Qwen3 ASR model path")
        };
        // SAFETY: This repr(C) config and its nested fields contain only raw
        // pointers, integers and floats; zero is valid for each of them.
        let mut config: sys::OfflineRecognizerConfig = unsafe { std::mem::zeroed() };
        config.feat_config = sys::FeatureConfig {
            sample_rate: ASR_RATE,
            feature_dim: 80,
        };
        config.lm_config.scale = 1.0;
        config.max_active_paths = 4;
        config.ctc_fst_decoder_config.max_active = 3000;
        config.model_config.num_threads =
            i32::try_from(threads).context("ASR thread count is too large")?;
        config.model_config.provider = c"cuda".as_ptr();
        // Qwen3 uses a tokenizer directory instead of tokens.txt.
        config.model_config.tokens = c"".as_ptr();
        config.model_config.qwen3_asr = sys::OfflineQwen3ASRModelConfig {
            conv_frontend: path("conv_frontend.onnx")?,
            encoder: path("encoder.int8.onnx")?,
            decoder: path("decoder.int8.onnx")?,
            tokenizer: path("tokenizer")?,
            max_total_len: 512,
            max_new_tokens: 128,
            temperature: 1e-6,
            top_p: 0.8,
            seed: 42,
            hotwords: ptr::null(),
        };
        // SAFETY: All config strings live through creation; Sherpa copies them.
        let asr = unsafe { (self.api.create_asr)(&config) };
        let asr = self.own(asr, self.api.destroy_asr).with_context(|| {
            format!(
                "Sherpa could not create the CUDA Qwen3 recognizer from {}",
                dir.display()
            )
        })?;
        Ok(Engine {
            native: self,
            kind: EngineKind::Asr(asr),
            metadata: (ASR_RATE as u32, 0),
        })
    }

    // Only used with newly returned pointers and matching destructors from this API.
    fn own<T>(&self, raw: *const T, destroy: Destroy<T>) -> Option<Owned<'_, T>> {
        NonNull::new(raw.cast_mut()).map(|ptr| Owned {
            ptr,
            destroy,
            _native: PhantomData,
        })
    }
}

impl SherpaApi {
    unsafe fn load(lib: &Library) -> Result<Self> {
        // SAFETY: The caller checked the C ABI; each name matches its field's signature.
        unsafe {
            Ok(Self {
                create_asr: symbol(lib, c"SherpaOnnxCreateOfflineRecognizer")?,
                destroy_asr: symbol(lib, c"SherpaOnnxDestroyOfflineRecognizer")?,
                create_stream: symbol(lib, c"SherpaOnnxCreateOfflineStream")?,
                destroy_stream: symbol(lib, c"SherpaOnnxDestroyOfflineStream")?,
                accept_waveform: symbol(lib, c"SherpaOnnxAcceptWaveformOffline")?,
                decode: symbol(lib, c"SherpaOnnxDecodeOfflineStream")?,
                result_json: symbol(lib, c"SherpaOnnxGetOfflineStreamResultAsJson")?,
                destroy_json: symbol(lib, c"SherpaOnnxDestroyOfflineStreamResultJson")?,
                create_tts: symbol(lib, c"SherpaOnnxCreateOfflineTts")?,
                destroy_tts: symbol(lib, c"SherpaOnnxDestroyOfflineTts")?,
                sample_rate: symbol(lib, c"SherpaOnnxOfflineTtsSampleRate")?,
                speakers: symbol(lib, c"SherpaOnnxOfflineTtsNumSpeakers")?,
                generate: symbol(lib, c"SherpaOnnxOfflineTtsGenerateWithConfig")?,
                destroy_audio: symbol(lib, c"SherpaOnnxDestroyOfflineTtsGeneratedAudio")?,
            })
        }
    }
}

pub(super) struct Engine<'a> {
    native: &'a Native,
    kind: EngineKind<'a>,
    metadata: (u32, i32),
}

enum EngineKind<'a> {
    Asr(Owned<'a, sys::OfflineRecognizer>),
    Tts(Owned<'a, sys::SherpaOnnxOfflineTts>),
}

impl Engine<'_> {
    pub(super) fn metadata(&self) -> (u32, i32) {
        self.metadata
    }

    pub(super) fn decode(&self, samples: &[f32]) -> Result<String> {
        let EngineKind::Asr(asr) = &self.kind else {
            bail!("speech engine is not an ASR recognizer")
        };
        ensure!(!samples.is_empty(), "ASR input is empty");
        let n = i32::try_from(samples.len()).context("ASR input is too long")?;
        let api = &self.native.api;
        // SAFETY: asr remains alive for the entire lifetime of the stream.
        let stream = unsafe { (api.create_stream)(asr.as_ptr()) };
        let stream = self
            .native
            .own(stream, api.destroy_stream)
            .context("Sherpa could not create an ASR stream")?;
        // SAFETY: Samples are readable for n floats, and the stream belongs to asr.
        let result = unsafe {
            (api.accept_waveform)(stream.as_ptr(), ASR_RATE, samples.as_ptr(), n);
            (api.decode)(asr.as_ptr(), stream.as_ptr());
            (api.result_json)(stream.as_ptr())
        };
        let result = self
            .native
            .own(result, api.destroy_json)
            .context("Sherpa did not return an ASR result")?;
        // SAFETY: The result points to Sherpa's NUL-terminated JSON until dropped.
        let json = unsafe { CStr::from_ptr(result.as_ptr()) };
        #[derive(serde::Deserialize)]
        struct Text {
            text: String,
        }
        let parsed: Text =
            serde_json::from_slice(json.to_bytes()).context("invalid Sherpa ASR JSON result")?;
        Ok(parsed.text)
    }

    pub(super) fn generate(
        &self,
        text: &str,
        req: &TtsRequest,
        reference_audio: &[f32],
    ) -> Result<Vec<f32>> {
        let EngineKind::Tts(tts) = &self.kind else {
            bail!("speech engine is not a TTS engine")
        };
        ensure!(
            req.speed.is_finite() && req.speed > 0.0,
            "invalid TTS speed"
        );
        ensure!(
            req.silence_scale.is_finite() && req.silence_scale >= 0.0,
            "invalid TTS silence scale"
        );
        ensure!(
            reference_audio.is_empty() || req.reference_sample_rate > 0,
            "invalid TTS reference sample rate"
        );
        let text = CString::new(text).context("TTS text contains NUL")?;
        let reference_text = req
            .reference_text
            .as_deref()
            .map(CString::new)
            .transpose()
            .context("TTS reference text contains NUL")?;
        let config = sys::SherpaOnnxGenerationConfig {
            silence_scale: req.silence_scale,
            speed: req.speed,
            sid: req.sid,
            reference_audio: if reference_audio.is_empty() {
                ptr::null()
            } else {
                reference_audio.as_ptr()
            },
            reference_audio_len: i32::try_from(reference_audio.len())
                .context("TTS reference audio is too long")?,
            reference_sample_rate: req.reference_sample_rate,
            reference_text: reference_text.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            num_steps: req.num_steps,
            extra: c"{}".as_ptr(),
            phoneme_codepoints: ptr::null(),
            phoneme_codepoints_lens: ptr::null(),
            phoneme_codepoints_num_sentences: 0,
            tokens: ptr::null(),
            tokens_lens: ptr::null(),
            tokens_num_sentences: 0,
        };
        let api = &self.native.api;
        // SAFETY: The config, strings and reference samples outlive this synchronous call.
        let audio =
            unsafe { (api.generate)(tts.as_ptr(), text.as_ptr(), &config, None, ptr::null_mut()) };
        let audio = self
            .native
            .own(audio, api.destroy_audio)
            .context("Sherpa did not return generated audio")?;
        // SAFETY: Sherpa returned an initialized output struct owned by the guard.
        let output = unsafe { &*audio.as_ptr() };
        ensure!(
            output.sample_rate == self.metadata.0 as i32,
            "Sherpa returned audio at an unexpected sample rate"
        );
        ensure!(
            output.n > 0 && !output.samples.is_null(),
            "Sherpa returned empty audio"
        );
        let n = output.n as usize;
        ensure!(
            n <= isize::MAX as usize / size_of::<f32>(),
            "Sherpa audio is too large"
        );
        // SAFETY: The C API guarantees n initialized floats for a nonempty output;
        // copy them before returning the C buffer to its allocator.
        Ok(unsafe { slice::from_raw_parts(output.samples, n) }.to_vec())
    }
}

struct Owned<'a, T> {
    ptr: NonNull<T>,
    destroy: Destroy<T>,
    _native: PhantomData<&'a Native>,
}

impl<T> Owned<'_, T> {
    fn as_ptr(&self) -> *const T {
        self.ptr.as_ptr()
    }
}

impl<T> Drop for Owned<'_, T> {
    fn drop(&mut self) {
        // SAFETY: This guard exclusively owns the non-null object and borrows its library.
        unsafe { (self.destroy)(self.as_ptr()) };
    }
}

#[derive(Default)]
struct ConfigStrings(Vec<CString>);

impl ConfigStrings {
    fn push(&mut self, value: impl Into<Vec<u8>>, label: &str) -> Result<*const c_char> {
        let value = CString::new(value).with_context(|| format!("{label} contains NUL"))?;
        let raw = value.as_ptr();
        // Moving a CString into (or within) the Vec does not move its buffer.
        self.0.push(value);
        Ok(raw)
    }
}

fn find_library(dir: &Path, names: &[&str]) -> Result<PathBuf> {
    [dir.to_path_buf(), dir.join("lib"), dir.join("bin")]
        .iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())
        .with_context(|| {
            format!(
                "could not find {} under {}",
                names.join(" or "),
                dir.display()
            )
        })
}

fn open_library(path: &Path) -> Result<Library> {
    ensure!(path.is_absolute(), "native library path must be absolute");
    #[cfg(windows)]
    let loaded = {
        use libloading::os::windows::{
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32, Library as WinLibrary,
        };
        // SAFETY: This is the explicitly configured native package in the worker.
        // Resolve imports from the DLL directory and System32, not cwd or PATH.
        unsafe {
            WinLibrary::load_with_flags(
                path,
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        }
        .map(Library::from)
    };
    #[cfg(target_os = "linux")]
    let loaded = {
        use libloading::os::unix::{Library as UnixLibrary, RTLD_LOCAL, RTLD_NOW};
        // SAFETY: The child was started with the package on LD_LIBRARY_PATH.
        unsafe { UnixLibrary::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.map(Library::from)
    };
    #[cfg(any(windows, target_os = "linux"))]
    return loaded.with_context(|| format!("loading {}", path.display()));
    #[cfg(not(any(windows, target_os = "linux")))]
    bail!("Sherpa CUDA library loading is unsupported on this platform");
}

// SAFETY: T must be the symbol's real function-pointer type, and calls to the
// copied pointer must not outlive lib.
unsafe fn symbol<T: Copy>(lib: &Library, name: &CStr) -> Result<T> {
    unsafe { lib.get::<T>(name.to_bytes_with_nul()) }
        .map(|symbol| *symbol)
        .with_context(|| format!("missing native symbol {}", name.to_string_lossy()))
}

type GetOrtApiBase = unsafe extern "system" fn() -> *const OrtApiBase;
type OrtStatus = c_void;
type OrtSessionOptions = c_void;
type OrtCudaOptions = c_void;
type OrtSlot = MaybeUninit<unsafe extern "system" fn()>;

#[repr(C)]
struct OrtApiBase {
    get_api: unsafe extern "system" fn(u32) -> *const OrtApi,
    version: VersionOrt,
}
type VersionOrt = unsafe extern "system" fn() -> *const c_char;

// Prefix of the append-only OrtApi table in ONNX Runtime's onnxruntime_c_api.h
// at API v12. Unused entries are reserved function-pointer-sized slots.
#[repr(C)]
struct OrtApi {
    _before_error_message: [OrtSlot; 2],
    error_message: unsafe extern "system" fn(*const OrtStatus) -> *const c_char, // 2
    create_env: unsafe extern "system" fn(i32, *const c_char, *mut *mut c_void) -> *mut OrtStatus, // 3
    _before_create_session: [OrtSlot; 6],
    create_session_options:
        unsafe extern "system" fn(*mut *mut OrtSessionOptions) -> *mut OrtStatus, // 10
    _before_release_env: [OrtSlot; 81],
    release_env: unsafe extern "system" fn(*mut c_void), // 92
    release_status: unsafe extern "system" fn(*mut OrtStatus), // 93
    _before_release_session: [OrtSlot; 6],
    release_session_options: unsafe extern "system" fn(*mut OrtSessionOptions), // 100
    _before_append_cuda: [OrtSlot; 103],
    append_cuda:
        unsafe extern "system" fn(*mut OrtSessionOptions, *const OrtCudaOptions) -> *mut OrtStatus, // 204, CUDA V2
    create_cuda_options: unsafe extern "system" fn(*mut *mut OrtCudaOptions) -> *mut OrtStatus, // 205
    _before_release_cuda: [OrtSlot; 2],
    release_cuda_options: unsafe extern "system" fn(*mut OrtCudaOptions), // 208
}

fn probe_cuda(api: &OrtApi) -> Result<()> {
    let mut options = ProbeOptions {
        api,
        env: ptr::null_mut(),
        session: ptr::null_mut(),
        cuda: ptr::null_mut(),
    };
    // SAFETY: api is a live v12 table. The output pointers are writable and the
    // guard releases each allocation, even if a later probe step fails.
    unsafe {
        ort_check(
            api,
            (api.create_env)(2, c"tfy-sherpa-cuda".as_ptr(), &mut options.env),
            "could not create ONNX Runtime environment",
        )?;
        ort_check(
            api,
            (api.create_session_options)(&mut options.session),
            "could not create ONNX Runtime session options",
        )?;
        ort_check(
            api,
            (api.create_cuda_options)(&mut options.cuda),
            "could not create ONNX Runtime CUDA options",
        )?;
        ensure!(
            !options.session.is_null() && !options.cuda.is_null(),
            "ONNX Runtime returned null options"
        );
        // Unlike enumerating provider names, appending CUDA forces ORT to load
        // the provider and its dependencies. Device 0 is local to the worker.
        ort_check(
            api,
            (api.append_cuda)(options.session, options.cuda),
            "could not enable ONNX Runtime CUDA (CPU package, missing CUDA dependencies, or unavailable GPU)",
        )
    }
}

unsafe fn ort_check(api: &OrtApi, status: *mut OrtStatus, operation: &str) -> Result<()> {
    if status.is_null() {
        return Ok(());
    }
    // SAFETY: A non-null status is owned by this call and comes from this API.
    let message = unsafe {
        let raw = (api.error_message)(status);
        let message = if raw.is_null() {
            "unknown error".to_owned()
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        };
        (api.release_status)(status);
        message
    };
    bail!("{operation}: {message}")
}

struct ProbeOptions<'a> {
    api: &'a OrtApi,
    env: *mut c_void,
    session: *mut OrtSessionOptions,
    cuda: *mut OrtCudaOptions,
}

impl Drop for ProbeOptions<'_> {
    fn drop(&mut self) {
        // SAFETY: Non-null pointers are owned options from the borrowed API.
        unsafe {
            if !self.session.is_null() {
                (self.api.release_session_options)(self.session);
            }
            if !self.cuda.is_null() {
                (self.api.release_cuda_options)(self.cuda);
            }
            if !self.env.is_null() {
                (self.api.release_env)(self.env);
            }
        }
    }
}
