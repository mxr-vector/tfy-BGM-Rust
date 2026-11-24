//! 「字幕生成」：人声分离 → 人声轨归一化 → 分片 ASR → SRT / LRC。
//!
//! 数据流（顺序固定，不允许跳过或合并）：
//! 1. 用户在生成前确认人声分离模型；未确认时直接拒绝；
//! 2. 先占用曲目 key 锁（与 UI / HTTP 的其他生成链路互斥），再由
//!    `separation::separate_locked` 把原始音频分离为本地 `vocals.wav` 与伴奏文件，
//!    并在返回前校验两个文件存在且非空；失败即终止；
//! 3. 只对人声 WAV 用 ffmpeg 归一化为 16 kHz 单声道 PCM（sherpa-onnx 的
//!    `Wave::read` 只接受单声道 WAV）；绝不读取原始混合音频或伴奏轨做识别；
//! 4. `ModelRuntime::transcribe_segments` 按固定时间窗切片逐段识别，得到带时间轴的
//!    `SubtitleSegment[]`，每完成一片推送一次进度；
//! 5. 结果格式化为 SRT / LRC，连同分离模型与文件路径持久化到应用配置目录的
//!    `subtitles/` 下，供「桌面歌词」按曲目 key 读取。
//!
//! 字幕生成只依赖 `AppHandle`、`ModelRuntime`、`Song` 与模型根目录，不持有 `State`，
//! 便于放进 `spawn_blocking` 而不阻塞异步运行时。

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Song;
use bgm_core::export::safe_file_name;
use bgm_core::library::{AUDIO_EXTENSIONS, audio_extension};
use bgm_core::loudness::ffmpeg_path;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::models::ModelRuntime;

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
    /// 本次使用的分离模型与本地人声 / 伴奏文件；歌词识别仅使用其中的人声文件。
    pub separation: crate::separation::SeparationInfo,
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
/// 不向界面广播，避免字幕页把后台任务的进度显示在当前曲目下。
fn emit_progress(
    sink: &mut dyn FnMut(&SubtitleProgress),
    stage: SubtitleStage,
    message: impl Into<String>,
    current: usize,
    total: usize,
    error: Option<String>,
) {
    sink(&SubtitleProgress {
        stage,
        message: message.into(),
        current,
        total,
        error,
    });
}

fn subtitle_dir(app: &AppHandle) -> Result<PathBuf> {
    let dir = app
        .path()
        .app_config_dir()
        .context("无法定位应用配置目录")?
        .join(SUBTITLES_DIR);
    fs::create_dir_all(&dir).with_context(|| format!("无法创建字幕目录：{}", dir.display()))?;
    Ok(dir)
}

/// key 里可能含路径分隔符与中文，用「可读前缀 + FNV-1a 哈希」保证唯一且稳定。
pub(crate) fn subtitle_stem(key: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let prefix: String = safe_file_name(key).chars().take(48).collect();
    format!("{prefix}-{hash:016x}")
}

fn stored_path(app: &AppHandle, key: &str) -> Result<PathBuf> {
    Ok(subtitle_dir(app)?.join(format!("{}.json", subtitle_stem(key))))
}

fn wav_path_for(app: &AppHandle, key: &str) -> Result<PathBuf> {
    let dir = subtitle_dir(app)?.join("wav");
    fs::create_dir_all(&dir)
        .with_context(|| format!("无法创建归一化音频目录：{}", dir.display()))?;
    Ok(dir.join(format!("{}.wav", subtitle_stem(key))))
}

/// 读取某曲目已生成的字幕；文件缺失或损坏时返回 `None`。
///
/// 预览文本始终按当前格式化规则重建，避免旧文件缺少字段或曲目改名后与导出结果不一致。
pub fn load_stored(app: &AppHandle, key: &str) -> Option<StoredSubtitle> {
    let path = stored_path(app, key).ok()?;
    let text = fs::read_to_string(path).ok()?;
    let mut stored = serde_json::from_str::<StoredSubtitle>(&text).ok()?;
    stored.srt = format_srt(&stored.segments);
    stored.lrc = format_lrc(&stored.title, &stored.segments);
    Some(stored)
}

/// 列出配置目录中所有已生成的字幕，按生成时间倒序排列。
///
/// 字幕页的「字幕生成记录列表」以此为准：即使曲目已从曲库移除，
/// 只要字幕文件还在，就仍能看到、预览、导出或删除该记录。
pub fn list_stored(app: &AppHandle) -> Vec<StoredSubtitle> {
    let Ok(dir) = subtitle_dir(app) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut stored) = serde_json::from_str::<StoredSubtitle>(&text) else {
            continue;
        };
        stored.srt = format_srt(&stored.segments);
        stored.lrc = format_lrc(&stored.title, &stored.segments);
        items.push(stored);
    }
    items.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| a.title.cmp(&b.title))
    });
    items
}

/// 删除某曲目的字幕记录，同时清理可能残留的临时文件与归一化 WAV。
/// 返回是否真的删除了字幕 JSON；文件本来就不存在时返回 `false`。
pub fn delete_stored(app: &AppHandle, key: &str) -> Result<bool> {
    // 生成中删除会把正在写入的交付目录一起清掉，先抢 key 锁。
    let _guard = crate::key_lock::try_claim(key)
        .ok_or_else(|| anyhow!("{}", crate::key_lock::BUSY_MESSAGE))?;
    let path = stored_path(app, key)?;
    let mut removed = false;
    match fs::remove_file(&path) {
        Ok(()) => removed = true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            bail!("无法删除字幕文件 {}：{error}", path.display());
        }
    }
    // 生成中断留下的临时文件与归一化音频都不应残留。
    let _ = fs::remove_file(path.with_extension("json.part"));
    let wav = subtitle_dir(app)?
        .join("wav")
        .join(format!("{}.wav", subtitle_stem(key)));
    let _ = fs::remove_file(wav);
    // 人声 / 伴奏文件是本曲目字幕的中间交付物；删除记录时一起清理。
    crate::separation::cleanup(app, key);
    Ok(removed)
}

