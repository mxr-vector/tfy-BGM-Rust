//! 「人声分离」：原始音频 → 44.1 kHz 双声道 PCM → sherpa-onnx 分离 → 本地人声 / 伴奏文件。
//!
//! 字幕生成必须先经过本模块：人声由用户选择的模型生成，伴奏固定由
//! `UVR-MDX-NET-Inst_HQ_3` 生成；两者都产出并校验 `vocals.wav` 与
//! `accompaniment.wav` 后，再由字幕链路只对 `vocals.wav` 做归一化与 ASR。
//! 分离失败、文件缺失或为空时直接返回错误，调用方不得继续识别。
//!
//! sherpa-onnx 的 Rust 封装尚未暴露 `OfflineSourceSeparation`，这里直接绑定其
//! C API（与 `sherpa-onnx` crate 链接同一份静态库），模型格式由 sherpa-onnx 校验。

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_float};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Song;
use bgm_core::loudness::ffmpeg_path;
use serde::{Deserialize, Serialize};
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

use crate::subtitles::subtitle_stem;

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
/// 超过该时长的音频改为串行分离：两个引擎的整轨输出不同时驻留，峰值内存约减半。
const SEQUENTIAL_SEPARATION_SECONDS: usize = 10 * 60;
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

// ---------------- 分离 worker（独立进程） ----------------

/// 主进程写给 worker 的任务描述；worker 只做解码、分离与写盘，不接触 Tauri 状态。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerJob {
    source_path: String,
    /// 用户选择的人声模型。
    vocals: WorkerEngine,
    vocals_target: String,
    /// 固定伴奏模型（UVR-MDX-NET-Inst_HQ_3）。
    accompaniment: WorkerEngine,
    accompaniment_target: String,
    vocals_path: String,
    accompaniment_path: String,
    result_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
enum WorkerEngine {
    Spleeter {
        vocals: String,
        accompaniment: String,
    },
    Uvr {
        model: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkerResult {
    ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ---------------- sherpa-onnx C API ----------------

#[repr(C)]
struct SpleeterModelConfig {
    vocals: *const c_char,
    accompaniment: *const c_char,
}

#[repr(C)]
struct UvrModelConfig {
    model: *const c_char,
}

#[repr(C)]
struct SourceSeparationModelConfig {
    spleeter: SpleeterModelConfig,
    uvr: UvrModelConfig,
    num_threads: i32,
    debug: i32,
    provider: *const c_char,
}

#[repr(C)]
struct SourceSeparationConfig {
    model: SourceSeparationModelConfig,
}

#[repr(C)]
struct OfflineSourceSeparation {
    _private: [u8; 0],
}

#[repr(C)]
struct SourceSeparationStem {
    samples: *const *const c_float,
    num_channels: i32,
    n: i32,
}

#[repr(C)]
struct SourceSeparationOutput {
    stems: *const SourceSeparationStem,
    num_stems: i32,
    sample_rate: i32,
}

unsafe extern "C" {
    fn SherpaOnnxCreateOfflineSourceSeparation(
        config: *const SourceSeparationConfig,
    ) -> *const OfflineSourceSeparation;
    fn SherpaOnnxDestroyOfflineSourceSeparation(ss: *const OfflineSourceSeparation);
    fn SherpaOnnxOfflineSourceSeparationProcess(
        ss: *const OfflineSourceSeparation,
        samples: *const *const c_float,
        num_channels: i32,
        num_samples: i32,
        sample_rate: i32,
    ) -> *const SourceSeparationOutput;
    fn SherpaOnnxDestroySourceSeparationOutput(p: *const SourceSeparationOutput);
}

struct Engine(*const OfflineSourceSeparation);

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 句柄只由本模块创建，且 Drop 只执行一次。
            unsafe { SherpaOnnxDestroyOfflineSourceSeparation(self.0) };
        }
    }
}

struct OutputGuard(*const SourceSeparationOutput);

impl Drop for OutputGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 输出只由 sherpa-onnx 的 Process 分配，且只释放一次。
            unsafe { SherpaOnnxDestroySourceSeparationOutput(self.0) };
        }
    }
}

// ---------------- 模型目录 ----------------

/// 随包模型目录：优先 Tauri 资源目录，开发时回退到仓库 `models/`。
fn bundled_dir(app: &AppHandle) -> Option<PathBuf> {
    let resource = app
        .path()
        .resolve("models", BaseDirectory::Resource)
        .ok()
        .filter(|path| path.is_dir());
    if resource.is_some() {
        return resource;
    }
    #[cfg(debug_assertions)]
    {
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../models")
            .canonicalize()
            .ok()
            .filter(|path| path.is_dir());
        if dev.is_some() {
            return dev;
        }
    }
    None
}

fn extracted_spleeter_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_data_dir()
        .context("无法定位应用数据目录")?
        .join("separation-models")
        .join(SPLEETER_DIR);
    fs::create_dir_all(&dir)
        .with_context(|| format!("无法创建 spleeter 模型目录：{}", dir.display()))?;
    Ok(dir)
}

fn find_named_onnx(dir: &Path, needle: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    let mut found = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".onnx") && name.contains(needle) {
            // 名称完全相同的候选按字典序取第一个，保证多次运行结果稳定。
            if found
                .as_ref()
                .is_none_or(|current: &PathBuf| path < *current)
            {
                found = Some(path);
            }
        }
    }
    found
}

fn spleeter_files(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let vocals = find_named_onnx(dir, "vocals")?;
    let accompaniment = find_named_onnx(dir, "accompaniment")?;
    Some((vocals, accompaniment))
}

fn spleeter_spec(app: &AppHandle, bundled: Option<&Path>) -> Spec {
    let extracted = extracted_spleeter_dir(app).ok();
    let ready = extracted.as_deref().and_then(spleeter_files);
    let archive = bundled.map(|dir| dir.join(SPLEETER_ARCHIVE));
    let archive_exists = archive.as_deref().is_some_and(Path::is_file);
    let (source, detail) = match ready {
        Some((vocals, accompaniment)) => (
            Some(Source::Spleeter {
                vocals,
                accompaniment,
            }),
            "已就绪（随包模型）".to_owned(),
        ),
        None if archive_exists => (None, "随包模型，首次使用时会自动解压".to_owned()),
        None => (None, format!("缺少模型文件：{SPLEETER_ARCHIVE}")),
    };
    Spec {
        id: "spleeter-2stems-int8".to_owned(),
        name: "sherpa-onnx-spleeter-2stems-int8".to_owned(),
        engine: "Spleeter 2stems".to_owned(),
        target: StemTarget::Vocals,
        target_known: true,
        description: "双轨分离（人声 + 伴奏），速度较快，适合人声清晰的歌曲。".to_owned(),
        source,
        installable: archive_exists,
        detail,
    }
}

