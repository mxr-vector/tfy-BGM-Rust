//! 统一桌面控件、焦点可见性与多文件夹路径展示辅助。

use std::collections::HashMap;
use std::path::Path;

use gpui::{
    ClickEvent, Context, Div, ElementId, Entity, FocusHandle, FontWeight, Hsla, InteractiveElement,
    MouseButton, ParentElement, SharedString, Stateful, StatefulInteractiveElement, Styled, Window,
    div, prelude::FluentBuilder, px,
};

use super::{TextField, Workspace, fade, icon, theme_color};
use crate::design::{
    ACCENT, ACCENT_HOVER, ACCENT_INK, CONTROL_TINT, DANGER, FAINT, LINE, MUTED, PANEL, SCRIM, TEXT,
};
use crate::library::FolderStatus;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ButtonTone {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

pub(super) fn keyboard_control(id: impl Into<ElementId>, show_focus: bool) -> Stateful<Div> {
    div()
        .id(id)
        .key_context("Control")
        .tab_index(0)
        .focus(move |style| {
            if show_focus {
                style.border_1().border_color(theme_color(ACCENT))
            } else {
                style
            }
        })
}

pub(super) fn ui_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    icon_name: Option<&'static str>,
    tone: ButtonTone,
    enabled: bool,
    compact: bool,
    show_focus: bool,
    cx: &mut Context<Workspace>,
    on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<Div> {
    let label = label.into();
    let (bg, hover_bg, border, text_color) = match tone {
        ButtonTone::Primary => (
            fade(theme_color(ACCENT), 1.0),
            fade(theme_color(ACCENT_HOVER), 1.0),
            fade(theme_color(ACCENT), 1.0),
            theme_color(ACCENT_INK),
        ),
        ButtonTone::Secondary => (
            fade(theme_color(CONTROL_TINT), 0.08),
            fade(theme_color(CONTROL_TINT), 0.14),
            fade(theme_color(CONTROL_TINT), 0.14),
            theme_color(TEXT),
        ),
        ButtonTone::Ghost => (
            fade(theme_color(CONTROL_TINT), 0.0),
            fade(theme_color(CONTROL_TINT), 0.09),
            fade(theme_color(CONTROL_TINT), 0.0),
            theme_color(MUTED),
        ),
        ButtonTone::Danger => (
            fade(theme_color(DANGER), 0.12),
            fade(theme_color(DANGER), 0.22),
            fade(theme_color(DANGER), 0.30),
            theme_color(DANGER),
        ),
    };
    let height = if compact { px(28.) } else { px(33.) };
    let px_pad = if compact { px(10.) } else { px(14.) };
    let mut el = keyboard_control(id, show_focus)
        .h(height)
        .px(px_pad)
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(text_color)
        .when(!enabled, |div| div.opacity(0.42))
        .when(enabled, |div| {
            div.cursor_pointer()
                .hover(move |style| style.bg(hover_bg))
                .active(move |style| style.opacity(0.88))
        });

    if let Some(icon_name) = icon_name {
        el = el.child(icon(icon_name, 13., text_color));
    }
    el = el.child(div().whitespace_nowrap().child(label));

    if enabled {
        el = el.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            on_click(this, window, cx);
            this.after_button_click(window, cx);
        }));
    }
    el
}