/// 一次生成需要随记录持久化的交付信息。
struct Delivery<'a> {
    separation: &'a crate::separation::SeparationInfo,
    srt_path: &'a Path,
    lrc_path: &'a Path,
}

fn save_stored(
    app: &AppHandle,
    key: &str,
    title: &str,
    file_name: Option<&str>,
    duration_ms: Option<u32>,
    segments: &[SubtitleSegment],
    delivery: &Delivery<'_>,
) -> Result<PathBuf> {
    let path = stored_path(app, key)?;
    let separation = delivery.separation;
    let stored = StoredSubtitle {
        key: key.to_owned(),
        title: title.to_owned(),
        segments: segments.to_vec(),
        created_at_ms: now_ms(),
        updated_at_ms: None,
        file_name: file_name.map(str::to_owned),
        duration_ms,
        srt: format_srt(segments),
        lrc: format_lrc(title, segments),
        separation_model_name: Some(separation.model_name.clone()),
        accompaniment_model_name: Some(separation.accompaniment_model_name.clone()),
        vocals_path: Some(separation.vocals_path.clone()),
        accompaniment_path: Some(separation.accompaniment_path.clone()),
        accompaniment_kind: Some(separation.accompaniment_kind.clone()),
        srt_path: Some(delivery.srt_path.to_string_lossy().into_owned()),
        lrc_path: Some(delivery.lrc_path.to_string_lossy().into_owned()),
    };
    let json = serde_json::to_string_pretty(&stored).context("序列化字幕结果失败")?;
    overwrite(&path, &json)?;
    Ok(path)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 曲目重命名 / 移动后，把字幕记录迁移到新 key（含 key、标题与源文件名）。
///
/// 只在应用内写操作与文件监听发现 `from != to` 时调用；删除曲目不在其列——孤立
/// 字幕按产品规则保留，仍可在字幕页预览、导出与删除。返回迁移过程中的告警，
/// 由调用方并入曲库快照展示。
pub fn migrate_keys(app: &AppHandle, changes: &[(String, String)], songs: &[Song]) -> Vec<String> {
    let mut warnings = Vec::new();
    for (from, to) in changes {
        if from == to {
            continue;
        }
        let Ok(old_path) = stored_path(app, from) else {
            continue;
        };
        if !old_path.is_file() {
            continue;
        }
        let song = songs.iter().find(|song| &song.key == to);
        // 先搬分离目录；失败时记录告警并保留旧路径，避免把记录指向不存在的文件。
        let moved = match crate::separation::migrate(app, from, to) {
            Ok(moved) => moved,
            Err(error) => {
                warnings.push(format!("分离目录迁移失败（{from} → {to}）：{error:#}"));
                false
            }
        };
        if let Err(error) = migrate_one(app, from, to, song, &old_path, moved) {
            // JSON 迁移失败时回滚目录，保证旧记录里的路径仍然有效。
            if moved {
                let _ = crate::separation::migrate(app, to, from);
            }
            warnings.push(format!("字幕记录迁移失败（{from} → {to}）：{error:#}"));
        }
    }
    warnings
}

fn migrate_one(
    app: &AppHandle,
    from: &str,
    to: &str,
    song: Option<&Song>,
    old_path: &Path,
    remap_separation: bool,
) -> Result<()> {
    let text = fs::read_to_string(old_path)
        .with_context(|| format!("无法读取字幕文件：{}", old_path.display()))?;
    let stored: StoredSubtitle = serde_json::from_str(&text)
        .with_context(|| format!("字幕文件损坏：{}", old_path.display()))?;
    let mut migrated = migrated_stored(&stored, to, song);
    if remap_separation {
        let new_dir = crate::separation::dir_for(app, to)?;
        remap_separation_path(&mut migrated.vocals_path, from, &new_dir);
        remap_separation_path(&mut migrated.accompaniment_path, from, &new_dir);
        remap_separation_path(&mut migrated.srt_path, from, &new_dir);
        remap_separation_path(&mut migrated.lrc_path, from, &new_dir);
    }
    let json = serde_json::to_string_pretty(&migrated).context("序列化字幕结果失败")?;
    let target = stored_path(app, to)?;

    if target == old_path {
        return overwrite(&target, &json);
    }
    overwrite(&target, &json)?;
    fs::remove_file(old_path)
        .with_context(|| format!("无法删除旧字幕文件：{}", old_path.display()))?;
    // 归一化 WAV 通常在生成结束后已删除；若上次中断留下，也一并清理。
    if let Ok(dir) = subtitle_dir(app) {
        let _ = fs::remove_file(dir.join("wav").join(format!("{}.wav", subtitle_stem(from))));
    }
    Ok(())
}

/// 把交付文件路径的目录部分从旧曲目目录替换为新曲目目录。
///
/// 只处理确实位于 `<separation>/<旧 stem>/` 下的路径，避免误改用户自定义路径。
fn remap_separation_path(path: &mut Option<String>, from: &str, new_dir: &Path) {
    let Some(current) = path.as_deref() else {
        return;
    };
    let current = Path::new(current);
    let old_stem = subtitle_stem(from);
    let inside_old = current
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some(old_stem.as_str());
    if !inside_old {
        return;
    }
    if let Some(name) = current.file_name() {
        *path = Some(new_dir.join(name).to_string_lossy().into_owned());
    }
}

/// 先写临时文件再 rename，保证迁移中断时不会留下半截 JSON。
fn overwrite(path: &Path, json: &str) -> Result<()> {
    let part = path.with_extension("json.part");
    fs::write(&part, json).with_context(|| format!("无法写入字幕文件：{}", part.display()))?;
    fs::rename(&part, path).with_context(|| format!("无法保存字幕文件：{}", path.display()))?;
    Ok(())
}

/// 迁移后的记录：key 跟随新曲目，标题 / 文件名 / 时长以最新曲库信息为准，
/// 预览文本按当前格式化规则重建；分段与生成时间保持不变。
fn migrated_stored(stored: &StoredSubtitle, to: &str, song: Option<&Song>) -> StoredSubtitle {
    let mut next = stored.clone();
    next.key = to.to_owned();
    if let Some(song) = song {
        next.title = song.title.clone();
        next.file_name = Some(song.file_name.clone());
        next.duration_ms = song.duration_ms;
    }
    next.srt = format_srt(&next.segments);
    next.lrc = format_lrc(&next.title, &next.segments);
    next
}

/// Windows 绿色版不弹控制台窗口（与 `bgm_core::loudness` 的处理一致）。
fn ffmpeg_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    command.stdout(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

fn stderr_tail(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "（ffmpeg 未输出错误信息）".to_owned()
    } else {
        trimmed
            .chars()
            .rev()
            .take(400)
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    }
}

/// 把分离后的人声 WAV 归一化为 16 kHz 单声道 PCM（sherpa-onnx 要求）。
///
/// 分离阶段已强制要求 ffmpeg，这里不再保留「直接使用源文件」的降级路径；
/// 返回的临时文件由调用方在识别结束后删除。
fn normalize_to_wav(app: &AppHandle, source: &Path, key: &str) -> Result<PathBuf> {
    let ffmpeg = ffmpeg_path().ok_or_else(|| {
        anyhow!("未检测到 ffmpeg，无法归一化人声轨。请安装 ffmpeg 或设置 TFY_FFMPEG 环境变量后重试")
    })?;

    let target = wav_path_for(app, key)?;
    let output = ffmpeg_command(&ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-i"])
        .arg(source)
        .args([
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ])
        .arg(&target)
        .output()
        .with_context(|| format!("无法启动 ffmpeg 转换：{}", ffmpeg.display()))?;

    if !output.status.success() {
        let _ = fs::remove_file(&target);
        bail!("音频归一化失败：{}", stderr_tail(&output.stderr));
    }
    let size = fs::metadata(&target).map(|meta| meta.len()).unwrap_or(0);
    if size == 0 {
        let _ = fs::remove_file(&target);
        bail!("音频归一化失败：ffmpeg 输出为空");
    }
    Ok(target)
}

/// 生成字幕主流程：人声分离 → 人声轨归一化 → 分片 ASR → SRT / LRC。
///
/// 强制顺序：必须先产出并校验人声 / 伴奏文件，随后**只**对分离得到的人声 WAV
/// 做归一化与识别。分离失败或文件缺失时直接返回错误，不会回退到原始混合音频。
/// 进度与错误都通过 `subtitle-progress` 事件推送，命令返回值用于结果展示。
pub fn generate(
    app: &AppHandle,
    runtime: &ModelRuntime,
    song: &Song,
    separation_model: &str,
    separation_target: Option<&str>,
    models_root: &Path,
) -> Result<SubtitleResult> {
    generate_with_progress(
        app,
        runtime,
        song,
        separation_model,
        separation_target,
        models_root,
        |progress| {
            let _ = app.emit(SUBTITLE_PROGRESS_EVENT, progress);
        },
    )
}

/// 同 [`generate`]，但把每次进度交给 `on_progress`，不直接发界面事件。
///
/// 界面事件由 [`generate`] 在回调里发送；HTTP 任务队列（[`crate::tasks`]）只更新任务快照，
/// 避免字幕页把后台任务的进度显示成当前曲目。
pub fn generate_with_progress<F>(
    app: &AppHandle,
    runtime: &ModelRuntime,
    song: &Song,
    separation_model: &str,
    separation_target: Option<&str>,
    models_root: &Path,
    mut on_progress: F,
) -> Result<SubtitleResult>
where
    F: FnMut(&SubtitleProgress),
{
    // 整条链路（分离 → 归一化 → ASR → 写 SRT / LRC）都写同一个交付目录，
    // 因此 key 锁要覆盖全程；与 HTTP 任务、UI 命令共用同一把锁。
    let _key_guard = crate::key_lock::claim(&song.key)?;
    let mut sink = |progress: &SubtitleProgress| on_progress(progress);
    let source = PathBuf::from(&song.path);
    if !source.is_file() {
        bail!("音频文件不存在：{}", source.display());
    }
    let ext = audio_extension(&song.file_name).ok_or_else(|| {
        anyhow!(
            "不支持的音频格式：{}（仅支持 {}）",
            song.file_name,
            AUDIO_EXTENSIONS.join(" / ")
        )
    })?;
    // 从人声分离开始占用模型，避免删除或释放模型导致后半段字幕任务失败。
    let _model_task = runtime.reserve_model_task()?;
    if runtime.loaded().is_none() {
        bail!("尚未加载 ASR 模型，请先在「设置 → 模型」中完成模型加载");
    }
    if separation_model.trim().is_empty() {
        bail!("请先选择并确认人声分离模型；未确认前不会开始分离");
    }

    emit_progress(
        &mut sink,
        SubtitleStage::Separating,
        "正在分离人声与伴奏（模型并行运行）…",
        0,
        0,
        None,
    );
    let separation = crate::separation::separate_locked(
        app,
        song,
        separation_model,
        models_root,
        separation_target,
    )?;
    // 交付物必须真实落盘；这里再次校验，避免任何降级路径进入识别阶段。
    for path in [&separation.vocals_path, &separation.accompaniment_path] {
        let path = Path::new(path);
        let size = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        if !path.is_file() || size <= 44 {
            bail!("人声分离未产出有效文件：{}；已停止生成字幕", path.display());
        }
    }

    emit_progress(
        &mut sink,
        SubtitleStage::Normalizing,
        "正在归一化分离后的人声轨…",
        0,
        0,
        None,
    );
    let normalized = normalize_to_wav(app, Path::new(&separation.vocals_path), &song.key)?;

    emit_progress(
        &mut sink,
        SubtitleStage::Transcribing,
        "正在识别分离后的人声轨…",
        0,
        0,
        None,
    );
    let transcribed = runtime.transcribe_segments(
        &normalized,
        crate::models::SEGMENT_TARGET_SECS,
        &mut |current, total| {
            emit_progress(
                &mut sink,
                SubtitleStage::Transcribing,
                format!("正在识别第 {current}/{total} 段…"),
                current,
                total,
                None,
            );
        },
    );
    // 归一化 WAV 只服务于本次识别；临时文件用完即删，避免在配置目录长期累积。
    let _ = fs::remove_file(&normalized);
    let segments = transcribed?;

    let srt = format_srt(&segments);
    let lrc = format_lrc(&song.title, &segments);
    // 字幕文件与分离交付物放在同一目录，便于一次性交付 / 核对。
    let delivery = crate::separation::separation_dir(app, &song.key)?;
    let srt_path = delivery.join("subtitles.srt");
    let lrc_path = delivery.join("subtitles.lrc");
    write_subtitle(&srt_path, &srt)?;
    write_subtitle(&lrc_path, &lrc)?;
    let saved_path = save_stored(
        app,
        &song.key,
        &song.title,
        Some(&song.file_name),
        song.duration_ms,
        &segments,
        &Delivery {
            separation: &separation,
            srt_path: &srt_path,
            lrc_path: &lrc_path,
        },
    )?;
    let duration = segments.last().map(|segment| segment.end).unwrap_or(0.0);

    emit_progress(
        &mut sink,
        SubtitleStage::Done,
        format!("字幕生成完成，共 {} 条", segments.len()),
        segments.len(),
        segments.len(),
        None,
    );

    Ok(SubtitleResult {
        key: song.key.clone(),
        title: song.title.clone(),
        source_path: source.to_string_lossy().into_owned(),
        source_format: ext.to_owned(),
        separation,
        duration,
        segments,
        srt,
        lrc,
        saved_path: saved_path.to_string_lossy().into_owned(),
        srt_path: srt_path.to_string_lossy().into_owned(),
        lrc_path: lrc_path.to_string_lossy().into_owned(),
    })
}

/// 生成失败时把原因推给前端，便于进度区直接展示。
pub fn emit_failure(app: &AppHandle, error: &anyhow::Error) {
    // 失败事件只服务界面；HTTP 任务队列的失败由 `generate_with_progress` 的返回值处理。
    let _ = app.emit(
        SUBTITLE_PROGRESS_EVENT,
        SubtitleProgress {
            stage: SubtitleStage::Error,
            message: "字幕生成失败".to_owned(),
            current: 0,
            total: 0,
            error: Some(format!("{error:#}")),
        },
    );
}

// ---------------- SRT / LRC 格式化 ----------------

/// SRT 时间：`HH:MM:SS,mmm`。
pub fn srt_time(seconds: f64) -> String {
    let millis = (seconds.max(0.0) * 1000.0).round() as u64;
    let hours = millis / 3_600_000;
    let minutes = (millis / 60_000) % 60;
    let secs = (millis / 1000) % 60;
    let ms = millis % 1000;
    format!("{hours:02}:{minutes:02}:{secs:02},{ms:03}")
}

/// LRC 时间：`MM:SS.cc`（百分秒）。
pub fn lrc_time(seconds: f64) -> String {
    let centis = (seconds.max(0.0) * 100.0).round() as u64;
    let minutes = centis / 6000;
    let secs = (centis / 100) % 60;
    let cs = centis % 100;
    format!("{minutes:02}:{secs:02}.{cs:02}")
}

pub fn format_srt(segments: &[SubtitleSegment]) -> String {
    let mut out = String::new();
    for (index, segment) in segments.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            index + 1,
            srt_time(segment.start),
            srt_time(segment.end),
            segment.text
        ));
    }
    out
}

