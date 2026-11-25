//! 「基本系统配置 → 模型」：ASR 模型目录、扫描、下载与 sherpa-onnx 推理。
//!
//! 技术路线固定为 sherpa-onnx：Rust 端直接调用其安全封装
//! （`OfflineRecognizer` + `OfflineQwen3ASRModelConfig`），不引入其他推理框架。
//! 当前只支持 ASR 模型，也只识别 Qwen3 ASR 的四件套目录结构：
//! `conv_frontend.onnx`、`encoder.int8.onnx`、`decoder.int8.onnx`、`tokenizer/`。
//!
//! 默认模型固定为 0.6B-int8 版本；下载清单写死为这四个构件（外加 tokenizer 内
//! 的词表文件），配置里只允许替换下载地址，不允许换成本、换参数量。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Prefs;
use bgm_core::paths::MODELS_DIR_NAME;
use parking_lot::Mutex;
use serde::Serialize;
use sherpa_onnx::{OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig, Wave};
use tauri::{AppHandle, Emitter};

/// ASR 默认模型（仅 0.6B-int8 版本）。
pub const DEFAULT_MODEL_NAME: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";
/// 默认下载地址：ModelScope 上的 0.6B-int8 模型目录，仅作为镜像入口。
/// 值定义在 `bgm_core::prefs`，此处复用，保证偏好层与运行时使用同一个地址。
pub const DEFAULT_DOWNLOAD_URL: &str = bgm_core::prefs::DEFAULT_ASR_DOWNLOAD_URL;
/// 下载进度事件名；前端据此刷新进度条。
pub const DOWNLOAD_EVENT: &str = "model-download-progress";
/// 模型加载状态事件名；`idle / loading / success / failed` 四态均会推送，
/// 下载完成后的自动加载与手动加载共用同一通道，前端据此显示状态与重试入口。
pub const LOAD_STATUS_EVENT: &str = "model-load-status";
/// 推理引擎标识，界面上原样展示。
const ENGINE: &str = "sherpa-onnx";
/// 扫描目录的最大深度，避免异常目录结构拖慢界面。
const MAX_SCAN_DEPTH: usize = 4;
/// 进度事件的最小间隔，避免大文件下载时刷爆事件通道。
const PROGRESS_INTERVAL: Duration = Duration::from_millis(240);
/// sherpa-onnx 特征提取固定为 16 kHz（`OfflineRecognizerConfig` 的 feat 默认值）。
/// ffmpeg 归一化已按此输出；识别层再做一次兜底重采样，让「ffmpeg 缺失时直用源 WAV」
/// 与「设置 → 模型 → 选择 WAV 识别」这两条入口也统一到同一采样率，避免按源采样率
/// 接收 PCM 造成的特征错位与识别为空。
pub const ASR_SAMPLE_RATE: i32 = 16_000;
/// 重采样插值核半宽（源样本数）；越大阻带衰减越好、CPU 开销越高。
const RESAMPLE_TAPS: usize = 8;
/// 字幕分片目标时长（秒）；实际切点会吸附到附近能量最低处，避免切在词中间。
pub const SEGMENT_TARGET_SECS: f64 = 10.0;
/// 切点搜索半径（秒）：在目标点前后各找一段能量最低的位置。
const SEGMENT_SEARCH_SECS: f64 = 2.5;
/// 单个分片的最短时长（秒），避免切出碎片。
const SEGMENT_MIN_SECS: f64 = 3.0;
/// 计算能量包络的帧长（秒）。
const ENERGY_FRAME_SECS: f64 = 0.02;

/// Qwen3 ASR 必需的四个构件；顺序即界面上的提示顺序。
const COMPONENTS: [&str; 4] = [
    "conv_frontend.onnx",
    "encoder.int8.onnx",
    "decoder.int8.onnx",
    "tokenizer",
];

/// 下载清单：远端路径（ModelScope 仓库内）→ 本地相对路径。
///
/// 远端路径写死为 0.6B-int8 目录与仓库根部的 `tokenizer/`，因此即使使用者把
/// 下载地址换成同一仓库的其他目录，也只会取这四个构件，不会误下 fp32 或 1.7B。
struct RemoteFile {
    remote: &'static str,
    local: &'static str,
    bytes: u64,
}

const QWEN3_FILES: [RemoteFile; 9] = [
    RemoteFile {
        remote: "model_0.6B/conv_frontend.onnx",
        local: "conv_frontend.onnx",
        bytes: 44_148_281,
    },
    RemoteFile {
        remote: "model_0.6B/encoder.int8.onnx",
        local: "encoder.int8.onnx",
        bytes: 182_491_662,
    },
    RemoteFile {
        remote: "model_0.6B/decoder.int8.onnx",
        local: "decoder.int8.onnx",
        bytes: 755_914_231,
    },
    RemoteFile {
        remote: "tokenizer/merges.txt",
        local: "tokenizer/merges.txt",
        bytes: 1_671_853,
    },
    RemoteFile {
        remote: "tokenizer/vocab.json",
        local: "tokenizer/vocab.json",
        bytes: 2_776_833,
    },
    RemoteFile {
        remote: "tokenizer/tokenizer_config.json",
        local: "tokenizer/tokenizer_config.json",
        bytes: 12_487,
    },
    RemoteFile {
        remote: "tokenizer/config.json",
        local: "tokenizer/config.json",
        bytes: 6_193,
    },
    RemoteFile {
        remote: "tokenizer/chat_template.json",
        local: "tokenizer/chat_template.json",
        bytes: 1_161,
    },
    RemoteFile {
        remote: "tokenizer/preprocessor_config.json",
        local: "tokenizer/preprocessor_config.json",
        bytes: 330,
    },
];

/// 扫描到的一个 ASR 模型；目录内的 `.onnx` 统一归到同一条记录。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrModel {
    /// 相对模型目录的稳定标识，用作前端列表 key。
    pub id: String,
    pub name: String,
    /// 模型所在目录（根目录下的单文件模型为模型目录本身）。
    pub path: String,
    /// 目录内的 `.onnx` 文件名，按名称排序。
    pub files: Vec<String>,
    /// 模型目录总大小（含 tokenizer 等附属文件）。
    pub size_bytes: u64,
    pub is_default: bool,
    /// Qwen3 四件套是否齐全；不齐全时前端不提供「加载」。
    pub loadable: bool,
    pub missing: Vec<String>,
}