fn uvr_spec(bundled: Option<&Path>, file_name: &str, id: &str, target: StemTarget) -> Spec {
    let path = bundled.map(|dir| dir.join(file_name));
    let available = path.as_deref().is_some_and(Path::is_file);
    let description = match target {
        StemTarget::Vocals => "UVR 人声分离：模型直接输出人声，伴奏由混合音轨减人声得到。",
        StemTarget::Instrumental => "UVR 乐器分离：模型输出乐器轨，人声由混合音轨减乐器得到。",
    };
    Spec {
        id: id.to_owned(),
        name: file_name.trim_end_matches(".onnx").to_owned(),
        engine: "UVR MDX-Net".to_owned(),
        target,
        target_known: true,
        description: description.to_owned(),
        source: available.then(|| Source::Uvr {
            model: path.unwrap(),
        }),
        installable: false,
        detail: if available {
            file_name.to_owned()
        } else {
            format!("缺少模型文件：{file_name}")
        },
    }
}

fn uvr_target_from_name(name: &str) -> StemTarget {
    let lower = name.to_ascii_lowercase();
    if lower.contains("inst") {
        StemTarget::Instrumental
    } else {
        StemTarget::Vocals
    }
}

/// 扫描 `<模型目录>/separation/` 下的自定义模型。
///
/// - 目录内同时有 `vocals*.onnx` 与 `accompaniment*.onnx` → Spleeter 双模型；
/// - `separation/` 根目录下的其余 `.onnx` → UVR 模型，文件名含 `Inst` 时按乐器模型处理。
fn custom_specs(root: &Path) -> Vec<Spec> {
    let dir = root.join(CUSTOM_DIR_NAME);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut specs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if let Some((vocals, accompaniment)) = spleeter_files(&path) {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "自定义 Spleeter".to_owned());
                let relative = path
                    .strip_prefix(&dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                specs.push(Spec {
                    id: format!("custom:{relative}"),
                    name: name.clone(),
                    engine: "Spleeter 2stems".to_owned(),
                    target: StemTarget::Vocals,
                    target_known: true,
                    description: "自定义双轨分离模型（vocals + accompaniment）。".to_owned(),
                    source: Some(Source::Spleeter {
                        vocals,
                        accompaniment,
                    }),
                    installable: false,
                    detail: path.to_string_lossy().into_owned(),
                });
                continue;
            }
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("onnx"))
        {
            let name = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_else(|| "自定义 UVR".to_owned());
            let relative = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            specs.push(Spec {
                id: format!("custom:{relative}"),
                name: name.clone(),
                engine: "UVR MDX-Net".to_owned(),
                target: uvr_target_from_name(&name),
                // 自定义 UVR 模型没有角色元数据，文件名只是猜测；生成前让用户确认。
                target_known: false,
                description: "自定义 UVR 模型；必须是 sherpa-onnx 兼容的 2-stem ONNX 模型。"
                    .to_owned(),
                source: Some(Source::Uvr {
                    model: path.clone(),
                }),
                installable: false,
                detail: path.to_string_lossy().into_owned(),
            });
        }
    }
    specs.sort_by_key(|spec| spec.name.to_lowercase());
    specs
}

fn specs(app: &AppHandle, models_root: &Path) -> Vec<Spec> {
    let bundled = bundled_dir(app);
    let mut list = vec![
        spleeter_spec(app, bundled.as_deref()),
        uvr_spec(
            bundled.as_deref(),
            ACCOMPANIMENT_MODEL_FILE,
            ACCOMPANIMENT_MODEL_ID,
            StemTarget::Instrumental,
        ),
        uvr_spec(
            bundled.as_deref(),
            "UVR-MDX-NET-Voc_FT.onnx",
            "uvr-voc-ft",
            StemTarget::Vocals,
        ),
    ];
    list.extend(custom_specs(models_root));
    list
}

fn target_label(target: StemTarget) -> &'static str {
    match target {
        StemTarget::Vocals => "vocals",
        StemTarget::Instrumental => "instrumental",
    }
}

impl Spec {
    /// 转成前端展示项；随包模型即使缺失也保留一条，界面据此显示缺失原因。
    fn info(&self) -> SeparationModelInfo {
        let available = self.installable || self.source.as_ref().is_some_and(source_exists);
        let detail = if available || self.detail.starts_with("缺少") {
            self.detail.clone()
        } else {
            format!("{}；首次使用时会自动解压", self.detail)
        };
        SeparationModelInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            engine: self.engine.clone(),
            target: target_label(self.target).to_owned(),
            target_known: self.target_known,
            description: self.description.clone(),
            available,
            detail,
        }
    }
}

/// 分离模型目录：人声模型由用户选择，伴奏固定为 UVR-MDX-NET-Inst_HQ_3。
pub fn catalog(app: &AppHandle, models_root: &Path) -> SeparationCatalog {
    let mut specs = specs(app, models_root);
    let accompaniment = specs
        .iter()
        .find(|spec| spec.id == ACCOMPANIMENT_MODEL_ID)
        .map(Spec::info)
        .expect("随包伴奏模型条目必须存在");
    // 人声列表只保留人声主输出的模型；伴奏固定由内置乐器模型生成，不在此列出。
    specs.retain(|spec| spec.id != ACCOMPANIMENT_MODEL_ID && spec.target == StemTarget::Vocals);
    SeparationCatalog {
        vocals: specs.iter().map(Spec::info).collect(),
        accompaniment,
    }
}

fn source_exists(source: &Source) -> bool {
    match source {
        Source::Spleeter {
            vocals,
            accompaniment,
        } => vocals.is_file() && accompaniment.is_file(),
        Source::Uvr { model } => model.is_file(),
    }
}

/// 找到模型并确保 spleeter 已解压；返回可直接送入引擎的模型文件。
fn resolve_spec(app: &AppHandle, models_root: &Path, id: &str) -> Result<Spec> {
    let mut spec = specs(app, models_root)
        .into_iter()
        .find(|spec| spec.id == id)
        .ok_or_else(|| anyhow!("所选人声分离模型不存在：{id}。请重新选择模型"))?;
    if spec.source.is_none() && spec.installable {
        let bundled = bundled_dir(app);
        let archive = bundled
            .as_deref()
            .map(|dir| dir.join(SPLEETER_ARCHIVE))
            .filter(|path| path.is_file())
            .ok_or_else(|| anyhow!("分离模型不可用：{}（{}）", spec.name, spec.detail))?;
        let target = extracted_spleeter_dir(app)?;
        extract_archive(&archive, &target)?;
        let (vocals, accompaniment) = spleeter_files(&target)
            .ok_or_else(|| anyhow!("解压后未找到 spleeter 模型文件：{}", target.display()))?;
        spec.source = Some(Source::Spleeter {
            vocals,
            accompaniment,
        });
        spec.detail = "已就绪（随包模型）".to_owned();
        spec.installable = false;
    }
    if spec.source.is_none() {
        bail!("分离模型不可用：{}（{}）", spec.name, spec.detail);
    }
    Ok(spec)
}