pub fn format_lrc(title: &str, segments: &[SubtitleSegment]) -> String {
    let safe_title = title.replace(['\r', '\n'], " ");
    let mut out = format!("[ar:]\n[ti:{safe_title}]\n");
    for segment in segments {
        out.push_str(&format!("[{}]{}\n", lrc_time(segment.start), segment.text));
    }
    out
}

/// 写出字幕文件；返回字节数。
pub fn write_subtitle(path: &Path, content: &str) -> Result<u64> {
    fs::write(path, content.as_bytes())
        .map_err(|error: io::Error| anyhow!("无法写入字幕文件 {}：{error}", path.display()))?;
    Ok(content.as_bytes().len() as u64)
}

// ---------------- 字幕编辑：校验、解析与写回 ----------------

/// 单句允许的最大字符数，避免异常粘贴撑爆导出文件与歌词窗口；超出时明确报错而非截断。
const MAX_SEGMENT_TEXT_CHARS: usize = 2_000;
/// 一次编辑允许的最多分段数。
const MAX_SEGMENTS: usize = 5_000;

/// 规范化编辑后的分段：校验时间合法性、折叠文本换行、按开始时间排序。
///
/// 「分段编辑」与「原文编辑」两个入口共用此函数，保证落盘数据始终满足
/// `start >= 0`、`end > start`、文本为单行，且与桌面歌词 / 导出共用同一顺序。
pub fn normalize_segments(mut segments: Vec<SubtitleSegment>) -> Result<Vec<SubtitleSegment>> {
    if segments.is_empty() {
        bail!("字幕内容不能为空，至少需要保留一条");
    }
    if segments.len() > MAX_SEGMENTS {
        bail!("字幕条数过多（最多 {MAX_SEGMENTS} 条），请精简后重试");
    }
    for (index, segment) in segments.iter_mut().enumerate() {
        let label = index + 1;
        if !segment.start.is_finite() || !segment.end.is_finite() {
            bail!("第 {label} 条的开始 / 结束时间不是有效数字");
        }
        if segment.start < 0.0 || segment.end < 0.0 {
            bail!("第 {label} 条的时间不能为负数");
        }
        if segment.end <= segment.start {
            bail!("第 {label} 条的结束时间必须大于开始时间");
        }
        // SRT 以空行分块、文本单行；换行会破坏结构，统一折叠为空格。
        // 超长文本直接报错（不静默截断），避免用户内容悄悄丢失。
        let text = segment.text.replace(['\r', '\n'], " ");
        let text = text.trim();
        if text.chars().count() > MAX_SEGMENT_TEXT_CHARS {
            bail!("第 {label} 条文本过长（最多 {MAX_SEGMENT_TEXT_CHARS} 字），请精简后重试");
        }
        segment.text = text.to_owned();
    }
    segments.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.end.partial_cmp(&b.end).unwrap_or(Ordering::Equal))
    });
    Ok(segments)
}

