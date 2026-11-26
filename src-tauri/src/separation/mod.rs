//! 「人声分离」：原始音频 → 44.1 kHz 双声道 PCM → sherpa-onnx 分离 → 本地人声 / 伴奏文件。
//!
//! 字幕生成必须先经过本模块：人声由用户选择的模型生成，伴奏固定由
//! `UVR-MDX-NET-Inst_HQ_3` 生成；两者都产出并校验 `vocals.wav` 与
//! `accompaniment.wav` 后，再由字幕链路只对 `vocals.wav` 做归一化与 ASR。
//! 分离失败、文件缺失或为空时直接返回错误，调用方不得继续识别。
//!
//! sherpa-onnx 的 Rust 封装尚未暴露 `OfflineSourceSeparation`，这里直接绑定其
//! C API（与 `sherpa-onnx` crate 链接同一份静态库），模型格式由 sherpa-onnx 校验。
//!
//! 子模块按职责划分：
//! - [`models`]：随包 / 自定义模型目录、解压与兼容性预检；
//! - [`ffi`]：唯一的 `unsafe` 边界，封装 C API 与句柄生命周期；
//! - [`decode`]：ffmpeg 解码与 PCM 读取；
//! - [`worker`]：独立子进程内的实际分离任务。
//!
//! 本文件只保留跨模块共用的类型、常量与对外交付 API。

mod decode;
mod ffi;
mod models;
mod worker;

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Song;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::subtitles::subtitle_stem;
use models::{preflight_custom, resolve_accompaniment, resolve_spec, source_exists, target_label};
use worker::{WorkerJob, WorkerResult, run_worker_process, worker_engine, worker_failure_detail};

pub use models::catalog;
pub use worker::worker_main;

/// 自定义分离模型目录名（位于「模型目录」下）；与 ASR 模型扫描隔离。
pub const CUSTOM_DIR_NAME: &str = "separation";
/// 随包分发的 spleeter 模型压缩包文件名。
const SPLEETER_ARCHIVE: &str = "sherpa-onnx-spleeter-2stems-int8.tar.bz2";
/// 解压后的 spleeter 模型目录名。
const SPLEETER_DIR: &str = "sherpa-onnx-spleeter-2stems-int8";
/// 伴奏固定使用的 UVR 乐器模型；用户不可更改。
const ACCOMPANIMENT_MODEL_FILE: &str = "UVR-MDX-NET-Inst_HQ_3.onnx";
const ACCOMPANIMENT_MODEL_ID: &str = "uvr-inst-hq-3";
/// 分离引擎输入采样率；sherpa-onnx 内部会重采样到模型要求的采样率。
const INPUT_SAMPLE_RATE: u32 = 44_100;
const INPUT_CHANNELS: usize = 2;
/// 分离产物目录名（位于应用配置目录下）。
const SEPARATIONS_DIR: &str = "separations";
/// 自身可执行文件的隐藏子进程参数：分离在独立进程内执行。
///
/// sherpa-onnx 遇到不兼容模型时会直接 `_Exit(-1)`，无法在进程内捕获；
/// 把引擎调用隔离到子进程后，最坏情况只是 worker 退出，主进程照常报错。
pub const WORKER_ARG: &str = "--separation-worker";

/// 模型的主输出角色：决定 `stems[0]` 装的是人声还是乐器。
///
/// 注意：sherpa-onnx 只保证「`stems[0]` = 模型主输出，`stems[1]` = 混合 − 主输出」，
/// 并不保证 `stems[1]` 一定是伴奏。主输出是人声还是乐器，取决于模型自身的训练目标，
/// 因此取某个角色时必须结合本枚举换算下标，不能写死。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StemTarget {
    /// 人声模型：stem[0] 人声，stem[1] = 混合 − 人声（伴奏）。
    Vocals,
    /// 乐器模型：stem[0] 乐器，stem[1] = 混合 − 乐器（人声）。
    Instrumental,
}

#[derive(Debug, Clone)]
enum Source {
    Spleeter {
        vocals: PathBuf,
        accompaniment: PathBuf,
    },
    Uvr {
        model: PathBuf,
    },
}

