//! 字幕记录的读写、key 迁移与归一化音频路径。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use bgm_core::Song;
use bgm_core::export::safe_file_name;
use tauri::{AppHandle, Manager};

use super::format::{format_lrc, format_srt, normalize_segments, write_subtitle};
use super::{SUBTITLES_DIR, StoredSubtitle, SubtitleProgress, SubtitleSegment, SubtitleStage};

pub(super) fn emit_progress(
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

pub(super) fn subtitle_dir(app: &AppHandle) -> Result<PathBuf> {
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

pub(super) fn stored_path(app: &AppHandle, key: &str) -> Result<PathBuf> {
    Ok(subtitle_dir(app)?.join(format!("{}.json", subtitle_stem(key))))
}

pub(super) fn wav_path_for(app: &AppHandle, key: &str) -> Result<PathBuf> {
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
pub(super) struct Delivery<'a> {
    pub(crate) separation: &'a crate::separation::SeparationInfo,
    pub(crate) srt_path: &'a Path,
    pub(crate) lrc_path: &'a Path,
}

pub(super) fn save_stored(
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

pub(super) fn now_ms() -> u64 {
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

pub(super) fn migrate_one(
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
pub(crate) fn remap_separation_path(path: &mut Option<String>, from: &str, new_dir: &Path) {
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
pub(super) fn overwrite(path: &Path, json: &str) -> Result<()> {
    let part = path.with_extension("json.part");
    fs::write(&part, json).with_context(|| format!("无法写入字幕文件：{}", part.display()))?;
    fs::rename(&part, path).with_context(|| format!("无法保存字幕文件：{}", path.display()))?;
    Ok(())
}

/// 迁移后的记录：key 跟随新曲目，标题 / 文件名 / 时长以最新曲库信息为准，
/// 预览文本按当前格式化规则重建；分段与生成时间保持不变。
pub(crate) fn migrated_stored(
    stored: &StoredSubtitle,
    to: &str,
    song: Option<&Song>,
) -> StoredSubtitle {
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

/// 把分离后的人声 WAV 归一化为 16 kHz 单声道 PCM（sherpa-onnx 要求）。
///
/// 分离阶段已强制要求 ffmpeg，这里不再保留「直接使用源文件」的降级路径；
/// 返回的临时文件由调用方在识别结束后删除。
pub(super) fn normalize_to_wav(app: &AppHandle, source: &Path, key: &str) -> Result<PathBuf> {
    let target = wav_path_for(app, key)?;
    if let Err(error) = crate::speech_audio::to_asr_wav(source, &target) {
        let _ = fs::remove_file(&target);
        return Err(error).context("无法归一化人声轨");
    }
    Ok(target)
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
