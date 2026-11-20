//! 暴露给前端的命令。所有命令只做参数校验、调用核心逻辑并序列化结果，
//! 业务规则集中在 `bgm-core`，便于脱离 WebView 测试。

use std::path::Path;

use bgm_core::LibrarySnapshot;
use bgm_core::Prefs;
use bgm_core::export::copy_validated;
use serde::Serialize;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use crate::state::{AppState, Bootstrap};

type CommandResult<T> = Result<T, String>;

fn describe(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[tauri::command]
pub fn bootstrap(state: State<'_, AppState>) -> Bootstrap {
    state.bootstrap()
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

    let file_name = format!("{}.mp3", bgm_core::export::safe_file_name(&song.title));
    let mut dialog = app
        .dialog()
        .file()
        .set_title("导出音频")
        .set_file_name(file_name)
        .add_filter("MP3 音频", &["mp3"]);
    if let Ok(download_dir) = app.path().download_dir() {
        dialog = dialog.set_directory(download_dir);
    }

    let Some(target) = dialog.blocking_save_file() else {
        return Ok(None);
    };
    let target = target
        .into_path()
        .map_err(|error| format!("保存路径无效：{error}"))?;

    let bytes =
        copy_validated(Path::new(&song.path), &target).map_err(|error| error.to_string())?;
    Ok(Some(ExportResult {
        path: target.to_string_lossy().into_owned(),
        bytes,
    }))
}
