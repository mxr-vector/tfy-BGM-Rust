//! 暴露给前端的命令。所有命令只做参数校验、调用核心逻辑并序列化结果，
//! 业务规则集中在 `bgm-core`，便于脱离 WebView 测试。

use std::path::{Path, PathBuf};

use bgm_core::BatchReport;
use bgm_core::LibrarySnapshot;
use bgm_core::Prefs;
use bgm_core::export::export_audio;
use bgm_core::library::{AUDIO_EXTENSIONS, DEFAULT_AUDIO_EXTENSION, audio_extension};
use bgm_core::loudness::{EXPORT_PRESETS, ExportPreset, NORMALIZED_EXTENSION, ffmpeg_path};
use bgm_core::paths::TRASH_DIR_NAME;
use bgm_core::{LoopBook, Schedule};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::api::{ApiBridge, ApiEndpoint, ApiStatus, CommandFailure};
use crate::desktop::{self, DesktopState};
use crate::fonts;
use crate::models::ModelsInfo;
use crate::state::{AppState, Bootstrap, CategoryOutcome, ConfigReload, LibraryMutation};
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
#[tauri::command]
pub async fn download_asr_model(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CommandResult<ModelsInfo> {
    let runtime = state.model_runtime();
    let root = state.models_root();
    let url = state.asr_download_url();
    let task_app = app.clone();
    tauri::async_runtime::spawn_blocking(move || runtime.download(&task_app, &root, &url))
        .await
        .map_err(|error| format!("下载任务异常结束：{error}"))?
        .map_err(describe)?;
    Ok(state.models_info())
}

/// 用 sherpa-onnx 加载指定模型目录；加载 1GB 左右的模型会占用数秒。
#[tauri::command]
pub async fn load_asr_model(state: State<'_, AppState>, path: String) -> CommandResult<ModelsInfo> {
    let runtime = state.model_runtime();
    let root = state.models_root();
    let dir = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || runtime.load(&root, &dir))
        .await
        .map_err(|error| format!("加载任务异常结束：{error}"))?
        .map_err(describe)?;
    Ok(state.models_info())
}

/// 释放已加载的模型，归还内存。
#[tauri::command]
pub async fn unload_asr_model(state: State<'_, AppState>) -> CommandResult<ModelsInfo> {
    state.model_runtime().unload();
    Ok(state.models_info())
}

/// 请求取消正在进行的模型下载；返回是否确实有任务被中断。
#[tauri::command]
pub fn cancel_asr_download(state: State<'_, AppState>) -> bool {
    state.model_runtime().cancel_download()
}

/// 选择 WAV 文件并用已加载的模型识别；取消选择时返回 `null`。
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