/// 默认模型（0.6B-int8）的安装状态；缺失时界面显示「下载」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultModel {
    pub name: String,
    pub path: String,
    pub installed: bool,
    pub missing: Vec<String>,
    pub size_bytes: u64,
    /// 完整下载后的体积，用于下载前展示。
    pub expected_bytes: u64,
}

/// 已加载到内存的 ASR 模型。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedModel {
    pub name: String,
    pub path: String,
    pub engine: &'static str,
    pub threads: u32,
}

/// 模型加载阶段；自动加载与手动加载共用。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LoadPhase {
    #[default]
    Idle,
    Loading,
    Success,
    Failed,
}

/// 最近一次（或正在进行的）模型加载状态快照。
///
/// 同时通过 `ModelsInfo` 拉取与 `model-load-status` 事件推送：前者保证刷新界面后
/// 仍能恢复状态，后者保证下载完成后的自动加载能实时反馈「加载中」。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelLoadStatus {
    pub phase: LoadPhase,
    /// 本次加载 / 尝试加载的模型目录。
    pub path: Option<String>,
    /// `failed` 时的具体原因，直接来自 sherpa-onnx 或路径校验的错误链。
    pub error: Option<String>,
}

/// 「模型」分组的完整快照，前端一次读取即可渲染。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsInfo {
    pub root: String,
    /// 模型目录是否为默认位置（系统「下载」目录下的 `bgm-models/`）。
    pub is_default_root: bool,
    pub default_download_url: String,
    /// 当前生效的下载地址；自定义地址为空时等于默认地址。
    pub download_url: String,
    pub default_model: DefaultModel,
    pub models: Vec<AsrModel>,
    pub loaded: Option<LoadedModel>,
    /// 最近一次模型加载状态（含失败原因）；桌面歌词开关据此判断是否可用。
    pub load_status: ModelLoadStatus,
    pub downloading: bool,
    pub warnings: Vec<String>,
}

/// 下载进度事件负载；`done` 表示本次任务结束（成功或失败）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub file: String,
    pub index: usize,
    pub total_files: usize,
    pub received: u64,
    pub file_bytes: u64,
    pub overall_received: u64,
    pub overall_bytes: u64,
    pub done: bool,
    pub error: Option<String>,
}

/// 默认模型目录：系统「下载」目录下的 `bgm-models`。
///
/// Windows 取 `FOLDERID_Downloads`（用户重定向过的下载目录同样有效），
/// Linux 取 `XDG_DOWNLOAD_DIR`；两者通常都不在程序所在盘，避免大体积模型
/// 占满软件安装分区。系统未定义下载目录时退到 `<用户主目录>/Downloads`。
pub fn default_root() -> PathBuf {
    let base = dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(MODELS_DIR_NAME)
}

/// 当前生效的模型目录：用户自定义优先，否则回退默认位置。
pub fn root_for(prefs: &Prefs) -> PathBuf {
    prefs
        .models_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(default_root)
}

/// 目录不存在时创建；返回的错误由调用方决定是否作为告警展示。
pub fn ensure_root(root: &Path) -> Result<()> {
    fs::create_dir_all(root).with_context(|| format!("无法创建模型目录：{}", root.display()))
}

/// 扫描模型目录下所有 `.onnx`，按所在目录聚合成 ASR 模型列表。
///
/// 目录内的多个 `.onnx`（如 Qwen3 的 conv_frontend / encoder / decoder）属于同一个
/// 模型；直接放在模型根目录下的 `.onnx` 视为单文件模型，各自成条。
pub fn scan(root: &Path) -> (Vec<AsrModel>, Vec<String>) {
    let mut warnings = Vec::new();
    if !root.is_dir() {
        return (
            Vec::new(),
            vec![format!("模型目录不存在：{}", root.display())],
        );
    }

    let mut onnx_files = Vec::new();
    collect_onnx(root, 0, &mut onnx_files, &mut warnings);

    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for path in onnx_files {
        let parent = path.parent().unwrap_or(root).to_path_buf();
        groups.entry(parent).or_default().push(path);
    }

    let mut models = Vec::with_capacity(groups.len());
    for (dir, mut files) in groups {
        files.sort();
        let relative = dir.strip_prefix(root).unwrap_or(Path::new(""));
        let is_root = relative.as_os_str().is_empty();
        let file_names: Vec<String> = files
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .collect();

        if is_root {
            // 根目录下的单文件模型：文件名即模型名，不满足 Qwen3 目录结构。
            for (path, file_name) in files.iter().zip(&file_names) {
                let name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| file_name.clone());
                models.push(AsrModel {
                    id: format!("file:{name}"),
                    name,
                    path: root.to_string_lossy().into_owned(),
                    files: vec![file_name.clone()],
                    size_bytes: fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                    is_default: false,
                    loadable: false,
                    // 单文件模型本来就不满足 Qwen3 目录结构，逐项列出缺失构件只会误导。
                    missing: Vec::new(),
                });
            }
            continue;
        }

        let name = dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| relative.to_string_lossy().replace('\\', "/"));
        let missing = missing_components(&dir);
        models.push(AsrModel {
            id: relative.to_string_lossy().replace('\\', "/"),
            name,
            path: dir.to_string_lossy().into_owned(),
            files: file_names,
            size_bytes: dir_size(&dir),
            is_default: relative == Path::new(DEFAULT_MODEL_NAME),
            loadable: missing.is_empty(),
            missing,
        });
    }

    models.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    (models, warnings)
}

/// 按扫描结果解析并校验删除目标；与真正删除分开，便于运行时先卸载再移除文件。
///
/// 目录模型返回目录本身，根目录下的单文件模型返回该文件；两者都做 canonical 校验，
/// 防止模型目录或目标文件被替换为指向外部的符号链接。真正的删除由
/// [`delete_model_files`] 完成。
fn resolve_scanned_model(root: &Path, id: &str) -> Result<(AsrModel, PathBuf)> {
    let (models, _) = scan(root);
    let model = models
        .into_iter()
        .find(|model| model.id == id)
        .ok_or_else(|| anyhow!("模型不存在或已被删除：{id}"))?;
    let canonical_root =
        fs::canonicalize(root).with_context(|| format!("模型目录不存在：{}", root.display()))?;
    let target = if model.id.starts_with("file:") {
        root.join(&model.files[0])
    } else {
        PathBuf::from(&model.path)
    };
    let canonical_target =
        fs::canonicalize(&target).with_context(|| format!("模型不存在：{}", target.display()))?;
    if canonical_target == canonical_root || !canonical_target.starts_with(&canonical_root) {
        bail!("模型必须位于当前模型目录下：{}", root.display());
    }
    Ok((model, target))
}

