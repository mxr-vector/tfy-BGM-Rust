//! 暴露给前端的命令。所有命令只做参数校验、调用核心逻辑并序列化结果，
//! 业务规则集中在 `bgm-core`，便于脱离 WebView 测试。

use std::path::{Path, PathBuf};

use bgm_core::BatchReport;
use bgm_core::LibrarySnapshot;
use bgm_core::Prefs;
use bgm_core::export::{export_audio, safe_file_name};
use bgm_core::library::{AUDIO_EXTENSIONS, DEFAULT_AUDIO_EXTENSION, audio_extension};
use bgm_core::loudness::{EXPORT_PRESETS, ExportPreset, NORMALIZED_EXTENSION, ffmpeg_path};
use bgm_core::paths::TRASH_DIR_NAME;
use bgm_core::{LoopBook, Schedule};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::api::{ApiBridge, ApiEndpoint, ApiStatus, CommandFailure};
use crate::desktop::{self, DesktopState};
use crate::fonts;
use crate::lyrics::{self, LyricsBootstrap, LyricsControl, LyricsSnapshot, LyricsState};
use crate::models::{self, LoadPhase, ModelLoadStatus, ModelRuntime, ModelsInfo};
use crate::state::{AppState, Bootstrap, CategoryOutcome, ConfigReload, LibraryMutation};
use crate::subtitles;
use crate::wallpapers::{self, WallpaperAsset, WallpaperPick};

type CommandResult<T> = Result<T, String>;

fn describe(error: anyhow::Error) -> String {
    format!("{error:#}")
}

/// 导出目标扩展名：归一化（需要 ffmpeg）输出 mp3，否则原样复制源格式。
fn export_extension(state: &AppState, file_name: &str) -> &'static str {
    let settings = state.export_settings();
    if settings.normalize && ffmpeg_path().is_some() {
        NORMALIZED_EXTENSION
    } else {
        audio_extension(file_name).unwrap_or(DEFAULT_AUDIO_EXTENSION)
    }
}

/// 在文件管理器中定位配置文件；定位失败时退化为打开其所在目录。
fn reveal_in_dir(app: &AppHandle, path: &Path, label: &str) -> CommandResult<()> {
    if app.opener().reveal_item_in_dir(path).is_err() {
        let parent = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        app.opener()
            .open_path(parent.to_string_lossy().into_owned(), None::<&str>)
            .map_err(|error| format!("打开{label}目录失败：{error}"))?;
    }
    Ok(())
}

#[tauri::command]
pub fn bootstrap(state: State<'_, AppState>) -> Bootstrap {
    state.bootstrap()
}

/// 开放接口状态：监听端口、token 与启动错误，供「播放输出」面板展示。
#[tauri::command]
pub fn api_info(bridge: State<'_, ApiBridge>) -> ApiStatus {
    bridge.status()
}

/// 「第三方接入」展示的开放接口清单。
#[tauri::command]
pub fn api_endpoints() -> Vec<ApiEndpoint> {
    crate::api::endpoints()
}

/// 保存 token 与跨域策略；立即作用于正在运行的接口服务，并写回 `config.json`。
#[tauri::command]
pub fn update_api_config(
    state: State<'_, AppState>,
    bridge: State<'_, ApiBridge>,
    token: String,
    cors: bgm_core::CorsConfig,
) -> CommandResult<ApiStatus> {
    let api = state.update_api_config(token, cors).map_err(describe)?;
    bridge.apply_config(&api.token, &api.cors);
    Ok(bridge.status())
}

/// 「常规」中立即生效的桌面集成项：开机自动启动与最小化托盘。
#[tauri::command]
pub fn apply_desktop_settings(
    app: AppHandle,
    desktop_state: State<'_, DesktopState>,
    autostart: bool,
    minimize_to_tray: bool,
) -> CommandResult<()> {
    desktop::apply(&app, &desktop_state, autostart, minimize_to_tray).map_err(describe)
}

/// 关闭提示中选择「最小化托盘」：确保托盘存在后隐藏窗口。
#[tauri::command]
pub fn hide_to_tray(app: AppHandle) -> CommandResult<()> {
    desktop::hide_to_tray(&app).map_err(describe)
}

