//! 暴露给前端的命令。所有命令只做参数校验、调用核心逻辑并序列化结果，
//! 业务规则集中在 `bgm-core`，便于脱离 WebView 测试。

use std::path::{Path, PathBuf};

use bgm_core::BatchReport;
use bgm_core::LibrarySnapshot;
use bgm_core::Prefs;
use bgm_core::export::export_audio;
use bgm_core::library::AUDIO_EXTENSION;
use bgm_core::loudness::{EXPORT_PRESETS, ExportPreset, ffmpeg_path};
use bgm_core::paths::TRASH_DIR_NAME;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_opener::OpenerExt;

use crate::state::{AppState, Bootstrap, CategoryOutcome, ConfigReload, LibraryMutation};
use crate::wallpapers::{self, WallpaperAsset, WallpaperPick};

type CommandResult<T> = Result<T, String>;

fn describe(error: anyhow::Error) -> String {
    format!("{error:#}")
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

#[tauri::command]
pub fn reload_config(app: AppHandle, state: State<'_, AppState>) -> ConfigReload {
    state.reload_config(&app)
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
        .add_filter("MP3 音频", &[AUDIO_EXTENSION])
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

    let file_name = format!(
        "{}.{AUDIO_EXTENSION}",
        bgm_core::export::safe_file_name(&song.title)
    );
    let mut dialog = app
        .dialog()
        .file()
        .set_title("导出音频")
        .set_file_name(file_name)
        .add_filter("MP3 音频", &[AUDIO_EXTENSION]);
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
