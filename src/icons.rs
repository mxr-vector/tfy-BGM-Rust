//! 内嵌 Lucide 图标。路径与 Tauri 版保持一致，由 GPUI 的 SVG 渲染器着色。

use gpui::{Hsla, Styled, Svg, px, svg};

pub fn icon(name: &'static str, icon_size: f32, color: impl Into<Hsla>) -> Svg {
    svg()
        .path(format!("icons/{name}.svg"))
        .w(px(icon_size))
        .h(px(icon_size))
        .text_color(color)
}

pub fn category_icon(name: &str) -> &'static str {
    match name {
        "heart" => "heart",
        "sparkles" => "sparkles",
        "coffee" => "coffee",
        "clock" => "clock",
        "flame" => "flame",
        "feather" => "feather",
        "sun" => "sun",
        _ => "music",
    }
}

pub fn source(name: &str) -> String {
    let body = match name {
        "audio-lines" => r##"<path d="M3 10v4m4-7v10m5-14v18m5-14v10m4-7v4"/>"##,
        "search" => r##"<circle cx="11" cy="11" r="7"/><path d="m20 20-3.6-3.6"/>"##,
        "x" => r##"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"##,
        "sort" => {
            r##"<path d="m21 16-4 4-4-4"/><path d="M17 20V4"/><path d="m3 8 4-4 4 4"/><path d="M7 4v16"/>"##
        }
        "music" => {
            r##"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"##
        }
        "heart" => {
            r##"<path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.29 1.51 4.04 3 5.5l7 7Z"/>"##
        }
        "heart-fill" => {
            r##"<path fill="#fff" stroke="none" d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.29 1.51 4.04 3 5.5l7 7Z"/>"##
        }
        "disc" => r##"<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="2.5"/>"##,
        "play-fill" => {
            r##"<path d="M8 5.14v13.72a1 1 0 0 0 1.54.84l10.29-6.86a1 1 0 0 0 0-1.68L9.54 4.3A1 1 0 0 0 8 5.14Z" fill="#fff" stroke="none"/>"##
        }
        "pause-fill" => {
            r##"<rect x="6" y="4" width="4" height="16" rx="1.2" fill="#fff" stroke="none"/><rect x="14" y="4" width="4" height="16" rx="1.2" fill="#fff" stroke="none"/>"##
        }
        "skip-back" => r##"<path d="M19 20 9 12l10-8v16Z"/><path d="M5 19V5"/>"##,
        "skip-forward" => r##"<path d="m5 4 10 8-10 8V4Z"/><path d="M19 5v14"/>"##,
        "shuffle" => {
            r##"<path d="M2 18h1.4c1.3 0 2.5-.6 3.3-1.7l6.1-8.6c.8-1.1 2-1.7 3.3-1.7H22"/><path d="m18 2 4 4-4 4"/><path d="M2 6h1.9c1.5 0 2.9.9 3.6 2.2"/><path d="M22 18h-5.9c-1.3 0-2.6-.7-3.3-1.8l-.5-.8"/><path d="m18 14 4 4-4 4"/>"##
        }
        "repeat" => {
            r##"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/>"##
        }
        "repeat-1" => {
            r##"<path d="m17 2 4 4-4 4"/><path d="M3 11v-1a4 4 0 0 1 4-4h14"/><path d="m7 22-4-4 4-4"/><path d="M21 13v1a4 4 0 0 1-4 4H3"/><path d="M11 10h1v4"/>"##
        }
        "volume" => {
            r##"<path d="m11 5-5 4H2v6h4l5 4V5Z"/><path d="M15.5 8.5a5 5 0 0 1 0 7"/><path d="M19 5a10 10 0 0 1 0 14"/>"##
        }
        "volume-mute" => {
            r##"<path d="m11 5-5 4H2v6h4l5 4V5Z"/><path d="m22 9-6 6"/><path d="m16 9 6 6"/>"##
        }
        "download" => {
            r##"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/>"##
        }
        "upload" => {
            r##"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m17 8-5-5-5 5"/><path d="M12 3v12"/>"##
        }
        "plus" => r##"<path d="M12 5v14"/><path d="M5 12h14"/>"##,
        "more" => {
            r##"<circle cx="5" cy="12" r="1" fill="#fff" stroke="none"/><circle cx="12" cy="12" r="1" fill="#fff" stroke="none"/><circle cx="19" cy="12" r="1" fill="#fff" stroke="none"/>"##
        }
        "list-ordered" => {
            r##"<path d="M10 6h11M10 12h11M10 18h11M3 5h1v4M3 9h2M3 14h1a1 1 0 0 1 .7 1.7L3 18h2"/>"##
        }
        "list-music" => {
            r##"<path d="M21 15V6"/><circle cx="18.5" cy="18" r="2.5"/><path d="M12 12H3"/><path d="M16 6H3"/><path d="M12 18H3"/>"##
        }
        "edit" => {
            r##"<path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/>"##
        }
        "trash" => {
            r##"<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"##
        }
        "alert" => {
            r##"<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z"/><path d="M12 9v4"/><path d="M12 17h.01"/>"##
        }
        "folder" => {
            r##"<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>"##
        }
        "folder-plus" => {
            r##"<path d="M12 10v6"/><path d="M9 13h6"/><path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>"##
        }
        "folder-open" => {
            r##"<path d="m6 14 1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2"/>"##
        }
        "external" => {
            r##"<path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/>"##
        }
        "refresh" => r##"<path d="M21 12a9 9 0 1 1-2.64-6.36"/><path d="M21 3v6h-6"/>"##,
        "settings" => {
            r##"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z"/><circle cx="12" cy="12" r="3"/>"##
        }
        "chevron-left" => r##"<path d="m15 6-6 6 6 6"/>"##,
        "chevron-right" => r##"<path d="m9 6 6 6-6 6"/>"##,
        "chevron-up" => r##"<path d="m6 15 6-6 6 6"/>"##,
        "chevron-down" => r##"<path d="m6 9 6 6 6-6"/>"##,
        "rotate" => r##"<path d="M3 12a9 9 0 1 0 2.64-6.36"/><path d="M3 3v6h6"/>"##,
        "sparkles" => {
            r##"<path d="m12 3 1.9 5.8a2 2 0 0 0 1.3 1.3L21 12l-5.8 1.9a2 2 0 0 0-1.3 1.3L12 21l-1.9-5.8a2 2 0 0 0-1.3-1.3L3 12l5.8-1.9a2 2 0 0 0 1.3-1.3Z"/>"##
        }
        "coffee" => {
            r##"<path d="M10 2v2"/><path d="M14 2v2"/><path d="M16 8a1 1 0 0 1 1 1v8a4 4 0 0 1-4 4H7a4 4 0 0 1-4-4V9a1 1 0 0 1 1-1h14a4 4 0 1 1 0 8h-1"/><path d="M6 2v2"/>"##
        }
        "clock" => r##"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>"##,
        "flame" => {
            r##"<path d="M8.5 14.5A2.5 2.5 0 0 0 11 12c0-1.38-.5-2-1-3-1.072-2.143-.224-4.054 2-6 .5 2.5 2 4.9 4 6.5 2 1.6 3 3.5 3 5.5a7 7 0 1 1-14 0c0-1.153.433-2.294 1-3a2.5 2.5 0 0 0 2.5 2.5z"/>"##
        }
        "feather" => {
            r##"<path d="M20.24 12.24a6 6 0 0 0-8.49-8.49L5 10.5V19h8.5z"/><path d="m16 8-14 14"/><path d="M17.5 15H9"/>"##
        }
        "sun" => {
            r##"<circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>"##
        }
        "check" => r##"<path d="M20 6 9 17l-5-5"/>"##,
        "image" => {
            r##"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21"/>"##
        }
        "sliders" => {
            r##"<line x1="4" x2="4" y1="21" y2="14"/><line x1="4" x2="4" y1="10" y2="3"/><line x1="12" x2="12" y1="21" y2="12"/><line x1="12" x2="12" y1="8" y2="3"/><line x1="20" x2="20" y1="21" y2="16"/><line x1="20" x2="20" y1="12" y2="3"/><line x1="2" x2="6" y1="14" y2="14"/><line x1="10" x2="14" y1="8" y2="8"/><line x1="18" x2="22" y1="16" y2="16"/>"##
        }
        "grip" => {
            r##"<circle cx="9" cy="5" r="1" fill="#fff" stroke="none"/><circle cx="9" cy="12" r="1" fill="#fff" stroke="none"/><circle cx="9" cy="19" r="1" fill="#fff" stroke="none"/><circle cx="15" cy="5" r="1" fill="#fff" stroke="none"/><circle cx="15" cy="12" r="1" fill="#fff" stroke="none"/><circle cx="15" cy="19" r="1" fill="#fff" stroke="none"/>"##
        }
        _ => {
            r##"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"##
        }
    };
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#fff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    )
}
