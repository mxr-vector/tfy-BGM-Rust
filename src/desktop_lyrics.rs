//! 桌面歌词与独立样式设置。

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Context, ElementId, FontWeight, HighlightStyle, IntoElement, Render,
    SharedString, StyledText, Window, WindowHandle, div, prelude::*, px, rgb, rgba,
};
use serde::Serialize;
use serde_json::Value;

use crate::lyrics;

pub const STYLE_KEY: &str = "desktopLyricsStyle";
const VERSION: u64 = 4;

const COLORS: [(&str, &str); 5] = [
    ("薄荷绿", "#19d98b"),
    ("纯白", "#e6f4ee"),
    ("暖黄", "#efb32e"),
    ("樱粉", "#ee6f9f"),
    ("天蓝", "#5fa8e8"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Style {
    version: u64,
    pub font_family: String,
    pub font_size: u16,
    pub highlight: String,
    pub background: u16,
    pub width: u16,
    pub height: u16,
    pub locked: bool,
    pub lead_seconds: u16,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            version: VERSION,
            font_family: String::new(),
            font_size: 24,
            highlight: COLORS[0].1.into(),
            background: 62,
            width: 760,
            height: 120,
            locked: false,
            lead_seconds: 1,
        }
    }
}

impl Style {
    pub fn read(value: Option<&Value>) -> (Self, bool) {
        let defaults = Self::default();
        let Some(value) = value.filter(|value| value.is_object()) else {
            return (defaults, false);
        };
        let old = value["version"].as_u64().unwrap_or(0) < VERSION;
        let number = |field: &str, fallback: u16, min: u16, max: u16| {
            value[field]
                .as_f64()
                .unwrap_or(f64::from(fallback))
                .round()
                .clamp(f64::from(min), f64::from(max)) as u16
        };
        let mut highlight = value["highlight"]
            .as_str()
            .filter(|value| {
                value.len() == 7
                    && value.starts_with('#')
                    && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
            })
            .map_or(defaults.highlight, str::to_ascii_lowercase);
        if old {
            let previous = ["#6ff3bc", "#ffffff", "#ffd479", "#ff9ec4", "#8ecbff"];
            if let Some(index) = previous.iter().position(|value| *value == highlight) {
                highlight = COLORS[index].1.into();
            }
        }
        (
            Self {
                version: VERSION,
                font_family: value["fontFamily"].as_str().unwrap_or_default().into(),
                font_size: number("fontSize", defaults.font_size, 12, 64),
                highlight,
                background: number("background", defaults.background, 0, 90),
                width: number("width", defaults.width, 420, 1600),
                height: if old {
                    defaults.height
                } else {
                    number("height", defaults.height, 120, 900)
                },
                locked: value["locked"].as_bool().unwrap_or(false),
                lead_seconds: number("leadSeconds", defaults.lead_seconds, 0, 2),
            },
            old,
        )
    }

    fn color(&self) -> u32 {
        u32::from_str_radix(&self.highlight[1..], 16).unwrap_or(0x19d98b)
    }
}

pub struct DesktopLyrics {
    pub style: Style,
    pub overlay: Option<WindowHandle<Overlay>>,
    pub settings: Option<WindowHandle<Settings>>,
    pub quitting: bool,
    pub size_to_save: Option<((u16, u16), std::time::Instant)>,
}

impl DesktopLyrics {
    pub fn new(style: Style) -> Self {
        Self {
            style,
            overlay: None,
            settings: None,
            quitting: false,
            size_to_save: None,
        }
    }
}

#[derive(Clone)]
pub enum Event {
    Close,
    Settings,
    CloseSettings,
    Style(Style),
}

pub type Handler = Rc<dyn Fn(Event, &mut App)>;

#[derive(PartialEq, Eq)]
pub enum Lines {
    Message(SharedString),
    Lyrics {
        previous: Option<SharedString>,
        active: Option<(SharedString, usize)>,
        next: Option<SharedString>,
    },
}

impl From<lyrics::Frame<'_>> for Lines {
    fn from(frame: lyrics::Frame<'_>) -> Self {
        Self::Lyrics {
            previous: frame.previous.cloned(),
            active: frame.active.map(|(text, end)| (text.clone(), end)),
            next: frame.next.cloned(),
        }
    }
}

