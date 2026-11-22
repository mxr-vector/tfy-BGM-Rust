//! 「字幕生成」：音乐库曲目 → WAV 归一化 → 分片 ASR → SRT / LRC。
//!
//! 数据流：
//! 1. 从曲库快照取 `Song.path`，按扩展名校验（wav / mp3 / flac / m4a / ogg）；
//! 2. 非 WAV 用 ffmpeg 转为 16 kHz 单声道 PCM WAV（sherpa-onnx 的 `Wave::read`
//!    只接受单声道 WAV），WAV 也统一重采样以保证可识别；ffmpeg 缺失且源已是 WAV 时
//!    降级为直接使用源文件，采样率差异由识别层的兜底重采样消除；
//! 3. `ModelRuntime::transcribe_segments` 先把 PCM 统一到 16 kHz，再按固定时间窗
//!    切片逐段识别，得到带时间轴的
//!    `SubtitleSegment[]`，每完成一片推送一次进度；
//! 4. 结果格式化为 SRT / LRC，同时持久化到应用配置目录的 `subtitles/` 下，
//!    供「桌面歌词」按曲目 key 读取。
//!
//! 字幕生成只依赖 `AppHandle`、`ModelRuntime` 与 `Song`，不持有 `State`，便于放进
//! `spawn_blocking` 而不阻塞异步运行时。

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
    /// 是否发生了「非 WAV → WAV」的格式转换。
    pub converted: bool,
    /// ffmpeg 缺失时直接使用源 WAV 的降级说明（采样率已由识别层兜底重采样）。
    pub note: Option<String>,
    pub duration: f64,
    pub segments: Vec<SubtitleSegment>,
    pub srt: String,
    pub lrc: String,
    pub saved_path: String,
}

/// 持久化到磁盘的字幕（桌面歌词与导出共用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSubtitle {
    pub key: String,
    pub title: String,
    pub segments: Vec<SubtitleSegment>,
    pub created_at_ms: u64,
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
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleExportResult {
    pub path: String,
    pub bytes: u64,
    pub format: String,
}

struct NormalizedAudio {
    path: PathBuf,
    converted: bool,
    /// 是否为 ffmpeg 生成的临时文件；识别结束后需要删除，源文件绝不能被清理。
    owned: bool,
    note: Option<String>,
}

