//! GPU 加速：WebKitGTK 通过环境变量关闭合成模式，必须在创建 WebView 之前设置。

pub fn apply<R: tauri::Runtime>(_context: &mut tauri::Context<R>, enabled: bool) {
    if enabled {
        return;
    }
    // 关掉合成模式会让页面走软件渲染，代价是动画变慢，换取老显卡上不闪屏。
    unsafe { std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1") };
}
