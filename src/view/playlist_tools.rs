//! 公开歌单批量读取、预览与文本导出。

use std::sync::atomic::{AtomicBool, Ordering};

use crate::playlist_migration::{self, MAX_INPUTS, Outcome};

use super::audio_tools::{
    tool_banner, tool_header_badge, tool_message, tool_page, tool_panel, tool_progress_bar,
};
use super::*;

const PREVIEW_ROWS: usize = 40;

pub(super) struct PlaylistTools {
    link: Entity<TextField>,
    inputs: Vec<String>,
    results: Vec<Outcome>,
    job: Option<Arc<AtomicBool>>,
    preview: Option<(usize, usize)>,
    message: Option<String>,
    _observe: Subscription,
}

impl PlaylistTools {
    pub(super) fn any_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.link.focus_handle(cx).is_focused(window)
    }

    pub(super) fn new(cx: &mut Context<Workspace>) -> Self {
        let link = cx.new(|cx| TextField::new(cx, "粘贴公开歌单链接或分享文案"));
        let observe = cx.observe(&link, |_, _, cx| cx.notify());
        Self {
            link,
            inputs: Vec::new(),
            results: Vec::new(),
            job: None,
            preview: None,
            message: None,
            _observe: observe,
        }
    }

    fn append(&mut self, text: &str) {
        let mut links = playlist_migration::share_inputs(text);
        if links.is_empty() {
            self.message = Some("请粘贴公开歌单的分享链接。".into());
            return;
        }
        let mut seen: HashSet<&str> = self.inputs.iter().map(String::as_str).collect();
        let mut new_links = Vec::new();
        for link in &links {
            if seen.insert(link) {
                new_links.push(link);
            }
        }
        let available = MAX_INPUTS.saturating_sub(self.inputs.len());
        self.message = if new_links.is_empty() {
            Some("这些链接已在列表中。".into())
        } else if new_links.len() > available {
            Some(format!("一次最多读取 {MAX_INPUTS} 个歌单。"))
        } else {
            None
        };
        links = new_links.into_iter().take(available).cloned().collect();
        self.inputs.extend(links);
    }
}

impl Drop for PlaylistTools {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            job.store(true, Ordering::Relaxed);
        }
    }
}

impl Workspace {
    pub(super) fn confirm_playlist_link(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let state = &self.playlist_tools;
        if !state.link.focus_handle(cx).is_focused(window) || state.link.read(cx).has_marked_text()
        {
            return false;
        }
        self.start_playlist_migration(cx);
        true
    }

