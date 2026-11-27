//! 字幕生成主流程：人声分离 → 归一化 → 分片 ASR → SRT / LRC。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use bgm_core::Song;
use bgm_core::library::{AUDIO_EXTENSIONS, audio_extension};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use super::format::{format_lrc, format_srt, write_subtitle};
use super::store::{Delivery, emit_progress, normalize_to_wav, save_stored};
use super::{SUBTITLE_PROGRESS_EVENT, SubtitleProgress, SubtitleResult, SubtitleStage};
use crate::models::ModelRuntime;
use crate::tts::check_cancelled;

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
    token: &CancellationToken,
) -> Result<SubtitleResult> {
    generate_with_progress(
        app,
        runtime,
        song,
        separation_model,
        separation_target,
        models_root,
        token,
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
    token: &CancellationToken,
    mut on_progress: F,
) -> Result<SubtitleResult>
where
    F: FnMut(&SubtitleProgress),
{
    check_cancelled(token)?;
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

    check_cancelled(token)?;
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
        token,
        |status| crate::models::emit_load_status(app, status),
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
    check_cancelled(token)?;

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
