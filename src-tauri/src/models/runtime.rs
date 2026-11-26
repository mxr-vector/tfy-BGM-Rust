//! 模型运行期状态：加载、释放、占用协调与 ASR 推理。

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::{Context, Result, anyhow, bail};
use parking_lot::Mutex;
use sherpa_onnx::{OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig, Wave};
use tauri::{AppHandle, Emitter};

use super::catalog::{
    delete_model_files, missing_components, resolve_scanned_model, snapshot_model_files,
    total_bytes,
};
use super::download::download_files;
use super::segment::{clean_asr_text, plan_segments, resample_to_asr, strip_meta_instructions};
use super::{
    ASR_SAMPLE_RATE, AsrModel, DEFAULT_MODEL_NAME, DOWNLOAD_EVENT, DownloadProgress, ENGINE,
    LoadPhase, LoadedModel, ModelLoadStatus, QWEN3_FILES,
};

#[derive(Clone, Default)]
pub struct ModelRuntime {
    pub(super) loaded: Arc<Mutex<Option<Arc<Loaded>>>>,
    pub(super) downloading: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    /// 同一时刻只允许一个加载任务，防止下载完成后的自动加载与手动加载重复触发。
    pub(super) loading: Arc<AtomicBool>,
    /// 删除任务进行中；阻止新的加载 / 下载与正在删除的模型竞争。
    deleting: Arc<AtomicBool>,
    /// 加载、下载与删除共享的状态切换锁，避免检查和设置任务标志之间发生竞态。
    operation: Arc<Mutex<()>>,
    /// 正在使用已加载模型的 ASR / 字幕任务数；删除、释放或重新加载前必须协调。
    inference_tasks: Arc<AtomicUsize>,
    /// 最近一次加载状态，供 `ModelsInfo` 与状态事件读取。
    status: Arc<Mutex<ModelLoadStatus>>,
}

pub(super) struct Loaded {
    model: LoadedModel,
    recognizer: OfflineRecognizer,
}

/// ASR 推理或字幕生成任务对当前模型的占用；`Drop` 时自动释放。
pub struct ModelTaskGuard {
    tasks: Arc<AtomicUsize>,
}

impl Drop for ModelTaskGuard {
    fn drop(&mut self) {
        self.tasks.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `Drop` 时复位加载标志；即使 `load_inner` panic，后续加载也不会被永久拒绝。
/// 加载标志的守卫。
pub(super) struct LoadFlag<'a>(&'a AtomicBool);

impl Drop for LoadFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// `Drop` 时复位删除标志，文件删除失败也不会永久阻止后续加载。
/// 删除标志的守卫。
pub(super) struct DeleteFlag<'a>(&'a AtomicBool);

impl Drop for DeleteFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl ModelRuntime {
    pub fn loaded(&self) -> Option<LoadedModel> {
        self.loaded
            .lock()
            .as_ref()
            .map(|loaded| loaded.model.clone())
    }

    pub fn load_status(&self) -> ModelLoadStatus {
        self.status.lock().clone()
    }

    /// 供命令层在任务异常终止（阻塞线程 panic）时修正状态；正常流程由 `load` 自己维护。
    pub fn set_load_status(&self, status: ModelLoadStatus) {
        *self.status.lock() = status;
    }

    /// 目标模型是否已经成功加载；自动加载据此跳过重复加载。
    pub fn is_loaded_path(&self, dir: &Path) -> bool {
        if self.load_status().phase != LoadPhase::Success {
            return false;
        }
        let path = dir.to_string_lossy();
        self.loaded
            .lock()
            .as_ref()
            .is_some_and(|loaded| loaded.model.path == path)
    }

    pub fn is_downloading(&self) -> bool {
        self.downloading.load(Ordering::SeqCst)
    }

    /// 是否有加载任务在运行；命令层据此避免重复触发并保护正在进行的加载状态。
    pub fn is_loading(&self) -> bool {
        self.loading.load(Ordering::SeqCst)
    }

    /// 登记一次模型任务占用，阻止删除、释放或重新加载与任务竞争。
    pub fn reserve_model_task(&self) -> Result<ModelTaskGuard> {
        let _operation = self.operation.lock();
        if self.loading.load(Ordering::SeqCst) {
            bail!("模型正在加载中，请等待加载完成后再识别");
        }
        if self.deleting.load(Ordering::SeqCst) {
            bail!("模型正在删除中，请稍后再识别");
        }
        self.inference_tasks.fetch_add(1, Ordering::SeqCst);
        Ok(ModelTaskGuard {
            tasks: Arc::clone(&self.inference_tasks),
        })
    }