#[derive(Debug, Clone)]
struct Spec {
    id: String,
    name: String,
    engine: String,
    target: StemTarget,
    /// 模型主输出角色是否确定；自定义 UVR 模型只能靠文件名猜测，需要用户确认。
    target_known: bool,
    description: String,
    source: Option<Source>,
    /// 压缩包在随包资源中、首次使用时可解压（仅 spleeter）。
    installable: bool,
    /// 供给界面展示的可用性说明。
    detail: String,
}

/// 前端模型选择列表的一项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeparationModelInfo {
    pub id: String,
    pub name: String,
    pub engine: String,
    /// `vocals` 表示模型直接输出人声；`instrumental` 表示输出乐器、人声取差值。
    pub target: String,
    /// `false` 时前端必须让用户确认主输出角色（自定义 UVR 模型）。
    pub target_known: bool,
    pub description: String,
    pub available: bool,
    pub detail: String,
}

/// 分离模型目录：人声模型由用户选择，伴奏模型固定。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeparationCatalog {
    pub vocals: Vec<SeparationModelInfo>,
    pub accompaniment: SeparationModelInfo,
}

/// 本次分离的完整交付信息；随字幕结果一起返回并持久化。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeparationInfo {
    pub model_id: String,
    pub model_name: String,
    pub engine: String,
    pub target: String,
    /// 固定的伴奏模型显示名。
    pub accompaniment_model_name: String,
    pub vocals_path: String,
    pub accompaniment_path: String,
    /// `伴奏` 或 `乐器`，用于界面标注第二个文件的性质；新版固定为「伴奏」。
    pub accompaniment_kind: String,
    pub sample_rate: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SeparationRecord<'a> {
    key: &'a str,
    model_id: &'a str,
    model_name: &'a str,
    engine: &'a str,
    target: &'a str,
    accompaniment_model_name: &'a str,
    vocals_path: &'a str,
    accompaniment_path: &'a str,
    accompaniment_kind: &'a str,
    sample_rate: u32,
    source_path: String,
    created_at_ms: u64,
}

// ---------------- 交付目录 ----------------

/// 显式保存分离结果目录；同曲目重新生成会覆盖旧文件。
pub fn separation_dir(app: &AppHandle, key: &str) -> Result<PathBuf> {
    let dir = dir_for(app, key)?;
    fs::create_dir_all(&dir).with_context(|| format!("无法创建人声分离目录：{}", dir.display()))?;
    Ok(dir)
}

/// 某曲目的分离交付目录；不创建目录。
pub fn dir_for(app: &AppHandle, key: &str) -> Result<PathBuf> {
    Ok(app
        .path()
        .app_config_dir()
        .context("无法定位应用配置目录")?
        .join(SEPARATIONS_DIR)
        .join(subtitle_stem(key)))
}

/// 读取某曲目的分离记录（`separation.json`）；未执行过分离时返回 `None`。
///
/// 只读交付目录下的固定文件，不创建任何目录，供开放接口查询已有结果。
pub fn load_record(app: &AppHandle, key: &str) -> Result<Option<serde_json::Value>> {
    let path = dir_for(app, key)?.join("separation.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("无法读取分离记录：{}", path.display()));
        }
    };
    let value =
        serde_json::from_str(&text).with_context(|| format!("分离记录损坏：{}", path.display()))?;
    Ok(Some(value))
}

/// 曲目 key 变化时迁移分离目录；返回是否真的移动了目录。
pub fn migrate(app: &AppHandle, from: &str, to: &str) -> Result<bool> {
    if from == to {
        return Ok(false);
    }
    let old = dir_for(app, from)?;
    if !old.is_dir() {
        return Ok(false);
    }
    let new = dir_for(app, to)?;
    if new.exists() {
        fs::remove_dir_all(&new)
            .with_context(|| format!("无法替换旧的分离目录：{}", new.display()))?;
    }
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("无法创建分离目录：{}", parent.display()))?;
    }
    fs::rename(&old, &new)
        .with_context(|| format!("无法迁移分离目录 {} → {}", old.display(), new.display()))?;
    Ok(true)
}