/// 手动编辑后写回：保留生成时间与曲目元信息，只替换分段与格式化文本。
///
/// 记录必须已存在（编辑不会凭空创建字幕），`created_at_ms` 保持不变，
/// `updated_at_ms` 记录本次修改；返回的新快照直接交给前端刷新预览 / 导出 / 桌面歌词。
pub fn update_stored(
    app: &AppHandle,
    key: &str,
    segments: Vec<SubtitleSegment>,
) -> Result<StoredSubtitle> {
    // 与生成链路共用 key 锁：后台任务写入期间不接受编辑，避免互相覆盖。
    let _guard = crate::key_lock::try_claim(key)
        .ok_or_else(|| anyhow!("{}", crate::key_lock::BUSY_MESSAGE))?;
    let segments = normalize_segments(segments)?;
    let path = stored_path(app, key)?;
    let text = fs::read_to_string(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            anyhow!("该曲目尚未生成字幕，请先生成后再编辑")
        } else {
            anyhow!("无法读取字幕文件 {}：{error}", path.display())
        }
    })?;
    let mut stored: StoredSubtitle =
        serde_json::from_str(&text).with_context(|| format!("字幕文件损坏：{}", path.display()))?;
    stored.segments = segments;
    stored.updated_at_ms = Some(now_ms());
    // 与 load_stored 一致：预览文本始终按当前格式化规则重建。
    stored.srt = format_srt(&stored.segments);
    stored.lrc = format_lrc(&stored.title, &stored.segments);
    // 自动落盘的 SRT / LRC 只是交付副本：写入失败（目录被手动删除、文件被占用）
    // 不应阻断字幕记录本身的保存；旧记录没有路径字段则跳过。
    for (path, content) in [
        (stored.srt_path.as_deref(), stored.srt.as_str()),
        (stored.lrc_path.as_deref(), stored.lrc.as_str()),
    ] {
        let Some(path) = path else {
            continue;
        };
        let path = Path::new(path);
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = write_subtitle(path, content);
    }
    let json = serde_json::to_string_pretty(&stored).context("序列化字幕结果失败")?;
    overwrite(&path, &json)?;
    Ok(stored)
}