    pub fn unload(&self) -> Result<bool> {
        let _operation = self.operation.lock();
        if self.loading.load(Ordering::SeqCst) {
            bail!("模型正在加载中，请等待加载完成后再释放");
        }
        if self.deleting.load(Ordering::SeqCst) {
            bail!("模型正在删除中，请稍后再释放");
        }
        if self.inference_tasks.load(Ordering::SeqCst) > 0 {
            bail!("ASR 识别或字幕生成正在进行，请等待完成后再释放模型");
        }
        let removed = self.loaded.lock().take().is_some();
        if removed {
            self.set_load_status(ModelLoadStatus::default());
        }
        Ok(removed)
    }

    /// 请求中断正在进行的下载；返回是否有下载任务在运行。
    pub fn cancel_download(&self) -> bool {
        if self.downloading.load(Ordering::SeqCst) {
            self.cancelled.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    /// 用 sherpa-onnx 的 Rust API 加载 Qwen3 ASR；耗时较长，调用方应放到阻塞线程。
    ///
    /// 全程维护 `loading` 原子标志与 `status` 状态：并发的第二个加载请求直接被拒绝，
    /// 成功 / 失败都会写入可展示的状态（失败保留完整错误链），供界面重试。
    pub fn load(&self, root: &Path, dir: &Path) -> Result<LoadedModel> {
        {
            let _operation = self.operation.lock();
            if self.deleting.load(Ordering::SeqCst) {
                bail!("模型正在删除中，请稍后再加载");
            }
            if self.inference_tasks.load(Ordering::SeqCst) > 0 {
                bail!("ASR 识别或字幕生成正在进行，请等待完成后再加载模型");
            }
            if self
                .loading
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                bail!("已有模型加载任务正在进行");
            }
        }
        // 抢占成功后由 guard 负责复位，正常返回与 panic 展开都会释放标志。
        let _flag = LoadFlag(&self.loading);
        let target = dir.to_string_lossy().into_owned();
        self.set_load_status(ModelLoadStatus {
            phase: LoadPhase::Loading,
            path: Some(target.clone()),
            error: None,
        });

        let result = self.load_inner(root, dir);
        match &result {
            Ok(model) => self.set_load_status(ModelLoadStatus {
                phase: LoadPhase::Success,
                path: Some(model.path.clone()),
                error: None,
            }),
            Err(error) => self.set_load_status(ModelLoadStatus {
                phase: LoadPhase::Failed,
                path: Some(target),
                error: Some(format!("{error:#}")),
            }),
        }
        result
    }

    fn load_inner(&self, root: &Path, dir: &Path) -> Result<LoadedModel> {
        // canonical 只用于越界校验；Windows 上它会带 `\\?\` 前缀，
        // 因此仍用调用方传入的路径（来自扫描结果）作为模型标识，保证前端逐字匹配。
        let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let canonical_dir =
            fs::canonicalize(dir).with_context(|| format!("模型目录不存在：{}", dir.display()))?;
        if !canonical_dir.starts_with(&canonical_root) {
            bail!("模型必须位于当前模型目录下：{}", root.display());
        }
        let missing = missing_components(dir);
        if !missing.is_empty() {
            bail!("模型缺少必要构件：{}", missing.join("、"));
        }

        let threads = thread_count();
        let recognizer = create_recognizer(dir, threads)?;
        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| DEFAULT_MODEL_NAME.to_owned());
        let model = LoadedModel {
            name,
            path: dir.to_string_lossy().into_owned(),
            engine: ENGINE,
            threads,
        };
        *self.loaded.lock() = Some(Arc::new(Loaded {
            model: model.clone(),
            recognizer,
        }));
        Ok(model)
    }

    /// 用已加载的模型识别一个 WAV 文件；未加载模型时直接报错。
    ///
    /// 只在取模型句柄时短暂持锁，解码在锁外进行，避免识别期间阻塞
    /// `models_info` / 加载 / 释放等命令。
    pub fn transcribe(&self, wav: &Path) -> Result<String> {
        let _model_task = self.reserve_model_task()?;
        let loaded = {
            let guard = self.loaded.lock();
            guard.as_ref().cloned()
        }
        .ok_or_else(|| anyhow!("尚未加载 ASR 模型"))?;
        let filename = wav.to_string_lossy();
        let wave = Wave::read(&filename)
            .ok_or_else(|| anyhow!("无法读取音频文件（仅支持 WAV）：{}", wav.display()))?;

        // ffmpeg 归一化路径已输出 16 kHz；这里再做一次兜底重采样，覆盖
        // 「设置 → 模型 → 选择 WAV 识别」与「ffmpeg 缺失时直用源 WAV」两条入口，
        // 保证送入引擎的 PCM 恒为 16 kHz。
        let pcm = resample_to_asr(wave.samples(), wave.sample_rate())?;
        let stream = loaded.recognizer.create_stream();
        stream.accept_waveform(ASR_SAMPLE_RATE, &pcm);
        loaded.recognizer.decode(&stream);
        stream
            .get_result()
            .map(|result| strip_meta_instructions(&clean_asr_text(&result.text)))
            .ok_or_else(|| anyhow!("识别失败：sherpa-onnx 未返回结果"))
    }