fn delete_model_files(model: &AsrModel, target: &Path) -> Result<()> {
    if model.id.starts_with("file:") {
        fs::remove_file(target)
            .with_context(|| format!("删除模型文件失败：{}", target.display()))?;
    } else {
        fs::remove_dir_all(target)
            .with_context(|| format!("删除模型目录失败：{}", target.display()))?;
    }
    Ok(())
}

/// 删除前记录模型文件清单；失败时据此判断能否安全恢复已卸载的模型句柄。
fn snapshot_model_files(model: &AsrModel, target: &Path) -> Result<BTreeMap<PathBuf, u64>> {
    let mut files = BTreeMap::new();
    if model.id.starts_with("file:") {
        let metadata = fs::metadata(target)
            .with_context(|| format!("无法读取模型文件：{}", target.display()))?;
        if !metadata.is_file() {
            bail!("模型目标不是文件：{}", target.display());
        }
        files.insert(target.to_path_buf(), metadata.len());
    } else {
        collect_file_sizes(target, &mut files)?;
    }
    Ok(files)
}

fn collect_file_sizes(dir: &Path, files: &mut BTreeMap<PathBuf, u64>) -> Result<()> {
    let entries =
        fs::read_dir(dir).with_context(|| format!("无法读取模型目录：{}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("无法读取模型目录项：{}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("无法读取模型文件类型：{}", path.display()))?;
        if file_type.is_dir() {
            collect_file_sizes(&path, files)?;
        } else {
            let metadata = entry
                .metadata()
                .with_context(|| format!("无法读取模型文件：{}", path.display()))?;
            files.insert(path, metadata.len());
        }
    }
    Ok(())
}

/// 默认模型的安装状态；目录存在但构件不全时同样视为未安装，下载会续传缺失文件。
pub fn default_model(root: &Path) -> DefaultModel {
    let dir = root.join(DEFAULT_MODEL_NAME);
    let exists = dir.is_dir();
    let missing = if exists {
        missing_components(&dir)
    } else {
        COMPONENTS.iter().map(|name| name.to_string()).collect()
    };
    DefaultModel {
        name: DEFAULT_MODEL_NAME.to_owned(),
        path: dir.to_string_lossy().into_owned(),
        installed: exists && missing.is_empty(),
        missing,
        size_bytes: if exists { dir_size(&dir) } else { 0 },
        expected_bytes: QWEN3_FILES.iter().map(|file| file.bytes).sum(),
    }
}

/// 已加载模型与下载任务的运行期状态；克隆只复制内部共享句柄。
#[derive(Clone, Default)]
pub struct ModelRuntime {
    loaded: Arc<Mutex<Option<Arc<Loaded>>>>,
    downloading: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    /// 同一时刻只允许一个加载任务，防止下载完成后的自动加载与手动加载重复触发。
    loading: Arc<AtomicBool>,
    /// 删除任务进行中；阻止新的加载 / 下载与正在删除的模型竞争。
    deleting: Arc<AtomicBool>,
    /// 加载、下载与删除共享的状态切换锁，避免检查和设置任务标志之间发生竞态。
    operation: Arc<Mutex<()>>,
    /// 正在使用已加载模型的 ASR / 字幕任务数；删除、释放或重新加载前必须协调。
    inference_tasks: Arc<AtomicUsize>,
    /// 最近一次加载状态，供 `ModelsInfo` 与状态事件读取。
    status: Arc<Mutex<ModelLoadStatus>>,
}

struct Loaded {
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
struct LoadFlag<'a>(&'a AtomicBool);

impl Drop for LoadFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// `Drop` 时复位删除标志，文件删除失败也不会永久阻止后续加载。
struct DeleteFlag<'a>(&'a AtomicBool);

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

    fn remove_with(
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
fn create_recognizer(dir: &Path, threads: u32) -> Result<OfflineRecognizer> {
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

/// 下载地址的两种形态：ModelScope 仓库入口，或可直接拼接文件名的目录地址。
enum DownloadSource {
    ModelScope {
        owner: String,
        repo: String,
        revision: String,
    },
    Base(String),
}

impl DownloadSource {
    fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim().trim_end_matches('/');
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            bail!("下载地址需以 http:// 或 https:// 开头");
        }

        // 参考地址形如 https://modelscope.cn/models/{owner}/{repo}/tree/{revision}/{path}
        // 只取仓库信息；文件清单固定为 0.6B-int8，不使用地址里的路径。
        let lower = trimmed.to_ascii_lowercase();
        if let Some(position) = lower.find("modelscope.cn/models/") {
            let tail = &trimmed[position + "modelscope.cn/models/".len()..];
            let mut parts = tail.split('/');
            let owner = parts.next().unwrap_or_default().trim().to_owned();
            let repo = parts
                .next()
                .unwrap_or_default()
                .split(['?', '#'])
                .next()
                .unwrap_or_default()
                .trim()
                .to_owned();
            if !owner.is_empty() && !repo.is_empty() {
                let rest: Vec<&str> = parts.collect();
                let revision = match rest.as_slice() {
                    ["tree", revision, ..] if !revision.is_empty() => (*revision).to_owned(),
                    _ => "master".to_owned(),
                };
                return Ok(Self::ModelScope {
                    owner,
                    repo,
                    revision,
                });
            }
        }

        Ok(Self::Base(trimmed.to_owned()))
    }

    fn file_url(&self, file: &RemoteFile) -> String {
        match self {
            Self::ModelScope {
                owner,
                repo,
                revision,
            } => format!(
                "https://modelscope.cn/api/v1/models/{owner}/{repo}/repo?Revision={revision}&FilePath={}",
                file.remote
            ),
            Self::Base(base) => format!("{base}/{}", file.local),
        }
    }
}