/// 解析 SRT 原文为分段；兼容缺失序号行、`MM:SS,mmm` 简写、`.` 毫秒分隔与多行文本。
///
/// 与 `format_srt` 互为逆操作：仅把「能被解析成起止时间且含 `-->`」的行当作时间轴，
/// 其余非空行并入当前分段的文本，纯数字行（序号）忽略。
pub fn parse_srt(text: &str) -> Result<Vec<SubtitleSegment>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut segments: Vec<SubtitleSegment> = Vec::new();
    let mut current: Option<(f64, f64, Vec<String>)> = None;

    fn flush(segments: &mut Vec<SubtitleSegment>, current: &mut Option<(f64, f64, Vec<String>)>) {
        if let Some((start, end, lines)) = current.take() {
            segments.push(SubtitleSegment {
                start,
                end,
                text: lines.join(" ").trim().to_owned(),
            });
        }
    }

    for (line_index, raw) in normalized.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            flush(&mut segments, &mut current);
            continue;
        }
        if let Some((start, end)) = parse_srt_range(line) {
            flush(&mut segments, &mut current);
            current = Some((start, end, Vec::new()));
            continue;
        }
        // 含 `-->` 却解析不出时间轴：多半是手误。直接报错而不是并入上一句，
        // 否则时间轴和随后的正文会被静默拼进上一条字幕。
        if line.contains("-->") {
            bail!("SRT 第 {} 行的时间轴无法解析：{line}", line_index + 1);
        }
        match current.as_mut() {
            Some((_, _, lines)) => lines.push(line.to_owned()),
            // 序号行；LRC 元数据等其它无头文本一律拒绝，避免静默丢内容。
            None if line.chars().all(|ch| ch.is_ascii_digit()) => {}
            None => bail!("SRT 第 {} 行既不是时间轴也不是序号：{line}", line_index + 1),
        }
    }
    flush(&mut segments, &mut current);
    normalize_segments(segments)
}

