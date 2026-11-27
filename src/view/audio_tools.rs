//! 曲库指纹、以声搜声和查重页面。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::TryRecvError;

use crate::audio_index::{self, DuplicateGroup, FileId, Hit, Index, Progress, Query, Settings};
use crate::microphone::Recording;

use super::*;

pub(super) struct AudioTools {
    settings_path: PathBuf,
    cache_path: PathBuf,
    settings: Settings,
    roots: Vec<crate::audio_index::IndexedRoot>,
    index: Option<Arc<Index>>,
    cached_checked: bool,
    scan_revision: Option<u64>,
    indexing: Option<Arc<Progress>>,
    retry: bool,
    hits: Vec<Hit>,
    query: Option<Arc<AtomicBool>>,
    query_message: Option<String>,
    duplicates_message: Option<String>,
    index_error: Option<String>,
    duplicates: Vec<DuplicateGroup>,
    selected: HashSet<FileId>,
    duplicates_open: bool,
    failures_open: bool,
    settings_open: bool,
    search_options_open: bool,
    recording: Option<Recording>,
}

impl AudioTools {
    pub(super) fn new(config: &Path, roots: Vec<crate::audio_index::IndexedRoot>) -> Self {
        let settings_path = config.with_file_name("audio-index-settings.json");
        let settings = std::fs::read(&settings_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            settings_path,
            cache_path: config.with_file_name("audio-index.json"),
            settings,
            roots,
            index: None,
            cached_checked: false,
            scan_revision: None,
            indexing: None,
            retry: false,
            hits: Vec::new(),
            query: None,
            query_message: None,
            duplicates_message: None,
            index_error: None,
            duplicates: Vec::new(),
            selected: HashSet::new(),
            duplicates_open: false,
            failures_open: false,
            settings_open: false,
            search_options_open: false,
            recording: None,
        }
    }

    pub(super) fn recording(&self) -> bool {
        self.recording.is_some()
    }

    fn update_settings(&mut self, settings: Settings) {
        match bgm_core::persist::write_json(&self.settings_path, &settings) {
            Ok(()) => self.settings = settings,
            Err(error) => self.index_error = Some(error.to_string()),
        }
    }

    fn discard_query(&mut self) {
        if let Some(query) = self.query.take() {
            query.store(true, Ordering::Relaxed);
        }
        self.recording = None;
        self.hits.clear();
        self.query_message = None;
    }

    fn reset_index(&mut self) {
        if let Some(progress) = &self.indexing {
            progress.cancelled.store(true, Ordering::Relaxed);
        }
        self.discard_query();
        self.index = None;
        self.scan_revision = None;
        self.cached_checked = false;
        self.duplicates.clear();
        self.duplicates_message = None;
        self.selected.clear();
        self.index_error = None;
    }
}

impl Drop for AudioTools {
    fn drop(&mut self) {
        self.reset_index();
    }
}