/// 逐文件下载到 `models/<默认模型名>/`；已完整存在的文件直接跳过，便于失败后续传。
///
/// 进度与完整性以响应的 `Content-Length` 为准（缺失时回退清单字节数），
/// 避免上游文件更新后因大小不一致而永久失败；取消或读写失败时清理 `.part`。
fn download_files(app: &AppHandle, root: &Path, url: &str, cancelled: &AtomicBool) -> Result<()> {
    let url = if url.trim().is_empty() {
        DEFAULT_DOWNLOAD_URL
    } else {
        url.trim()
    };
    let source = DownloadSource::parse(url)?;
    ensure_root(root)?;

    let target = root.join(DEFAULT_MODEL_NAME);
    fs::create_dir_all(&target)
        .with_context(|| format!("无法创建模型目录：{}", target.display()))?;

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60 * 60))
        .user_agent(concat!("tfy-bgm/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("初始化下载客户端失败")?;

    let expected = total_bytes();
    let mut overall = 0u64;
    for file in &QWEN3_FILES {
        if is_complete(&target.join(file.local), file) {
            overall += file.bytes;
        }
    }

    for (index, file) in QWEN3_FILES.iter().enumerate() {
        if cancelled.load(Ordering::SeqCst) {
            bail!("下载已取消");
        }
        let path = target.join(file.local);
        if is_complete(&path, file) {
            emit_progress(
                app,
                DownloadProgress {
                    file: file.local.to_owned(),
                    index,
                    total_files: QWEN3_FILES.len(),
                    received: file.bytes,
                    file_bytes: file.bytes,
                    overall_received: overall,
                    overall_bytes: expected,
                    done: true,
                    error: None,
                },
            );
            continue;
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("无法创建目录：{}", parent.display()))?;
        }
        let file_url = source.file_url(file);
        let mut response = client
            .get(&file_url)
            .send()
            .with_context(|| format!("请求下载地址失败：{file_url}"))?;
        if !response.status().is_success() {
            bail!(
                "下载 {} 失败：HTTP {}（{file_url}）",
                file.local,
                response.status()
            );
        }
        if response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        {
            bail!("下载地址返回的是网页而不是模型文件，请填写模型文件目录或可直连的地址");
        }

        let expected_len = response.content_length().unwrap_or(file.bytes);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.local.to_owned());
        let part = path.with_file_name(format!("{file_name}.part"));
        let mut writer =
            File::create(&part).with_context(|| format!("无法写入临时文件：{}", part.display()))?;

        let mut received = 0u64;
        let mut last_emit = Instant::now()
            .checked_sub(PROGRESS_INTERVAL)
            .unwrap_or_else(Instant::now);
        let mut buffer = vec![0u8; 256 * 1024];
        let mut copy_error: Option<anyhow::Error> = None;
        loop {
            if cancelled.load(Ordering::SeqCst) {
                copy_error = Some(anyhow!("下载已取消"));
                break;
            }
            let read = match response.read(&mut buffer) {
                Ok(read) => read,
                Err(error) => {
                    copy_error = Some(anyhow!("下载 {} 失败：{error}", file.local));
                    break;
                }
            };
            if read == 0 {
                break;
            }
            if let Err(error) = writer.write_all(&buffer[..read]) {
                copy_error = Some(anyhow!("写入 {} 失败：{error}", part.display()));
                break;
            }
            received += read as u64;
            if last_emit.elapsed() >= PROGRESS_INTERVAL {
                emit_progress(
                    app,
                    DownloadProgress {
                        file: file.local.to_owned(),
                        index,
                        total_files: QWEN3_FILES.len(),
                        received,
                        file_bytes: expected_len,
                        overall_received: overall + received,
                        overall_bytes: expected,
                        done: false,
                        error: None,
                    },
                );
                last_emit = Instant::now();
            }
        }
        writer.flush().ok();
        drop(writer);

        if let Some(error) = copy_error {
            let _ = fs::remove_file(&part);
            return Err(error);
        }
        if received == 0 {
            let _ = fs::remove_file(&part);
            bail!("下载 {} 失败：服务器返回空文件", file.local);
        }
        if expected_len > 0 && received < expected_len {
            let _ = fs::remove_file(&part);
            bail!(
                "下载 {} 不完整：已收到 {} 字节，期望 {} 字节",
                file.local,
                received,
                expected_len
            );
        }
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        fs::rename(&part, &path)
            .with_context(|| format!("无法保存模型文件：{}", path.display()))?;

        overall += received;
        emit_progress(
            app,
            DownloadProgress {
                file: file.local.to_owned(),
                index,
                total_files: QWEN3_FILES.len(),
                received,
                file_bytes: expected_len,
                overall_received: overall,
                overall_bytes: expected,
                done: true,
                error: None,
            },
        );
    }

    let missing = missing_components(&target);
    if !missing.is_empty() {
        bail!("下载完成后仍缺少构件：{}", missing.join("、"));
    }
    Ok(())
}

fn emit_progress(app: &AppHandle, progress: DownloadProgress) {
    let _ = app.emit(DOWNLOAD_EVENT, progress);
}

