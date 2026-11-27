//! 「基本系统配置 → 模型」：ASR 模型目录、扫描、下载与 sherpa-onnx 推理。
//!
//! 技术路线固定为 sherpa-onnx：Rust 端直接调用其安全封装
//! （`OfflineRecognizer` + `OfflineQwen3ASRModelConfig`），不引入其他推理框架。
//! 当前只支持 ASR 模型，也只识别 Qwen3 ASR 的四件套目录结构：
//! `conv_frontend.onnx`、`encoder.int8.onnx`、`decoder.int8.onnx`、`tokenizer/`。
//!
//! 默认模型固定为 0.6B-int8 版本；下载清单写死为这四个构件（外加 tokenizer 内
//! 的词表文件），配置里只允许替换下载地址，不允许换成本、换参数量。

use std::time::Duration;

use serde::Serialize;
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

pub(crate) fn emit_load_status(app: &AppHandle, status: &ModelLoadStatus) {
    let _ = app.emit(LOAD_STATUS_EVENT, status);
}

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
mod catalog;
mod download;
mod runtime;
mod segment;

#[cfg(test)]
mod tests;

pub use catalog::{default_model, default_root, ensure_root, is_loadable, root_for, scan};
pub use runtime::ModelRuntime;