/// 解析固定的伴奏模型；缺失时返回可读错误，不静默降级。
fn resolve_accompaniment(app: &AppHandle, models_root: &Path) -> Result<Spec> {
    let spec = specs(app, models_root)
        .into_iter()
        .find(|spec| spec.id == ACCOMPANIMENT_MODEL_ID)
        .ok_or_else(|| anyhow!("缺少固定的伴奏分离模型：{ACCOMPANIMENT_MODEL_FILE}"))?;
    if spec
        .source
        .as_ref()
        .is_none_or(|source| !source_exists(source))
    {
        bail!("伴奏分离模型不可用：{}（{}）", spec.name, spec.detail);
    }
    Ok(spec)
}

/// 解压随包模型；先解到临时目录再整体改名，避免中断留下半截模型。
fn extract_archive(archive: &Path, target: &Path) -> Result<()> {
    if spleeter_files(target).is_some() {
        return Ok(());
    }
    let part = target.with_extension("part");
    let _ = fs::remove_dir_all(&part);
    fs::create_dir_all(&part).with_context(|| format!("无法创建解压目录：{}", part.display()))?;
    let file = File::open(archive)
        .with_context(|| format!("无法读取分离模型压缩包：{}", archive.display()))?;
    let decoder = bzip2::read::BzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(&part)
        .with_context(|| format!("解压分离模型失败：{}", archive.display()))?;
    let nested = part.join(SPLEETER_DIR);
    let extracted = if nested.is_dir() {
        nested
    } else {
        part.clone()
    };
    if spleeter_files(&extracted).is_none() {
        let _ = fs::remove_dir_all(&part);
        bail!("解压后的压缩包内未找到 vocals / accompaniment 模型");
    }
    if target.exists() {
        fs::remove_dir_all(target)
            .with_context(|| format!("无法替换旧模型目录：{}", target.display()))?;
    }
    if extracted == part {
        fs::rename(&part, target)
            .with_context(|| format!("无法保存 spleeter 模型：{}", target.display()))?;
    } else {
        fs::rename(&extracted, target)
            .with_context(|| format!("无法保存 spleeter 模型：{}", target.display()))?;
        let _ = fs::remove_dir_all(&part);
    }
    Ok(())
}

// ---------------- 分离执行 ----------------

#[cfg_attr(not(test), allow(dead_code))]
struct Stems {
    sample_rate: u32,
    stems: Vec<Vec<Vec<f32>>>,
}

/// 引擎输出；声道切片借用 sherpa-onnx 的缓冲区，仅在 `with_engine` 的回调内有效。
struct RawStems<'a> {
    sample_rate: u32,
    stems: Vec<Vec<&'a [f32]>>,
}

