mod api;
mod commands;
mod library_root;
mod library_watch;
mod settings;
mod state;
mod wallpapers;

use api::ApiBridge;
use state::AppState;
use tauri::Manager;
use tauri::webview::{PermissionKind, PermissionResponse};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // WebView 只有在获得麦克风权限后才会暴露完整的输出设备列表。
        // 应用本身不录音：前端拿到设备列表后立即停止音轨，仅用于多输出路由。
        .on_permission_request(|_, kind| match kind {
            PermissionKind::Microphone => PermissionResponse::Allow,
            _ => PermissionResponse::Default,
        })
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
            let api_config = state.api_config();
            app.manage(state);

            // 本机开放接口：端口占用或配置关闭时只记录状态，不影响应用启动。
            // 先注册状态再开始监听，避免首批请求找不到 ApiBridge。
            let bridge = ApiBridge::new(app.handle().clone(), &api_config);
            app.manage(bridge.clone());
            bridge.serve();
            if let Some(error) = bridge.status().error {
                eprintln!("[api] {error}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::api_info,
            commands::api_publish_state,
            commands::api_reply,
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