/// 去掉 Qwen3-ASR 输出里可能夹带的 `<|...|>` 控制标记，并压平空白。
///
/// 官方 `generation_config.json` 把 `<|endoftext|>`(151643) 与 `<|im_end|>`(151645)
/// 都声明为终止符，但 sherpa-onnx 的 `QwenAsrTokenizer` 只把 `<|im_end|>` 当 EOS，
/// `<|endoftext|>` 仅作为 pad token。模型一旦先吐出 `<|endoftext|>`，解码不会停止，
/// 而会在没有音频约束的情况下自由续写，把预训练语料里的原文带进字幕（典型表现是
/// 歌词后面接一段与歌曲无关的百科式介绍）。因此正文之后第一次出现控制标记即视为
/// 本次识别的终止，丢弃其后的全部内容；开头连续的控制标记仍按脚手架跳过。
fn clean_asr_text(raw: &str) -> String {
    let mut rest = raw;
    loop {
        let trimmed = rest.trim_start();
        if !trimmed.starts_with("<|") {
            rest = trimmed;
            break;
        }
        let Some(end) = trimmed.find("|>") else {
            rest = "";
            break;
        };
        rest = &trimmed[end + 2..];
    }
    let text = match rest.find("<|") {
        Some(position) => &rest[..position],
        None => rest,
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉 Qwen3-ASR 输出里偶发夹带的训练指令句（元指令幻觉）。
///
/// 模型在没有明确歌词的窗口（歌曲尾部、纯乐器段）上偶尔会把训练期的系统指令
/// 当作识别结果输出，例如「特别注意，不要把“system”翻译成“系统”。」。
/// 这类句子含「翻译 / 转写」且带引号或「不要 / 请」，不属于歌词；按句丢弃，
/// 同一段里正常识别出的歌词句子保留。
fn strip_meta_instructions(text: &str) -> String {
    text.split_inclusive(|c: char| matches!(c, '。' | '！' | '？' | '；' | '!' | '?' | ';' | '.'))
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty() && !is_meta_instruction(sentence))
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_meta_instruction(sentence: &str) -> bool {
    let mentions_translation = sentence.contains("翻译") || sentence.contains("转写");
    let quoted = sentence.contains('“')
        || sentence.contains('”')
        || sentence.contains('"')
        || sentence.contains('「');
    mentions_translation && (quoted || sentence.contains("不要") || sentence.contains("请"))
}

/// 规划字幕分片区间：按目标时长切分，并把每个切点吸附到附近能量最低的帧。
///
/// 返回首尾相接、覆盖整段音频的 `[start, end)` 秒区间；音频很短时只返回一段。
fn plan_segments(samples: &[f32], rate: usize, total_secs: f64, window: f64) -> Vec<(f64, f64)> {
    if rate == 0 || samples.is_empty() || total_secs <= 0.0 {
        return Vec::new();
    }
    let window = if window.is_finite() && window > 0.0 {
        window
    } else {
        SEGMENT_TARGET_SECS
    };
    let min_len = SEGMENT_MIN_SECS.min(window / 2.0).max(ENERGY_FRAME_SECS);
    let energies = frame_energies(samples, rate);
    let mut ranges = Vec::new();
    let mut start = 0.0;
    // 剩余时长不足 1.5 个窗口时直接作为最后一段，避免切出过短的尾巴。
    while total_secs - start > window * 1.5 {
        let target = start + window;
        let lo = (start + min_len).max(target - SEGMENT_SEARCH_SECS);
        let hi = (total_secs - min_len).min(target + SEGMENT_SEARCH_SECS);
        let end = quietest_point(&energies, lo, hi, target).unwrap_or(target);
        if end > start + min_len {
            ranges.push((start, end));
            start = end;
            continue;
        }
        // 搜索区间不可用（接近结尾 / 全部越界）时退回固定切点。
        let fallback = (start + window).min(total_secs);
        if fallback <= start + min_len {
            break;
        }
        ranges.push((start, fallback));
        start = fallback;
    }
    if total_secs > start {
        ranges.push((start, total_secs));
    }
    ranges
}

/// 每 [`ENERGY_FRAME_SECS`] 一帧的 RMS 能量包络。
fn frame_energies(samples: &[f32], rate: usize) -> Vec<f64> {
    let frame_len = ((rate as f64 * ENERGY_FRAME_SECS).round() as usize).max(1);
    samples
        .chunks(frame_len)
        .map(|frame| {
            let sum: f64 = frame
                .iter()
                .map(|value| (*value as f64) * (*value as f64))
                .sum();
            (sum / frame.len() as f64).sqrt()
        })
        .collect()
}

/// 在 `[lo, hi]` 秒内找能量最低的帧中心；能量相同时取离 `target` 最近的一个。
fn quietest_point(energies: &[f64], lo: f64, hi: f64, target: f64) -> Option<f64> {
    if energies.is_empty() || !(hi > lo) {
        return None;
    }
    let first = (lo / ENERGY_FRAME_SECS).ceil().max(0.0) as usize;
    let last = ((hi / ENERGY_FRAME_SECS).floor() as usize).min(energies.len() - 1);
    if first > last {
        return None;
    }
    let mut best: Option<(f64, f64, f64)> = None;
    for index in first..=last {
        let time = (index as f64 + 0.5) * ENERGY_FRAME_SECS;
        let distance = (time - target).abs();
        let energy = energies[index];
        let better = match best {
            None => true,
            Some((best_energy, best_distance, _)) => {
                energy < best_energy - 1e-9
                    || ((energy - best_energy).abs() <= 1e-9 && distance < best_distance)
            }
        };
        if better {
            best = Some((energy, distance, time));
        }
    }
    best.map(|(_, _, time)| time)
}

/// 归一化 sinc：`sin(πx)/(πx)`，`x→0` 时取 1。
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

/// 把 sherpa-onnx 读出的单声道 PCM 重采样到 [`ASR_SAMPLE_RATE`]。
///
/// 采用带限 Lanczos 插值，核为 `sinc(cutoff·x) · sinc(x/A)`，其中
/// `cutoff = min(1, 16000/src_rate)`：
/// - 下采样时把截止频率收窄到目标 Nyquist（8 kHz），抑制会混叠进 fbank 的高频；
/// - 上采样时 `cutoff = 1`，仅做平滑插值，保留原带内容。
///
/// 逐输出点按核权重归一化，保证直流增益为 1，信号首尾不会因核被截断而衰减。
/// 已是 16 kHz 时直接返回副本（恒等路径），不改变音质也不增加耗时。
fn resample_to_asr(samples: &[f32], src_rate: i32) -> Result<Vec<f32>> {
    if src_rate <= 0 {
        bail!("音频采样率无效：{src_rate}");
    }
    if samples.is_empty() || src_rate == ASR_SAMPLE_RATE {
        return Ok(samples.to_vec());
    }

    let ratio = ASR_SAMPLE_RATE as f64 / src_rate as f64;
    let cutoff = ratio.min(1.0);
    let a = RESAMPLE_TAPS as f64;
    let out_len = ((samples.len() as f64 * ratio).floor() as usize).max(1);
    let mut out = Vec::with_capacity(out_len);

    for n in 0..out_len {
        // n 为输出域样本序号，pos 为对应的源域浮点位置。
        let pos = n as f64 / ratio;
        let center = pos.floor();
        let mut acc = 0.0;
        let mut norm = 0.0;
        let from = (center - a).floor() as isize;
        let to = (center + a).ceil() as isize;
        for i in from..=to {
            if i < 0 || i as usize >= samples.len() {
                continue;
            }
            let x = pos - i as f64;
            let weight = sinc(x * cutoff) * sinc(x / a);
            acc += weight * samples[i as usize] as f64;
            norm += weight;
        }
        out.push(if norm.abs() > 1e-12 {
            (acc / norm) as f32
        } else {
            0.0
        });
    }
    Ok(out)
}

fn total_bytes() -> u64 {
    QWEN3_FILES.iter().map(|file| file.bytes).sum()
}

fn is_complete(path: &Path, file: &RemoteFile) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.len() == file.bytes)
        .unwrap_or(false)
}

/// 目录是否为构件齐全、可直接加载的 Qwen3 模型。
pub fn is_loadable(dir: &Path) -> bool {
    dir.is_dir() && missing_components(dir).is_empty()
}

/// Qwen3 ASR 四件套中缺失的项；`tokenizer` 单独按目录判断。
fn missing_components(dir: &Path) -> Vec<String> {
    let mut missing = Vec::new();
    for name in [
        "conv_frontend.onnx",
        "encoder.int8.onnx",
        "decoder.int8.onnx",
    ] {
        if !dir.join(name).is_file() {
            missing.push(name.to_owned());
        }
    }
    if !dir.join("tokenizer").is_dir() {
        missing.push("tokenizer/".to_owned());
    }
    missing
}