/// 调用 sherpa-onnx 引擎并把结果交给回调处理。
///
/// 回调直接消费借用切片，避免为整首歌的人声 / 伴奏再复制一整份内存；
/// 返回前引擎缓冲区由 `OutputGuard` 释放。
fn with_engine<R>(
    source: &Source,
    channels: &[Vec<f32>],
    sample_rate: u32,
    num_threads: i32,
    consume: impl FnOnce(RawStems<'_>) -> Result<R>,
) -> Result<R> {
    if channels.is_empty() {
        bail!("分离输入没有音频声道");
    }
    let n = channels[0].len();
    if n == 0 {
        bail!("分离输入音频为空");
    }
    if channels.iter().any(|channel| channel.len() != n) {
        bail!("分离输入各声道长度不一致");
    }

    let empty = CString::new("").context("空字符串转换失败")?;
    let provider = CString::new("cpu").context("引擎提供者转换失败")?;
    let vocals_path;
    let accompaniment_path;
    let uvr_model;
    let config = match source {
        Source::Spleeter {
            vocals: vocals_file,
            accompaniment,
        } => {
            vocals_path = CString::new(vocals_file.to_string_lossy().as_bytes())
                .context("模型路径包含 NUL 字符")?;
            accompaniment_path = CString::new(accompaniment.to_string_lossy().as_bytes())
                .context("模型路径包含 NUL 字符")?;
            uvr_model = empty.clone();
            SourceSeparationConfig {
                model: SourceSeparationModelConfig {
                    spleeter: SpleeterModelConfig {
                        vocals: vocals_path.as_ptr(),
                        accompaniment: accompaniment_path.as_ptr(),
                    },
                    uvr: UvrModelConfig {
                        model: uvr_model.as_ptr(),
                    },
                    num_threads,
                    debug: 0,
                    provider: provider.as_ptr(),
                },
            }
        }
        Source::Uvr { model } => {
            vocals_path = empty.clone();
            accompaniment_path = empty.clone();
            uvr_model = CString::new(model.to_string_lossy().as_bytes())
                .context("模型路径包含 NUL 字符")?;
            SourceSeparationConfig {
                model: SourceSeparationModelConfig {
                    spleeter: SpleeterModelConfig {
                        vocals: vocals_path.as_ptr(),
                        accompaniment: accompaniment_path.as_ptr(),
                    },
                    uvr: UvrModelConfig {
                        model: uvr_model.as_ptr(),
                    },
                    num_threads,
                    debug: 0,
                    provider: provider.as_ptr(),
                },
            }
        }
    };

    // SAFETY: config 内的指针指向本函数持有的 CString，调用期间保持有效。
    let engine = unsafe { SherpaOnnxCreateOfflineSourceSeparation(&config) };
    if engine.is_null() {
        bail!(
            "无法创建人声分离引擎（模型需为 sherpa-onnx 兼容的 Spleeter/UVR 模型；当前模型不兼容时引擎会拒绝加载）"
        );
    }
    let engine = Engine(engine);

    let pointers: Vec<*const c_float> = channels.iter().map(|channel| channel.as_ptr()).collect();
    // SAFETY: 声道指针与样本数均由上面的校验保证有效；输出由 guard 释放。
    let output = unsafe {
        SherpaOnnxOfflineSourceSeparationProcess(
            engine.0,
            pointers.as_ptr(),
            channels.len() as i32,
            n as i32,
            sample_rate as i32,
        )
    };
    if output.is_null() {
        bail!("人声分离失败：sherpa-onnx 未返回结果");
    }
    let guard = OutputGuard(output);
    // SAFETY: output 非空且由 sherpa-onnx 填充；在 guard 生命周期内只读。
    let data = unsafe { &*guard.0 };
    if data.stems.is_null() {
        bail!("人声分离失败：模型未返回音轨数据");
    }
    if data.num_stems < 2 {
        bail!("人声分离失败：模型只输出 {} 个音轨", data.num_stems);
    }
    if data.sample_rate <= 0 {
        bail!("人声分离失败：输出采样率无效");
    }

    let mut stems = Vec::with_capacity(data.num_stems as usize);
    for index in 0..data.num_stems as usize {
        // SAFETY: stems 数组长度为 num_stems，索引在范围内。
        let stem = unsafe { &*data.stems.add(index) };
        let channels = unsafe { read_stem(stem, index) }?;
        stems.push(channels);
    }

    consume(RawStems {
        sample_rate: data.sample_rate as u32,
        stems,
    })
}

/// 把一条 stem 的声道指针转成借用切片；调用方保证输出缓冲区在切片使用期间有效。
unsafe fn read_stem(stem: &SourceSeparationStem, index: usize) -> Result<Vec<&[f32]>> {
    if stem.n <= 0 || stem.num_channels <= 0 {
        bail!("人声分离失败：第 {} 个音轨为空", index + 1);
    }
    let mut channels = Vec::with_capacity(stem.num_channels as usize);
    for channel in 0..stem.num_channels as usize {
        // SAFETY: samples 数组长度为 num_channels，n 为每声道样本数。
        let pointer = unsafe { *stem.samples.add(channel) };
        if pointer.is_null() {
            bail!("人声分离失败：第 {} 个音轨缺少声道数据", index + 1);
        }
        channels.push(unsafe { std::slice::from_raw_parts(pointer, stem.n as usize) });
    }
    Ok(channels)
}

/// 调用引擎并复制出独立音轨；仅测试与需要持有数据的调用方使用。
#[cfg_attr(not(test), allow(dead_code))]
fn run_engine(
    source: &Source,
    channels: &[Vec<f32>],
    sample_rate: u32,
    num_threads: i32,
) -> Result<Stems> {
    with_engine(source, channels, sample_rate, num_threads, |raw| {
        let stems = raw
            .stems
            .into_iter()
            .map(|channels| channels.into_iter().map(<[f32]>::to_vec).collect())
            .collect();
        Ok(Stems {
            sample_rate: raw.sample_rate,
            stems,
        })
    })
}

/// 可用 CPU 核心数；探测失败时按 2 核处理。
fn available_cores() -> usize {
    std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(2)
}

/// 并行引擎各自分到的线程数：按引擎数均分核心，避免两个模型互相抢核。
fn split_thread_count(cores: usize, engines: usize) -> i32 {
    (cores / engines.max(1)).clamp(1, 4) as i32
}

/// sherpa-onnx 对 UVR 模型要求的全部元数据键；缺失任一项会 `_Exit(-1)`。
const UVR_REQUIRED_METADATA: &[&str] = &[
    "sample_rate",
    "n_fft",
    "center",
    "win_length",
    "hop_length",
    "dim_t",
    "dim_f",
    "dim_c",
    "window_type",
];

/// 流式扫描 ONNX 元数据，避免把整个模型（几十 MB）读进内存。
///
/// ONNX 的 `metadata_props` 是 `StringStringEntryProto`（key=1, value=2），
/// 在未压缩的 protobuf 字节流里表现为 `0x0a <len> key 0x12 <len> value`。
fn scan_metadata(path: &Path) -> Result<HashMap<String, String>> {
    const CHUNK: usize = 1 << 20;
    /// 单条元数据远小于该值；保留尾部以处理跨块条目。
    const OVERLAP: usize = 8 << 10;
    const KEYS: &[&str] = &[
        "model_type",
        "model_name",
        "sample_rate",
        "n_fft",
        "center",
        "win_length",
        "hop_length",
        "dim_t",
        "dim_f",
        "dim_c",
        "window_type",
        "stems",
    ];

    fn collect(buffer: &[u8], map: &mut HashMap<String, String>) {
        let mut index = 0;
        while index + 2 <= buffer.len() {
            if buffer[index] == 0x0a {
                let key_len = buffer[index + 1] as usize;
                let key_end = index + 2 + key_len;
                if key_len > 0 && key_end + 2 <= buffer.len() && buffer[key_end] == 0x12 {
                    let value_len = buffer[key_end + 1] as usize;
                    let value_start = key_end + 2;
                    if value_start + value_len <= buffer.len()
                        && let (Ok(key), Ok(value)) = (
                            std::str::from_utf8(&buffer[index + 2..key_end]),
                            std::str::from_utf8(&buffer[value_start..value_start + value_len]),
                        )
                        && KEYS.contains(&key)
                    {
                        map.insert(key.to_owned(), value.to_owned());
                    }
                }
            }
            index += 1;
        }
    }

    let file = File::open(path).with_context(|| format!("无法读取分离模型：{}", path.display()))?;
    let mut reader = BufReader::with_capacity(CHUNK, file);
    let mut chunk = vec![0u8; CHUNK];
    let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK + OVERLAP);
    let mut map = HashMap::new();
    loop {
        let read = reader
            .read(&mut chunk)
            .with_context(|| format!("无法读取分离模型：{}", path.display()))?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        collect(&buffer, &mut map);
        if buffer.len() > OVERLAP {
            buffer.drain(..buffer.len() - OVERLAP);
        }
    }
    Ok(map)
}

/// 自定义模型进入 sherpa-onnx 前先校验元数据。
///
/// sherpa-onnx 遇到不兼容模型会直接 `_Exit(-1)`；虽然分离已放到子进程执行，
/// 这里仍做前置校验，把「模型不兼容」变成可读错误而不是子进程异常退出。
fn preflight_custom(source: &Source) -> Result<()> {
    fn inspect(path: &Path, model_type: &str, required: &[&str]) -> Result<()> {
        let metadata = scan_metadata(path)?;
        let actual = metadata.get("model_type").map(String::as_str).unwrap_or("");
        if actual != model_type {
            bail!(
                "模型 {} 不是 sherpa-onnx 兼容的 {model_type} 模型（model_type={actual:?}），已拒绝加载",
                path.display()
            );
        }
        let stems = metadata
            .get("stems")
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(0);
        if stems != 2 {
            bail!(
                "模型 {} 不是 2-stem 模型（stems={stems}），已拒绝加载",
                path.display()
            );
        }
        for key in required {
            if !metadata.contains_key(*key) {
                bail!(
                    "模型 {} 缺少 sherpa-onnx 必需的元数据 `{key}`，已拒绝加载",
                    path.display()
                );
            }
        }
        Ok(())
    }
    match source {
        Source::Spleeter {
            vocals,
            accompaniment,
        } => {
            inspect(vocals, "spleeter", &[])?;
            inspect(accompaniment, "spleeter", &[])?;
        }
        Source::Uvr { model } => inspect(model, "UVR", UVR_REQUIRED_METADATA)?,
    }
    Ok(())
}