/// 尝试把一行解析为 SRT 时间轴；不是时间轴时返回 `None`（当作普通文本）。
fn parse_srt_range(line: &str) -> Option<(f64, f64)> {
    let (start, end) = line.split_once("-->")?;
    Some((parse_srt_time(start)?, parse_srt_time(end)?))
}

/// 支持 `HH:MM:SS,mmm` / `HH:MM:SS.mmm` / `MM:SS,mmm` / `SS.mmm`。
fn parse_srt_time(raw: &str) -> Option<f64> {
    let cleaned = raw.trim().replace(',', ".");
    let mut parts = cleaned.split(':');
    let first = parts.next()?;
    let second = parts.next();
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let value = match (second, third) {
        (Some(minutes), Some(rest)) => {
            let hours: f64 = first.parse().ok()?;
            let minutes: f64 = minutes.parse().ok()?;
            let seconds: f64 = rest.parse().ok()?;
            hours * 3600.0 + minutes * 60.0 + seconds
        }
        (Some(rest), None) => {
            let minutes: f64 = first.parse().ok()?;
            let seconds: f64 = rest.parse().ok()?;
            minutes * 60.0 + seconds
        }
        (None, _) => first.parse().ok()?,
    };
    value.is_finite().then_some(value)
}

/// 解析 LRC 原文为分段；元数据标签忽略，同一行的多个时间标签展开为多条。
///
/// LRC 只有起点没有终点：每条的 `end` 取下一条起点，末条取音频时长
/// （`duration_secs` 可用且晚于起点时），否则取起点后 5 秒。解析结果同样经
/// `normalize_segments` 校验与排序，确保 SRT / LRC / 桌面歌词三处时间轴一致。
pub fn parse_lrc(text: &str, duration_secs: Option<f64>) -> Result<Vec<SubtitleSegment>> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut timed: Vec<(f64, String)> = Vec::new();
    for (line_index, raw) in normalized.lines().enumerate() {
        let mut rest = raw.trim();
        if rest.is_empty() {
            continue;
        }
        let mut tags = 0usize;
        let mut times = Vec::new();
        // 只解析行首连续的时间标签；出现时间标签之后的 `[...]`（如 `[笑]`）属于正文。
        while rest.starts_with('[') {
            let Some(close) = rest.find(']') else { break };
            let tag = &rest[1..close];
            if let Some(seconds) = parse_lrc_time(tag) {
                times.push(seconds);
                rest = rest[close + 1..].trim_start();
                tags += 1;
                continue;
            }
            if times.is_empty() && is_lrc_metadata_tag(tag) {
                rest = rest[close + 1..].trim_start();
                tags += 1;
                continue;
            }
            if times.is_empty() {
                // 形如 `[00:01,500]` 的手误标签若被当作元数据跳过，会连带丢掉整行歌词。
                bail!("LRC 第 {} 行的标签无法识别：{tag}", line_index + 1);
            }
            break;
        }
        if times.is_empty() {
            // 只有「纯元数据行」（[ar:] / [ti:] / [offset:] 等）可以忽略；
            // 元数据之后还有正文却没有时间标签时按解析失败处理，避免静默丢内容。
            if tags > 0 && rest.is_empty() {
                continue;
            }
            bail!("LRC 第 {} 行缺少时间标签：[mm:ss.cc]", line_index + 1);
        }
        for seconds in times {
            timed.push((seconds, rest.trim().to_owned()));
        }
    }
    if timed.is_empty() {
        bail!("LRC 文本中没有可识别的歌词行");
    }
    timed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
    let mut segments = Vec::with_capacity(timed.len());
    for (index, (start, text)) in timed.iter().enumerate() {
        let next = timed.get(index + 1).map(|(start, _)| *start);
        let mut end = next
            .or_else(|| duration_secs.filter(|duration| *duration > *start))
            .unwrap_or(start + 5.0);
        if end <= *start {
            end = start + 0.1;
        }
        segments.push(SubtitleSegment {
            start: *start,
            end,
            text: text.clone(),
        });
    }
    normalize_segments(segments)
}

/// LRC 元数据标签键（不区分大小写）：这些标签的值不是时间，解析失败时按元数据忽略。
///
/// 只有白名单键会被当作元数据；其它无法解析为时间的标签一律报错，避免 `[00:01,500]`
/// 这类手误让整行歌词被静默丢弃。`[offset:]` 仅作为元数据忽略，不参与时间轴调整。
fn is_lrc_metadata_tag(tag: &str) -> bool {
    let key = tag
        .split(':')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        key.as_str(),
        "ar" | "al"
            | "ti"
            | "au"
            | "by"
            | "offset"
            | "re"
            | "ve"
            | "length"
            | "tool"
            | "kana"
            | "language"
    )
}

