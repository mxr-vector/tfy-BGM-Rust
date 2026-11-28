//! 「字幕生成」：人声分离（可选）→ 识别音频归一化 → 分片 ASR → SRT / LRC。
//!
//! 数据流：
//! 1. 生成前由界面确认「人声分离」开关（默认开启）与分离模型；开启分离但未确认
//!    模型时直接拒绝；
//! 2. 先占用曲目 key 锁（与 UI / HTTP 的其他生成链路互斥）。开启分离时由
//!    `separation::separate_locked` 把原始音频分离为本地 `vocals.wav` 与伴奏文件，
//!    并在返回前校验两个文件存在且非空；失败即终止。关闭分离时跳过该步，
//!    直接使用原始音频，适合口播 / 语音素材；
//! 3. 用 ffmpeg 把识别输入（人声 WAV 或原始音频）归一化为 16 kHz 单声道 PCM
//!    （sherpa-onnx 的 `Wave::read` 只接受单声道 WAV）；开启分离时绝不读取
//!    原始混合音频或伴奏轨做识别；
//! 4. `ModelRuntime::transcribe_segments` 按固定时间窗切片逐段识别，得到带时间轴的
//!    `SubtitleSegment[]`，每完成一片推送一次进度；
//! 5. 结果格式化为 SRT / LRC，连同可选的人声分离交付信息持久化到应用配置目录的
//!    `subtitles/` 下，供「桌面歌词」按曲目 key 读取。
//!
//! 字幕生成只依赖 `AppHandle`、`ModelRuntime`、`Song` 与模型根目录，不持有 `State`，
//! 便于放进 `spawn_blocking` 而不阻塞异步运行时。

use serde::{Deserialize, Serialize};

/// 字幕生成进度事件名；前端据此显示阶段与分片进度。
pub const SUBTITLE_PROGRESS_EVENT: &str = "subtitle-progress";
/// 字幕产物目录名（位于应用配置目录下）。
const SUBTITLES_DIR: &str = "subtitles";

/// 一条带起止时间的字幕。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 字幕生成阶段。
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleStage {
    Separating,
    Normalizing,
    Transcribing,
    Done,
    Error,
}

/// 进度事件负载；`error` 非空表示本次生成失败。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleProgress {
    pub stage: SubtitleStage,
    pub message: String,
    pub current: usize,
    pub total: usize,
    pub error: Option<String>,
}

/// 生成的完整结果，含预览文本与两种字幕文件内容。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleResult {
    pub key: String,
    pub title: String,
    pub source_path: String,
    pub source_format: String,
    /// 本次使用的分离模型与本地人声 / 伴奏文件；关闭人声分离时为 `None`。
    pub separation: Option<crate::separation::SeparationInfo>,
    pub duration: f64,
    pub segments: Vec<SubtitleSegment>,
    pub srt: String,
    pub lrc: String,
    /// 字幕记录 JSON 的本地路径。
    pub saved_path: String,
    /// 自动落盘的 SRT / LRC 本地文件路径（与分离交付物放在同一目录）。
    pub srt_path: String,
    pub lrc_path: String,
}

/// 持久化到磁盘的字幕（桌面歌词与导出共用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSubtitle {
    pub key: String,
    pub title: String,
    pub segments: Vec<SubtitleSegment>,
    pub created_at_ms: u64,
    /// 最近一次手动编辑的时间（毫秒）；未编辑过的旧记录没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at_ms: Option<u64>,
    /// 源音频文件名；旧字幕文件没有该字段，记录列表用曲目 key 的末段兜底。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// 源音频时长（毫秒）；旧字幕文件没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// 按统一规则格式化好的文本；旧文件可能缺失，读取时由 `load_stored` 重建。
    #[serde(default)]
    pub srt: String,
    #[serde(default)]
    pub lrc: String,
    // ---- 人声分离交付信息（旧记录没有这些字段）----
    /// 分离模型的显示名（不是偏好里保存的模型 id）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separation_model_name: Option<String>,
    /// 固定的伴奏模型显示名；旧记录没有该字段。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accompaniment_model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocals_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accompaniment_path: Option<String>,
    /// `伴奏` 或 `乐器`；新版固定为「伴奏」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accompaniment_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub srt_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lrc_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleExportResult {
    pub path: String,
    pub bytes: u64,
    pub format: String,
}

/// 推送一次进度给调用方；是否同时发界面事件由调用方决定。
///
/// `generate`（界面发起）在回调里发 `subtitle-progress`；HTTP 任务队列只更新任务快照，
mod format;
mod generate;
mod store;

#[cfg(test)]
mod tests;

pub use format::{parse_lrc, parse_srt, write_subtitle};
pub use generate::{emit_failure, generate, generate_with_progress};
pub(crate) use store::subtitle_stem;
pub use store::{delete_stored, list_stored, load_stored, migrate_keys, update_stored};