/// 删除字幕记录时一并清理该曲目的人声 / 伴奏文件，避免配置目录无限增长。
pub fn cleanup(app: &AppHandle, key: &str) {
    let Ok(dir) = dir_for(app, key) else {
        return;
    };
    let _ = fs::remove_dir_all(dir);
}

// ---------------- 分离执行 ----------------

/// 写出 PCM16 WAV；保留给测试与需要独立 Vec 的调用方。
#[cfg_attr(not(test), allow(dead_code))]
fn write_wav16(path: &Path, channels: &[Vec<f32>], sample_rate: u32) -> Result<u64> {
    let slices: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
    write_wav16_slices(path, &slices, sample_rate)
}

pub(crate) fn write_wav16_slices(
    path: &Path,
    channels: &[&[f32]],
    sample_rate: u32,
) -> Result<u64> {
    if channels.is_empty() || channels[0].is_empty() {
        bail!("无法写出空音轨：{}", path.display());
    }
    if channels
        .iter()
        .any(|channel| channel.len() != channels[0].len())
    {
        bail!("无法写出声道长度不一致的音轨：{}", path.display());
    }
    if sample_rate == 0 || sample_rate > u32::from(u16::MAX) {
        bail!("无法写出采样率异常的 WAV：{sample_rate}");
    }
    let channel_count = u16::try_from(channels.len()).map_err(|_| anyhow!("声道数过多"))?;
    let frames = channels[0].len();
    let data_bytes = (frames as u64) * (channel_count as u64) * 2;
    let riff_bytes = 36u64
        .checked_add(data_bytes)
        .filter(|value| *value <= u64::from(u32::MAX))
        .ok_or_else(|| anyhow!("音频过长，WAV 文件超过 4 GB 上限"))?;

    let file =
        File::create(path).with_context(|| format!("无法创建音频文件：{}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(b"RIFF").context("写入 WAV 头失败")?;
    writer
        .write_all(&(riff_bytes as u32).to_le_bytes())
        .context("写入 WAV 头失败")?;
    writer.write_all(b"WAVEfmt ").context("写入 WAV 头失败")?;
    writer
        .write_all(&16u32.to_le_bytes())
        .context("写入 WAV 头失败")?;
    writer
        .write_all(&1u16.to_le_bytes())
        .context("写入 WAV 头失败")?;
    writer
        .write_all(&channel_count.to_le_bytes())
        .context("写入 WAV 头失败")?;
    writer
        .write_all(&sample_rate.to_le_bytes())
        .context("写入 WAV 头失败")?;
    let byte_rate = sample_rate * u32::from(channel_count) * 2;
    writer
        .write_all(&byte_rate.to_le_bytes())
        .context("写入 WAV 头失败")?;
    let block_align = channel_count * 2;
    writer
        .write_all(&block_align.to_le_bytes())
        .context("写入 WAV 头失败")?;
    writer
        .write_all(&16u16.to_le_bytes())
        .context("写入 WAV 头失败")?;
    writer.write_all(b"data").context("写入 WAV 头失败")?;
    writer
        .write_all(&(data_bytes as u32).to_le_bytes())
        .context("写入 WAV 头失败")?;

    let mut buffer = Vec::with_capacity(16_384 * channels.len() * 2);
    for frame in 0..frames {
        for channel in channels {
            let sample = (channel[frame].clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
            buffer.extend_from_slice(&sample.to_le_bytes());
        }
        if buffer.len() >= 1 << 20 {
            writer.write_all(&buffer).context("写入 WAV 数据失败")?;
            buffer.clear();
        }
    }
    if !buffer.is_empty() {
        writer.write_all(&buffer).context("写入 WAV 数据失败")?;
    }
    writer.flush().context("写入 WAV 数据失败")?;
    let size = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    Ok(size)
}

/// 对当前曲目执行完整分离并保存人声 / 伴奏文件。
///
/// 同一曲目同时只允许一条生成 / 分离链路：UI 的「生成字幕」与 HTTP 任务队列
/// 都会写同一个交付目录，并发会互相覆盖 `.part` 与正式文件，因此这里按 key 互斥。
pub fn separate(
    app: &AppHandle,
    song: &Song,
    model_id: &str,
    models_root: &Path,
    target_override: Option<&str>,
) -> Result<SeparationInfo> {
    let _guard = crate::key_lock::claim(&song.key)?;
    separate_locked(app, song, model_id, models_root, target_override)
}

/// [`separate`] 的实际实现；调用方必须已持有该曲目的 key 锁。
///
/// 字幕链路会先占用 key（覆盖分离后的归一化、ASR 与字幕写盘），再调用这里，
/// 避免嵌套占用同一个 key。
///
/// 引擎调用在独立子进程中执行：sherpa-onnx 对不兼容模型会直接 `_Exit(-1)`，
/// 隔离后最坏情况只是 worker 退出，主进程仍能给出可读错误。
/// 返回前会校验两个文件确实存在且非空；任一环节失败都返回错误。
pub(crate) fn separate_locked(
    app: &AppHandle,
    song: &Song,
    model_id: &str,
    models_root: &Path,
    target_override: Option<&str>,
) -> Result<SeparationInfo> {
    let model_id = model_id.trim();
    if model_id.is_empty() {
        bail!("请先选择并确认人声分离模型");
    }
    let spec = resolve_spec(app, models_root, model_id)?;
    if spec.id == ACCOMPANIMENT_MODEL_ID {
        bail!("{ACCOMPANIMENT_MODEL_FILE} 固定用于伴奏轨道，请选择人声分离模型");
    }
    let accompaniment_spec = resolve_accompaniment(app, models_root)?;
    let source = spec
        .source
        .clone()
        .ok_or_else(|| anyhow!("分离模型不可用：{}（{}）", spec.name, spec.detail))?;
    if !source_exists(&source) {
        bail!("分离模型文件缺失：{}（{}）", spec.name, spec.detail);
    }
    let accompaniment_source = accompaniment_spec
        .source
        .clone()
        .ok_or_else(|| anyhow!("伴奏分离模型不可用：{}", accompaniment_spec.name))?;
    if !source_exists(&accompaniment_source) {
        bail!(
            "伴奏分离模型文件缺失：{}（{}）",
            accompaniment_spec.name,
            accompaniment_spec.detail
        );
    }
    let target = match target_override {
        None => spec.target,
        Some("vocals") => StemTarget::Vocals,
        Some("instrumental") => StemTarget::Instrumental,
        Some(other) => bail!("未知的分离主输出：{other}"),
    };
    if matches!(source, Source::Spleeter { .. }) && target == StemTarget::Instrumental {
        bail!("Spleeter 双轨模型的输出固定为人声 + 伴奏，不能按「主输出为乐器」解释");
    }
    if spec.id.starts_with("custom:") {
        preflight_custom(&source)?;
    }

    let dir = separation_dir(app, &song.key)?;
    let vocals_path = dir.join("vocals.wav");
    let accompaniment_path = dir.join("accompaniment.wav");
    // worker 先写 `.part`，成功后再替换正式文件：重新生成失败不会破坏上一份交付物。
    let voices_part = dir.join("vocals.wav.part");
    let accompaniment_part = dir.join("accompaniment.wav.part");
    let parts = [&voices_part, &accompaniment_part];
    for part in parts {
        let _ = fs::remove_file(part);
    }

    let work_dir = std::env::temp_dir().join(format!(
        "tfy-bgm-separation-job-{}-{}",
        std::process::id(),
        now_ms()
    ));
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("无法创建分离任务目录：{}", work_dir.display()))?;
    let job_path = work_dir.join("job.json");
    let result_path = work_dir.join("result.json");
    let job = WorkerJob {
        source_path: song.path.clone(),
        vocals: worker_engine(&source),
        vocals_target: target_label(target).to_owned(),
        accompaniment: worker_engine(&accompaniment_source),
        accompaniment_target: target_label(StemTarget::Instrumental).to_owned(),
        vocals_path: voices_part.to_string_lossy().into_owned(),
        accompaniment_path: accompaniment_part.to_string_lossy().into_owned(),
        result_path: result_path.to_string_lossy().into_owned(),
    };
    let job_json = serde_json::to_vec(&job).context("序列化分离任务失败")?;
    fs::write(&job_path, job_json)
        .with_context(|| format!("无法写入分离任务：{}", job_path.display()))?;

    let output = run_worker_process(&job_path);
    let parsed = fs::read(&result_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WorkerResult>(&bytes).ok());
    let _ = fs::remove_dir_all(&work_dir);

    let output = match output {
        Ok(output) => output,
        Err(error) => return abort_parts(&parts, error),
    };
    let status = &output.status;
    let sample_rate = match parsed {
        Some(result) if result.ok => match result.sample_rate.filter(|rate| *rate > 0) {
            Some(rate) => rate,
            None => {
                return abort_parts(&parts, anyhow!("人声分离子进程未返回有效采样率"));
            }
        },
        Some(result) => {
            return abort_parts(
                &parts,
                anyhow!(
                    "人声分离失败：{}",
                    result.error.unwrap_or_else(|| "未知原因".to_owned())
                ),
            );
        }
        None => {
            return abort_parts(
                &parts,
                anyhow!(
                    "人声分离子进程异常退出（{}）；人声模型 {} 或伴奏模型 {} 可能与 sherpa-onnx 不兼容",
                    worker_failure_detail(&output),
                    spec.name,
                    accompaniment_spec.name
                ),
            );
        }
    };
    if !status.success() {
        return abort_parts(
            &parts,
            anyhow!(
                "人声分离子进程异常退出（{}）；人声模型 {} 或伴奏模型 {} 中有一个退出异常",
                worker_failure_detail(&output),
                spec.name,
                accompaniment_spec.name
            ),
        );
    }

    for part in parts {
        let size = fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
        if size <= 44 {
            return abort_parts(
                &[part],
                anyhow!("人声分离失败：未生成有效的音频文件 {}", part.display()),
            );
        }
    }
    // 交付物就绪：替换正式文件。Windows 的 rename 不能覆盖已存在文件，先删旧文件。
    for (part, target_path) in [
        (&voices_part, &vocals_path),
        (&accompaniment_part, &accompaniment_path),
    ] {
        let _ = fs::remove_file(target_path);
        if let Err(error) = fs::rename(part, target_path) {
            let _ = fs::remove_file(&voices_part);
            let _ = fs::remove_file(&accompaniment_part);
            return Err(error)
                .with_context(|| format!("无法保存分离文件：{}", target_path.display()));
        }
    }
    // 旧版本可能把伴奏写成 instrumental.wav；统一为 accompaniment.wav 后清掉旧交付物。
    let _ = fs::remove_file(dir.join("instrumental.wav"));

    // 伴奏固定由 UVR 乐器模型生成，交付物统一标注为「伴奏」。
    let accompaniment_kind = "伴奏";
    let info = SeparationInfo {
        model_id: spec.id.clone(),
        model_name: spec.name.clone(),
        engine: spec.engine.clone(),
        target: target_label(target).to_owned(),
        accompaniment_model_name: accompaniment_spec.name.clone(),
        vocals_path: vocals_path.to_string_lossy().into_owned(),
        accompaniment_path: accompaniment_path.to_string_lossy().into_owned(),
        accompaniment_kind: accompaniment_kind.to_owned(),
        sample_rate,
    };

    let record = SeparationRecord {
        key: &song.key,
        model_id: &info.model_id,
        model_name: &info.model_name,
        engine: &info.engine,
        target: &info.target,
        accompaniment_model_name: &info.accompaniment_model_name,
        vocals_path: &info.vocals_path,
        accompaniment_path: &info.accompaniment_path,
        accompaniment_kind: &info.accompaniment_kind,
        sample_rate: info.sample_rate,
        source_path: song.path.clone(),
        created_at_ms: now_ms(),
    };
    let json = serde_json::to_string_pretty(&record).context("序列化分离记录失败")?;
    fs::write(dir.join("separation.json"), json).context("保存分离记录失败")?;

    Ok(info)
}

/// 失败时清理未完成的 `.part` 文件，避免残留半截交付物。
fn abort_parts<T>(parts: &[&PathBuf], error: anyhow::Error) -> Result<T> {
    for part in parts {
        let _ = fs::remove_file(*part);
    }
    Err(error)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
