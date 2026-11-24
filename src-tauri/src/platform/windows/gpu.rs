//! GPU 加速：wry 创建 WebView2 环境时总显式写入 additional browser arguments，
//! 环境变量会被忽略，因此改的是 `Context` 里的窗口配置。

pub fn apply<R: tauri::Runtime>(context: &mut tauri::Context<R>, enabled: bool) {
    if enabled {
        return;
    }
    // wry 的默认参数；显式写入 additionalBrowserArgs 后需要一并保留。
    const DEFAULT_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";
    for window in context.config_mut().app.windows.iter_mut() {
        let args = window
            .additional_browser_args
            .get_or_insert_with(|| DEFAULT_ARGS.to_owned());
        if args.contains("--disable-gpu") {
            continue;
        }
        if !args.is_empty() {
            args.push(' ');
        }
        args.push_str("--disable-gpu");
    }
}