/// 关闭提示中选择「关闭程序」：即使存在托盘图标也立即退出。
#[tauri::command]
pub fn quit_app(app: AppHandle) {
    // 退出会关闭所有窗口；先置标记，避免歌词窗口的关闭事件把「开启」偏好清掉。
    if let Some(desktop) = app.try_state::<DesktopState>() {
        desktop.begin_quit();
    }
    app.exit(0);
}

/// 前端把当前播放状态推给接口层；同时广播给 SSE 订阅者。
#[tauri::command]
pub fn api_publish_state(bridge: State<'_, ApiBridge>, snapshot: Value) {
    bridge.publish(snapshot);
}

/// 前端回传 HTTP 命令的执行结果；`code` 是前端给出的 HTTP 语义状态（400/404/409…）。
#[tauri::command]
pub fn api_reply(
    bridge: State<'_, ApiBridge>,
    id: u64,
    ok: bool,
    data: Option<Value>,
    error: Option<String>,
    code: Option<u16>,
) {
    let result = if ok {
        Ok(data.unwrap_or(Value::Null))
    } else {
        Err(CommandFailure::from_frontend(
            code,
            error.unwrap_or_else(|| "命令执行失败".to_owned()),
        ))
    };
    bridge.reply(id, result);
}

#[tauri::command]
pub fn reload_config(app: AppHandle, state: State<'_, AppState>) -> ConfigReload {
    state.reload_config(&app)
}

/// 系统已安装字体清单。字体选择改用系统字体后，「选项存在」即代表本机已安装。
#[tauri::command]
pub async fn system_fonts() -> CommandResult<Vec<fonts::SystemFont>> {
    // 需要逐个读取字体文件的 name 表，放到阻塞线程池，避免占用异步运行时。
    tauri::async_runtime::spawn_blocking(fonts::list)
        .await
        .map_err(|error| format!("字体扫描失败：{error}"))
}

/// 内置壁纸清单；资源缺失的条目会被后端过滤，前端据此隐藏对应选项。
#[tauri::command]
pub fn builtin_wallpapers(app: AppHandle) -> Vec<WallpaperAsset> {
    wallpapers::builtin_catalog(&app)
}

/// 选择本地自定义壁纸，只接受 jpg / png / webp / mp4。
#[tauri::command]
pub async fn choose_wallpaper_file(app: AppHandle) -> CommandResult<Option<WallpaperPick>> {
    let picked = app
        .dialog()
        .file()
        .set_title("选择界面壁纸")
        .add_filter(
            "壁纸（JPG / PNG / WebP / MP4）",
            &["jpg", "jpeg", "png", "webp", "mp4"],
        )
        .blocking_pick_file();
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked
        .into_path()
        .map_err(|error| format!("所选文件无效：{error}"))?;
    if !path.is_file() {
        return Err("所选壁纸文件不存在".to_owned());
    }
    if !wallpapers::is_supported(&path) {
        return Err("仅支持 jpg / png / webp / mp4 格式的壁纸".to_owned());
    }
    wallpapers::allow_wallpaper_file(&app, &path).map_err(describe)?;
    Ok(Some(WallpaperPick {
        label: wallpapers::file_label(&path),
        kind: wallpapers::kind_of(&path).to_owned(),
        path: path.to_string_lossy().into_owned(),
    }))
}

#[tauri::command]
pub fn open_config_file(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    reveal_in_dir(&app, &state.config_path(), "应用配置")
}

#[tauri::command]
pub fn save_prefs(state: State<'_, AppState>, prefs: Prefs) -> CommandResult<Prefs> {
    state.save_prefs(prefs).map_err(describe)
}

#[tauri::command]
pub fn save_schedule(state: State<'_, AppState>, schedule: Schedule) -> CommandResult<Schedule> {
    state.save_schedule(schedule).map_err(describe)
}

#[tauri::command]
pub fn save_loop_point(
    state: State<'_, AppState>,
    key: String,
    start: f64,
    end: f64,
    enabled: bool,
) -> CommandResult<LoopBook> {
    state
        .save_loop_point(&key, start, end, enabled)
        .map_err(describe)
}

#[tauri::command]
pub fn remove_loop_point(state: State<'_, AppState>, key: String) -> CommandResult<LoopBook> {
    state.remove_loop_point(&key).map_err(describe)
}

