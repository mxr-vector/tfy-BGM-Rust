mod api;
mod commands;
mod desktop;
mod event_bus;
mod fonts;
mod key_lock;
mod library_root;
mod library_watch;
mod lyrics;
mod models;
mod separation;
mod settings;
mod state;
mod subtitles;
mod tasks;
mod wallpapers;

use api::ApiBridge;
use desktop::DesktopState;
use lyrics::LyricsState;
use state::AppState;
use tauri::Manager;
use tauri::webview::{PermissionKind, PermissionResponse};

/// 分离 worker 入口：若本次进程由人声分离任务启动，执行任务并返回退出码。
///
/// 必须在初始化 Tauri 之前调用，worker 不创建窗口、不加载状态。
pub fn separation_worker_main() -> Option<i32> {
    separation::worker_main()
}

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
            // Linux 的 WebKitGTK 无法通过 asset:// 播放视频，先启动本地 Range 媒体服务，
            // 再初始化偏好，确保首屏恢复的自定义视频壁纸也能拿到 HTTP 地址。
            app.manage(wallpapers::WallpaperMediaServer::start());
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
            // 桌面歌词窗口的开关与最新快照；歌词窗口启动时从此处拉取初始状态。
            app.manage(LyricsState::default());

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

            // 上次加载成功的 ASR 模型在后台恢复：重新编译 / 重启后无需再手动加载。
            commands::spawn_restore_last_model(app.handle());
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
            commands::wallpaper_media_url,
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
            commands::delete_asr_model,
            commands::cancel_asr_download,
            commands::transcribe_wav,
            commands::separation_models,
            commands::generate_subtitles,
            commands::export_subtitle,
            commands::subtitle_for_track,
            commands::list_subtitles,
            commands::delete_subtitle,
            commands::reveal_subtitle_audio,
            commands::update_subtitle,
            commands::update_subtitle_text,
            commands::set_desktop_lyrics_enabled,
            commands::sync_desktop_lyrics,
            commands::desktop_lyrics_snapshot,
            commands::keep_desktop_lyrics_on_top,
            commands::resize_desktop_lyrics,
            commands::toggle_desktop_lyrics_settings,
            commands::place_desktop_lyrics,
            commands::set_desktop_lyrics_lock,
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