    fn start_playlist_migration(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.playlist_tools;
        if state.job.is_some() {
            return;
        }
        let field = state.link.read(cx).text().to_owned();
        if !field.trim().is_empty() {
            state.append(&field);
            state.link.update(cx, |field, cx| field.set_text("", cx));
        }
        let inputs = state.inputs.clone();
        if inputs.is_empty() {
            state.message = Some("请先添加公开歌单链接".into());
            cx.notify();
            return;
        }
        let token = Arc::new(AtomicBool::new(false));
        state.results.clear();
        state.preview = None;
        state.message = None;
        state.job = Some(token.clone());
        cx.spawn(async move |this, cx| {
            for input in inputs {
                if token.load(Ordering::Relaxed) {
                    break;
                }
                let link = input.clone();
                let task_token = token.clone();
                let task = cx
                    .background_executor()
                    .spawn(async move { playlist_migration::resolve(&link, &task_token) });
                let result = task.await;
                if token.load(Ordering::Relaxed) {
                    break;
                }
                if this
                    .update(cx, |this, cx| {
                        let state = &mut this.playlist_tools;
                        if state.preview.is_none() && result.is_ok() {
                            state.preview = Some((state.results.len(), 0));
                        }
                        state.results.push(Outcome { input, result });
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                let state = &mut this.playlist_tools;
                state.job = None;
                state.message = token
                    .load(Ordering::Relaxed)
                    .then(|| "已停止读取，已完成的歌单可继续导出".into());
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn save_playlist_text(&mut self, cx: &mut Context<Self>) {
        let text = playlist_migration::export_text(&self.playlist_tools.results);
        if text.is_empty() {
            return;
        }
        let directory = dirs::download_dir().unwrap_or_else(|| self.library.root().to_path_buf());
        let answer = cx.prompt_for_new_path(&directory, Some("歌单迁移.txt"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = answer.await else {
                return;
            };
            let task = cx.background_executor().spawn(async move {
                bgm_core::persist::write_atomic(&path, text.as_bytes())
                    .map(|()| format!("已保存 {}", path.display()))
                    .unwrap_or_else(|error| format!("保存歌单失败：{error}"))
            });
            let message = task.await;
            this.update(cx, |this, cx| {
                this.playlist_tools.message = Some(message);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn playlist_page(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.playlist_tools;
        let busy = state.job.is_some();
        let filled = !state.inputs.is_empty() || !state.link.read(cx).text().trim().is_empty();
        let input_count = state.inputs.len();
        let results_count = state.results.len();
        let message = state.message.clone();
        let input = state.link.clone();
        let input_rows = state
            .inputs
            .iter()
            .enumerate()
            .map(|(index, link)| {
                div()
                    .id(SharedString::from(format!("playlist-input-{index}")))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(fade(theme_color(CONTROL_TINT), 0.03))
                    .child(
                        div()
                            .w(px(20.))
                            .text_size(px(11.))
                            .text_color(theme_color(FAINT))
                            .child(format!("{}.", index + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_xs()
                            .child(link.clone()),
                    )
                    .when(!busy, |row| {
                        row.child(self.action_button(
                            SharedString::from(format!("remove-playlist-link-{index}")),
                            "移除",
                            None,
                            super::controls::ButtonTone::Ghost,
                            true,
                            true,
                            cx,
                            move |this, _, cx| {
                                if index < this.playlist_tools.inputs.len() {
                                    this.playlist_tools.inputs.remove(index);
                                }
                                cx.notify();
                            },
                        ))
                    })
            })
            .collect::<Vec<_>>();
        let mut sources = tool_panel()
            .child(tool_header_badge(
                "公开歌单链接",
                "粘贴网易云音乐、QQ 音乐或汽水音乐的公开歌单链接，即可读取曲名和艺人。",
                format!("{input_count} / {MAX_INPUTS}"),
                input_count > 0,
            ))
            .children(message.map(|msg| tool_banner(msg, false)))
            .children(input_rows);
        if busy {
            let ratio = if input_count == 0 {
                0.0
            } else {
                (results_count as f32 / input_count as f32).clamp(0.0, 1.0)
            };
            sources = sources.child(tool_progress_bar(ratio)).child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(tool_message(format!(
                        "正在读取 · {results_count} / {input_count}"
                    )))
                    .child(
                        self.tiny_button("停止读取", "stop-playlists", cx, |this, _, _| {
                            if let Some(job) = &this.playlist_tools.job {
                                job.store(true, Ordering::Relaxed);
                            }
                        }),
                    ),
            );
        } else {
            sources = sources
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(super::audio_tools::tool_text_input(input)),
                        )
                        .child(self.settings_button(
                            "add-playlist-url",
                            "加入列表",
                            false,
                            filled && !state.link.read(cx).text().trim().is_empty(),
                            cx,
                            |this, _, cx| {
                                let state = &mut this.playlist_tools;
                                let text = state.link.read(cx).text().to_owned();
                                state.append(&text);
                                state.link.update(cx, |field, cx| field.set_text("", cx));
                                cx.notify();
                            },
                        )),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(self.tiny_button(
                            "从剪贴板添加",
                            "paste-playlist-urls",
                            cx,
                            |this, _, cx| {
                                let text = cx
                                    .read_from_clipboard()
                                    .and_then(|clip| clip.text())
                                    .unwrap_or_default();
                                this.playlist_tools.append(&text);
                                cx.notify();
                            },
                        ))
                        .when(input_count > 0, |row| {
                            row.child(self.tiny_button(
                                "清空列表",
                                "clear-playlist-urls",
                                cx,
                                |this, _, cx| {
                                    this.playlist_tools.inputs.clear();
                                    this.playlist_tools.message = None;
                                    cx.notify();
                                },
                            ))
                        })
                        .child(div().flex_1())
                        .child(self.action_button(
                            "read-playlists",
                            "读取歌单",
                            Some("download"),
                            super::controls::ButtonTone::Primary,
                            filled,
                            false,
                            cx,
                            |this, _, cx| this.start_playlist_migration(cx),
                        )),
                );
        }

        let state = &self.playlist_tools;
        let total = state
            .results
            .iter()
            .filter_map(|entry| entry.result.as_ref().ok())
            .map(|list| list.tracks.len())
            .sum::<usize>();
        let rows = state
            .results
            .iter()
            .enumerate()
            .map(|(index, outcome)| {
                let mut row = div()
                    .id(SharedString::from(format!("playlist-result-{index}")))
                    .py_2()
                    .border_b_1()
                    .border_color(theme_color(LINE))
                    .flex()
                    .flex_col()
                    .gap_1();
                match &outcome.result {
                    Err(error) => {
                        row = row
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(fade(theme_color(DANGER), 0.12))
                                            .text_size(px(10.))
                                            .text_color(theme_color(DANGER))
                                            .child("读取失败"),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_xs()
                                            .child(outcome.input.clone()),
                                    ),
                            )
                            .child(tool_message(error.clone()));
                    }
                    Ok(list) => {
                        let expanded = state.preview.is_some_and(|(selected, _)| selected == index);
                        let has_warning = list.warning.is_some();
                        row = row
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(fade(theme_color(ACCENT), 0.12))
                                            .text_size(px(10.))
                                            .text_color(theme_color(ACCENT))
                                            .child(list.platform),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(list.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(fade(
                                                theme_color(if has_warning {
                                                    WARNING
                                                } else {
                                                    CONTROL_TINT
                                                }),
                                                0.09,
                                            ))
                                            .text_size(px(10.))
                                            .text_color(theme_color(if has_warning {
                                                WARNING
                                            } else {
                                                MUTED
                                            }))
                                            .child(format!("{} 首", list.tracks.len())),
                                    )
                                    .child(self.tiny_button(
                                        if expanded { "收起" } else { "预览" },
                                        SharedString::from(format!("preview-playlist-{index}")),
                                        cx,
                                        move |this, _, cx| {
                                            this.playlist_tools.preview =
                                                (!expanded).then_some((index, 0));
                                            cx.notify();
                                        },
                                    )),
                            )
                            .children(
                                list.warning
                                    .as_ref()
                                    .map(|warning| tool_message(warning.clone())),
                            );
                    }
                }
                row
            })
            .collect::<Vec<_>>();
        let preview = state.preview.and_then(|(index, page)| {
            let list = state.results.get(index)?.result.as_ref().ok()?;
            let start = page.saturating_mul(PREVIEW_ROWS);
            let end = (start + PREVIEW_ROWS).min(list.tracks.len());
            let lines = list
                .tracks
                .get(start..end)?
                .iter()
                .enumerate()
                .map(|(offset, track)| {
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_1p5()
                        .border_b_1()
                        .border_color(theme_color(LINE))
                        .child(
                            div()
                                .w(px(34.))
                                .text_xs()
                                .text_color(theme_color(FAINT))
                                .child(format!("{}", start + offset + 1)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_sm()
                                .child(track.title.clone()),
                        )
                        .child(
                            div()
                                .max_w(px(240.))
                                .min_w(px(0.))
                                .truncate()
                                .text_xs()
                                .text_color(theme_color(MUTED))
                                .child(if track.artists.is_empty() {
                                    "—".into()
                                } else {
                                    track.artists.join("、")
                                }),
                        )
                })
                .collect::<Vec<_>>();
            Some(
                tool_panel()
                    .child(tool_header_badge(
                        "曲目预览",
                        list.title.clone(),
                        format!("{} 首", list.tracks.len()),
                        false,
                    ))
                    .children(lines)
                    .when(list.tracks.len() > PREVIEW_ROWS, |panel| {
                        panel.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(page > 0, |row| {
                                    row.child(self.tiny_button(
                                        "上一页",
                                        "prior-playlist-page",
                                        cx,
                                        move |this, _, cx| {
                                            this.playlist_tools.preview = Some((index, page - 1));
                                            cx.notify();
                                        },
                                    ))
                                })
                                .child(tool_message(format!(
                                    "第 {}–{end} / {} 首",
                                    start + 1,
                                    list.tracks.len()
                                )))
                                .when(end < list.tracks.len(), |row| {
                                    row.child(self.tiny_button(
                                        "下一页",
                                        "next-playlist-page",
                                        cx,
                                        move |this, _, cx| {
                                            this.playlist_tools.preview = Some((index, page + 1));
                                            cx.notify();
                                        },
                                    ))
                                }),
                        )
                    }),
            )
        });
        let outputs = tool_panel()
            .child(tool_header_badge(
                "读取结果",
                format!("共 {results_count} 个来源 · 可导出 {total} 首曲目"),
                format!("{total} 首"),
                total > 0,
            ))
            .children(rows)
            .when(total > 0, |panel| {
                panel.child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(self.hero_button(
                            "save-playlists",
                            "download",
                            "保存曲目清单…",
                            true,
                            cx,
                            |this, _, cx| this.save_playlist_text(cx),
                        ))
                        .child(self.tiny_button(
                            "复制曲目清单",
                            "copy-playlists",
                            cx,
                            |this, _, cx| {
                                let text =
                                    playlist_migration::export_text(&this.playlist_tools.results);
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                                this.playlist_tools.message = Some("已复制曲名和艺人".into());
                                cx.notify();
                            },
                        )),
                )
            });
        tool_page("playlist-migration-page", "歌单迁移")
            .child(sources)
            .when(results_count > 0, |page| page.child(outputs))
            .children(preview)
            .into_any_element()
    }
}
