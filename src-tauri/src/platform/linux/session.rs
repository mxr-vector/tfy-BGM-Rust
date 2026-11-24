//! 会话类型探测：原生 Wayland 与 XWayland 的窗口能力不同。

/// 是否运行在原生 Wayland 后端；XWayland 仍走普通窗口逻辑。
pub fn is_native_wayland() -> bool {
    let has_wayland_display =
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let backend = std::env::var("GDK_BACKEND").unwrap_or_default();
    let session = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
    has_wayland_display && backend != "x11" && (backend == "wayland" || session == "wayland")
}
