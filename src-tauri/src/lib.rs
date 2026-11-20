mod commands;
mod library_root;
mod library_watch;
mod settings;
mod state;
mod wallpapers;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = AppState::initialize(app.handle())?;
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title(&state.window_title());
            }
            // 监听失败不影响启动，前端仍可手动「重新扫描」。
            if let Err(error) = state.start_watching(app.handle()) {
                eprintln!("[watch] 文件监听启动失败：{error:#}");
            }
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::reload_config,
            commands::builtin_wallpapers,
            commands::choose_wallpaper_file,
            commands::open_config_file,
            commands::save_prefs,
            commands::save_schedule,
            commands::save_loop_point,
            commands::remove_loop_point,
            commands::rescan_library,
            commands::choose_library_dir,
            commands::reset_library_dir,
            commands::open_library_dir,
            commands::export_info,
            commands::export_track,
            commands::create_category,
            commands::update_category,
            commands::open_categories_file,
            commands::import_tracks,
            commands::update_track,
            commands::move_tracks,
            commands::delete_tracks,
            commands::export_tracks,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run TFY BGM");
}