fn collect_onnx(dir: &Path, depth: usize, out: &mut Vec<PathBuf>, warnings: &mut Vec<String>) {
    if depth > MAX_SCAN_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!("无法读取目录 {}：{error}", dir.display()));
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // 不跟随符号链接，避免自引用目录造成无限扫描。
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            let hidden = name.to_string_lossy().starts_with('.');
            let other_category = depth == 0
                && (name == crate::separation::CUSTOM_DIR_NAME || name == crate::tts_models::DIR);
            if !hidden && !other_category {
                collect_onnx(&path, depth + 1, out, warnings);
            }
        } else if is_onnx(&path) {
            out.push(path);
        }
    }
}

fn is_onnx(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("onnx"))
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return total;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            total += dir_size(&path);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
}

/// 推理线程数：取可用核心数并限制上限，避免占满整机。
fn thread_count() -> u32 {
    std::thread::available_parallelism()
        .map(|count| count.get().min(4) as u32)
        .unwrap_or(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_root_is_under_system_download_dir() {
        assert_eq!(MODELS_DIR_NAME, "bgm-models");
        let root = default_root();
        assert!(root.ends_with(MODELS_DIR_NAME));
        // 默认目录必须挂在系统「下载」目录下，而不是程序安装盘（exe 同级）下。
        let base = dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|home| home.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."));
        assert_eq!(root, base.join(MODELS_DIR_NAME));
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        assert_ne!(root.parent(), exe_dir.as_deref());
    }

    #[test]
    fn custom_root_wins_over_default() {
        let prefs = Prefs {
            models_dir: Some("D:/models".into()),
            ..Prefs::default()
        };
        assert_eq!(root_for(&prefs), PathBuf::from("D:/models"));
    }

    #[test]
    fn scan_groups_onnx_by_directory() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join(DEFAULT_MODEL_NAME);
        fs::create_dir_all(model.join("tokenizer")).unwrap();
        fs::write(model.join("conv_frontend.onnx"), b"x").unwrap();
        fs::write(model.join("encoder.int8.onnx"), b"xx").unwrap();
        fs::write(model.join("decoder.int8.onnx"), b"xxx").unwrap();
        fs::write(model.join("tokenizer/vocab.json"), b"{}").unwrap();
        fs::write(dir.path().join("single.onnx"), b"x").unwrap();

        let (models, warnings) = scan(dir.path());
        assert!(warnings.is_empty());
        assert_eq!(models.len(), 2);

        let default = models
            .iter()
            .find(|model| model.is_default)
            .expect("default model");
        assert_eq!(default.name, DEFAULT_MODEL_NAME);
        assert_eq!(default.files.len(), 3);
        assert!(default.loadable);

        let single = models
            .iter()
            .find(|model| model.id == "file:single")
            .expect("single file model");
        assert!(!single.loadable);
        // 单文件模型不逐项提示缺失构件，界面按「不支持加载」处理。
        assert!(single.missing.is_empty());
    }

    #[test]
    fn remove_removes_directory_and_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("custom-model");
        fs::create_dir_all(model.join("tokenizer")).unwrap();
        fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
        fs::write(model.join("tokenizer/vocab.json"), b"{}").unwrap();
        fs::write(dir.path().join("keep.onnx"), b"keep").unwrap();

        ModelRuntime::default()
            .remove(dir.path(), "custom-model")
            .unwrap();
        assert!(!model.exists());
        assert!(dir.path().join("keep.onnx").exists());
    }

    #[test]
    fn remove_removes_only_root_single_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("single.onnx"), b"x").unwrap();
        fs::write(dir.path().join("keep.txt"), b"keep").unwrap();

        ModelRuntime::default()
            .remove(dir.path(), "file:single")
            .unwrap();
        assert!(!dir.path().join("single.onnx").exists());
        assert!(dir.path().join("keep.txt").exists());
        assert!(dir.path().exists());
    }

    #[test]
    fn remove_rejects_unknown_or_outside_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("single.onnx"), b"x").unwrap();
        let runtime = ModelRuntime::default();
        assert!(runtime.remove(dir.path(), "../outside").is_err());
        assert!(runtime.remove(dir.path(), "file:missing").is_err());
        assert!(dir.path().join("single.onnx").exists());
    }

    #[test]
    fn remove_rejects_loading_or_downloading_models() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("custom-model")).unwrap();
        fs::write(dir.path().join("custom-model/encoder.int8.onnx"), b"x").unwrap();
        let runtime = ModelRuntime::default();

        runtime.loading.store(true, Ordering::SeqCst);
        assert!(runtime.remove(dir.path(), "custom-model").is_err());
        runtime.loading.store(false, Ordering::SeqCst);
        runtime.downloading.store(true, Ordering::SeqCst);
        assert!(runtime.remove(dir.path(), "custom-model").is_err());
        runtime.downloading.store(false, Ordering::SeqCst);
        assert!(dir.path().join("custom-model").exists());
    }

    #[test]
    fn remove_rejects_active_model_tasks_and_recovers_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("custom-model");
        fs::create_dir_all(&model).unwrap();
        fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
        let runtime = ModelRuntime::default();

        let task = runtime.reserve_model_task().unwrap();
        assert!(runtime.remove(dir.path(), "custom-model").is_err());
        assert!(runtime.unload().is_err());
        assert!(runtime.load(dir.path(), &model).is_err());
        assert!(model.exists());

        drop(task);
        runtime.remove(dir.path(), "custom-model").unwrap();
        assert!(!model.exists());
    }

    #[test]
    fn remove_failure_restores_status_when_files_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("custom-model");
        fs::create_dir_all(&model).unwrap();
        fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
        let runtime = ModelRuntime::default();
        let path = model.to_string_lossy().into_owned();
        runtime.set_load_status(ModelLoadStatus {
            phase: LoadPhase::Failed,
            path: Some(path.clone()),
            error: Some("原始加载错误".to_owned()),
        });

        let error = runtime
            .remove_with(dir.path(), "custom-model", |_, _| {
                Err(anyhow!("模拟删除失败"))
            })
            .unwrap_err();

        assert!(error.to_string().contains("模拟删除失败"));
        assert!(model.exists());
        let status = runtime.load_status();
        assert_eq!(status.phase, LoadPhase::Failed);
        assert_eq!(status.path.as_deref(), Some(path.as_str()));
        assert_eq!(status.error.as_deref(), Some("原始加载错误"));
    }

    #[test]
    fn remove_partial_failure_marks_model_as_damaged() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("custom-model");
        fs::create_dir_all(&model).unwrap();
        fs::write(model.join("encoder.int8.onnx"), b"x").unwrap();
        let runtime = ModelRuntime::default();
        runtime.set_load_status(ModelLoadStatus {
            phase: LoadPhase::Success,
            path: Some(model.to_string_lossy().into_owned()),
            error: None,
        });

        let error = runtime
            .remove_with(dir.path(), "custom-model", |_, target| {
                fs::remove_file(target.join("encoder.int8.onnx"))?;
                Err(anyhow!("模拟部分删除失败"))
            })
            .unwrap_err();

        assert!(error.to_string().contains("模拟部分删除失败"));
        assert!(!model.join("encoder.int8.onnx").exists());
        let status = runtime.load_status();
        assert_eq!(status.phase, LoadPhase::Failed);
        assert!(status.error.unwrap().contains("模型文件已部分删除"));
    }

    #[test]
    fn default_model_reports_missing_components() {
        let dir = tempfile::tempdir().unwrap();
        let status = default_model(dir.path());
        assert!(!status.installed);
        assert_eq!(status.missing.len(), COMPONENTS.len());

        let model = dir.path().join(DEFAULT_MODEL_NAME);
        fs::create_dir_all(model.join("tokenizer")).unwrap();
        fs::write(model.join("conv_frontend.onnx"), b"x").unwrap();
        let status = default_model(dir.path());
        assert!(!status.installed);
        assert_eq!(status.missing, ["encoder.int8.onnx", "decoder.int8.onnx"]);
    }

    #[test]
    fn parse_modelscope_url_extracts_repo() {
        let source = DownloadSource::parse(DEFAULT_DOWNLOAD_URL).unwrap();
        match source {
            DownloadSource::ModelScope {
                owner,
                repo,
                revision,
            } => {
                assert_eq!(owner, "zengshuishui");
                assert_eq!(repo, "Qwen3-ASR-onnx");
                assert_eq!(revision, "master");
            }
            DownloadSource::Base(_) => panic!("应识别为 ModelScope 仓库"),
        }
    }

    #[test]
    fn file_urls_stay_on_int8_manifest() {
        let source = DownloadSource::parse("https://example.com/mirror/").unwrap();
        let url = source.file_url(&QWEN3_FILES[0]);
        assert_eq!(url, "https://example.com/mirror/conv_frontend.onnx");

        let source = DownloadSource::parse(DEFAULT_DOWNLOAD_URL).unwrap();
        let url = source.file_url(&QWEN3_FILES[2]);
        assert!(url.contains("FilePath=model_0.6B/decoder.int8.onnx"));
        assert!(QWEN3_FILES.iter().all(|file| !file.local.contains("1.7B")));
    }

    #[test]
    fn load_status_starts_idle_and_keeps_failure_reason() {
        let runtime = ModelRuntime::default();
        assert_eq!(runtime.load_status().phase, LoadPhase::Idle);
        runtime.set_load_status(ModelLoadStatus {
            phase: LoadPhase::Failed,
            path: Some("D:/models/demo".to_owned()),
            error: Some("sherpa-onnx 无法加载模型".to_owned()),
        });
        let status = runtime.load_status();
        assert_eq!(status.phase, LoadPhase::Failed);
        assert_eq!(status.error.as_deref(), Some("sherpa-onnx 无法加载模型"));
        // 失败状态下不得被误判为「已加载」，自动加载仍可重试。
        assert!(!runtime.is_loaded_path(Path::new("D:/models/demo")));
    }

    #[test]
    fn clean_asr_text_strips_control_tokens() {
        assert_eq!(
            clean_asr_text("<|im_start|>你好  世界\n第二行<|im_end|>"),
            "你好 世界 第二行"
        );
        assert_eq!(clean_asr_text("  纯文本  "), "纯文本");
        assert_eq!(clean_asr_text("<|im_start|><|endoftext|>"), "");
        assert_eq!(clean_asr_text("<|im_start|><|endoftext|>你好"), "你好");
    }

    #[test]
    fn clean_asr_text_drops_free_generation_after_end_token() {
        // sherpa-onnx 只把 <|im_end|> 当 EOS；Qwen3-ASR 先吐 <|endoftext|> 时
        // 解码会继续自由续写，其后的正文不属于识别结果，必须整段丢弃。
        assert_eq!(
            clean_asr_text(
                "浪漫情歌，春去，小梦不觉。<|endoftext|>Human Rights Watch（人权观察）是全球最大的人权组织。"
            ),
            "浪漫情歌，春去，小梦不觉。"
        );
        assert_eq!(
            clean_asr_text("离别。<|endoftext|>Human Rights Watch is a non-profit organization."),
            "离别。"
        );
    }

    #[test]
    fn strip_meta_instructions_drops_prompt_hallucinations() {
        // 实测：歌曲尾部窗口上 Qwen3-ASR 吐出训练指令而非歌词。
        assert_eq!(
            strip_meta_instructions("特别注意，不要把“system”翻译成“系统”。"),
            ""
        );
        assert_eq!(
            strip_meta_instructions("不要把\"system\"翻译成\"系统\""),
            ""
        );
        assert_eq!(strip_meta_instructions("请把这段音频转写出来。"), "");
        // 同段里真实歌词保留，只丢元指令句。
        assert_eq!(
            strip_meta_instructions("回头看，不曾走远。特别注意，不要把“system”翻译成“系统”。"),
            "回头看，不曾走远。"
        );
        // 歌词里的「翻译 / 请」不带指令特征时不能误伤。
        assert_eq!(
            strip_meta_instructions("把思念翻译成一首歌。"),
            "把思念翻译成一首歌。"
        );
        assert_eq!(strip_meta_instructions("请不要离开。"), "请不要离开。");
    }

    #[test]
    fn resample_16k_is_identity() {
        let pcm: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        assert_eq!(resample_to_asr(&pcm, ASR_SAMPLE_RATE).unwrap(), pcm);
    }

    #[test]
    fn resample_lengths_follow_ratio() {
        let pcm = vec![0.0f32; 48_000];
        assert_eq!(resample_to_asr(&pcm, 48_000).unwrap().len(), 16_000);
        assert_eq!(resample_to_asr(&pcm, 8_000).unwrap().len(), 96_000);
        // 44.1 kHz 的 1 秒音频重采样后时长仍为 1 秒，时间轴不会漂移。
        let one_second = vec![0.0f32; 44_100];
        let out = resample_to_asr(&one_second, 44_100).unwrap();
        assert!((out.len() as f64 / ASR_SAMPLE_RATE as f64 - 1.0).abs() < 1e-3);
        assert!(resample_to_asr(&[], 44_100).unwrap().is_empty());
    }

    #[test]
    fn resample_preserves_dc_gain() {
        for rate in [8_000, 44_100, 48_000] {
            let pcm = vec![0.25f32; rate as usize];
            let out = resample_to_asr(&pcm, rate).unwrap();
            let max_err = out
                .iter()
                .map(|value| (value - 0.25).abs())
                .fold(0.0f32, f32::max);
            assert!(max_err < 1e-4, "rate={rate} 直流增益偏差过大：{max_err}");
        }
    }

    #[test]
    fn resample_suppresses_out_of_band_on_downsample() {
        fn rms(signal: &[f32]) -> f64 {
            let sum: f64 = signal.iter().map(|v| (*v as f64).powi(2)).sum();
            (sum / signal.len().max(1) as f64).sqrt()
        }
        let rate = 48_000usize;
        let tone = |freq: f64| -> Vec<f32> {
            (0..rate)
                .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32)
                .collect()
        };
        // 1 kHz 在目标 16 kHz 的带内，应基本无损；12 kHz 高于目标 Nyquist(8 kHz)，
        // 若不做带限会混叠成 4 kHz 的假信号，必须被明显衰减。
        let in_band = rms(&resample_to_asr(&tone(1_000.0), rate as i32).unwrap());
        let out_band = rms(&resample_to_asr(&tone(12_000.0), rate as i32).unwrap());
        assert!(in_band > 0.5, "带内 1 kHz 应基本无损，实测 {in_band}");
        assert!(
            out_band < in_band * 0.25,
            "带外 12 kHz 未被抑制：{out_band} vs {in_band}"
        );
    }

    #[test]
    fn resample_rejects_invalid_rate() {
        assert!(resample_to_asr(&[0.1, 0.2], 0).is_err());
        assert!(resample_to_asr(&[0.1, 0.2], -1).is_err());
    }

    /// 走一遍真实的 WAV 读写链路：44.1 kHz WAV → `Wave::read` → 兜底重采样。
    #[test]
    fn resample_real_wav_44100_to_16k() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.wav");
        let pcm: Vec<f32> = (0..44_100)
            .map(|i| (2.0 * std::f64::consts::PI * 440.0 * i as f64 / 44_100.0).sin() as f32)
            .collect();
        assert!(sherpa_onnx::write(path.to_str().unwrap(), &pcm, 44_100));

        let wave = Wave::read(path.to_str().unwrap()).expect("应能读回 44.1 kHz 单声道 WAV");
        assert_eq!(wave.sample_rate(), 44_100);
        let out = resample_to_asr(wave.samples(), wave.sample_rate()).unwrap();
        assert_eq!(out.len(), 16_000, "重采样后应为 1 秒 16 kHz 音频");

        // 440 Hz 处于目标带内，重采样后能量应基本保留（正弦 RMS ≈ 1/√2）。
        let rms = (out.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / out.len() as f64).sqrt();
        assert!(
            (rms - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.05,
            "带内 440 Hz 能量损失过大：rms={rms}"
        );
    }

    fn tone_with_gaps(seconds: f64, gaps: &[(f64, f64)]) -> Vec<f32> {
        let rate = ASR_SAMPLE_RATE as usize;
        (0..(rate as f64 * seconds) as usize)
            .map(|index| {
                let time = index as f64 / rate as f64;
                if gaps.iter().any(|(from, to)| (*from..*to).contains(&time)) {
                    0.0
                } else {
                    (2.0 * std::f64::consts::PI * 440.0 * time).sin() as f32 * 0.2
                }
            })
            .collect()
    }

    #[test]
    fn plan_segments_snaps_to_quiet_point() {
        let rate = ASR_SAMPLE_RATE as usize;
        let total = 20.0;
        let pcm = tone_with_gaps(total, &[(9.0, 9.8)]);
        let ranges = plan_segments(&pcm, rate, total, SEGMENT_TARGET_SECS);
        assert_eq!(ranges.len(), 2, "20 秒音频应切成两段：{ranges:?}");
        assert!((ranges[0].0).abs() < 1e-9);
        assert!(
            (9.0..=9.8).contains(&ranges[0].1),
            "切点应落在静音段内，实测 {}",
            ranges[0].1
        );
        assert!((ranges[1].0 - ranges[0].1).abs() < 1e-9);
        assert!((ranges[1].1 - total).abs() < 1e-9);
    }

    #[test]
    fn plan_segments_cover_audio_without_gaps() {
        let rate = ASR_SAMPLE_RATE as usize;
        let total = 31.0;
        let pcm = tone_with_gaps(total, &[]);
        let ranges = plan_segments(&pcm, rate, total, SEGMENT_TARGET_SECS);
        assert!(ranges.len() >= 3, "31 秒音频至少切成三段：{ranges:?}");
        assert!((ranges[0].0).abs() < 1e-9);
        for pair in ranges.windows(2) {
            assert!(
                (pair[0].1 - pair[1].0).abs() < 1e-9,
                "分片必须首尾相接：{ranges:?}"
            );
        }
        let last = ranges.last().unwrap();
        assert!((last.1 - total).abs() < 1e-9, "最后一片应覆盖到音频结尾");
        for (start, end) in &ranges {
            assert!(
                *end - *start >= SEGMENT_MIN_SECS - 1e-9,
                "分片过短：{start}..{end}"
            );
            assert!(
                *end - *start <= SEGMENT_TARGET_SECS * 1.5 + 1e-9,
                "分片过长：{start}..{end}"
            );
        }
    }

    #[test]
    fn plan_segments_short_audio_is_single_range() {
        let rate = ASR_SAMPLE_RATE as usize;
        let total = 4.0;
        let pcm = tone_with_gaps(total, &[]);
        let ranges = plan_segments(&pcm, rate, total, SEGMENT_TARGET_SECS);
        assert_eq!(ranges.len(), 1);
        assert!((ranges[0].0).abs() < 1e-9);
        assert!((ranges[0].1 - total).abs() < 1e-9);
        assert!(plan_segments(&[], rate, 0.0, SEGMENT_TARGET_SECS).is_empty());
    }
}