pub(super) fn ui_icon_button(
    id: impl Into<ElementId>,
    icon_name: &'static str,
    active: bool,
    enabled: bool,
    show_focus: bool,
    cx: &mut Context<Workspace>,
    on_click: impl Fn(&mut Workspace, &ClickEvent, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<Div> {
    let fg = if active {
        theme_color(ACCENT)
    } else {
        theme_color(MUTED)
    };
    let mut el = keyboard_control(id, show_focus)
        .size(px(28.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .bg(if active {
            fade(theme_color(ACCENT), 0.14)
        } else {
            fade(theme_color(CONTROL_TINT), 0.0)
        })
        .when(!enabled, |div| div.opacity(0.4))
        .when(enabled, |div| {
            div.cursor_pointer()
                .hover(move |style| {
                    style
                        .bg(fade(theme_color(CONTROL_TINT), 0.10))
                        .text_color(theme_color(TEXT))
                })
                .active(|style| style.opacity(0.85))
        })
        .child(icon(icon_name, 14., fg));

    if enabled {
        el = el.on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            on_click(this, event, window, cx);
            this.after_button_click(window, cx);
        }));
    }
    el
}

pub(super) fn segmented_control<T: Copy + PartialEq + 'static>(
    id_prefix: &'static str,
    options: &[(T, &'static str)],
    current: T,
    show_focus: bool,
    cx: &mut Context<Workspace>,
    on_select: impl Fn(&mut Workspace, T, &mut Window, &mut Context<Workspace>) + Clone + 'static,
) -> Div {
    let mut group = div()
        .flex()
        .items_center()
        .gap_1()
        .p_1()
        .rounded_md()
        .bg(fade(theme_color(CONTROL_TINT), 0.05))
        .border_1()
        .border_color(theme_color(LINE));
    for (idx, (value, label)) in options.iter().enumerate() {
        let value = *value;
        let selected = value == current;
        let handler = on_select.clone();
        group = group.child(
            keyboard_control(SharedString::from(format!("{id_prefix}-{idx}")), show_focus)
                .h(px(26.))
                .px_2p5()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .text_xs()
                .font_weight(if selected {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .bg(if selected {
                    fade(theme_color(ACCENT), 0.16)
                } else {
                    fade(theme_color(CONTROL_TINT), 0.0)
                })
                .text_color(if selected {
                    theme_color(ACCENT)
                } else {
                    theme_color(MUTED)
                })
                .hover(move |style| {
                    if selected {
                        style
                    } else {
                        style
                            .bg(fade(theme_color(CONTROL_TINT), 0.08))
                            .text_color(theme_color(TEXT))
                    }
                })
                .child(*label)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    handler(this, value, window, cx);
                    this.after_button_click(window, cx);
                })),
        );
    }
    group
}

pub(super) fn toggle_switch(
    id: impl Into<ElementId>,
    checked: bool,
    show_focus: bool,
    cx: &mut Context<Workspace>,
    on_toggle: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<Div> {
    keyboard_control(id, show_focus)
        .w(px(38.))
        .h(px(22.))
        .p(px(2.))
        .rounded_full()
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_start()
        .bg(if checked {
            fade(theme_color(ACCENT), 1.0)
        } else {
            fade(theme_color(CONTROL_TINT), 0.16)
        })
        .hover(move |style| {
            if checked {
                style.bg(theme_color(ACCENT_HOVER))
            } else {
                style.bg(fade(theme_color(CONTROL_TINT), 0.24))
            }
        })
        .child(
            div()
                .size(px(18.))
                .rounded_full()
                .bg(if checked {
                    theme_color(ACCENT_INK)
                } else {
                    theme_color(TEXT)
                })
                .when(checked, |knob| knob.ml(px(16.))),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            on_toggle(this, window, cx);
            this.after_button_click(window, cx);
        }))
}

pub(super) fn checkbox_control(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    checked: bool,
    show_focus: bool,
    cx: &mut Context<Workspace>,
    on_toggle: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<Div> {
    keyboard_control(id, show_focus)
        .h(px(28.))
        .px_1p5()
        .rounded_md()
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.06)))
        .child(
            div()
                .size(px(16.))
                .rounded(px(4.))
                .border_1()
                .border_color(if checked {
                    fade(theme_color(ACCENT), 1.0)
                } else {
                    fade(theme_color(CONTROL_TINT), 0.28)
                })
                .bg(if checked {
                    fade(theme_color(ACCENT), 1.0)
                } else {
                    fade(theme_color(CONTROL_TINT), 0.04)
                })
                .flex()
                .items_center()
                .justify_center()
                .when(checked, |box_div| {
                    box_div.child(icon("check", 11., theme_color(ACCENT_INK)))
                }),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme_color(TEXT))
                .child(label.into()),
        )
        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            on_toggle(this, window, cx);
            this.after_button_click(window, cx);
        }))
}

