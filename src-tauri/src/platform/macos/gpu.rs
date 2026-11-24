//! GPU 加速：WKWebView 没有可切换的渲染后端，开关不生效。

pub fn apply<R: tauri::Runtime>(_context: &mut tauri::Context<R>, _enabled: bool) {}