/// 用 ffmpeg 把任意源音频解码为 44.1 kHz 双声道浮点 PCM。
fn decode_input(source: &Path) -> Result<Vec<Vec<f32>>> {
    let ffmpeg = ffmpeg_path().ok_or_else(|| {
        anyhow!("未检测到 ffmpeg，无法执行人声分离。请安装 ffmpeg 或设置 TFY_FFMPEG 环境变量后重试")
    })?;
    let raw = std::env::temp_dir().join(format!(
        "tfy-bgm-separation-{}-{}.f32",
        std::process::id(),
        now_ms()
    ));
    let output = bgm_core::process::quiet_command(&ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(source)
        .args([
            "-vn",
            "-ac",
            "2",
            "-ar",
            "44100",
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
        ])
        .arg(&raw)
        .output()
        .with_context(|| format!("无法启动 ffmpeg 解码：{}", ffmpeg.display()))?;
    if !output.status.success() {
        let _ = fs::remove_file(&raw);
        bail!("音频解码失败：{}", stderr_tail(&output.stderr));
    }

    // 分块读取解码结果，避免「整份原始字节 + 左右声道」三份音频同时驻留。
    let decoded = read_decoded(&raw);
    let _ = fs::remove_file(&raw);
    decoded
}

fn read_decoded(path: &Path) -> Result<Vec<Vec<f32>>> {
    const FRAME_BYTES: usize = std::mem::size_of::<f32>() * INPUT_CHANNELS;
    let file = File::open(path).with_context(|| format!("无法读取解码结果：{}", path.display()))?;
    let total = file
        .metadata()
        .map(|meta| meta.len())
        .with_context(|| format!("无法读取解码结果大小：{}", path.display()))?;
    if total == 0 || !total.is_multiple_of(FRAME_BYTES as u64) {
        bail!("音频解码失败：解码结果为空或长度异常");
    }
    let frames = usize::try_from(total / FRAME_BYTES as u64)
        .map_err(|_| anyhow!("音频过长，解码结果超出可处理范围"))?;
    if frames > i32::MAX as usize {
        bail!("音频过长（{frames} 帧），超出分离引擎的可处理范围");
    }

    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    let mut chunk = vec![0u8; (1 << 20) / FRAME_BYTES * FRAME_BYTES];
    loop {
        let read = reader
            .read(&mut chunk)
            .with_context(|| format!("无法读取解码结果：{}", path.display()))?;
        if read == 0 {
            break;
        }
        if !read.is_multiple_of(FRAME_BYTES) {
            bail!("音频解码失败：解码结果长度异常");
        }
        let (frames, _) = chunk[..read].as_chunks::<FRAME_BYTES>();
        for frame in frames {
            left.push(f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]));
            right.push(f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]));
        }
    }
    if left.len() != frames {
        bail!("音频解码失败：解码结果长度与文件大小不一致");
    }
    Ok(vec![left, right])
}

fn stderr_tail(bytes: &[u8]) -> String {
    tail_text(bytes, "（ffmpeg 未输出错误信息）")
}

/// 截取输出末尾 400 个字符；为空时返回调用方给的占位文本。
fn tail_text(bytes: &[u8], empty: &str) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return empty.to_owned();
    }
    trimmed
        .chars()
        .rev()
        .take(400)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

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

// ---------------- worker 进程 ----------------

/// 失败时清理未完成的 `.part` 文件，避免残留半截交付物。
fn abort_parts<T>(parts: &[&PathBuf], error: anyhow::Error) -> Result<T> {
    for part in parts {
        let _ = fs::remove_file(*part);
    }
    Err(error)
}

/// 以 worker 参数启动时执行分离任务；返回 `None` 表示当前进程不是 worker。
pub fn worker_main() -> Option<i32> {
    let mut args = std::env::args_os();
    let _exe = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new(WORKER_ARG)) {
        return None;
    }
    let Some(job_path) = args.next().map(PathBuf::from) else {
        return Some(2);
    };
    Some(run_worker(&job_path))
}

fn run_worker(job_path: &Path) -> i32 {
    let Ok(bytes) = fs::read(job_path) else {
        return 2;
    };
    let Ok(job) = serde_json::from_slice::<WorkerJob>(&bytes) else {
        return 2;
    };
    let result = execute_job(&job);
    let payload = match &result {
        Ok(sample_rate) => WorkerResult {
            ok: true,
            sample_rate: Some(*sample_rate),
            error: None,
        },
        Err(error) => WorkerResult {
            ok: false,
            sample_rate: None,
            error: Some(format!("{error:#}")),
        },
    };
    let result_path = Path::new(&job.result_path);
    let _ = fs::write(
        result_path,
        serde_json::to_vec(&payload).unwrap_or_default(),
    );
    if result.is_ok() { 0 } else { 1 }
}

fn worker_engine(source: &Source) -> WorkerEngine {
    match source {
        Source::Spleeter {
            vocals,
            accompaniment,
        } => WorkerEngine::Spleeter {
            vocals: vocals.to_string_lossy().into_owned(),
            accompaniment: accompaniment.to_string_lossy().into_owned(),
        },
        Source::Uvr { model } => WorkerEngine::Uvr {
            model: model.to_string_lossy().into_owned(),
        },
    }
}

fn worker_source(engine: &WorkerEngine) -> Source {
    match engine {
        WorkerEngine::Spleeter {
            vocals,
            accompaniment,
        } => Source::Spleeter {
            vocals: PathBuf::from(vocals),
            accompaniment: PathBuf::from(accompaniment),
        },
        WorkerEngine::Uvr { model } => Source::Uvr {
            model: PathBuf::from(model),
        },
    }
}

fn parse_target(value: &str) -> Result<StemTarget> {
    match value {
        "vocals" => Ok(StemTarget::Vocals),
        "instrumental" => Ok(StemTarget::Instrumental),
        other => bail!("未知的分离主输出：{other}"),
    }
}

/// 在引擎输出的两个 stem 中，找到指定角色所在的下标。
///
/// sherpa-onnx 只保证 `stems[0]` = 模型主输出、`stems[1]` = 「混合 − 主输出」。
/// 「想要的角色」与「模型主输出角色」一致时取 0，否则取 1：
/// - 人声模型取人声 → 0；人声模型取伴奏 → 1；
/// - 乐器模型取乐器 → 0；乐器模型取人声 → 1。
///
/// 这与旧版单引擎的 `Instrumental => (人声 1, 伴奏 0)` 映射一致；固定伴奏模型
/// 是乐器模型，因此伴奏取它自己的 `stems[0]`（不是角色枚举值本身）。
fn stem_index(primary: StemTarget, wanted: StemTarget) -> usize {
    if primary == wanted { 0 } else { 1 }
}

/// 取出线程 panic 的文本，方便在子进程日志里定位。
fn panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知 panic".to_owned())
}