#[tauri::command]
pub async fn rescan_library(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<LibrarySnapshot> {
    state.rescan(&app).map_err(describe)
}

#[tauri::command]
pub async fn choose_library_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<LibrarySnapshot>> {
    let picked = app
        .dialog()
        .file()
        .set_title("选择 BGM 曲库文件夹")
        .blocking_pick_folder();
    let dir = match picked {
        Some(file_path) => Some(
            file_path
                .into_path()
                .map_err(|error| format!("所选目录无效：{error}"))?,
        ),
        None => None,
    };
    state.choose_library_dir(&app, dir).map_err(describe)
}

#[tauri::command]
pub async fn reset_library_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<LibrarySnapshot> {
    state.reset_library_dir(&app).map_err(describe)
}

#[tauri::command]
pub fn open_library_dir(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    let root = state.root().to_string_lossy().into_owned();
    app.opener()
        .open_path(root, None::<&str>)
        .map_err(|error| format!("打开曲库目录失败：{error}"))
}

/// 「模型」分组：重新扫描模型目录，返回默认模型状态、模型列表与已加载模型。
/// 扫描可能遍历大目录，放在异步任务里执行，避免阻塞界面线程。
#[tauri::command]
pub async fn models_info(state: State<'_, AppState>) -> CommandResult<ModelsInfo> {
    Ok(state.models_info())
}

/// 选择自定义模型目录；取消时返回 `null`，保持现有目录不变。
#[tauri::command]
pub async fn choose_models_dir(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<ModelsInfo>> {
    let picked = app
        .dialog()
        .file()
        .set_title("选择 ASR 模型目录")
        .blocking_pick_folder();
    let dir = match picked {
        Some(file_path) => Some(
            file_path
                .into_path()
                .map_err(|error| format!("所选目录无效：{error}"))?,
        ),
        None => None,
    };
    state.choose_models_dir(dir).map_err(describe)
}

/// 恢复默认模型目录（系统「下载」目录下的 `bgm-models`）。
#[tauri::command]
pub async fn reset_models_dir(state: State<'_, AppState>) -> CommandResult<ModelsInfo> {
    state.reset_models_dir().map_err(describe)
}

#[tauri::command]
pub fn open_models_dir(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    let root = state.models_root();
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("无法创建模型目录 {}：{error}", root.display()))?;
    app.opener()
        .open_path(root.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|error| format!("打开模型目录失败：{error}"))
}

/// 下载默认 ASR 模型（固定 0.6B-int8 清单）；进度通过 `model-download-progress` 推送。
///
/// 下载成功后无需用户再点「加载」：本命令紧接着自动触发模型加载，加载状态通过
/// `model-load-status` 事件实时推送，并随返回的 `ModelsInfo` 一并带回。
#[tauri::command]
pub async fn download_asr_model(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<ModelsInfo> {
    let runtime = state.model_runtime();
    let root = state.models_root();
    let url = state.asr_download_url();
    let task_app = app.clone();
    let task_root = root.clone();
    let task_runtime = runtime.clone();
    tauri::async_runtime::spawn_blocking(move || {
        task_runtime.download(&task_app, &task_root, &url)
    })
    .await
    .map_err(|error| format!("下载任务异常结束：{error}"))?
    .map_err(describe)?;

    let dir = root.join(models::DEFAULT_MODEL_NAME);
    auto_load_after_download(&app, &runtime, &root, &dir).await;
    // 自动加载失败不覆盖「下载成功」的结果：失败原因已进入 loadStatus，
    // 由界面状态条展示并提供重试入口。
    // 只有下载的目录确实加载成功才记为启动恢复目标：并发的手动加载可能让
    // auto_load_after_download 提前返回，不能拿「别人的成功」当依据。
    if runtime.is_loaded_path(&dir) {
        state.remember_loaded_model(relative_model_id(&root, &dir));
    }
    Ok(state.models_info())
}

/// 模型相对模型根目录的 id（`/` 分隔），用于写入偏好供下次启动自动恢复。
fn relative_model_id(root: &Path, dir: &Path) -> Option<String> {
    dir.strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .filter(|id| !id.is_empty())
}

/// 启动后后台恢复上次加载成功的模型。
///
/// 模型只存在于进程内存，重新编译或重启应用后必须重新读入；这里按偏好里的相对
/// id 自动恢复，省去每次手动点「加载」。目录缺失或构件不全时静默跳过；加载失败
/// 仍会写入状态，界面可手动重试或释放（释放后不再自动恢复）。
pub fn spawn_restore_last_model(app: &AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Some(id) = state.last_asr_model() else {
        return;
    };
    let root = state.models_root();
    let dir = root.join(&id);
    if !dir.starts_with(&root) || !models::is_loadable(&dir) {
        return;
    }
    let runtime = state.model_runtime();
    // 手动加载 / 下载后的自动加载已经在跑时跳过恢复，避免覆盖它的状态与结果。
    if runtime.is_loading() {
        return;
    }
    let app = app.clone();

    tauri::async_runtime::spawn(async move {
        let display = dir.to_string_lossy().into_owned();
        emit_load_status(
            &app,
            &ModelLoadStatus {
                phase: LoadPhase::Loading,
                path: Some(display.clone()),
                error: None,
            },
        );
        let task_runtime = runtime.clone();
        let task_root = root.clone();
        let task_dir = dir.clone();
        let result =
            tauri::async_runtime::spawn_blocking(move || task_runtime.load(&task_root, &task_dir))
                .await;
        let status = match result {
            // `load` 内部已写入 success / failed（含完整错误链）。
            Ok(_) => runtime.load_status(),
            Err(error) => {
                let status = ModelLoadStatus {
                    phase: LoadPhase::Failed,
                    path: Some(display),
                    error: Some(format!("自动恢复模型失败：{error}")),
                };
                runtime.set_load_status(status.clone());
                status
            }
        };
        emit_load_status(&app, &status);
    });
}

/// 下载完成后的自动加载。已加载成功的同一模型直接跳过，避免重复加载 1GB 模型。
async fn auto_load_after_download(
    app: &AppHandle,
    runtime: &ModelRuntime,
    root: &Path,
    dir: &Path,
) {
    let display = dir.to_string_lossy().into_owned();
    if runtime.is_loaded_path(dir) {
        emit_load_status(
            app,
            &ModelLoadStatus {
                phase: LoadPhase::Success,
                path: Some(display),
                error: None,
            },
        );
        return;
    }

    // 已有（手动）加载任务在跑：不抢占，也不覆盖它的状态。
    if runtime.is_loading() {
        return;
    }

    emit_load_status(
        app,
        &ModelLoadStatus {
            phase: LoadPhase::Loading,
            path: Some(display.clone()),
            error: None,
        },
    );

    let task_runtime = (*runtime).clone();
    let task_root = root.to_path_buf();
    let task_dir = dir.to_path_buf();
    let result =
        tauri::async_runtime::spawn_blocking(move || task_runtime.load(&task_root, &task_dir))
            .await;

    let status = match result {
        // `load` 内部已写入 success / failed（含完整错误链）。
        Ok(_) => runtime.load_status(),
        // 阻塞线程 panic：load 来不及写入状态，这里补写失败。
        Err(error) => {
            let status = ModelLoadStatus {
                phase: LoadPhase::Failed,
                path: Some(display),
                error: Some(format!("自动加载任务异常结束：{error}")),
            };
            runtime.set_load_status(status.clone());
            status
        }
    };
    emit_load_status(app, &status);
}

/// 推送模型加载状态；自动加载、手动加载、释放与重试共用同一通道。
fn emit_load_status(app: &AppHandle, status: &ModelLoadStatus) {
    let _ = app.emit(models::LOAD_STATUS_EVENT, status);
}

/// 用 sherpa-onnx 加载指定模型目录；加载 1GB 左右的模型会占用数秒。
/// 全过程推送 `loading → success / failed`，失败原因随状态返回给界面。
#[tauri::command]
pub async fn load_asr_model(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> CommandResult<ModelsInfo> {
    let runtime = state.model_runtime();
    let root = state.models_root();
    let dir = PathBuf::from(path);
    let display = dir.to_string_lossy().into_owned();

    // 已有加载任务时直接拒绝，避免把正在进行的状态覆盖成 Failed。
    if runtime.is_loading() {
        return Err("已有模型加载任务正在进行".to_owned());
    }

    // 状态（loading → success / failed）由 `ModelRuntime::load` 原子地维护；
    // 命令层不再预先广播 Loading，避免与并发加载任务的状态互相覆盖。
    let model_id = relative_model_id(&root, &dir);
    let task_runtime = runtime.clone();
    let result =
        match tauri::async_runtime::spawn_blocking(move || task_runtime.load(&root, &dir)).await {
            Ok(result) => result,
            // 阻塞线程 panic：load 来不及写入状态，这里补写失败并广播。
            Err(error) => {
                let message = format!("加载任务异常结束：{error}");
                let status = ModelLoadStatus {
                    phase: LoadPhase::Failed,
                    path: Some(display),
                    error: Some(message.clone()),
                };
                runtime.set_load_status(status.clone());
                emit_load_status(&app, &status);
                return Err(message);
            }
        };
    // `load` 内部已写入 success / failed（含完整错误链），这里只补发事件。
    emit_load_status(&app, &runtime.load_status());
    result.map_err(describe)?;
    // 记住成功加载的模型，下次启动（含重新编译）自动恢复。
    state.remember_loaded_model(model_id);
    Ok(state.models_info())
}

/// 释放已加载的模型，归还内存；同步把加载状态复位，桌面歌词开关随之禁用。
#[tauri::command]
pub async fn unload_asr_model(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<ModelsInfo> {
    let runtime = state.model_runtime();
    // 加载任务结束后会把模型重新写回内存；此时释放只会被覆盖，直接拒绝。
    if runtime.is_loading() {
        return Err("模型正在加载中，请等待加载完成后再释放".to_owned());
    }
    runtime.unload();
    // 显式释放表示下次启动不再自动恢复，否则「释放模型」只在本次进程内有效。
    state.remember_loaded_model(None);
    emit_load_status(&app, &runtime.load_status());
    Ok(state.models_info())
}

/// 请求取消正在进行的模型下载；返回是否确实有任务被中断。
#[tauri::command]
pub fn cancel_asr_download(state: State<'_, AppState>) -> bool {
    state.model_runtime().cancel_download()
}

/// 选择 WAV 文件并用已加载的模型识别；任意采样率会在识别前自动重采样到 16 kHz，
/// 取消选择时返回 `null`。
#[tauri::command]
pub async fn transcribe_wav(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<Option<String>> {
    let Some(picked) = app
        .dialog()
        .file()
        .set_title("选择要识别的 WAV 音频")
        .add_filter("WAV 音频", &["wav"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    let wav = picked
        .into_path()
        .map_err(|error| format!("所选文件无效：{error}"))?;
    let runtime = state.model_runtime();
    let text = tauri::async_runtime::spawn_blocking(move || runtime.transcribe(&wav))
        .await
        .map_err(|error| format!("识别任务异常结束：{error}"))?
        .map_err(describe)?;
    Ok(Some(text))
}

/// 为音乐库曲目生成字幕：格式归一化 → 分片 ASR → SRT / LRC。
/// 过程进度通过 `subtitle-progress` 推送，失败也会推送带原因的 error 事件。
#[tauri::command]
pub async fn generate_subtitles(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> CommandResult<subtitles::SubtitleResult> {
    let song = state
        .song(&key)
        .ok_or_else(|| format!("曲目不存在：{key}"))?;
    let runtime = state.model_runtime();
    let task_app = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        subtitles::generate(&task_app, &runtime, &song)
    })
    .await
    .map_err(|error| format!("字幕生成任务异常结束：{error}"))?;
    if let Err(error) = &result {
        subtitles::emit_failure(&app, error);
    }
    result.map_err(describe)
}

/// 导出 SRT / LRC；默认文件名使用曲目名，内容取已生成的字幕结果。
#[tauri::command]
pub async fn export_subtitle(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    format: String,
) -> CommandResult<Option<subtitles::SubtitleExportResult>> {
    let ext = match format.as_str() {
        "srt" => "srt",
        "lrc" => "lrc",
        other => return Err(format!("不支持的字幕格式：{other}（仅支持 srt / lrc）")),
    };
    let stored = subtitles::load_stored(&app, &key)
        .ok_or_else(|| "该曲目尚未生成字幕，请先生成字幕".to_owned())?;
    let title = state
        .song(&key)
        .map(|song| song.title)
        .unwrap_or_else(|| stored.title.clone());
    // `load_stored` 已按统一规则格式化，预览与导出共用同一份文本。
    let content = if ext == "srt" { stored.srt } else { stored.lrc };

    let file_name = format!("{}.{ext}", safe_file_name(&title));
    let filter_label = format!("{} 字幕", ext.to_ascii_uppercase());
    let mut dialog = app
        .dialog()
        .file()
        .set_title("导出字幕")
        .set_file_name(file_name)
        .add_filter(&filter_label, &[ext]);
    if let Ok(download_dir) = app.path().download_dir() {
        dialog = dialog.set_directory(download_dir);
    }
    let Some(target) = dialog.blocking_save_file() else {
        return Ok(None);
    };
    let target = target
        .into_path()
        .map_err(|error| format!("保存路径无效：{error}"))?;
    let bytes = subtitles::write_subtitle(&target, &content).map_err(describe)?;
    Ok(Some(subtitles::SubtitleExportResult {
        path: target.to_string_lossy().into_owned(),
        bytes,
        format: ext.to_owned(),
    }))
}

/// 读取某曲目已生成的字幕；「桌面歌词」与字幕页据此显示歌词行。
#[tauri::command]
pub fn subtitle_for_track(app: AppHandle, key: String) -> Option<subtitles::StoredSubtitle> {
    subtitles::load_stored(&app, &key)
}

/// 列出所有已生成的字幕记录；字幕页启动 / 刷新时据此恢复记录列表。
#[tauri::command]
pub fn list_subtitles(app: AppHandle) -> Vec<subtitles::StoredSubtitle> {
    subtitles::list_stored(&app)
}

/// 删除某曲目的字幕记录（含 JSON 与残留临时文件）；返回是否确实删除了文件。
#[tauri::command]
pub fn delete_subtitle(app: AppHandle, key: String) -> CommandResult<bool> {
    subtitles::delete_stored(&app, &key).map_err(describe)
}

/// 保存「分段编辑」结果：校验 / 规范化分段后写回字幕文件，返回最新快照。
///
/// 保留原生成时间与曲目元信息，只更新分段、SRT / LRC 文本与 `updated_at_ms`。
#[tauri::command]
pub fn update_subtitle(
    app: AppHandle,
    key: String,
    segments: Vec<subtitles::SubtitleSegment>,
) -> CommandResult<subtitles::StoredSubtitle> {
    subtitles::update_stored(&app, &key, segments).map_err(describe)
}

/// 保存「原文编辑」结果：把 SRT / LRC 文本解析为分段后复用 `update_stored`。
///
/// LRC 缺少结束时间，末句用时长推导：优先字幕记录里的 `duration_ms`，其次曲库时长
/// （与前端校验取同一来源），都缺失时才回退到起点后 5 秒。
#[tauri::command]
pub fn update_subtitle_text(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    format: String,
    text: String,
) -> CommandResult<subtitles::StoredSubtitle> {
    let segments = match format.as_str() {
        "srt" => subtitles::parse_srt(&text),
        "lrc" => {
            let duration_ms = subtitles::load_stored(&app, &key)
                .and_then(|stored| stored.duration_ms)
                .or_else(|| state.song(&key).and_then(|song| song.duration_ms));
            let duration_secs = duration_ms.map(|ms| f64::from(ms) / 1000.0);
            subtitles::parse_lrc(&text, duration_secs)
        }
        other => return Err(format!("不支持的字幕格式：{other}（仅支持 srt / lrc）")),
    }
    .map_err(describe)?;
    subtitles::update_stored(&app, &key, segments).map_err(describe)
}

/// 开启 / 关闭桌面歌词窗口；返回生效后的开关状态。
///
/// 必须是异步命令：`WebviewWindowBuilder::build` 会向主线程事件循环派发创建消息并
/// 阻塞等待结果。若在同步命令（运行于主线程）里调用，会把主线程连同事件循环一起锁死，
/// 窗口停在 about:blank —— 透明置顶窗口只剩一片看不见的点击拦截层，所有 IPC 也会卡住。
#[tauri::command]
pub async fn set_desktop_lyrics_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> CommandResult<LyricsControl> {
    let task_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let lyrics = task_app.state::<LyricsState>();
        lyrics::set_enabled(&task_app, &lyrics, enabled)
    })
    .await
    .map_err(|error| format!("桌面歌词窗口任务异常结束：{error}"))?
    .map_err(describe)?;
    // 开关随偏好持久化，模型下次可用时（启动自动恢复）由前端恢复开启状态。
    state.set_desktop_lyrics_enabled(enabled);
    Ok(LyricsControl { enabled })
}

/// 主窗口把当前曲目 / 播放进度 / 歌词行推给后端，由后端广播给歌词窗口。
#[tauri::command]
pub fn sync_desktop_lyrics(
    app: AppHandle,
    lyrics: State<'_, LyricsState>,
    snapshot: LyricsSnapshot,
) -> CommandResult<()> {
    lyrics::sync_snapshot(&app, &lyrics, snapshot).map_err(describe)
}

/// 歌词窗口启动时拉取初始快照，避免错过创建窗口前的最后一次广播。
#[tauri::command]
pub fn desktop_lyrics_snapshot(lyrics: State<'_, LyricsState>) -> LyricsBootstrap {
    LyricsBootstrap {
        enabled: lyrics.enabled(),
        snapshot: lyrics.snapshot(),
    }
}

/// 设置桌面歌词锁定态：锁定后默认鼠标穿透，并把歌词热区交给后端轮询解锁悬停。
#[tauri::command]
pub fn set_desktop_lyrics_lock(
    app: AppHandle,
    lyrics: State<'_, LyricsState>,
    locked: bool,
    hot_zone: Option<lyrics::LyricsHotZone>,
) -> CommandResult<()> {
    lyrics::set_lock(&app, &lyrics, locked, hot_zone).map_err(describe)
}

/// 重新声明歌词窗口置顶；返回是否仍成功置顶。
///
/// 由歌词窗口定时巡检调用（全屏应用 / 虚拟桌面切换可能把 topmost 降级），
/// 也可供设置面板的「重新置顶」按钮手动触发。仅调整 Z 序，不影响焦点。
#[tauri::command]
pub fn keep_desktop_lyrics_on_top(app: AppHandle) -> bool {
    lyrics::enforce_top_most(&app)
}

/// 自定义桌面歌词窗口大小（逻辑像素）；Rust 侧负责范围钳制。
#[tauri::command]
pub fn resize_desktop_lyrics(app: AppHandle, width: f64, height: f64) -> CommandResult<()> {
    lyrics::resize(&app, width, height).map_err(describe)
}

/// 打开 / 关闭贴在歌词条上方的显示设置悬浮窗。
#[tauri::command]
pub async fn toggle_desktop_lyrics_settings(app: AppHandle) -> CommandResult<()> {
    let task_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || lyrics::toggle_settings(&task_app))
        .await
        .map_err(|error| format!("桌面歌词设置窗口任务异常结束：{error}"))?
        .map_err(describe)
}

/// 把桌面歌词窗口放到屏幕底部居中（默认位置）。
#[tauri::command]
pub fn place_desktop_lyrics(app: AppHandle, width: f64, height: f64) -> CommandResult<()> {
    lyrics::place_at_bottom(&app, width, height).map_err(describe)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub path: String,
    pub bytes: u64,
    /// 是否真正完成了响度归一化；`false` 且带 `warning` 表示已回退为复制原文件。
    pub normalized: bool,
    pub warning: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportBatchResult {
    pub dir: String,
    pub report: BatchReport,
}

/// 导出能力探测：ffmpeg 路径（未安装为 `null`）与场景预设清单。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportInfo {
    pub ffmpeg: Option<String>,
    pub presets: Vec<ExportPreset>,
}

#[tauri::command]
pub fn export_info() -> ExportInfo {
    ExportInfo {
        ffmpeg: ffmpeg_path().map(|path| path.to_string_lossy().into_owned()),
        presets: EXPORT_PRESETS.to_vec(),
    }
}

#[tauri::command]
pub fn create_category(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
    id: Option<String>,
) -> CommandResult<CategoryOutcome> {
    let id = id.as_deref().map(str::trim).filter(|id| !id.is_empty());
    state.create_category(&app, &name, id).map_err(describe)
}

#[tauri::command]
pub fn update_category(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    name: String,
    color: Option<String>,
) -> CommandResult<CategoryOutcome> {
    state
        .update_category(&app, &id, &name, color.as_deref())
        .map_err(describe)
}

#[tauri::command]
pub fn open_categories_file(app: AppHandle, state: State<'_, AppState>) -> CommandResult<()> {
    reveal_in_dir(&app, &state.categories_path(), "分类配置")
}

#[tauri::command]
pub async fn import_tracks(
    app: AppHandle,
    state: State<'_, AppState>,
    category: String,
) -> CommandResult<Option<LibraryMutation>> {
    if !state.has_category(&category) {
        return Err(format!("未注册的分类：{category}"));
    }
    let Some(files) = app
        .dialog()
        .file()
        .set_title("导入音频到曲库")
        .add_filter("音频（MP3 / M4A / FLAC / WAV / OGG）", &AUDIO_EXTENSIONS)
        .blocking_pick_files()
    else {
        return Ok(None);
    };
    let paths: Vec<PathBuf> = files
        .into_iter()
        .filter_map(|file| file.into_path().ok())
        .collect();
    if paths.is_empty() {
        return Ok(None);
    }
    state
        .import_tracks(&app, &category, paths)
        .map(Some)
        .map_err(describe)
}

#[tauri::command]
pub fn update_track(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
    title: String,
    category: String,
) -> CommandResult<LibraryMutation> {
    state
        .update_track(&app, &key, &title, &category)
        .map_err(describe)
}

#[tauri::command]
pub fn move_tracks(
    app: AppHandle,
    state: State<'_, AppState>,
    keys: Vec<String>,
    category: String,
) -> CommandResult<LibraryMutation> {
    state.move_tracks(&app, &keys, &category).map_err(describe)
}

#[tauri::command]
pub async fn delete_tracks(
    app: AppHandle,
    state: State<'_, AppState>,
    keys: Vec<String>,
) -> CommandResult<Option<LibraryMutation>> {
    if keys.is_empty() {
        return Err("未选择任何曲目".to_owned());
    }
    let message = format!(
        "将把选中的 {} 首曲目移动到曲库的 {TRASH_DIR_NAME} 回收站目录，之后可在文件管理器中手动恢复。",
        keys.len()
    );
    let confirmed = app
        .dialog()
        .message(message)
        .title("删除曲目")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "移到回收站".to_owned(),
            "取消".to_owned(),
        ))
        .blocking_show();
    if !confirmed {
        return Ok(None);
    }
    state.delete_tracks(&app, &keys).map(Some).map_err(describe)
}

#[tauri::command]
pub async fn export_tracks(
    app: AppHandle,
    state: State<'_, AppState>,
    keys: Vec<String>,
) -> CommandResult<Option<ExportBatchResult>> {
    if keys.is_empty() {
        return Err("未选择任何曲目".to_owned());
    }
    let Some(folder) = app
        .dialog()
        .file()
        .set_title("选择批量导出文件夹")
        .blocking_pick_folder()
    else {
        return Ok(None);
    };
    let dir = folder
        .into_path()
        .map_err(|error| format!("导出目录无效：{error}"))?;
    let outcome = state.export_tracks(&keys, &dir).map_err(describe)?;
    Ok(Some(ExportBatchResult {
        dir: dir.to_string_lossy().into_owned(),
        report: outcome.report,
    }))
}

#[tauri::command]
pub async fn export_track(
    app: AppHandle,
    state: State<'_, AppState>,
    key: String,
) -> CommandResult<Option<ExportResult>> {
    let song = state
        .song(&key)
        .ok_or_else(|| format!("曲目不存在：{key}"))?;

    let target_ext = export_extension(&state, &song.file_name);
    let file_name = format!(
        "{}.{target_ext}",
        bgm_core::export::safe_file_name(&song.title)
    );
    let filter_label = format!("{} 音频", target_ext.to_ascii_uppercase());
    let mut dialog = app
        .dialog()
        .file()
        .set_title("导出音频")
        .set_file_name(file_name)
        .add_filter(&filter_label, &[target_ext]);
    if let Ok(download_dir) = app.path().download_dir() {
        dialog = dialog.set_directory(download_dir);
    }

    let Some(target) = dialog.blocking_save_file() else {
        return Ok(None);
    };
    let target = target
        .into_path()
        .map_err(|error| format!("保存路径无效：{error}"))?;

    let outcome = export_audio(Path::new(&song.path), &target, state.export_settings())
        .map_err(|error| error.to_string())?;
    Ok(Some(ExportResult {
        path: target.to_string_lossy().into_owned(),
        bytes: outcome.bytes,
        normalized: outcome.normalized,
        warning: outcome.warning,
    }))
}