pub struct Overlay {
    style: Style,
    lines: Lines,
    unlock_hot: bool,
    handle: Handler,
}

impl Overlay {
    pub fn new(style: Style, handle: Handler) -> Self {
        Self {
            style,
            lines: Lines::Message("等待播放".into()),
            unlock_hot: false,
            handle,
        }
    }

    pub fn update_content(
        &mut self,
        style: &Style,
        lines: Lines,
        unlock_hot: bool,
        cx: &mut Context<Self>,
    ) {
        if &self.style != style || self.lines != lines || self.unlock_hot != unlock_hot {
            self.style = style.clone();
            self.lines = lines;
            self.unlock_hot = unlock_hot;
            cx.notify();
        }
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let accent = rgb(self.style.color());
        let line = |text: Option<SharedString>| {
            div()
                .w_full()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_size(px(14.))
                .text_color(rgb(0xb7c4c2))
                .children(text)
        };
        let font_size = f32::from(self.style.font_size);
        let main = div()
            .w_full()
            .whitespace_nowrap()
            .overflow_hidden()
            .text_size(px(font_size))
            .font_weight(FontWeight::SEMIBOLD);
        let lyrics = match &self.lines {
            Lines::Message(message) => {
                div().child(main.text_color(rgb(0xe6f4ee)).child(message.clone()))
            }
            Lines::Lyrics {
                previous,
                active,
                next,
            } => {
                let current = active.as_ref().map(|(text, end)| {
                    StyledText::new(text.clone()).with_highlights([(
                        0..*end,
                        HighlightStyle {
                            color: Some(accent.into()),
                            ..Default::default()
                        },
                    )])
                });
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(line(previous.clone()))
                    .child(
                        main.text_color(rgb(0xe6f4ee))
                            .when_some(current, |line, text| line.child(text)),
                    )
                    .child(line(next.clone()))
            }
        };
        let mut style = self.style.clone();
        style.locked = !style.locked;
        let opacity = (u32::from(self.style.background) * 255 / 100) & 0xff;
        div()
            .size_full()
            .relative()
            .p_3()
            .rounded(px(12.))
            .bg(rgba(0x10192000 | opacity))
            .text_center()
            .flex()
            .items_center()
            .justify_center()
            .when(!self.style.font_family.is_empty(), |panel| {
                panel.font_family(SharedString::from(self.style.font_family.clone()))
            })
            .child(div().w_full().px_8().child(lyrics))
            .when(!self.style.locked || self.unlock_hot, |panel| {
                panel.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .h(px(36.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .pr_2()
                        .when(!self.style.locked, |bar| {
                            bar.child(button(
                                "lyrics-settings",
                                "设置",
                                self.handle.clone(),
                                Event::Settings,
                            ))
                            .child(button(
                                "lyrics-close",
                                "关闭",
                                self.handle.clone(),
                                Event::Close,
                            ))
                        })
                        .child(button(
                            "lyrics-lock",
                            if self.style.locked {
                                "解锁"
                            } else {
                                "锁定"
                            },
                            self.handle.clone(),
                            Event::Style(style),
                        )),
                )
            })
    }
}

pub struct Settings {
    pub style: Style,
    handle: Handler,
}

impl Settings {
    pub fn new(style: Style, handle: Handler) -> Self {
        Self { style, handle }
    }

    fn change(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        change: impl FnOnce(&mut Style),
    ) -> gpui::Stateful<gpui::Div> {
        let mut style = self.style.clone();
        change(&mut style);
        button(id, label, self.handle.clone(), Event::Style(style))
    }

    fn stepper(
        &self,
        label: &str,
        field: fn(&mut Style) -> &mut u16,
        step: u16,
        min: u16,
        max: u16,
    ) -> impl IntoElement + use<> {
        let value = *field(&mut self.style.clone());
        setting(label).child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(self.change(
                    SharedString::from(format!("{label}-minus")),
                    "−",
                    |style| *field(style) = value.saturating_sub(step).max(min),
                ))
                .child(div().w(px(72.)).text_center().child(value.to_string()))
                .child(
                    self.change(SharedString::from(format!("{label}-plus")), "+", |style| {
                        *field(style) = value.saturating_add(step).min(max)
                    }),
                ),
        )
    }
}