fn emit_progress(
    app: &AppHandle,
    stage: SubtitleStage,
    message: impl Into<String>,
    current: usize,
    total: usize,
    error: Option<String>,
) {
    let _ = app.emit(
        SUBTITLE_PROGRESS_EVENT,
        SubtitleProgress {
            stage,
            message: message.into(),
            current,
            total,
            error,
        },
    );
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
fn subtitle_stem(key: &str) -> String {
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
    Ok(removed)
}

fn save_stored(
    app: &AppHandle,
    key: &str,
    title: &str,
    file_name: Option<&str>,
    duration_ms: Option<u32>,
    segments: &[SubtitleSegment],
) -> Result<PathBuf> {
    let path = stored_path(app, key)?;
    let stored = StoredSubtitle {
        key: key.to_owned(),
        title: title.to_owned(),
        segments: segments.to_vec(),
        created_at_ms: now_ms(),
        file_name: file_name.map(str::to_owned),
        duration_ms,
        srt: format_srt(segments),
        lrc: format_lrc(title, segments),
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
        if let Err(error) = migrate_one(app, from, to, song, &old_path) {
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
) -> Result<()> {
    let text = fs::read_to_string(old_path)
        .with_context(|| format!("无法读取字幕文件：{}", old_path.display()))?;
    let stored: StoredSubtitle = serde_json::from_str(&text)
        .with_context(|| format!("字幕文件损坏：{}", old_path.display()))?;
    let migrated = migrated_stored(&stored, to, song);
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

/// 把曲库音频归一化为 16 kHz 单声道 PCM WAV。
///
/// - 非 WAV：必须经 ffmpeg 转换，缺失时给出明确错误；
/// - WAV：有 ffmpeg 时同样重采样（保证 16 kHz 单声道），缺失时降级为直接使用源文件，
///   由 `ModelRuntime` 在识别前兜底重采样到 16 kHz。
fn normalize_to_wav(
    app: &AppHandle,
    source: &Path,
    key: &str,
    ext: &str,
) -> Result<NormalizedAudio> {
    let is_wav = ext.eq_ignore_ascii_case("wav");
    let Some(ffmpeg) = ffmpeg_path() else {
        if is_wav {
            return Ok(NormalizedAudio {
                path: source.to_path_buf(),
                converted: false,
                owned: false,
                note: Some(
                    "未检测到 ffmpeg，直接使用源 WAV（识别前会自动重采样至 16 kHz；源文件需为单声道）"
                        .to_owned(),
                ),
            });
        }
        bail!(
            "未检测到 ffmpeg，无法把 .{ext} 转换为 WAV。请安装 ffmpeg 或设置 TFY_FFMPEG 环境变量后重试"
        );
    };

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
    Ok(NormalizedAudio {
        path: target,
        converted: !is_wav,
        owned: true,
        note: None,
    })
}

/// 生成字幕主流程；进度与错误都通过 `subtitle-progress` 事件推送，命令返回值用于结果展示。
pub fn generate(app: &AppHandle, runtime: &ModelRuntime, song: &Song) -> Result<SubtitleResult> {
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
    if runtime.loaded().is_none() {
        bail!("尚未加载 ASR 模型，请先在「设置 → 模型」中完成模型加载");
    }

    emit_progress(
        app,
        SubtitleStage::Normalizing,
        if ext.eq_ignore_ascii_case("wav") {
            "正在归一化 WAV 音频…"
        } else {
            "正在将音频转换为 WAV…"
        },
        0,
        0,
        None,
    );
    let normalized = normalize_to_wav(app, &source, &song.key, ext)?;

    emit_progress(
        app,
        SubtitleStage::Transcribing,
        "正在识别音频…",
        0,
        0,
        None,
    );
    let progress_app = app.clone();
    let transcribed = runtime.transcribe_segments(
        &normalized.path,
        crate::models::SEGMENT_TARGET_SECS,
        &mut |current, total| {
            emit_progress(
                &progress_app,
                SubtitleStage::Transcribing,
                format!("正在识别第 {current}/{total} 段…"),
                current,
                total,
                None,
            );
        },
    );
    // 归一化 WAV 只服务于本次识别；临时文件用完即删，避免在配置目录长期累积。
    if normalized.owned {
        let _ = fs::remove_file(&normalized.path);
    }
    let segments = transcribed?;

    let srt = format_srt(&segments);
    let lrc = format_lrc(&song.title, &segments);
    let saved_path = save_stored(
        app,
        &song.key,
        &song.title,
        Some(&song.file_name),
        song.duration_ms,
        &segments,
    )?;
    let duration = segments.last().map(|segment| segment.end).unwrap_or(0.0);

    emit_progress(
        app,
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
        converted: normalized.converted,
        note: normalized.note,
        duration,
        segments,
        srt,
        lrc,
        saved_path: saved_path.to_string_lossy().into_owned(),
    })
}

/// 生成失败时把原因推给前端，便于进度区直接展示。
pub fn emit_failure(app: &AppHandle, error: &anyhow::Error) {
    emit_progress(
        app,
        SubtitleStage::Error,
        "字幕生成失败",
        0,
        0,
        Some(format!("{error:#}")),
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
            "1\n00:00:00,000 --> 00:00:10,000\n第一句歌词\n\n\
             2\n00:00:10,000 --> 00:00:20,500\n第二句歌词\n\n"
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
            file_name: Some("old_name.mp3".to_owned()),
            duration_ms: Some(1_000),
            srt: String::new(),
            lrc: String::new(),
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
    fn subtitle_stem_is_stable_and_distinct() {
        assert_eq!(subtitle_stem("pop/song.mp3"), subtitle_stem("pop/song.mp3"));
        assert_ne!(
            subtitle_stem("pop/song.mp3"),
            subtitle_stem("pop/song2.mp3")
        );
        assert!(!subtitle_stem("古典/曲 目.flac").contains('/'));
    }
}