impl Workspace {
    pub(super) fn poll_audio_tools(&mut self, cx: &mut Context<Self>) {
        let current_roots = self.library.indexed_roots();
        let tools = &mut self.audio_tools;
        if tools.roots != current_roots {
            tools.reset_index();
            tools.roots = current_roots.clone();
        }
        if self.view != AppView::AudioTools || self.now_playing_open {
            tools.recording = None;
        }
        let capture =
            tools
                .recording
                .as_ref()
                .and_then(|recording| match recording.result.try_recv() {
                    Ok(result) => Some(result),
                    Err(TryRecvError::Disconnected) => Some(Err("麦克风录音已中断".into())),
                    Err(TryRecvError::Empty) => None,
                });
        if let Some(capture) = capture {
            tools.recording = None;
            match capture {
                Ok(capture) => self.search_audio(
                    Query::Recording {
                        rate: capture.rate,
                        mono: capture.mono,
                    },
                    cx,
                ),
                Err(error) => self.audio_tools.query_message = Some(error),
            }
            cx.notify();
        } else if tools.recording.is_some() {
            cx.notify();
        }

        let tools = &mut self.audio_tools;
        let revision = self.library.scan_revision();
        if !tools.settings.enabled
            || tools.indexing.is_some()
            || tools.scan_revision == Some(revision)
        {
            return;
        }
        let roots = tools.roots.clone();
        if roots.is_empty() {
            tools.index = None;
            tools.duplicates.clear();
            tools.selected.clear();
            return;
        }
        let categories = self
            .library
            .snapshot
            .categories
            .iter()
            .map(|category| category.id.clone())
            .collect::<Vec<_>>();
        let songs = self
            .library
            .snapshot
            .songs
            .iter()
            .map(|song| {
                let folder = self.library.folder_for_song(&song.key);
                let name = if song.category == crate::library::UNCATEGORIZED_CATEGORY_ID {
                    folder
                        .and_then(|entry| Path::new(&song.path).strip_prefix(&entry.path).ok())
                        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_else(|| song.file_name.clone())
                } else {
                    song.file_name.clone()
                };
                FileId {
                    folder_id: folder.map_or_else(String::new, |entry| entry.id.clone()),
                    category: song.category.clone(),
                    name,
                    song_key: song.key.clone(),
                }
            })
            .collect();
        let previous = tools.index.clone();
        let load_cached = !tools.cached_checked;
        let retry = std::mem::take(&mut tools.retry);
        let cache_path = tools.cache_path.clone();
        let progress = Arc::new(Progress::default());
        tools.indexing = Some(progress.clone());
        tools.scan_revision = Some(revision);
        tools.cached_checked = true;
        tools.index_error = None;
        let task_progress = progress.clone();
        let task = cx.background_executor().spawn(async move {
            let cached = load_cached
                .then(|| Index::load_many(&cache_path, &roots))
                .flatten();
            let update = audio_index::refresh_many(
                &roots,
                &categories,
                previous.as_deref().or(cached.as_ref()),
                retry,
                &task_progress,
            )?;
            let mut warning = None;
            if let Some(index) = &update
                && !task_progress.cancelled.load(Ordering::Relaxed)
            {
                warning = index.save(&cache_path).err();
            }
            Ok::<_, String>(update.or(cached).map(|index| {
                let duplicates = index.duplicates(&songs);
                (Arc::new(index), duplicates, warning)
            }))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let current_roots = this.library.indexed_roots();
                let tools = &mut this.audio_tools;
                tools.indexing = None;
                if !progress.cancelled.load(Ordering::Relaxed)
                    && tools.settings.enabled
                    && tools.roots == current_roots
                {
                    match result {
                        Ok(Some((index, duplicates, warning))) => {
                            tools.index = Some(index);
                            tools.duplicates = duplicates;
                            tools.duplicates_message = None;
                            tools.selected.clear();
                            tools.index_error =
                                warning.map(|message| format!("保存索引失败：{message}"));
                        }
                        Err(error) => tools.index_error = Some(error),
                        Ok(None) => {}
                    }
                }
                this.poll_audio_tools(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn enable_audio_index(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let roots = self.library.indexed_roots();
        let answer = window.prompt(
            PromptLevel::Info,
            "启用音频索引？",
            Some("应用会在本机分析当前曲库的音频。启用后，曲库变化时会自动更新索引。"),
            &[
                PromptButton::Ok("启用".into()),
                PromptButton::Cancel("取消".into()),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| {
                    if roots != this.library.indexed_roots() {
                        return;
                    }
                    let tools = &mut this.audio_tools;
                    tools.update_settings(Settings {
                        enabled: true,
                        ..tools.settings
                    });
                    this.poll_audio_tools(cx);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn disable_audio_index(&mut self, cx: &mut Context<Self>) {
        let tools = &mut self.audio_tools;
        tools.update_settings(Settings {
            enabled: false,
            ..tools.settings
        });
        if !tools.settings.enabled {
            tools.reset_index();
        }
        cx.notify();
    }

    fn pick_query_audio(&mut self, cx: &mut Context<Self>) {
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择用于搜索的音频".into()),
        });
        let roots = self.library.indexed_roots();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = selection.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    if this.view == AppView::AudioTools && this.library.indexed_roots() == roots {
                        this.search_audio(Query::File(path), cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    fn record_search_audio(&mut self, cx: &mut Context<Self>) {
        if self.microphone_active() {
            return;
        }
        self.speech_tools.stop_preview();
        self.audio.pause();
        self.playing = false;
        let tools = &mut self.audio_tools;
        tools.discard_query();
        match Recording::start(5, 30) {
            Ok(recording) => tools.recording = Some(recording),
            Err(error) => tools.query_message = Some(error),
        }
        cx.notify();
    }

    fn search_audio(&mut self, input: Query, cx: &mut Context<Self>) {
        let current_roots = self.library.indexed_roots();
        let tools = &mut self.audio_tools;
        let Some(index) = tools.index.clone() else {
            return;
        };
        if index.roots != current_roots || self.microphone_active() {
            return;
        }
        let tools = &mut self.audio_tools;
        tools.discard_query();
        let cancelled = Arc::new(AtomicBool::new(false));
        tools.query = Some(cancelled.clone());
        let token = cancelled.clone();
        let limit = tools.settings.limit;
        let task = cx
            .background_executor()
            .spawn(async move { index.search(input, limit, &token) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                let current_roots = this.library.indexed_roots();
                let tools = &mut this.audio_tools;
                if !tools
                    .query
                    .as_ref()
                    .is_some_and(|query| Arc::ptr_eq(query, &cancelled))
                    || tools.roots != current_roots
                {
                    return;
                }
                tools.query = None;
                match result {
                    Ok(hits) => tools.hits = hits,
                    Err(error) => tools.query_message = Some(error),
                }
                if tools.hits.is_empty() && tools.query_message.is_none() {
                    tools.query_message = Some("没有找到足够相似的曲目".into());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn trash_selected_duplicates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tools = &self.audio_tools;
        let count = tools.selected.len();
        if count == 0 {
            return;
        }
        let roots = tools.roots.clone();
        let groups = tools
            .duplicates
            .iter()
            .filter(|group| {
                group
                    .files
                    .iter()
                    .any(|(id, _)| tools.selected.contains(id))
            })
            .cloned()
            .collect::<Vec<_>>();
        let selected = tools.selected.clone();
        let answer = window.prompt(
            PromptLevel::Warning,
            "将选中的重复文件移入回收站？",
            Some(&format!(
                "将移入曲库回收站 {count} 个文件，每组至少保留一个文件。"
            )),
            &[
                PromptButton::Ok("移入回收站".into()),
                PromptButton::Cancel("取消".into()),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            this.update(cx, |this, cx| {
                if this.library.indexed_roots() != roots || !this.audio_tools.settings.enabled {
                    return;
                }
                match this.library.trash_audio_copies(&groups, &selected) {
                    Ok(mutation) => {
                        this.apply_mutation(mutation);
                        this.reconcile_snapshot();
                        this.audio_tools.selected.clear();
                        this.audio_tools.duplicates_message = Some(this.notice.clone());
                        this.poll_audio_tools(cx);
                    }
                    Err(error) => this.audio_tools.duplicates_message = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn audio_tools_page(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = &self.audio_tools;
        let enabled = state.settings.enabled;
        let indexing_progress = state.indexing.as_ref().map(|progress| {
            let total = progress.total.load(Ordering::Relaxed);
            let complete = progress.complete.load(Ordering::Relaxed);
            (complete, total)
        });
        let (badge_text, badge_active, status) = if let Some((complete, total)) = indexing_progress
        {
            (
                "索引中",
                true,
                if total == 0 {
                    "正在检查曲库文件…".into()
                } else {
                    format!("正在建立音频指纹 · {complete} / {total}")
                },
            )
        } else if let Some(index) = &state.index {
            (
                "已就绪",
                true,
                format!(
                    "已索引 {} 个可检索文件 · {} 项失败",
                    index.available(),
                    index.failures()
                ),
            )
        } else if enabled {
            ("等待扫描", false, "准备检查曲库并建立索引…".into())
        } else {
            (
                "未启用",
                false,
                "启用后可用本地音频或麦克风片段查找相似音乐，并检测同曲重复副本。".into(),
            )
        };
        let failures = state
            .index
            .as_ref()
            .map(|index| index.failures_preview())
            .unwrap_or_default();
        let failure_count = failures.len();
        let failures_open = state.failures_open;
        let settings_open = state.settings_open;
        let search_options_open = state.search_options_open;
        let mut index_panel = tool_panel()
            .child(tool_header_badge(
                "音频索引",
                status,
                badge_text,
                badge_active,
            ))
            .when_some(indexing_progress, |panel, (complete, total)| {
                let ratio = if total == 0 {
                    0.0
                } else {
                    (complete as f32 / total as f32).clamp(0.0, 1.0)
                };
                panel.child(tool_progress_bar(ratio))
            })
            .children(
                state
                    .index_error
                    .as_ref()
                    .map(|error| tool_banner(error.clone(), true)),
            );
        let indexing = state.indexing.is_some();
        if enabled {
            index_panel = index_panel.child(div().flex().child(self.tiny_button(
                if settings_open {
                    "收起索引管理"
                } else {
                    "管理索引"
                },
                "toggle-index-settings",
                cx,
                |this, _, cx| {
                    this.audio_tools.settings_open = !this.audio_tools.settings_open;
                    cx.notify();
                },
            )));
        }
        if enabled && (settings_open || failure_count > 0) {
            index_panel = index_panel
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .when(!indexing && settings_open, |row| {
                            row.child(self.tiny_button(
                                if failure_count > 0 {
                                    "重试失败文件"
                                } else {
                                    "重新扫描文件"
                                },
                                "retry-index",
                                cx,
                                |this, _, cx| {
                                    this.audio_tools.scan_revision = None;
                                    this.audio_tools.retry = true;
                                    this.poll_audio_tools(cx);
                                },
                            ))
                        })
                        .when(failure_count > 0, |row| {
                            row.child(self.tiny_button(
                                if failures_open {
                                    "收起失败详情"
                                } else {
                                    "查看失败详情"
                                },
                                "toggle-index-failures",
                                cx,
                                |this, _, cx| {
                                    this.audio_tools.failures_open =
                                        !this.audio_tools.failures_open;
                                    cx.notify();
                                },
                            ))
                        })
                        .when(settings_open, |row| {
                            row.child(self.tiny_button(
                                "关闭索引",
                                "disable-index",
                                cx,
                                |this, _, cx| this.disable_audio_index(cx),
                            ))
                        }),
                )
                .when(failures_open && !failures.is_empty(), |panel| {
                    panel.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .bg(fade(theme_color(CONTROL_TINT), 0.04))
                            .children(failures.into_iter().map(tool_message)),
                    )
                });
        } else if !enabled {
            index_panel = index_panel.child(div().flex().child(self.hero_button(
                "enable-index",
                "search",
                "启用音频索引",
                true,
                cx,
                |this, window, cx| this.enable_audio_index(window, cx),
            )));
        }

        let state = &self.audio_tools;
        let usable = enabled
            && state
                .index
                .as_ref()
                .is_some_and(|index| index.available() > 0);
        let elapsed = state
            .recording
            .as_ref()
            .map(|recording| recording.elapsed.load(Ordering::Relaxed));
        let searching = state.query.is_some();
        let limit = state.settings.limit.clamp(1, 50);
        let show_folder = self.library.folders().len() > 1;
        let results = state
            .hits
            .iter()
            .enumerate()
            .map(|(rank, hit)| {
                let key = hit.file.key();
                let path = state
                    .index
                    .as_ref()
                    .and_then(|index| index.file_path(&hit.file))
                    .unwrap_or_else(|| hit.file.path(self.library.root()));
                let reveal_path = path.clone();
                let song = self
                    .library
                    .song(&key)
                    .filter(|song| Path::new(&song.path) == path);
                let name = song.map_or_else(|| hit.file.name.clone(), |song| song.title.clone());
                let offset = format_offset(hit.offset_seconds);
                let folder_name = self
                    .library
                    .folder_by_id(&hit.file.folder_id)
                    .map(|folder| folder.display_name.clone());
                let category_label =
                    if hit.file.category == crate::library::UNCATEGORIZED_CATEGORY_ID {
                        "未分类".to_owned()
                    } else {
                        hit.file.category.clone()
                    };
                let subtitle = if show_folder {
                    if let Some(folder_name) = folder_name {
                        format!(
                            "{folder_name} · {category_label}/{} · 匹配位置 {offset}",
                            hit.file.name
                        )
                    } else {
                        format!("{category_label}/{} · 匹配位置 {offset}", hit.file.name)
                    }
                } else {
                    format!("{category_label}/{} · 匹配位置 {offset}", hit.file.name)
                };
                div()
                    .id(SharedString::from(format!(
                        "hit-{rank}-{}-{}-{}",
                        hit.file.folder_id, hit.file.category, hit.file.name
                    )))
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_2()
                    .px_2()
                    .rounded_md()
                    .border_b_1()
                    .border_color(theme_color(LINE))
                    .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.04)))
                    .child(
                        div()
                            .w(px(22.))
                            .text_xs()
                            .text_color(theme_color(FAINT))
                            .child(format!("#{}", rank + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme_color(FAINT))
                                    .child(subtitle),
                            ),
                    )
                    .child(
                        div()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(fade(theme_color(ACCENT), 0.12))
                            .text_xs()
                            .text_color(theme_color(ACCENT))
                            .child(format!("{}%", hit.score)),
                    )
                    .child(self.tiny_button(
                        "在访达中显示",
                        SharedString::from(format!("reveal-hit-{rank}")),
                        cx,
                        move |_, _, cx| cx.reveal_path(&reveal_path),
                    ))
                    .when(song.is_some() && elapsed.is_none(), |row| {
                        row.child(self.tiny_button(
                            "试听",
                            SharedString::from(format!("audition-hit-{rank}")),
                            cx,
                            move |this, _, cx| {
                                this.play_key(key.clone());
                                cx.notify();
                            },
                        ))
                    })
            })
            .collect::<Vec<_>>();
        let query_message = state.query_message.clone();
        let mut search_panel = tool_panel()
            .child(tool_header(
                "以声搜声",
                "选择本地音频片段，或录制 5–30 秒环境人声/旋律来检索曲库。",
            ))
            .when(!usable && !searching, |panel| {
                panel.child(tool_message(if !enabled {
                    "启用音频索引后，即可搜索本地曲库。".into()
                } else if indexing {
                    "索引正在建立，完成后即可搜索。".into()
                } else {
                    "曲库内暂无可搜索的音频。导入音乐后即可建立索引。".into()
                }))
            })
            .when(searching, |panel| {
                panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(tool_message("正在查找匹配曲目…".into()))
                        .child(self.tiny_button(
                            "取消搜索",
                            "cancel-audio-query",
                            cx,
                            |this, _, cx| {
                                this.audio_tools.discard_query();
                                this.audio_tools.query_message = Some("已取消搜索。".into());
                                cx.notify();
                            },
                        )),
                )
            });
        if let Some(elapsed) = elapsed {
            let seconds = elapsed as f64 / 1000.0;
            let ready = elapsed >= 5_000;
            search_panel = search_panel
                .child(tool_progress_bar((seconds / 30.0).clamp(0.0, 1.0) as f32))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .flex_wrap()
                        .child(tool_message(if elapsed == 0 {
                            "正在等待麦克风输入…".into()
                        } else if !ready {
                            format!(
                                "录制 {seconds:.1} / 30.0 秒（还需 {:.1} 秒可结束）",
                                5.0 - seconds
                            )
                        } else {
                            format!("录制 {seconds:.1} / 30.0 秒 · 可随时结束并搜索")
                        }))
                        .when(ready, |row| {
                            row.child(self.hero_button(
                                "finish-mic-query",
                                "search",
                                "结束并搜索",
                                true,
                                cx,
                                |this, _, _| {
                                    if let Some(recording) = &this.audio_tools.recording {
                                        recording.finish();
                                    }
                                },
                            ))
                        })
                        .child(self.tiny_button(
                            "取消",
                            "cancel-mic-query",
                            cx,
                            |this, _, cx| {
                                this.audio_tools.recording = None;
                                cx.notify();
                            },
                        )),
                );
        } else if usable && !searching {
            search_panel = search_panel
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .child(self.hero_button(
                            "pick-query-audio",
                            "folder",
                            "选择音频文件…",
                            true,
                            cx,
                            |this, _, cx| this.pick_query_audio(cx),
                        ))
                        .child(self.hero_button(
                            "record-query-audio",
                            "audio-lines",
                            "录一段声音",
                            false,
                            cx,
                            |this, _, cx| this.record_search_audio(cx),
                        ))
                        .child(div().flex_1())
                        .child(self.tiny_button(
                            if search_options_open {
                                "收起搜索选项"
                            } else {
                                "搜索选项"
                            },
                            "query-options",
                            cx,
                            |this, _, cx| {
                                this.audio_tools.search_options_open =
                                    !this.audio_tools.search_options_open;
                                cx.notify();
                            },
                        )),
                )
                .when(search_options_open, |panel| {
                    panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(tool_message(format!("最多显示 {limit} 条结果")))
                            .child(self.settings_button(
                                "query-less",
                                "减少条数",
                                false,
                                limit > 1,
                                cx,
                                |this, _, cx| this.set_query_limit(-1, cx),
                            ))
                            .child(self.settings_button(
                                "query-more",
                                "增加条数",
                                false,
                                limit < 50,
                                cx,
                                |this, _, cx| this.set_query_limit(1, cx),
                            )),
                    )
                });
        }
        search_panel = search_panel
            .children(query_message.map(|message| tool_message(message)))
            .children(results);

        let state = &self.audio_tools;
        let duplicate_count = state.duplicates.len();
        let selected = state.selected.len();
        let duplicates_message = state.duplicates_message.clone();
        let groups =
            if state.duplicates_open {
                state
                    .duplicates
                    .iter()
                    .enumerate()
                    .map(|(i, group)| {
                        div()
                            .id(SharedString::from(format!("duplicate-group-{i}")))
                            .p_3()
                            .rounded_md()
                            .bg(fade(theme_color(CONTROL_TINT), 0.03))
                            .border_1()
                            .border_color(theme_color(LINE))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(theme_color(TEXT))
                                            .child(format!(
                                                "重复组 #{} · {} 个文件",
                                                i + 1,
                                                group.files.len()
                                            )),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(fade(theme_color(CONTROL_TINT), 0.08))
                                            .text_size(px(10.))
                                            .text_color(theme_color(MUTED))
                                            .child(format!("相似度 {}%", group.similarity)),
                                    ),
                            )
                            .children(group.files.iter().enumerate().map(|(j, (file, _))| {
                                let active = state.selected.contains(file);
                                let select = file.clone();
                                let reveal_path = state
                                    .index
                                    .as_ref()
                                    .and_then(|index| index.file_path(file))
                                    .unwrap_or_else(|| file.path(self.library.root()));
                                let shown = self.library.song(&file.key()).is_some();
                                let ext = Path::new(&file.name)
                                    .extension()
                                    .and_then(|ext| ext.to_str())
                                    .unwrap_or("AUDIO")
                                    .to_ascii_uppercase();
                                let folder_prefix = if show_folder {
                                    self.library
                                        .folder_by_id(&file.folder_id)
                                        .map(|folder| format!("{} · ", folder.display_name))
                                        .unwrap_or_default()
                                } else {
                                    String::new()
                                };
                                let cat_name =
                                    if file.category == crate::library::UNCATEGORIZED_CATEGORY_ID {
                                        "未分类"
                                    } else {
                                        file.category.as_str()
                                    };
                                div()
                                    .id(SharedString::from(format!(
                                        "duplicate-{i}-{j}-{}-{}-{}",
                                        file.folder_id, file.category, file.name
                                    )))
                                    .flex()
                                    .gap_2()
                                    .items_center()
                                    .py_1()
                                    .child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded(px(4.))
                                            .bg(fade(theme_color(CONTROL_TINT), 0.08))
                                            .text_size(px(10.))
                                            .text_color(theme_color(MUTED))
                                            .child(ext),
                                    )
                                    .child(
                                        div().flex_1().min_w(px(0.)).truncate().text_sm().child(
                                            format!("{folder_prefix}{cat_name}/{}", file.name),
                                        ),
                                    )
                                    .when(shown, |row| {
                                        row.child(
                                            div()
                                                .px_2()
                                                .py_0p5()
                                                .rounded_full()
                                                .bg(fade(theme_color(ACCENT), 0.12))
                                                .text_size(px(10.))
                                                .text_color(theme_color(ACCENT))
                                                .child("曲库可见"),
                                        )
                                    })
                                    .child(self.tiny_button(
                                        "在访达中显示",
                                        SharedString::from(format!("reveal-duplicate-{i}-{j}")),
                                        cx,
                                        move |_, _, cx| cx.reveal_path(&reveal_path),
                                    ))
                                    .child(self.tiny_button(
                                        if active {
                                            "取消选中"
                                        } else {
                                            "选中此副本"
                                        },
                                        SharedString::from(format!("select-duplicate-{i}-{j}")),
                                        cx,
                                        move |this, _, cx| {
                                            this.select_audio_copy(&select, cx);
                                        },
                                    ))
                            }))
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
        let duplicates = tool_panel()
            .child(tool_header_badge(
                "查找重复曲目",
                "按音频内容比对曲目。每组至少保留一个文件。",
                if duplicate_count == 0 {
                    if usable {
                        "未发现重复"
                    } else {
                        "等待索引"
                    }
                } else {
                    "发现重复"
                },
                duplicate_count > 0,
            ))
            .children(duplicates_message.map(tool_message))
            .when(duplicate_count > 0, |panel| {
                panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme_color(MUTED))
                                .child(format!("共 {duplicate_count} 组重复文件")),
                        )
                        .child(self.tiny_button(
                            if self.audio_tools.duplicates_open {
                                "收起"
                            } else {
                                "查看重复组"
                            },
                            "show-duplicates",
                            cx,
                            |this, _, cx| {
                                this.audio_tools.duplicates_open =
                                    !this.audio_tools.duplicates_open;
                                cx.notify();
                            },
                        ))
                        .when(selected > 0 && !indexing, |row| {
                            row.child(self.action_button(
                                "trash-duplicates",
                                "移入回收站",
                                None,
                                controls::ButtonTone::Danger,
                                true,
                                true,
                                cx,
                                |this, window, cx| this.trash_selected_duplicates(window, cx),
                            ))
                        })
                        .when(selected > 0, |row| {
                            row.child(tool_message(format!("已选 {selected} 项")))
                        }),
                )
            })
            .children(groups);
        tool_page("audio-tools-page", "音频工具")
            .child(index_panel)
            .child(search_panel)
            .child(duplicates)
            .into_any_element()
    }

    fn set_query_limit(&mut self, delta: isize, cx: &mut Context<Self>) {
        let tools = &mut self.audio_tools;
        let limit = tools
            .settings
            .limit
            .clamp(1, 50)
            .saturating_add_signed(delta)
            .clamp(1, 50);
        tools.update_settings(Settings {
            limit,
            ..tools.settings
        });
        cx.notify();
    }

    fn select_audio_copy(&mut self, file: &FileId, cx: &mut Context<Self>) {
        let tools = &mut self.audio_tools;
        if !tools.selected.remove(file)
            && let Some(group) = tools
                .duplicates
                .iter()
                .find(|group| group.files.iter().any(|(id, _)| id == file))
        {
            if group
                .files
                .iter()
                .filter(|(id, _)| !tools.selected.contains(id))
                .count()
                > 1
            {
                tools.selected.insert(file.clone());
                tools.duplicates_message = None;
            } else {
                tools.duplicates_message = Some("每组重复文件至少保留一个".into());
            }
        }
        cx.notify();
    }
}

fn format_offset(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

pub(super) fn tool_page(id: &'static str, title: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .px_6()
        .pb_4()
        .child(
            div()
                .flex_none()
                .text_size(px(PAGE_TITLE))
                .font_weight(FontWeight::BOLD)
                .child(title),
        )
}

pub(super) fn tool_text_input(input: Entity<TextField>) -> gpui::Div {
    div()
        .px_2()
        .py_0p5()
        .rounded_md()
        .bg(theme_color(BG))
        .border_1()
        .border_color(theme_color(LINE))
        .child(input)
}

pub(super) fn tool_panel() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_4()
        .rounded_lg()
        .bg(theme_color(PANEL))
        .border_1()
        .border_color(theme_color(LINE))
        .flex_none()
}

pub(super) fn tool_header(title: &'static str, subtitle: impl Into<SharedString>) -> gpui::Div {
    div()
        .child(
            div()
                .text_base()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme_color(MUTED))
                .child(subtitle.into()),
        )
}

pub(super) fn tool_header_badge(
    title: &'static str,
    subtitle: impl Into<SharedString>,
    badge: impl Into<SharedString>,
    accent: bool,
) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap_3()
        .child(tool_header(title, subtitle))
        .child(
            div()
                .flex_none()
                .px_2p5()
                .py_1()
                .rounded_full()
                .bg(if accent {
                    fade(theme_color(ACCENT), 0.13)
                } else {
                    fade(theme_color(CONTROL_TINT), 0.08)
                })
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(if accent {
                    theme_color(ACCENT)
                } else {
                    theme_color(MUTED)
                })
                .child(badge.into()),
        )
}

pub(super) fn tool_progress_bar(ratio: f32) -> gpui::Div {
    let ratio = ratio.clamp(0.0, 1.0);
    div()
        .h(px(6.))
        .w_full()
        .rounded_full()
        .bg(fade(theme_color(CONTROL_TINT), 0.1))
        .overflow_hidden()
        .child(
            div()
                .h_full()
                .w(relative(ratio))
                .rounded_full()
                .bg(theme_color(ACCENT)),
        )
}

pub(super) fn tool_banner(text: String, danger: bool) -> gpui::Div {
    let color = if danger { DANGER } else { WARNING };
    div()
        .px_3()
        .py_2()
        .rounded_md()
        .bg(fade(theme_color(color), 0.09))
        .border_1()
        .border_color(fade(theme_color(color), 0.25))
        .text_xs()
        .text_color(theme_color(color))
        .child(text)
}

pub(super) fn tool_message(text: String) -> gpui::Div {
    div().text_xs().text_color(theme_color(MUTED)).child(text)
}