    /// 按时间窗把单声道 WAV 切片逐段识别，生成带时间轴的字幕分段。
    ///
    /// Qwen3-ASR 的 Rust 结果虽然可能带 token 时间戳，但是否可用取决于模型版本；
    /// 这里改用「音频切片 + 逐片解码」这一与引擎版本无关的方式，保证 SRT / LRC
    /// 一定能得到起止时间。切点不按固定节拍硬切，而是在目标时长附近搜索能量最低的
    /// 20ms 帧（通常是换气 / 停顿处），减少把词切成两半。`progress(current, total)`
    /// 在每片完成后回调，供上层推送进度。
    pub fn transcribe_segments(
        &self,
        wav: &Path,
        window_secs: f64,
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<Vec<crate::subtitles::SubtitleSegment>> {
        use crate::subtitles::SubtitleSegment;

        let _model_task = self.reserve_model_task()?;
        let loaded = {
            let guard = self.loaded.lock();
            guard.as_ref().cloned()
        }
        .ok_or_else(|| anyhow!("尚未加载 ASR 模型，请先在「设置 → 模型」中完成加载"))?;
        let filename = wav.to_string_lossy();
        let wave = Wave::read(&filename)
            .ok_or_else(|| anyhow!("无法读取归一化音频（需为单声道 WAV）：{}", wav.display()))?;
        // ffmpeg 缺失时源 WAV 可能不是 16 kHz；先重采样再切片，
        // 样本区间与时间轴才严格对齐（16 kHz 输入为恒等路径，零额外开销）。
        let src_rate = wave.sample_rate();
        let pcm = resample_to_asr(wave.samples(), src_rate)?;
        let rate = ASR_SAMPLE_RATE;
        let samples = pcm.as_slice();
        let total_secs = samples.len() as f64 / rate as f64;
        if total_secs <= 0.0 {
            bail!("归一化后的音频为空，无法生成字幕");
        }

        let window = window_secs.clamp(2.0, 60.0);
        let ranges = plan_segments(samples, rate as usize, total_secs, window);
        if ranges.is_empty() {
            bail!("归一化后的音频为空，无法生成字幕");
        }
        let mut segments = Vec::with_capacity(ranges.len());
        for (index, (start, end)) in ranges.iter().copied().enumerate() {
            let from = ((start * rate as f64).round() as usize).min(samples.len());
            let to = ((end * rate as f64).round() as usize).min(samples.len());
            if to > from {
                let stream = loaded.recognizer.create_stream();
                stream.accept_waveform(rate, &samples[from..to]);
                loaded.recognizer.decode(&stream);
                if let Some(result) = stream.get_result() {
                    let text = strip_meta_instructions(&clean_asr_text(&result.text));
                    if !text.is_empty() {
                        segments.push(SubtitleSegment { start, end, text });
                    }
                }
            }
            progress(index + 1, ranges.len());
        }

        if segments.is_empty() {
            bail!("ASR 未识别出任何文本，请确认音频包含人声且模型已正确加载");
        }
        Ok(segments)
    }

    /// 删除当前模型目录中扫描到的指定 ASR 模型；已加载时自动释放并卸载。
    pub fn remove(&self, root: &Path, id: &str) -> Result<String> {
        self.remove_with(root, id, delete_model_files)
    }

    pub(super) fn remove_with(
        &self,
        root: &Path,
        id: &str,
        delete: impl FnOnce(&AsrModel, &Path) -> Result<()>,
    ) -> Result<String> {
        let _operation = self.operation.lock();
        if self.loading.load(Ordering::SeqCst) {
            bail!("模型正在加载中，请等待加载完成后再删除");
        }
        if self.downloading.load(Ordering::SeqCst) {
            bail!("模型正在下载中，请等待下载完成后再删除");
        }
        if self.inference_tasks.load(Ordering::SeqCst) > 0 {
            bail!("ASR 识别或字幕生成正在进行，请等待完成后再删除模型");
        }
        if self.deleting.swap(true, Ordering::SeqCst) {
            bail!("已有模型删除任务正在进行");
        }
        let _flag = DeleteFlag(&self.deleting);
        // 先校验目标并记录文件清单，避免无效 id 或删除失败破坏当前模型状态。
        let (model, target) = resolve_scanned_model(root, id)?;
        let files_before = snapshot_model_files(&model, &target)?;
        let previous_loaded = self.loaded.lock().as_ref().cloned();
        let previous_status = self.load_status();
        let matches_loaded = previous_loaded
            .as_ref()
            .is_some_and(|loaded| loaded.model.path == model.path);
        let unloaded = matches_loaded && self.loaded.lock().take().is_some();
        let status_matches = previous_status.path.as_deref() == Some(model.path.as_str());
        if unloaded || status_matches {
            self.set_load_status(ModelLoadStatus::default());
        }

        if let Err(error) = delete(&model, &target) {
            let intact =
                snapshot_model_files(&model, &target).is_ok_and(|current| current == files_before);
            if intact {
                if matches_loaded {
                    *self.loaded.lock() = previous_loaded;
                }
                if unloaded || status_matches {
                    self.set_load_status(previous_status);
                }
            } else if unloaded || status_matches {
                self.set_load_status(ModelLoadStatus {
                    phase: LoadPhase::Failed,
                    path: Some(model.path.clone()),
                    error: Some(format!("模型文件已部分删除，需重新下载后再加载：{error:#}")),
                });
            }
            return Err(error);
        }

        let path = model.path;
        // 删除失败加载记录的指向，避免状态条继续提示重试一个已经不存在的模型。
        if self.load_status().path.as_deref() == Some(path.as_str()) {
            self.set_load_status(ModelLoadStatus::default());
        }
        Ok(path)
    }

    /// 下载默认模型（0.6B-int8）；同一时间只允许一个下载任务。
    pub fn download(&self, app: &AppHandle, root: &Path, url: &str) -> Result<()> {
        // 先清掉上一次的取消标记，再抢下载标志，避免取消请求把新任务立刻中断。
        self.cancelled.store(false, Ordering::SeqCst);
        {
            let _operation = self.operation.lock();
            if self.deleting.load(Ordering::SeqCst) {
                bail!("模型正在删除中，请稍后再下载");
            }
            if self
                .downloading
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                bail!("已有模型下载任务正在进行");
            }
        }
        let result = download_files(app, root, url, &self.cancelled);
        self.downloading.store(false, Ordering::SeqCst);
        if let Err(error) = &result {
            let _ = app.emit(
                DOWNLOAD_EVENT,
                DownloadProgress {
                    file: String::new(),
                    index: 0,
                    total_files: QWEN3_FILES.len(),
                    received: 0,
                    file_bytes: 0,
                    overall_received: 0,
                    overall_bytes: total_bytes(),
                    done: true,
                    error: Some(format!("{error:#}")),
                },
            );
        }
        result
    }
}

