mod api;
mod commands;
mod desktop;
mod fonts;
mod library_root;
mod library_watch;
mod models;
mod settings;
mod state;
mod wallpapers;

use api::ApiBridge;
use desktop::DesktopState;
use state::AppState;
use tauri::Manager;
use tauri::webview::{PermissionKind, PermissionResponse};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // GPU 加速只能在创建 WebView 之前决定，因此先按偏好设置渲染后端开关。
    let mut context = tauri::generate_context!();
    let gpu_acceleration = settings::startup_gpu_acceleration(&context.config().identifier);
    desktop::apply_gpu_acceleration(&mut context, gpu_acceleration);

    tauri::Builder::default()
        // WebView 只有在获得麦克风权限后才会暴露完整的输出设备列表。
        // 应用本身不录音：前端拿到设备列表后立即停止音轨，仅用于多输出路由。
        .on_permission_request(|_, kind| match kind {
            PermissionKind::Microphone => PermissionResponse::Allow,
            // 「程序启动时自动播放」与 SMTC / MPRIS2 的静音媒体元素都需要自动播放权限。
            PermissionKind::Autoplay => PermissionResponse::Allow,
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
            let (autostart, minimize_to_tray, _) = state.desktop_prefs();
            app.manage(state);

            // 托盘与最小化行为在窗口事件里读取，先注册状态再挂事件。
            app.manage(DesktopState::new(minimize_to_tray));
            if let Err(error) = desktop::install(app.handle(), autostart, minimize_to_tray) {
                eprintln!("[desktop] 桌面集成初始化失败：{error:#}");
            }

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
            commands::api_endpoints,
            commands::update_api_config,
            commands::api_publish_state,
            commands::api_reply,
            commands::apply_desktop_settings,
            commands::hide_to_tray,
            commands::quit_app,
            commands::reload_config,
            commands::system_fonts,
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
            commands::models_info,
            commands::choose_models_dir,
            commands::reset_models_dir,
            commands::open_models_dir,
            commands::download_asr_model,
            commands::load_asr_model,
            commands::unload_asr_model,
            commands::cancel_asr_download,
            commands::transcribe_wav,
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
        .run(context)
        .expect("failed to run TFY BGM");
}