fn execute_job(job: &WorkerJob) -> Result<u32> {
    let vocals_source = worker_source(&job.vocals);
    let accompaniment_source = worker_source(&job.accompaniment);
    let vocals_target = parse_target(&job.vocals_target)?;
    let accompaniment_target = parse_target(&job.accompaniment_target)?;
    let input = decode_input(Path::new(&job.source_path))?;
    let vocals_path = PathBuf::from(&job.vocals_path);
    let accompaniment_path = PathBuf::from(&job.accompaniment_path);
    let vocals_index = stem_index(vocals_target, StemTarget::Vocals);
    let accompaniment_index = stem_index(accompaniment_target, StemTarget::Instrumental);

    let run_vocals = |threads: i32| -> Result<u32> {
        with_engine(&vocals_source, &input, INPUT_SAMPLE_RATE, threads, |raw| {
            if raw.stems.len() < 2 {
                bail!("人声分离失败：所选模型未同时输出人声轨与残差轨");
            }
            let stem = &raw.stems[vocals_index];
            if stem.is_empty() || stem[0].is_empty() {
                bail!("人声分离失败：人声轨为空");
            }
            write_wav16_slices(&vocals_path, stem, raw.sample_rate)?;
            Ok(raw.sample_rate)
        })
    };
    let run_accompaniment = |threads: i32| -> Result<()> {
        with_engine(
            &accompaniment_source,
            &input,
            INPUT_SAMPLE_RATE,
            threads,
            |raw| {
                if raw.stems.len() < 2 {
                    bail!("伴奏分离失败：伴奏模型未同时输出乐器轨与残差轨");
                }
                let stem = &raw.stems[accompaniment_index];
                if stem.is_empty() || stem[0].is_empty() {
                    bail!("伴奏分离失败：伴奏轨为空");
                }
                write_wav16_slices(&accompaniment_path, stem, raw.sample_rate)?;
                Ok(())
            },
        )
    };

    let cores = available_cores();
    // 超长音频串行执行：两个引擎的整轨输出不同时驻留，避免峰值内存翻倍。
    let samples = input.first().map_or(0, Vec::len);
    if samples > SEQUENTIAL_SEPARATION_SECONDS * INPUT_SAMPLE_RATE as usize {
        let threads = split_thread_count(cores, 1);
        let sample_rate = run_vocals(threads)?;
        run_accompaniment(threads)?;
        return Ok(sample_rate);
    }

    // 常规歌曲：解码只做一次，两个独立引擎并行生成，线程数按核心均分。
    let threads = split_thread_count(cores, 2);
    let (vocals_result, accompaniment_result) = std::thread::scope(|scope| {
        let vocals = scope.spawn(|| run_vocals(threads));
        let accompaniment = scope.spawn(|| run_accompaniment(threads));
        (vocals.join(), accompaniment.join())
    });
    let sample_rate = vocals_result.map_err(|payload| {
        anyhow!("人声分离线程异常结束：{}", panic_payload(payload.as_ref()))
    })??;
    accompaniment_result.map_err(|payload| {
        anyhow!("伴奏分离线程异常结束：{}", panic_payload(payload.as_ref()))
    })??;
    Ok(sample_rate)
}

fn run_worker_process(job_path: &Path) -> Result<std::process::Output> {
    let exe = std::env::current_exe().context("无法定位应用可执行文件")?;
    let mut command = bgm_core::process::command(&exe);
    command
        .arg(WORKER_ARG)
        .arg(job_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
        .output()
        .with_context(|| format!("无法启动人声分离子进程：{}", exe.display()))
}

/// 子进程异常退出时的诊断信息：退出码 + sherpa-onnx 在 stderr 留下的最后一段日志。
fn worker_failure_detail(output: &std::process::Output) -> String {
    let status = describe_status(&output.status);
    let tail = tail_text(&output.stderr, "").trim().to_owned();
    if tail.is_empty() {
        status
    } else {
        format!("{status}；{tail}")
    }
}

fn describe_status(status: &std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("退出码 {code}"),
        None => "被异常终止".to_owned(),
    }
}