/// 支持 `[mm:ss.cc]` / `[mm:ss:cc]` / `[mm:ss.xxx]`（百分秒或毫秒）。
fn parse_lrc_time(tag: &str) -> Option<f64> {
    let parts: Vec<&str> = tag.split(':').collect();
    let minutes: f64;
    let seconds: f64;
    match parts.as_slice() {
        [m, s] => {
            minutes = m.parse().ok()?;
            seconds = s.parse().ok()?;
        }
        [m, s, frac] => {
            minutes = m.parse().ok()?;
            let whole: f64 = s.parse().ok()?;
            let frac_value: f64 = frac.parse().ok()?;
            // 三位小数按毫秒，两位按百分秒。
            let divisor = if frac.len() == 3 { 1000.0 } else { 100.0 };
            seconds = whole + frac_value / divisor;
        }
        _ => return None,
    }
    let value = minutes * 60.0 + seconds;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<SubtitleSegment> {
        vec![
            SubtitleSegment {
                start: 0.0,
                end: 10.0,
                text: "第一句歌词".to_owned(),
            },
            SubtitleSegment {
                start: 10.0,
                end: 20.5,
                text: "第二句歌词".to_owned(),
            },
        ]
    }

    #[test]
    fn srt_has_index_timecodes_and_text() {
        let srt = format_srt(&sample());
        assert_eq!(
            srt,
            concat!(
                "1\n00:00:00,000 --> 00:00:10,000\n第一句歌词\n\n",
                "2\n00:00:10,000 --> 00:00:20,500\n第二句歌词\n\n",
            )
        );
    }

    #[test]
    fn lrc_has_title_and_centisecond_timecodes() {
        let lrc = format_lrc("测试曲目", &sample());
        assert!(lrc.starts_with("[ar:]\n[ti:测试曲目]\n"));
        assert!(lrc.contains("[00:00.00]第一句歌词\n"));
        assert!(lrc.contains("[00:10.00]第二句歌词\n"));
    }

    #[test]
    fn time_helpers_round_and_clamp() {
        assert_eq!(srt_time(0.0), "00:00:00,000");
        assert_eq!(srt_time(3661.234), "01:01:01,234");
        assert_eq!(srt_time(-5.0), "00:00:00,000");
        assert_eq!(lrc_time(61.25), "01:01.25");
    }

    #[test]
    fn migration_rewrites_key_title_and_texts() {
        let stored = StoredSubtitle {
            key: "ambient/old_name".to_owned(),
            title: "旧标题".to_owned(),
            segments: sample(),
            created_at_ms: 42,
            updated_at_ms: None,
            file_name: Some("old_name.mp3".to_owned()),
            duration_ms: Some(1_000),
            srt: String::new(),
            lrc: String::new(),
            separation_model_name: None,
            accompaniment_model_name: None,
            vocals_path: None,
            accompaniment_path: None,
            accompaniment_kind: None,
            srt_path: None,
            lrc_path: None,
        };
        let song = Song {
            key: "epic/new_name".to_owned(),
            title: "新标题".to_owned(),
            file_name: "new_name.mp3".to_owned(),
            category: "epic".to_owned(),
            path: "D:/music/epic/new_name.mp3".to_owned(),
            artist: None,
            album: None,
            genre: None,
            duration_ms: Some(62_000),
        };
        let next = migrated_stored(&stored, &song.key, Some(&song));
        assert_eq!(next.key, "epic/new_name");
        assert_eq!(next.title, "新标题");
        assert_eq!(next.file_name.as_deref(), Some("new_name.mp3"));
        assert_eq!(next.duration_ms, Some(62_000));
        // 分段与生成时间保持不变，预览文本按新标题重建。
        assert_eq!(next.segments, stored.segments);
        assert_eq!(next.created_at_ms, 42);
        assert!(next.lrc.starts_with("[ar:]\n[ti:新标题]\n"));
        assert_eq!(next.srt, format_srt(&stored.segments));
    }

    #[test]
    fn migration_remaps_separation_paths_only_inside_old_dir() {
        let from = "ambient/old_name";
        let old_dir = PathBuf::from("C:/config/separations").join(subtitle_stem(from));
        let new_dir = PathBuf::from("C:/config/separations").join(subtitle_stem("epic/new_name"));

        let mut vocals = Some(old_dir.join("vocals.wav").to_string_lossy().into_owned());
        remap_separation_path(&mut vocals, from, &new_dir);
        assert_eq!(
            vocals.as_deref(),
            Some(new_dir.join("vocals.wav").to_string_lossy().as_ref())
        );

        // 不在旧分离目录下的自定义路径保持原样。
        let external = Some("D:/deliveries/custom.srt".to_owned());
        let mut untouched = external.clone();
        remap_separation_path(&mut untouched, from, &new_dir);
        assert_eq!(untouched, external);

        let mut empty: Option<String> = None;
        remap_separation_path(&mut empty, from, &new_dir);
        assert!(empty.is_none());
    }

    #[test]
    fn subtitle_stem_is_stable_and_distinct() {
        assert_eq!(subtitle_stem("pop/song.mp3"), subtitle_stem("pop/song.mp3"));
        assert_ne!(
            subtitle_stem("pop/song.mp3"),
            subtitle_stem("pop/song2.mp3")
        );
        assert!(!subtitle_stem("古典/曲 目.flac").contains('/'));
    }

    #[test]
    fn normalize_segments_validates_and_sorts() {
        let normalized = normalize_segments(vec![
            SubtitleSegment {
                start: 10.0,
                end: 12.0,
                text: "  第二句 \n 换行 ".to_owned(),
            },
            SubtitleSegment {
                start: 0.0,
                end: 5.0,
                text: "第一句".to_owned(),
            },
        ])
        .unwrap();
        assert_eq!(normalized[0].start, 0.0);
        assert_eq!(normalized[0].text, "第一句");
        assert!(!normalized[1].text.contains('\n'));
        assert!(normalized[1].text.starts_with("第二句"));

        assert!(normalize_segments(Vec::new()).is_err());
        assert!(
            normalize_segments(vec![SubtitleSegment {
                start: 5.0,
                end: 5.0,
                text: "零长度".to_owned(),
            }])
            .is_err()
        );
        assert!(
            normalize_segments(vec![SubtitleSegment {
                start: -1.0,
                end: 2.0,
                text: "负数".to_owned(),
            }])
            .is_err()
        );
        assert!(
            normalize_segments(vec![SubtitleSegment {
                start: f64::NAN,
                end: 2.0,
                text: "非数字".to_owned(),
            }])
            .is_err()
        );
    }

    #[test]
    fn parse_srt_accepts_missing_indexes_and_multiline_text() {
        let srt =
            "1\n00:00:01,500 --> 00:00:03.250\n第一句\n第二行\n\n00:04.0 --> 00:06.0\n第三句\n";
        let segments = parse_srt(srt).unwrap();
        assert_eq!(segments.len(), 2);
        assert!((segments[0].start - 1.5).abs() < 1e-9);
        assert!((segments[0].end - 3.25).abs() < 1e-9);
        assert_eq!(segments[0].text, "第一句 第二行");
        assert_eq!(segments[1].start, 4.0);
        assert_eq!(segments[1].text, "第三句");
    }

    #[test]
    fn parse_srt_roundtrips_format_srt() {
        let segments = sample();
        let parsed = parse_srt(&format_srt(&segments)).unwrap();
        assert_eq!(parsed.len(), segments.len());
        assert_eq!(parsed[0].text, "第一句歌词");
        assert_eq!(parsed[1].text, "第二句歌词");
        assert!((parsed[1].end - 20.5).abs() < 1e-9);
    }

    #[test]
    fn parse_srt_rejects_malformed_input() {
        assert!(parse_srt("这不是字幕").is_err());
        assert!(parse_srt("1\n00:00:05,000 --> 00:00:02,000\n结束早于开始\n").is_err());
        assert!(parse_srt("").is_err());
    }

    #[test]
    fn parse_lrc_expands_tags_and_derives_ends() {
        let lrc = "[ar:]\n[ti:测试]\n[00:01.00][00:03.50]同一句\n[00:08.25]末句\n";
        let segments = parse_lrc(lrc, Some(12.0)).unwrap();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].start, 1.0);
        assert_eq!(segments[0].end, 3.5);
        assert_eq!(segments[0].text, "同一句");
        assert_eq!(segments[2].text, "末句");
        assert!((segments[2].end - 12.0).abs() < 1e-9);
    }

    #[test]
    fn parse_lrc_falls_back_when_duration_unknown() {
        let segments = parse_lrc("[00:02.00]唯一一句\n", None).unwrap();
        assert_eq!(segments.len(), 1);
        assert!((segments[0].end - 7.0).abs() < 1e-9);
    }

    #[test]
    fn parse_lrc_rejects_lines_without_timestamp() {
        assert!(parse_lrc("没有时间标签的一行", None).is_err());
        assert!(parse_lrc("[ar:]\n[ti:只有元数据]\n", None).is_err());
    }

    #[test]
    fn parse_lrc_rejects_malformed_tags_instead_of_dropping_lines() {
        // 常见手误：沿用 SRT 的逗号毫秒分隔，不能被当作元数据跳过。
        assert!(parse_lrc("[00:01,500]手误\n", None).is_err());
        // 非白名单标签同样应报错，避免静默丢行。
        assert!(parse_lrc("[Verse 1]歌词\n", None).is_err());
        assert!(parse_lrc("[ab:12]歌词\n", None).is_err());
        // 元数据之后仍有正文却缺时间标签：报错，不能按纯元数据行跳过。
        assert!(parse_lrc("[ar:歌手]没有时间的正文\n", None).is_err());
        // 白名单元数据标签仍被忽略，不影响时间轴。
        let segments = parse_lrc("[ar:歌手][ti:标题]\n[00:03.00]正文\n", None).unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start, 3.0);
        assert_eq!(segments[0].text, "正文");
        // 时间标签之后的方括号属于歌词正文，应原样保留而不是被当作标签吞掉。
        let segments = parse_lrc("[00:05.00][笑]歌词\n", None).unwrap();
        assert_eq!(segments[0].text, "[笑]歌词");
    }

    #[test]
    fn parse_srt_rejects_broken_timeline_after_first_cue() {
        let srt =
            "1\n00:00:01,000 --> 00:00:02,000\n第一句\n00:00:03,XXX --> 00:00:04,000\n第二句\n";
        let message = parse_srt(srt).unwrap_err().to_string();
        assert!(message.contains("时间轴无法解析"), "{message}");
        assert!(message.contains("第 4 行"), "{message}");
    }

    #[test]
    fn normalize_segments_rejects_overlong_text() {
        let text = "字".repeat(MAX_SEGMENT_TEXT_CHARS + 1);
        let message = normalize_segments(vec![SubtitleSegment {
            start: 0.0,
            end: 1.0,
            text,
        }])
        .unwrap_err()
        .to_string();
        assert!(message.contains("文本过长"), "{message}");
    }
}