/// 组装 `OfflineQwen3ASRModelConfig`：四个路径全部指向模型目录内的构件。
pub(super) fn create_recognizer(dir: &Path, threads: u32) -> Result<OfflineRecognizer> {
    let path_of = |name: &str| dir.join(name).to_string_lossy().into_owned();
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
        conv_frontend: Some(path_of("conv_frontend.onnx")),
        encoder: Some(path_of("encoder.int8.onnx")),
        decoder: Some(path_of("decoder.int8.onnx")),
        tokenizer: Some(dir.join("tokenizer").to_string_lossy().into_owned()),
        ..Default::default()
    };
    // Qwen3 使用 tokenizer 目录，不读取 tokens.txt；官方 Rust 示例同样传空串。
    config.model_config.tokens = Some(String::new());
    config.model_config.provider = Some("cpu".to_owned());
    config.model_config.num_threads = threads as i32;
    config.model_config.debug = false;

    OfflineRecognizer::create(&config).with_context(|| {
        format!(
            "sherpa-onnx 无法加载模型 {}（请检查 conv_frontend / encoder.int8 / decoder.int8 / tokenizer）",
            dir.display()
        )
    })
}

/// 推理线程数：取可用核心数并限制上限，避免占满整机。
pub(super) fn thread_count() -> u32 {
    std::thread::available_parallelism()
        .map(|count| count.get().min(4) as u32)
        .unwrap_or(2)
}