/// 删除字幕记录时一并清理该曲目的人声 / 伴奏文件，避免配置目录无限增长。
pub fn cleanup(app: &AppHandle, key: &str) {
    let Ok(dir) = dir_for(app, key) else {
        return;
    };
    let _ = fs::remove_dir_all(dir);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uvr_target_uses_inst_in_name() {
        assert_eq!(
            uvr_target_from_name("UVR-MDX-NET-Inst_HQ_3"),
            StemTarget::Instrumental
        );
        assert_eq!(
            uvr_target_from_name("UVR-MDX-NET-Voc_FT"),
            StemTarget::Vocals
        );
        assert_eq!(uvr_target_from_name("custom_model"), StemTarget::Vocals);
    }

    /// 回归用例：下标由「模型主输出角色」与「想取的角色」共同决定。
    ///
    /// sherpa-onnx 只保证 `stems[0]` 是模型主输出。人声模型的主输出是人声，
    /// 乐器模型（含固定伴奏模型）的主输出是乐器；两者取到「伴奏」时下标分别是 1 和 0。
    /// 该映射与旧版单引擎的 `Instrumental => (人声 1, 伴奏 0)` 完全一致。
    #[test]
    fn stem_index_follows_primary_output() {
        assert_eq!(stem_index(StemTarget::Vocals, StemTarget::Vocals), 0);
        assert_eq!(stem_index(StemTarget::Vocals, StemTarget::Instrumental), 1);
        assert_eq!(
            stem_index(StemTarget::Instrumental, StemTarget::Instrumental),
            0
        );
        assert_eq!(stem_index(StemTarget::Instrumental, StemTarget::Vocals), 1);
    }

    #[test]
    fn split_thread_count_shares_cores_between_engines() {
        assert_eq!(split_thread_count(12, 2), 4);
        assert_eq!(split_thread_count(8, 2), 4);
        assert_eq!(split_thread_count(4, 2), 2);
        assert_eq!(split_thread_count(2, 2), 1);
        assert_eq!(split_thread_count(1, 2), 1);
        assert_eq!(split_thread_count(0, 2), 1);
        assert_eq!(split_thread_count(16, 1), 4);
    }

    fn repo_path(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(relative)
    }

    /// 真实链路验证：Spleeter 分离 → 保存人声/伴奏 → 只对 vocals 做 ASR。
    ///
    /// 需要仓库内随包模型、ffmpeg、真实歌曲与已下载的 ASR 模型；默认不执行。
    /// 可用 `TFY_E2E_SONG` / `TFY_ASR_MODEL_DIR` 覆盖输入与模型位置。
    #[test]
    #[ignore = "需要真实歌曲、随包分离模型与 ASR 模型"]
    fn spleeter_then_asr_only_on_vocals() {
        let song = std::env::var_os("TFY_E2E_SONG")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_path("bgm/cheerful/上春山.mp3"));
        if !song.is_file() {
            eprintln!("跳过：找不到测试歌曲 {}", song.display());
            return;
        }
        let archive = repo_path(&format!("models/{SPLEETER_ARCHIVE}"));
        assert!(archive.is_file(), "缺少随包模型：{}", archive.display());
        let temp = tempfile::tempdir().unwrap();
        extract_archive(&archive, temp.path()).unwrap();
        let (vocals, accompaniment) = spleeter_files(temp.path()).expect("解压后应有双模型");
        let input = decode_input(&song).unwrap();
        assert_eq!(input.len(), 2);

        let output = run_engine(
            &Source::Spleeter {
                vocals,
                accompaniment,
            },
            &input,
            44_100,
            split_thread_count(available_cores(), 1),
        )
        .unwrap();
        assert_eq!(output.stems.len(), 2, "Spleeter 应输出两轨");
        assert!(!output.stems[0][0].is_empty());
        assert!(!output.stems[1][0].is_empty());

        let vocals_path = temp.path().join("vocals.wav");
        let accompaniment_path = temp.path().join("accompaniment.wav");
        let vocals_size = write_wav16(&vocals_path, &output.stems[0], output.sample_rate).unwrap();
        let accompaniment_size =
            write_wav16(&accompaniment_path, &output.stems[1], output.sample_rate).unwrap();
        assert!(vocals_size > 44);
        assert!(accompaniment_size > 44);
        eprintln!("人声文件：{}（{vocals_size} 字节）", vocals_path.display());
        eprintln!(
            "伴奏文件：{}（{accompaniment_size} 字节）",
            accompaniment_path.display()
        );

        let asr_wav = temp.path().join("vocals-16k.wav");
        let ffmpeg = ffmpeg_path().expect("需要 ffmpeg");
        let status = bgm_core::process::quiet_command(&ffmpeg)
            .args(["-hide_banner", "-nostdin", "-y", "-i"])
            .arg(&vocals_path)
            .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
            .arg(&asr_wav)
            .output()
            .expect("启动 ffmpeg 失败");
        assert!(status.status.success(), "归一化人声轨失败");

        let model_dir = std::env::var_os("TFY_ASR_MODEL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::download_dir()
                    .unwrap_or_default()
                    .join("bgm-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25")
            });
        if !model_dir.is_dir() {
            eprintln!("跳过 ASR：找不到模型 {}", model_dir.display());
            return;
        }
        let runtime = crate::models::ModelRuntime::default();
        runtime
            .load(model_dir.parent().unwrap(), &model_dir)
            .unwrap();
        let segments = runtime
            .transcribe_segments(&asr_wav, 10.0, &mut |_, _| {})
            .unwrap();
        assert!(!segments.is_empty(), "人声轨应能识别出歌词");
        let text = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        eprintln!("仅人声轨识别结果：{text}");
    }

    /// 两个随包 UVR 单模型都能被 C API 加载并输出两条非空音轨。
    #[test]
    #[ignore = "需要随包 UVR 模型、ffmpeg 与真实歌曲"]
    fn uvr_models_run_on_short_clip() {
        let song = std::env::var_os("TFY_E2E_SONG")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_path("bgm/cheerful/上春山.mp3"));
        if !song.is_file() {
            eprintln!("跳过：找不到测试歌曲 {}", song.display());
            return;
        }
        let mut input = decode_input(&song).unwrap();
        // 只取前 10 秒，控制验证耗时；分离引擎本身支持整首。
        for channel in &mut input {
            channel.truncate(10 * INPUT_SAMPLE_RATE as usize);
        }
        for file in ["UVR-MDX-NET-Voc_FT.onnx", "UVR-MDX-NET-Inst_HQ_3.onnx"] {
            let model = repo_path(&format!("models/{file}"));
            assert!(model.is_file(), "缺少随包模型：{}", model.display());
            let output = run_engine(
                &Source::Uvr { model },
                &input,
                INPUT_SAMPLE_RATE,
                split_thread_count(available_cores(), 1),
            )
            .unwrap_or_else(|error| panic!("{file} 分离失败：{error:#}"));
            assert_eq!(output.stems.len(), 2, "{file} 应输出两轨");
            assert!(!output.stems[0][0].is_empty(), "{file} 主轨为空");
            assert!(!output.stems[1][0].is_empty(), "{file} 残差轨为空");
            eprintln!(
                "{file} 输出 {} 轨 @ {} Hz",
                output.stems.len(),
                output.sample_rate
            );
        }
    }

    /// 两个 UVR 引擎在独立线程中并发运行，验证 C API / ORT 会话可并行。
    #[test]
    #[ignore = "需要随包 UVR 模型、ffmpeg 与真实歌曲"]
    fn parallel_uvr_engines_run_on_short_clip() {
        let song = std::env::var_os("TFY_E2E_SONG")
            .map(PathBuf::from)
            .unwrap_or_else(|| repo_path("bgm/cheerful/上春山.mp3"));
        if !song.is_file() {
            eprintln!("跳过：找不到测试歌曲 {}", song.display());
            return;
        }
        let mut input = decode_input(&song).unwrap();
        for channel in &mut input {
            channel.truncate(10 * INPUT_SAMPLE_RATE as usize);
        }
        let vocals_model = repo_path("models/UVR-MDX-NET-Voc_FT.onnx");
        let accompaniment_model = repo_path("models/UVR-MDX-NET-Inst_HQ_3.onnx");
        assert!(
            vocals_model.is_file(),
            "缺少随包模型：{}",
            vocals_model.display()
        );
        assert!(
            accompaniment_model.is_file(),
            "缺少随包模型：{}",
            accompaniment_model.display()
        );
        let threads = split_thread_count(available_cores(), 2);
        let input = &input;
        let (vocals, accompaniment) = std::thread::scope(|scope| {
            let vocals = scope.spawn(move || {
                run_engine(
                    &Source::Uvr {
                        model: vocals_model,
                    },
                    input,
                    INPUT_SAMPLE_RATE,
                    threads,
                )
            });
            let accompaniment = scope.spawn(move || {
                run_engine(
                    &Source::Uvr {
                        model: accompaniment_model,
                    },
                    input,
                    INPUT_SAMPLE_RATE,
                    threads,
                )
            });
            (vocals.join().unwrap(), accompaniment.join().unwrap())
        });
        let vocals = vocals.expect("人声模型并行运行失败");
        let accompaniment = accompaniment.expect("伴奏模型并行运行失败");
        assert_eq!(vocals.stems.len(), 2);
        assert_eq!(accompaniment.stems.len(), 2);
        assert!(!vocals.stems[0][0].is_empty());
        assert!(!accompaniment.stems[0][0].is_empty());
        eprintln!(
            "并行分离完成：人声 {} 轨 / 伴奏 {} 轨",
            vocals.stems.len(),
            accompaniment.stems.len()
        );
    }

    #[test]
    fn wav_header_is_valid_pcm16() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vocals.wav");
        let left = vec![0.0f32, 0.5, -0.5];
        let right = vec![0.1f32, -0.1, 0.0];
        let size = write_wav16(&path, &[left, right], 44_100).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(size as usize, bytes.len());
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2);
        assert_eq!(
            u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
            44_100
        );
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(bytes.len(), 44 + 3 * 2 * 2);
    }

    fn write_tarball(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let encoder = bzip2::write::BzEncoder::new(file, bzip2::Compression::best());
        let mut archive = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        for (name, data) in entries {
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, name, *data).unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extract_archive_installs_models_and_replaces_stale_target() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join(SPLEETER_ARCHIVE);
        let vocals = format!("{SPLEETER_DIR}/vocals.int8.onnx");
        let accompaniment = format!("{SPLEETER_DIR}/accompaniment.int8.onnx");
        write_tarball(
            &archive,
            &[
                (vocals.as_str(), b"vocals"),
                (accompaniment.as_str(), b"accompaniment"),
            ],
        );

        let target = dir.path().join("installed");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("stale.txt"), b"old").unwrap();

        extract_archive(&archive, &target).unwrap();
        assert!(spleeter_files(&target).is_some(), "应解压出双模型");
        assert!(!target.join("stale.txt").exists(), "旧内容应被替换");

        // 已就绪时再次调用应为幂等操作。
        extract_archive(&archive, &target).unwrap();
        assert!(spleeter_files(&target).is_some());
    }

    #[test]
    fn extract_archive_rejects_archive_without_models() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join(SPLEETER_ARCHIVE);
        write_tarball(&archive, &[("README.txt", b"no models here")]);
        let target = dir.path().join("installed");
        let error = extract_archive(&archive, &target).unwrap_err();
        assert!(error.to_string().contains("未找到"), "{error}");
        assert!(!target.exists(), "失败时不应留下目标目录");
    }

    fn metadata_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (key, value) in entries {
            bytes.push(0x0a);
            bytes.push(key.len() as u8);
            bytes.extend_from_slice(key.as_bytes());
            bytes.push(0x12);
            bytes.push(value.len() as u8);
            bytes.extend_from_slice(value.as_bytes());
        }
        bytes
    }

    fn complete_uvr_metadata(model_type: &str, stems: &str) -> Vec<u8> {
        let mut entries = vec![
            ("model_type", model_type),
            ("stems", stems),
            ("sample_rate", "44100"),
            ("n_fft", "6144"),
            ("center", "1"),
            ("win_length", "6144"),
            ("hop_length", "1024"),
            ("dim_t", "256"),
            ("dim_f", "3072"),
            ("dim_c", "4"),
            ("window_type", "hann"),
        ];
        let refs: Vec<(&str, &str)> = entries.drain(..).collect();
        metadata_bytes(&refs)
    }

    #[test]
    fn preflight_accepts_complete_uvr_model() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.onnx");
        fs::write(&model, complete_uvr_metadata("UVR", "2")).unwrap();
        preflight_custom(&Source::Uvr { model }).unwrap();
    }

    #[test]
    fn preflight_rejects_wrong_type_stem_count_and_missing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("model.onnx");

        fs::write(&model, complete_uvr_metadata("spleeter", "2")).unwrap();
        let error = preflight_custom(&Source::Uvr {
            model: model.clone(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("model_type"), "{error}");

        fs::write(&model, complete_uvr_metadata("UVR", "4")).unwrap();
        let error = preflight_custom(&Source::Uvr {
            model: model.clone(),
        })
        .unwrap_err();
        assert!(error.to_string().contains("2-stem"), "{error}");

        let missing = metadata_bytes(&[
            ("model_type", "UVR"),
            ("stems", "2"),
            ("sample_rate", "44100"),
        ]);
        fs::write(&model, missing).unwrap();
        let error = preflight_custom(&Source::Uvr { model }).unwrap_err();
        assert!(error.to_string().contains("缺少"), "{error}");
        assert!(error.to_string().contains("元数据"), "{error}");
    }

    #[test]
    fn read_decoded_splits_interleaved_stereo() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("decoded.f32");
        let mut bytes = Vec::new();
        for sample in [0.0f32, 1.0, 0.5, -0.5, -1.0, 0.25] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(&raw, bytes).unwrap();
        let channels = read_decoded(&raw).unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0], vec![0.0, 0.5, -1.0]);
        assert_eq!(channels[1], vec![1.0, -0.5, 0.25]);
    }

    #[test]
    fn worker_rejects_malformed_job() {
        let dir = tempfile::tempdir().unwrap();
        let job = dir.path().join("job.json");
        fs::write(&job, b"not json").unwrap();
        assert_eq!(run_worker(&job), 2);
    }

    /// 用真实模型验证 stem 语义：伴奏模型的主输出（stem[0]）是乐器，不是人声。
    ///
    /// 以人声模型 stem[0] 为参照，乐器模型的 stem[1] 应与之高度相关（残差 = 人声），
    /// 而 stem[0] 几乎不相关（乐器）。若有人把「伴奏 → 下标」写成 1，
    /// 本测试会失败，提醒伴奏轨会被写成人声。
    #[test]
    #[ignore = "需要随包 UVR 模型、ffmpeg 与真实歌曲"]
    fn accompaniment_model_primary_stem_is_instrumental() {
        fn correlation(a: &[f32], b: &[f32]) -> f32 {
            let n = a.len().min(b.len());
            let (mut mean_a, mut mean_b) = (0f64, 0f64);
            for i in 0..n {
                mean_a += a[i] as f64;
                mean_b += b[i] as f64;
            }
            mean_a /= n as f64;
            mean_b /= n as f64;
            let (mut num, mut dev_a, mut dev_b) = (0f64, 0f64, 0f64);
            for i in 0..n {
                let x = a[i] as f64 - mean_a;
                let y = b[i] as f64 - mean_b;
                num += x * y;
                dev_a += x * x;
                dev_b += y * y;
            }
            (num / (dev_a.sqrt() * dev_b.sqrt() + 1e-12)) as f32
        }

        let song = repo_path("bgm/cheerful/buttercup.mp3");
        let mut input = decode_input(&song).unwrap();
        for channel in &mut input {
            channel.truncate(10 * INPUT_SAMPLE_RATE as usize);
        }
        let threads = split_thread_count(available_cores(), 1);
        let vocals = run_engine(
            &Source::Uvr {
                model: repo_path("models/UVR-MDX-NET-Voc_FT.onnx"),
            },
            &input,
            INPUT_SAMPLE_RATE,
            threads,
        )
        .unwrap();
        let accompaniment = run_engine(
            &Source::Uvr {
                model: repo_path(&format!("models/{ACCOMPANIMENT_MODEL_FILE}")),
            },
            &input,
            INPUT_SAMPLE_RATE,
            threads,
        )
        .unwrap();

        let vocals_ch0 = &vocals.stems[0][0];
        let with_primary = correlation(vocals_ch0, &accompaniment.stems[0][0]);
        let with_residual = correlation(vocals_ch0, &accompaniment.stems[1][0]);
        assert!(
            with_residual > with_primary + 0.3,
            "伴奏模型的 stem[1]（残差）应是人声：与主输出的相关度 {with_residual} 未显著高于 {with_primary}"
        );
    }
}
