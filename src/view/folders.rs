//! 音乐文件夹管理界面与多来源添加、移除交互。

use std::path::PathBuf;

use gpui::{
    ClickEvent, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    PathPromptOptions, PromptButton, PromptLevel, SharedString, StatefulInteractiveElement, Styled,
    Window, div, prelude::FluentBuilder, px,
};

use super::controls::{
    ButtonTone, compact_display_path, distinguishing_folder_paths, modal_backdrop, modal_card,
    ui_button, ui_icon_button,
};
use super::{Workspace, fade, icon, theme_color};
use crate::design::{ACCENT, CONTROL_TINT, DANGER, FAINT, LINE, MUTED, PANEL, TEXT, WARNING};
use crate::library::FolderStatus;

impl Workspace {
    pub(super) fn open_folders_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.action_menu = None;
        self.queue_open = false;
        self.diagnostics_open = false;
        self.settings_page = None;
        self.folder_menu = None;
        self.folders_warnings_open = false;
        self.folders_modal_open = true;
        window.focus(&self.folders_focus);
        cx.notify();
    }

    pub(super) fn close_folders_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.folders_modal_open = false;
        self.folder_menu = None;
        window.focus(&self.focus);
        cx.notify();
    }

    pub(super) fn add_library_folders(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.folder_menu = None;
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("选择一个或多个音乐文件夹".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            if paths.is_empty() {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.apply_add_library_folders(paths, cx);
            });
        })
        .detach();
    }

    fn apply_add_library_folders(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let count = paths.len();
        match self.library.add_folders(paths) {
            Ok(outcome) => {
                self.finish_folder_mutation(cx);
                let added = outcome.added.len();
                let mut parts = Vec::new();
                if added > 0 {
                    parts.push(format!("已添加 {added} 个音乐文件夹"));
                }
                if outcome.skipped_duplicate > 0 {
                    parts.push(format!("已跳过 {} 个重复目录", outcome.skipped_duplicate));
                }
                if outcome.skipped_covered > 0 {
                    parts.push(format!(
                        "已跳过 {} 个已被上级目录包含的子目录",
                        outcome.skipped_covered
                    ));
                }
                if outcome.skipped_invalid > 0 {
                    parts.push(format!("已忽略 {} 个不可用路径", outcome.skipped_invalid));
                }
                let message = if parts.is_empty() {
                    if count > 0 {
                        "所选文件夹已在音乐库中".to_owned()
                    } else {
                        "未添加任何文件夹".to_owned()
                    }
                } else {
                    parts.join("，")
                };
                self.show_toast(message, added == 0 && outcome.skipped_invalid > 0, cx);
            }
            Err(error) => {
                self.show_toast(error, true, cx);
            }
        }
    }

    fn confirm_remove_folder(
        &mut self,
        folder_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.folder_menu = None;
        let Some(folder) = self.library.folder_by_id(&folder_id).cloned() else {
            return;
        };
        let detail = format!(
            "将从音乐库停止收录「{}」（{}）。磁盘上的原始音频文件与文件夹本身会完整保留；已有的收藏、节目单和循环点记录也会保留，重新添加同一目录后可恢复。",
            folder.display_name,
            compact_display_path(&folder.path)
        );
        let answer = window.prompt(
            PromptLevel::Info,
            &format!("从音乐库移除「{}」？", folder.display_name),
            Some(&detail),
            &[
                PromptButton::Ok("从音乐库移除".into()),
                PromptButton::Cancel("取消".into()),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                match this.library.remove_folder(&folder_id) {
                    Ok(removed) => {
                        this.finish_folder_mutation(cx);
                        this.show_toast(
                            format!(
                                "已停止收录「{}」，原始文件仍保留在磁盘中",
                                removed.display_name
                            ),
                            false,
                            cx,
                        );
                    }
                    Err(error) => {
                        this.show_toast(error, true, cx);
                    }
                }
            });
        })
        .detach();
    }

    fn finish_folder_mutation(&mut self, cx: &mut Context<Self>) {
        self.reconcile_snapshot();
        self.install_watcher();
        self.api_library_dirty = true;
        self.publish_api();
        self.poll_audio_tools(cx);
        cx.notify();
    }

    pub(super) fn folders_modal_layer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.folders_modal_open {
            return None;
        }
        let show_focus = self.keyboard_focus_visible;
        let folders: Vec<FolderStatus> = self.library.folders().to_vec();
        let display_paths = distinguishing_folder_paths(&folders);
        let warnings: Vec<String> = self.library.snapshot.warnings.clone();
        let warning_count = warnings.len();
        let online_count = folders.iter().filter(|folder| folder.online).count();
        let offline_count = folders.len().saturating_sub(online_count);
        let total_songs: usize = folders
            .iter()
            .filter(|folder| folder.online)
            .map(|folder| folder.song_count)
            .sum();

        let body = if folders.is_empty() {
            div()
                .flex_1()
                .min_h(px(220.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .px_6()
                .child(
                    div()
                        .size(px(52.))
                        .rounded(px(14.))
                        .bg(fade(theme_color(CONTROL_TINT), 0.06))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon("folder-open", 24., theme_color(MUTED))),
                )
                .child(
                    div()
                        .text_base()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme_color(TEXT))
                        .child("还没有添加音乐文件夹"),
                )
                .child(
                    div()
                        .max_w(px(360.))
                        .text_center()
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(theme_color(MUTED))
                        .child("添加一个或多个本地或外置磁盘目录后，其中的场景分类与散落音频会自动聚合到这里，移除来源也不会删除磁盘文件。"),
                )
                .child(
                    div().pt_1().child(ui_button(
                        "folders-empty-add",
                        "添加文件夹…",
                        Some("folder-plus"),
                        ButtonTone::Primary,
                        true,
                        false,
                        show_focus,
                        cx,
                        |this, window, cx| this.add_library_folders(window, cx),
                    )),
                )
                .into_any_element()
        } else {
            let rows = folders
                .iter()
                .enumerate()
                .map(|(index, folder)| {
                    let folder_id = folder.id.clone();
                    let folder_id_for_menu = folder.id.clone();
                    let path_text = display_paths
                        .get(&folder.id)
                        .cloned()
                        .unwrap_or_else(|| compact_display_path(&folder.path));
                    let menu_open = self
                        .folder_menu
                        .as_ref()
                        .is_some_and(|(id, _)| id == &folder.id);
                    let online = folder.online;
                    let badge_text = if online {
                        format!("{} 首曲目", folder.song_count)
                    } else {
                        "暂时离线 · 配置已保留".to_owned()
                    };

                    div()
                        .id(("folder-row", index))
                        .px_3p5()
                        .py_3()
                        .rounded_lg()
                        .bg(fade(theme_color(CONTROL_TINT), 0.04))
                        .border_1()
                        .border_color(if menu_open {
                            fade(theme_color(ACCENT), 0.45)
                        } else if !online {
                            fade(theme_color(WARNING), 0.35)
                        } else {
                            theme_color(LINE).into()
                        })
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size(px(36.))
                                .rounded_md()
                                .flex_none()
                                .bg(if online {
                                    fade(theme_color(ACCENT), 0.12)
                                } else {
                                    fade(theme_color(WARNING), 0.14)
                                })
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(
                                    if online { "folder-open" } else { "alert" },
                                    17.,
                                    theme_color(if online { ACCENT } else { WARNING }),
                                )),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .min_w(px(0.))
                                        .child(
                                            div()
                                                .truncate()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(theme_color(TEXT))
                                                .child(folder.display_name.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .px_2()
                                                .py_0p5()
                                                .rounded_full()
                                                .bg(if online {
                                                    fade(theme_color(CONTROL_TINT), 0.08)
                                                } else {
                                                    fade(theme_color(WARNING), 0.16)
                                                })
                                                .text_size(px(11.))
                                                .text_color(if online {
                                                    theme_color(MUTED)
                                                } else {
                                                    theme_color(WARNING)
                                                })
                                                .child(badge_text),
                                        ),
                                )
                                .child(
                                    div()
                                        .truncate()
                                        .text_xs()
                                        .text_color(theme_color(FAINT))
                                        .child(path_text),
                                ),
                        )
                        .child(ui_icon_button(
                            SharedString::from(format!("folder-more-{folder_id}")),
                            "more",
                            menu_open,
                            true,
                            show_focus,
                            cx,
                            move |this, event: &ClickEvent, _, cx| {
                                this.folder_menu = if this
                                    .folder_menu
                                    .as_ref()
                                    .is_some_and(|(id, _)| id == &folder_id_for_menu)
                                {
                                    None
                                } else {
                                    Some((folder_id_for_menu.clone(), event.position()))
                                };
                                cx.notify();
                            },
                        ))
                })
                .collect::<Vec<_>>();

            div()
                .id("folders-list-scroll")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .children(rows)
                .into_any_element()
        };

        let warnings_panel = (warning_count > 0 && self.folders_warnings_open).then(|| {
            div()
                .max_h(px(112.))
                .id("folders-warnings-scroll")
                .overflow_y_scroll()
                .px_3()
                .py_2()
                .rounded_md()
                .bg(fade(theme_color(WARNING), 0.08))
                .border_1()
                .border_color(fade(theme_color(WARNING), 0.25))
                .flex()
                .flex_col()
                .gap_1()
                .children(warnings.iter().take(20).map(|item| {
                    div()
                        .text_xs()
                        .text_color(theme_color(TEXT))
                        .child(format!("• {item}"))
                }))
        });

        let summary = if offline_count > 0 {
            format!(
                "在线 {online_count} / 共 {} 个文件夹 · {total_songs} 首曲目 · {offline_count} 个暂时离线",
                folders.len()
            )
        } else {
            format!("共 {} 个文件夹 · {total_songs} 首曲目", folders.len())
        };

        let card = modal_card(620.)
            .max_h(px(540.))
            .flex()
            .flex_col()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    if this.folder_menu.is_some() {
                        this.folder_menu = None;
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .px_5()
                    .pt_5()
                    .pb_3p5()
                    .border_b_1()
                    .border_color(theme_color(LINE))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme_color(TEXT))
                                    .child("音乐文件夹"),
                            )
                            .child(
                                div().text_xs().text_color(theme_color(MUTED)).child(
                                    "将分散在不同目录或外置磁盘里的配乐汇聚到同一个音乐库中。",
                                ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui_button(
                                "folders-add-btn",
                                "添加文件夹…",
                                Some("folder-plus"),
                                ButtonTone::Primary,
                                true,
                                false,
                                show_focus,
                                cx,
                                |this, window, cx| this.add_library_folders(window, cx),
                            ))
                            .child(ui_icon_button(
                                "folders-close-x",
                                "x",
                                false,
                                true,
                                show_focus,
                                cx,
                                |this, _, window, cx| this.close_folders_modal(window, cx),
                            )),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .px_5()
                    .py_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(body)
                    .children(warnings_panel),
            )
            .child(
                div()
                    .px_5()
                    .py_3p5()
                    .border_t_1()
                    .border_color(theme_color(LINE))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(ui_button(
                                "folders-rescan-low",
                                "重新扫描",
                                Some("refresh"),
                                ButtonTone::Ghost,
                                !folders.is_empty(),
                                true,
                                show_focus,
                                cx,
                                |this, window, cx| {
                                    this.folder_menu = None;
                                    this.rescan(window, cx);
                                },
                            ))
                            .when(warning_count > 0, |left| {
                                left.child(ui_button(
                                    "folders-warnings-toggle",
                                    format!("提示 ({warning_count})"),
                                    Some("alert"),
                                    ButtonTone::Ghost,
                                    true,
                                    true,
                                    show_focus,
                                    cx,
                                    |this, _, cx| {
                                        this.folders_warnings_open = !this.folders_warnings_open;
                                        cx.notify();
                                    },
                                ))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme_color(FAINT))
                                    .child(summary),
                            )
                            .child(ui_button(
                                "folders-done-btn",
                                "完成",
                                None,
                                ButtonTone::Secondary,
                                true,
                                true,
                                show_focus,
                                cx,
                                |this, window, cx| this.close_folders_modal(window, cx),
                            )),
                    ),
            );

        let backdrop = modal_backdrop(
            "folders-backdrop",
            &self.folders_focus,
            cx,
            |this, window, cx| {
                if this.folder_menu.is_some() {
                    this.folder_menu = None;
                    window.focus(&this.folders_focus);
                } else {
                    this.close_folders_modal(window, cx);
                }
            },
        )
        .child(card);

        let overlay_menu = self.folder_menu.as_ref().and_then(|(folder_id, position)| {
            let folder = self.library.folder_by_id(folder_id)?.clone();
            let reveal_path = folder.path.clone();
            let remove_id = folder.id.clone();
            let left = (position.x - px(164.))
                .max(px(12.))
                .min(window.viewport_size().width - px(188.));
            let top = (position.y + px(6.))
                .min(window.viewport_size().height - px(96.))
                .max(px(12.));
            Some(
                div()
                    .id("folder-row-menu")
                    .absolute()
                    .left(left)
                    .top(top)
                    .w(px(176.))
                    .p_1p5()
                    .rounded_lg()
                    .bg(theme_color(PANEL))
                    .border_1()
                    .border_color(fade(theme_color(CONTROL_TINT), 0.16))
                    .shadow_lg()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .id("folder-menu-reveal")
                            .h(px(30.))
                            .px_2p5()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(if folder.online {
                                theme_color(TEXT)
                            } else {
                                theme_color(FAINT)
                            })
                            .when(folder.online, |row| {
                                row.cursor_pointer()
                                    .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.08)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.folder_menu = None;
                                        cx.reveal_path(&reveal_path);
                                        cx.notify();
                                    }))
                            })
                            .child(icon(
                                "external",
                                13.,
                                theme_color(if folder.online { MUTED } else { FAINT }),
                            ))
                            .child(if folder.online {
                                "在访达中打开"
                            } else {
                                "在访达中打开（离线）"
                            }),
                    )
                    .child(
                        div()
                            .id("folder-menu-remove")
                            .h(px(30.))
                            .px_2p5()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(theme_color(DANGER))
                            .cursor_pointer()
                            .hover(|style| style.bg(fade(theme_color(DANGER), 0.10)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_remove_folder(remove_id.clone(), window, cx);
                            }))
                            .child(icon("trash", 13., theme_color(DANGER)))
                            .child("从音乐库移除"),
                    ),
            )
        });

        Some(backdrop.children(overlay_menu).into_any_element())
    }
}