impl Render for Settings {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let lead = setting("高亮提前").children([0, 1, 2].into_iter().map(|lead| {
            self.change(("lead", lead as u32), format!("{lead} 秒"), |style| {
                style.lead_seconds = lead
            })
            .when(self.style.lead_seconds == lead, |button| {
                button.bg(rgb(0x345446))
            })
        }));
        let colors = setting("高亮颜色").children(COLORS.into_iter().map(|(label, color)| {
            let swatch = u32::from_str_radix(&color[1..], 16).unwrap_or_default();
            self.change(color, "", |style| style.highlight = color.into())
                .w(px(26.))
                .h(px(26.))
                .bg(rgb(swatch))
                .when(self.style.highlight == color, |button| {
                    button.border_2().border_color(gpui::white())
                })
                .tooltip(move |_, cx| cx.new(|_| ColorLabel(label)).into())
        }));
        let fonts = setting("字体").children(
            [
                ("系统", ""),
                ("苹方", "PingFang SC"),
                ("宋体", "Songti SC"),
                ("华文黑体", "Heiti SC"),
            ]
            .into_iter()
            .map(|(name, family)| {
                self.change(name, name, |style| style.font_family = family.into())
                    .when(self.style.font_family == family, |button| {
                        button.bg(rgb(0x345446))
                    })
            }),
        );
        div()
            .size_full()
            .p_4()
            .bg(rgb(0x1a2229))
            .text_color(rgb(0xe6f4ee))
            .flex()
            .flex_col()
            .gap_3()
            .child(fonts)
            .child(self.stepper("字号", |s| &mut s.font_size, 2, 12, 64))
            .child(colors)
            .child(self.stepper("背景不透明度", |s| &mut s.background, 5, 0, 90))
            .child(lead)
            .child(self.stepper("窗口宽度", |s| &mut s.width, 40, 420, 1600))
            .child(self.stepper("窗口高度", |s| &mut s.height, 20, 120, 900))
            .child(div().flex().justify_end().child(button(
                "reset-lyrics-style",
                "恢复默认",
                self.handle.clone(),
                Event::Style(Style::default()),
            )))
    }
}

struct ColorLabel(&'static str);

impl Render for ColorLabel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(rgb(0x26343e))
            .text_color(gpui::white())
            .child(self.0)
    }
}

fn setting(title: &str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().text_sm().child(title.to_owned()))
}

fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    handler: Handler,
    event: Event,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .rounded_sm()
        .px_2()
        .py_1()
        .bg(rgb(0x283940))
        .text_color(rgb(0xe6f4ee))
        .text_xs()
        .cursor_pointer()
        .hover(|style| style.bg(rgb(0x385149)))
        .child(label.into())
        .on_click(move |_: &ClickEvent, _, cx| handler(event.clone(), cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn legacy_styles_keep_custom_preferences_and_migrate_v4_defaults() {
        let old = json!({
            "version": 3,
            "fontFamily": "Songti SC",
            "fontSize": 50.2,
            "highlight": "#FFD479",
            "background": 58,
            "width": 999,
            "height": 710,
            "locked": true
        });
        let (style, changed) = Style::read(Some(&old));
        assert!(changed);
        assert_eq!(style.font_family, "Songti SC");
        assert_eq!(style.font_size, 50);
        assert_eq!(style.highlight, "#efb32e");
        assert_eq!(style.background, 58);
        assert_eq!(style.width, 999);
        assert_eq!(style.height, 120);
        assert_eq!(style.lead_seconds, 1);
        assert!(style.locked);
        let migrated = serde_json::to_value(&style).unwrap();
        assert_eq!(migrated["version"], 4);
        assert_eq!(Style::read(Some(&migrated)), (style, false));

        let (custom, _) = Style::read(Some(&json!({
            "version": 3, "highlight": "#312ABC", "width": 10000, "leadSeconds": -1
        })));
        assert_eq!(custom.highlight, "#312abc");
        assert_eq!(custom.width, 1600);
        assert_eq!(custom.lead_seconds, 0);
    }
}