pub(super) fn option_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    available: bool,
    show_focus: bool,
    cx: &mut Context<Workspace>,
    on_click: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<Div> {
    let bg: Hsla = if active {
        fade(theme_color(ACCENT), 0.16)
    } else {
        fade(theme_color(CONTROL_TINT), 0.05)
    };
    let border: Hsla = if active {
        fade(theme_color(ACCENT), 0.45)
    } else {
        theme_color(LINE).into()
    };
    let mut chip = keyboard_control(id, show_focus)
        .h(px(28.))
        .px_2p5()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_xs()
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            theme_color(ACCENT)
        } else if available {
            theme_color(TEXT)
        } else {
            theme_color(FAINT)
        })
        .when(!available, |div| div.opacity(0.55))
        .when(available, |div| {
            div.cursor_pointer().hover(move |style| {
                if active {
                    style
                } else {
                    style.bg(fade(theme_color(CONTROL_TINT), 0.10))
                }
            })
        })
        .child(label.into());
    if available {
        chip = chip.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            on_click(this, window, cx);
            this.after_button_click(window, cx);
        }));
    }
    chip
}

pub(super) fn input_box(field: Entity<TextField>) -> Div {
    div()
        .w_full()
        .min_w(px(0.))
        .h(px(36.))
        .px_3()
        .rounded_md()
        .bg(fade(theme_color(CONTROL_TINT), 0.05))
        .border_1()
        .border_color(fade(theme_color(CONTROL_TINT), 0.16))
        .flex()
        .items_center()
        .child(div().flex_1().min_w(px(0.)).child(field))
}

pub(super) fn modal_backdrop(
    id: &'static str,
    focus: &FocusHandle,
    cx: &mut Context<Workspace>,
    on_dismiss: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .inset_0()
        .occlude()
        .track_focus(focus)
        .key_context("Dialog")
        .bg(fade(theme_color(SCRIM), 0.34))
        .flex()
        .justify_center()
        .items_center()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                on_dismiss(this, window, cx);
                cx.notify();
            }),
        )
}

pub(super) fn modal_card(width: f32) -> Div {
    div()
        .w(px(width))
        .max_w(px(940.))
        .rounded(px(14.))
        .bg(theme_color(PANEL))
        .border_1()
        .border_color(fade(theme_color(CONTROL_TINT), 0.14))
        .shadow_lg()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

pub(super) fn compact_display_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir()
        && let Ok(rel) = path.strip_prefix(&home)
    {
        if rel.as_os_str().is_empty() {
            return "~".to_owned();
        }
        return format!("~/{}", rel.display());
    }
    path.display().to_string()
}

pub(super) fn distinguishing_folder_paths(folders: &[FolderStatus]) -> HashMap<String, String> {
    let mut name_counts = HashMap::<String, usize>::new();
    for folder in folders {
        *name_counts.entry(folder.display_name.clone()).or_insert(0) += 1;
    }

    let mut result = HashMap::with_capacity(folders.len());
    for folder in folders {
        let full = compact_display_path(&folder.path);
        let duplicate_name = name_counts.get(&folder.display_name).copied().unwrap_or(0) > 1;
        if !duplicate_name {
            result.insert(folder.id.clone(), full);
            continue;
        }
        let parent_hint = folder
            .path
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty());
        let label = match parent_hint {
            Some(parent) => format!("位于 {parent} · {full}"),
            None => full,
        };
        result.insert(folder.id.clone(), label);
    }
    result
}
