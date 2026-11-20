mod commands;
mod state;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = AppState::initialize(app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::save_prefs,
            commands::rescan_library,
            commands::choose_library_dir,
            commands::reset_library_dir,
            commands::open_library_dir,
            commands::export_track,
            commands::import_tracks,
            commands::update_track,
            commands::move_tracks,
            commands::delete_tracks,
            commands::export_tracks,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run TFY BGM");
}
