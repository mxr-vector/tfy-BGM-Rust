//! 曲库窗口。布局是顶栏、侧栏、曲目列表和悬浮播放器，交互走 GPUI 动作与系统文件框。

use std::ops::Range;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bgm_core::{PlayMode, Song, SortBy};
use gpui::{
    App, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable, FontWeight, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point, PromptButton,
    PromptLevel, Rgba, SharedString, Subscription, Window, div, linear_color_stop, prelude::*, px,
    relative, rgb, uniform_list,
};

use crate::actions::{
    Confirm, Dismiss, FocusSearch, Mute, PlayPause, SeekBack, SeekForward, VolumeDown, VolumeUp,
};
use crate::audio::AudioEngine;
use crate::icons::{category_icon, icon};
use crate::library::{Library, LibraryFilter, Mutation};
use crate::text_input::TextField;

const BG: u32 = 0x07110e;
const SIDE: u32 = 0x09130f;
const PANEL: u32 = 0x0d1b17;
const PANEL_LIGHT: u32 = 0x123d2d;
const LINE: u32 = 0x26372f;
const TEXT: u32 = 0xf1f7f4;
const MUTED: u32 = 0x91a59b;
const FAINT: u32 = 0x71867a;
const ACCENT: u32 = 0x19d98b;
const ACCENT_INK: u32 = 0x04150e;
const FAVORITE: u32 = 0xf472b6;
const DANGER: u32 = 0xe28d82;

#[derive(Clone)]
enum Dialog {
    Rename { key: String },
    NewCategory,
    Move { key: String },
    Import { files: Vec<PathBuf> },
}

pub struct Workspace {
    library: Library,
    audio: AudioEngine,
    search: Entity<TextField>,
    dialog_field: Entity<TextField>,
    focus: FocusHandle,
    _observe: Subscription,
    filter: LibraryFilter,
    query: String,
    dialog: Option<Dialog>,
    action_menu: Option<(String, Point<Pixels>)>,
    queue: Vec<String>,
    queue_index: usize,
    playing: bool,
    position: Duration,
    duration: Duration,
    notice: String,
    seek_bounds: Option<Bounds<Pixels>>,
    volume_bounds: Option<Bounds<Pixels>>,
    dragging_seek: bool,
    dragging_volume: bool,
    seen_audio: bool,
    track_started: Instant,
    failure_streak: u32,
    visible: Vec<Song>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let library = Library::open();
        let mut audio = AudioEngine::open();
        audio.set_volume(if library.prefs.muted {
            0.0
        } else {
            library.prefs.volume
        });
        audio.set_speed(library.prefs.playback_rate);
        let notice = audio
            .open_error()
            .map(str::to_owned)
            .or_else(|| library.snapshot.warnings.first().cloned())
            .unwrap_or_else(|| format!("已载入 {} 首", library.snapshot.songs.len()));

        let search = cx.new(|cx| TextField::new(cx, "搜索曲名或分类，按 / 聚焦"));
        let dialog_field = cx.new(|cx| TextField::new(cx, "输入名称"));
        let observe = cx.observe(&search, |this, search, cx| {
            let query = search.read(cx).text().to_owned();
            if this.query != query {
                this.query = query;
                cx.notify();
            }
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let was_playing = this.playing;
                        let position = this.position;
                        this.poll_playback();
                        if this.playing || this.position != position || was_playing {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            library,
            audio,
            search,
            dialog_field,
            focus: cx.focus_handle(),
            _observe: observe,
            filter: LibraryFilter::All,
            query: String::new(),
            dialog: None,
            action_menu: None,
            queue: Vec::new(),
            queue_index: 0,
            playing: false,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            notice,
            seek_bounds: None,
            volume_bounds: None,
            dragging_seek: false,
            dragging_volume: false,
            seen_audio: false,
            track_started: Instant::now(),
            failure_streak: 0,
            visible: Vec::new(),
        }
    }

    pub fn window_title(&self) -> String {
        self.library.window_title()
    }

    fn poll_playback(&mut self) {
        if self.dragging_seek || !self.playing {
            return;
        }
        if self.audio.queued() > 0 {
            if !self.seen_audio {
                self.failure_streak = 0;
            }
            self.seen_audio = true;
            self.position = self.audio.file_position();
            return;
        }
        if self.seen_audio {
            self.on_track_ended();
            return;
        }
        if self.track_started.elapsed() > Duration::from_secs(2) {
            self.fail_current("音频没有开始播放");
        }
    }

    fn on_track_ended(&mut self) {
        self.seen_audio = false;
        if self.library.prefs.play_mode == PlayMode::RepeatOne {
            if let Some(key) = self.current_key() {
                self.start_key(&key);
            }
            return;
        }
        self.advance(1);
    }

    fn play_pause(&mut self, _: &PlayPause, _window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        if self.playing && !self.audio.is_paused() {
            self.audio.pause();
            self.playing = false;
            cx.notify();
            return;
        }
        if self.audio.queued() > 0 {
            self.audio.play();
            self.playing = true;
            cx.notify();
            return;
        }
        if let Some(key) = self.current_key() {
            self.failure_streak = 0;
            self.start_key(&key);
            cx.notify();
            return;
        }
        if let Some(song) = self.library.visible(&self.filter, &self.query).first() {
            self.play_key(song.key.clone());
        }
        cx.notify();
    }

    fn seek_back(&mut self, _: &SeekBack, _: &mut Window, cx: &mut Context<Self>) {
        let step = self.library.config.player.seek_step_seconds;
        self.seek_by(-step);
        cx.notify();
    }

    fn seek_forward(&mut self, _: &SeekForward, _: &mut Window, cx: &mut Context<Self>) {
        let step = self.library.config.player.seek_step_seconds;
        self.seek_by(step);
        cx.notify();
    }

    fn volume_up(&mut self, _: &VolumeUp, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge_volume(self.library.config.player.volume_step);
        cx.notify();
    }

    fn volume_down(&mut self, _: &VolumeDown, _: &mut Window, cx: &mut Context<Self>) {
        self.nudge_volume(-self.library.config.player.volume_step);
        cx.notify();
    }

    fn toggle_mute(&mut self, _: &Mute, _: &mut Window, cx: &mut Context<Self>) {
        let muted = !self.library.prefs.muted;
        if let Err(error) = self.library.set_muted(muted) {
            self.notice = error;
        }
        self.audio.set_volume(self.output_volume());
        cx.notify();
    }

    fn focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.search.focus_handle(cx));
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.action_menu.is_some() {
            self.action_menu = None;
            cx.notify();
            return;
        }
        if self.dialog.is_some() {
            self.dialog = None;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if self.search.focus_handle(cx).is_focused(window) {
            if self.query.is_empty() {
                window.focus(&self.focus);
            } else {
                self.search.update(cx, |field, cx| field.set_text("", cx));
                self.query.clear();
            }
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        match dialog {
            Dialog::Rename { key } => {
                let title = self.dialog_field.read(cx).text().trim().to_owned();
                match self.library.rename(&key, &title) {
                    Ok(mutation) => self.apply_mutation(mutation),
                    Err(error) => self.notice = error,
                }
            }
            Dialog::NewCategory => {
                let name = self.dialog_field.read(cx).text().trim().to_owned();
                match self.library.create_category(&name) {
                    Ok(id) => {
                        self.filter = LibraryFilter::Category(id);
                        self.notice = format!("已新增分类 {name}");
                    }
                    Err(error) => self.notice = error,
                }
            }
            Dialog::Move { .. } | Dialog::Import { .. } => return,
        }
        self.dialog = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn play_key(&mut self, key: String) {
        let visible = self.library.visible(&self.filter, &self.query);
        self.queue = visible.into_iter().map(|song| song.key).collect();
        self.queue_index = self.queue.iter().position(|item| item == &key).unwrap_or(0);
        self.failure_streak = 0;
        self.start_key(&key);
    }

    fn start_key(&mut self, key: &str) {
        let Some(song) = self.library.song(key).cloned() else {
            self.fail_current("曲目已不在曲库中");
            return;
        };
        self.audio.set_speed(self.library.prefs.playback_rate);
        self.audio.set_volume(self.output_volume());
        match self.audio.load(std::path::Path::new(&song.path)) {
            Ok(duration) => {
                self.duration = duration;
                self.position = Duration::ZERO;
                self.playing = true;
                self.seen_audio = false;
                self.track_started = Instant::now();
            }
            Err(error) => {
                self.notice = format!("《{}》{error}", song.title);
                self.fail_current(&error);
            }
        }
    }

    fn fail_current(&mut self, reason: &str) {
        self.playing = false;
        self.audio.stop();
        self.seen_audio = false;
        self.failure_streak += 1;
        let title = self.current_title();
        let max_skips = self.library.config.player.max_auto_skips;
        if self.queue.len() > 1 && self.failure_streak <= max_skips {
            self.notice = format!("《{title}》{reason}，已跳到下一首");
            self.advance(1);
            return;
        }
        self.notice = format!("《{title}》{reason}");
    }

    fn advance(&mut self, direction: isize) {
        if self.queue.is_empty() {
            self.stop_playback();
            return;
        }
        if self.queue.len() == 1 {
            if let Some(key) = self.queue.first().cloned() {
                self.start_key(&key);
            }
            return;
        }
        self.queue_index = match self.library.prefs.play_mode {
            PlayMode::Shuffle => random_other(self.queue.len(), self.queue_index),
            _ => {
                let len = self.queue.len() as isize;
                (self.queue_index as isize + direction).rem_euclid(len) as usize
            }
        };
        if let Some(key) = self.queue.get(self.queue_index).cloned() {
            self.start_key(&key);
        }
    }

    fn previous(&mut self) {
        let threshold = self.library.config.player.restart_threshold_seconds;
        if self.position.as_secs_f64() > threshold || self.queue.len() <= 1 {
            let _ = self.audio.seek_file(Duration::ZERO);
            self.position = Duration::ZERO;
            self.audio.play();
            self.playing = true;
            return;
        }
        let len = self.queue.len();
        self.queue_index = (self.queue_index + len - 1) % len;
        self.failure_streak = 0;
        if let Some(key) = self.queue.get(self.queue_index).cloned() {
            self.start_key(&key);
        }
    }

    fn stop_playback(&mut self) {
        self.audio.stop();
        self.playing = false;
        self.position = Duration::ZERO;
        self.duration = Duration::ZERO;
        self.seen_audio = false;
    }

    fn seek_by(&mut self, delta: f64) {
        if self.duration.is_zero() {
            return;
        }
        let target = (self.position.as_secs_f64() + delta).clamp(0.0, self.duration.as_secs_f64());
        self.seek_to(Duration::from_secs_f64(target));
    }

    fn seek_to(&mut self, target: Duration) {
        if let Err(error) = self.audio.seek_file(target) {
            self.notice = error;
        } else {
            self.position = target;
        }
    }

    fn nudge_volume(&mut self, delta: f32) {
        let next = (self.library.prefs.volume + delta).clamp(0.0, 1.0);
        if let Err(error) = self.library.set_volume(next) {
            self.notice = error;
            return;
        }
        self.audio.set_volume(self.output_volume());
    }

    fn output_volume(&self) -> f32 {
        if self.library.prefs.muted {
            0.0
        } else {
            self.library.prefs.volume
        }
    }

    fn current_key(&self) -> Option<String> {
        self.queue.get(self.queue_index).cloned()
    }

    fn current_title(&self) -> String {
        self.current_key()
            .as_deref()
            .and_then(|key| self.library.song(key))
            .map(|song| song.title.clone())
            .unwrap_or_else(|| "未在播放".into())
    }

    fn apply_mutation(&mut self, mutation: Mutation) {
        self.notice = mutation.message;
        for change in &mutation.changes {
            if change.from == change.to {
                continue;
            }
            for key in &mut self.queue {
                if key == &change.from {
                    *key = change.to.clone();
                }
            }
        }
        if mutation.removed.is_empty() {
            return;
        }
        let current = self.current_key();
        self.queue
            .retain(|key| !mutation.removed.iter().any(|removed| removed == key));
        if let Some(current) = current {
            if mutation.removed.iter().any(|key| key == &current) {
                self.stop_playback();
                self.queue.clear();
                self.queue_index = 0;
                return;
            }
            self.queue_index =
                queue_index_after_removal(&self.queue, Some(&current), self.queue_index);
        } else if self.queue_index >= self.queue.len() {
            self.queue_index = 0;
        }
    }

    fn reconcile_snapshot(&mut self) {
        if let LibraryFilter::Category(id) = &self.filter
            && !self
                .library
                .snapshot
                .categories
                .iter()
                .any(|category| &category.id == id)
        {
            self.filter = LibraryFilter::All;
        }
        let current = self.current_key();
        self.queue.retain(|key| self.library.song(key).is_some());
        if let Some(current) = current {
            if let Some(index) = self.queue.iter().position(|key| key == &current) {
                self.queue_index = index;
            } else {
                self.stop_playback();
                self.queue.clear();
                self.queue_index = 0;
                self.notice = "当前曲目已不在曲库中，播放已停止".into();
            }
        } else {
            self.queue_index = 0;
        }
    }

    fn rescan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let warnings = self.library.rescan();
        self.notice = warnings
            .first()
            .cloned()
            .unwrap_or_else(|| format!("已重新扫描 {} 首", self.library.snapshot.songs.len()));
        self.reconcile_snapshot();
        window.set_window_title(&self.library.window_title());
        cx.notify();
    }

    fn choose_library(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择曲库".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(dir) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    match this.library.choose_dir(dir) {
                        Ok(()) => {
                            this.notice = format!("曲库：{}", this.library.root().display());
                            this.reconcile_snapshot();
                        }
                        Err(error) => this.notice = error,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn import_files(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("导入 MP3".into()),
        });
        let category = match &self.filter {
            LibraryFilter::Category(id) => Some(id.clone()),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let files: Vec<_> = paths
                .into_iter()
                .filter(|path| {
                    path.extension()
                        .and_then(|ext| ext.to_str())
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp3"))
                })
                .collect();
            this.update(cx, |this, cx| {
                if files.is_empty() {
                    this.notice = "没有选中 mp3 文件".into();
                } else if let Some(category) = category {
                    let mutation = this.library.import(&category, &files);
                    this.notice = mutation.message;
                } else {
                    this.dialog = Some(Dialog::Import { files });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn export_key(&mut self, key: String, cx: &mut Context<Self>) {
        let suggested = self
            .library
            .song(&key)
            .map(|song| format!("{}.mp3", bgm_core::export::safe_file_name(&song.title)));
        let directory = dirs::download_dir().unwrap_or_else(|| self.library.root().to_path_buf());
        let receiver = cx.prompt_for_new_path(&directory, suggested.as_deref());
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = receiver.await {
                this.update(cx, |this, cx| {
                    this.notice = match this.library.export_song(&key, &path) {
                        Ok(message) => message,
                        Err(error) => error,
                    };
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn trash_key(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .library
            .song(&key)
            .map(|song| song.title.clone())
            .unwrap_or_else(|| key.clone());
        let receiver = window.prompt(
            PromptLevel::Warning,
            "删除曲目",
            Some(&format!(
                "把《{title}》移动到曲库的 .tfy-trash，之后可以在访达里恢复。"
            )),
            &[PromptButton::ok("移到回收站"), PromptButton::cancel("取消")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if receiver.await == Ok(0) {
                this.update(cx, |this, cx| {
                    match this.library.trash(&key) {
                        Ok(mutation) => this.apply_mutation(mutation),
                        Err(error) => this.notice = error,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn open_rename(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .library
            .song(key)
            .map(|song| song.title.clone())
            .unwrap_or_default();
        self.dialog_field
            .update(cx, |field, cx| field.set_text(title, cx));
        self.dialog = Some(Dialog::Rename {
            key: key.to_owned(),
        });
        let field = self.dialog_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn open_category_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog_field
            .update(cx, |field, cx| field.set_text("", cx));
        self.dialog = Some(Dialog::NewCategory);
        let field = self.dialog_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn seek_down(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging_seek = true;
        self.seek_at(event.position);
        cx.notify();
    }

    fn seek_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging_seek {
            self.seek_at(event.position);
            cx.notify();
        }
    }

    fn seek_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging_seek = false;
        cx.notify();
    }

    fn seek_at(&mut self, position: Point<Pixels>) {
        let Some(bounds) = self.seek_bounds else {
            return;
        };
        if bounds.size.width <= px(1.) || self.duration.is_zero() {
            return;
        }
        let ratio = ((position.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        self.seek_to(Duration::from_secs_f32(self.duration.as_secs_f32() * ratio));
    }

    fn volume_down_at(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging_volume = true;
        self.volume_at(event.position);
        cx.notify();
    }

    fn volume_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging_volume {
            self.volume_at(event.position);
            cx.notify();
        }
    }

    fn volume_up_at(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging_volume = false;
        cx.notify();
    }

    fn volume_at(&mut self, position: Point<Pixels>) {
        let Some(bounds) = self.volume_bounds else {
            return;
        };
        if bounds.size.width <= px(1.) {
            return;
        }
        let ratio = ((position.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        if let Err(error) = self.library.set_volume(ratio) {
            self.notice = error;
            return;
        }
        self.audio.set_volume(self.output_volume());
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.visible = self.library.visible(&self.filter, &self.query);
        let topbar = self.topbar(window, cx);
        let sidebar = self.sidebar(cx);
        let compact = window.viewport_size().height < px(750.);
        let narrow = window.viewport_size().width < px(1130.);
        let main = self.main_column(compact, narrow, cx);
        let player = self.player_bar(narrow, cx);
        let action_menu = self.action_menu_layer(window, cx);
        let dialog = self.dialog_layer(window, cx);

        div()
            .id("workspace")
            .key_context("Library")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::play_pause))
            .on_action(cx.listener(Self::seek_back))
            .on_action(cx.listener(Self::seek_forward))
            .on_action(cx.listener(Self::volume_up))
            .on_action(cx.listener(Self::volume_down))
            .on_action(cx.listener(Self::toggle_mute))
            .on_action(cx.listener(Self::focus_search))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::confirm))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family("PingFang SC")
            .text_sm()
            .child(topbar)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(sidebar)
                    .child(main),
            )
            .child(
                div()
                    .absolute()
                    .left(px(16.))
                    .right(px(16.))
                    .bottom(px(14.))
                    .child(player),
            )
            .children(action_menu)
            .children(dialog)
    }
}

impl Workspace {
    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let all_count = self.library.snapshot.songs.len();
        let favorite_count = self
            .library
            .prefs
            .favorites
            .iter()
            .filter(|key| self.library.song(key).is_some())
            .count();
        let all_active = self.filter == LibraryFilter::All;
        let fav_active = self.filter == LibraryFilter::Favorites;
        let root = self.library.root().display().to_string();
        div()
            .w(px(232.))
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(SIDE))
            .border_r_1()
            .border_color(fade(rgb(LINE), 0.7))
            .child(
                div()
                    .px_5()
                    .pt_4()
                    .pb_2()
                    .text_size(px(10.))
                    .text_color(rgb(FAINT))
                    .child("浏览"),
            )
            .child(
                div()
                    .px_3()
                    .pt_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(self.nav_button(
                        "nav-all",
                        "music",
                        "音乐库",
                        all_count,
                        all_active,
                        false,
                        cx,
                        |this, cx| {
                            this.filter = LibraryFilter::All;
                            cx.notify();
                        },
                    ))
                    .child(self.nav_button(
                        "nav-fav",
                        "heart",
                        "我的收藏",
                        favorite_count,
                        fav_active,
                        true,
                        cx,
                        |this, cx| {
                            this.filter = LibraryFilter::Favorites;
                            cx.notify();
                        },
                    )),
            )
            .child(div().flex_1().min_h(px(12.)))
            .child(
                div()
                    .px_3()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .px_3()
                            .pb_2()
                            .text_size(px(10.))
                            .text_color(rgb(FAINT))
                            .child("曲库管理"),
                    )
                    .child(self.command_button(
                        "cmd-rescan",
                        "refresh",
                        "重新扫描",
                        cx,
                        |this, window, cx| this.rescan(window, cx),
                    ))
                    .child(self.command_button(
                        "cmd-choose",
                        "folder",
                        "切换曲库目录",
                        cx,
                        |this, window, cx| this.choose_library(&ClickEvent::default(), window, cx),
                    ))
                    .child(self.command_button(
                        "cmd-open",
                        "external",
                        "在访达中打开",
                        cx,
                        |this, _, cx| cx.reveal_path(this.library.root()),
                    ))
                    .when(self.library.using_custom_dir(), |this| {
                        this.child(self.command_button(
                            "cmd-reset",
                            "rotate",
                            "恢复默认曲库",
                            cx,
                            |this, _, cx| {
                                if let Err(error) = this.library.reset_dir() {
                                    this.notice = error;
                                } else {
                                    this.notice =
                                        format!("曲库：{}", this.library.root().display());
                                    this.reconcile_snapshot();
                                }
                                cx.notify();
                            },
                        ))
                    }),
            )
            .child(
                div()
                    .mx_3()
                    .mt_3()
                    .mb(px(118.))
                    .p_3()
                    .rounded_md()
                    .bg(fade(rgb(0xffffff), 0.025))
                    .border_1()
                    .border_color(fade(rgb(LINE), 0.65))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(5.)).rounded_full().bg(rgb(ACCENT)))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(ACCENT))
                                    .child("曲库就绪"),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(MUTED))
                                    .child(format!("{all_count} 首")),
                            ),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(10.))
                            .text_color(rgb(FAINT))
                            .text_ellipsis()
                            .child(root),
                    ),
            )
    }

    fn brand_mark(&self) -> impl IntoElement + use<> {
        div()
            .size(px(39.))
            .flex_none()
            .rounded_lg()
            .items_center()
            .justify_center()
            .bg(linear_gradient(150., rgb(0x0d2b20), rgb(0x071310)))
            .border_1()
            .border_color(fade(rgb(ACCENT), 0.28))
            .child(icon("brand-disc", 27., rgb(0xd9fbea)))
    }

    fn nav_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &'static str,
        count: usize,
        active: bool,
        favorite: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let color = if favorite { FAVORITE } else { ACCENT };
        div()
            .id(id)
            .h(px(39.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .bg(if active { rgb(PANEL_LIGHT) } else { rgb(SIDE) })
            .border_1()
            .border_color(if active {
                fade(rgb(color), 0.17)
            } else {
                fade(rgb(SIDE), 0.)
            })
            .text_color(if active { rgb(TEXT) } else { rgb(MUTED) })
            .hover(|style| style.bg(rgb(PANEL)).text_color(rgb(TEXT)))
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .child(icon(
                if favorite && active {
                    "heart-fill"
                } else {
                    icon_name
                },
                17.,
                if active { rgb(color) } else { rgb(FAINT) },
            ))
            .child(
                div()
                    .flex_1()
                    .text_size(px(13.))
                    .font_weight(if active {
                        FontWeight::MEDIUM
                    } else {
                        FontWeight::NORMAL
                    })
                    .child(label),
            )
            .child(
                div()
                    .min_w(px(25.))
                    .text_right()
                    .text_size(px(11.))
                    .text_color(if active { rgb(color) } else { rgb(FAINT) })
                    .child(count.to_string()),
            )
            .into_any_element()
    }

    fn command_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &'static str,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .h(px(31.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(rgb(MUTED))
            .hover(|style| style.bg(rgb(PANEL)).text_color(rgb(TEXT)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(icon_name, 14., rgb(FAINT)))
            .child(label)
            .into_any_element()
    }

    fn topbar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        let brand = self.library.config.brand.clone();
        div()
            .h(px(68.))
            .flex_none()
            .pl(px(92.))
            .pr_5()
            .flex()
            .items_center()
            .gap_4()
            .bg(linear_gradient(180., rgb(0x0b1713), rgb(0x08130f)))
            .border_b_1()
            .border_color(fade(rgb(LINE), 0.7))
            .child(
                div()
                    .w(px(208.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .child(self.brand_mark())
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(brand.title),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(FAINT))
                                    .child(brand.subtitle),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .max_w(px(700.))
                    .h(px(38.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .bg(rgb(0x111e18))
                    .border_1()
                    .border_color(if search_focused {
                        fade(rgb(ACCENT), 0.55)
                    } else {
                        fade(rgb(LINE), 0.8)
                    })
                    .child(icon(
                        "search",
                        15.,
                        if search_focused {
                            rgb(ACCENT)
                        } else {
                            rgb(FAINT)
                        },
                    ))
                    .child(div().flex_1().min_w(px(0.)).child(self.search.clone()))
                    .when(!self.query.is_empty(), |this| {
                        this.child(self.icon_button(
                            "search-clear",
                            "x",
                            "清除搜索",
                            false,
                            cx,
                            |this, _, cx| {
                                this.search.update(cx, |field, cx| field.set_text("", cx));
                                this.query.clear();
                                cx.notify();
                            },
                        ))
                    })
                    .when(self.query.is_empty(), |this| {
                        this.child(
                            div()
                                .size(px(21.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(5.))
                                .border_1()
                                .border_color(rgb(LINE))
                                .text_size(px(11.))
                                .text_color(rgb(FAINT))
                                .child("/"),
                        )
                    }),
            )
            .child(self.tool_button(
                "tool-import",
                "upload",
                "导入音乐",
                true,
                cx,
                |this, window, cx| {
                    this.import_files(&ClickEvent::default(), window, cx);
                },
            ))
            .child(self.tool_button(
                "tool-sort",
                "sort",
                if self.library.prefs.sort_by == SortBy::Name {
                    "按曲名 A–Z"
                } else {
                    "默认推荐"
                },
                false,
                cx,
                |this, _, cx| {
                    let next = if this.library.prefs.sort_by == SortBy::Name {
                        SortBy::Default
                    } else {
                        SortBy::Name
                    };
                    if let Err(error) = this.library.set_sort(next) {
                        this.notice = error;
                    }
                    cx.notify();
                },
            ))
            .child(self.icon_button(
                "tool-config",
                "settings",
                "打开配置文件",
                false,
                cx,
                |this, _, cx| {
                    cx.reveal_path(this.library.config_path());
                },
            ))
    }

    fn main_column(
        &mut self,
        compact: bool,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (title, description, results) = self.hero_copy();
        let count = self.visible.len();
        let category_count = self.library.snapshot.categories.len();
        let accent = self.hero_accent();
        let label = self.hero_label();
        let warning = self.library.snapshot.warnings.first().cloned();
        let hero_height = if compact { 98. } else { 110. };
        let notice = self.notice.clone();

        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(180., rgb(0x091510), rgb(BG)))
            .child(
                div()
                    .mx_6()
                    .mt_4()
                    .mb_3()
                    .relative()
                    .h(px(hero_height))
                    .flex_none()
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(linear_gradient(94., rgb(0x0e211a), rgb(0x0b1a14)))
                    .border_1()
                    .border_color(fade(accent, 0.14))
                    .when(!narrow, |this| this.child(self.hero_art(accent, hero_height)))
                    .child(
                        div()
                            .size_full()
                            .px_5()
                            .py_3()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w(px(0.))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .h(px(18.))
                                                            .px_2()
                                                            .flex()
                                                            .items_center()
                                                            .rounded(px(5.))
                                                            .bg(fade(accent, 0.12))
                                                            .border_1()
                                                            .border_color(fade(accent, 0.24))
                                                            .text_size(px(9.))
                                                            .text_color(accent)
                                                            .child(label),
                                                    )
                                                    .child(div().text_size(px(10.5)).text_color(rgb(FAINT)).child(format!("{count} 首 · {category_count} 种风格"))),
                                            )
                                            .child(div().mt_1().text_size(if compact { px(21.) } else { px(24.) }).font_weight(FontWeight::SEMIBOLD).text_ellipsis().child(title)),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .child(self.hero_button("hero-play", "play-fill", &format!("全部播放 ({count})"), true, cx, |this, _, cx| {
                                                if let Some(song) = this.visible.first().cloned() { this.play_key(song.key); }
                                                cx.notify();
                                            }))
                                            .child(self.hero_button("hero-shuffle", "shuffle", "随机试听", false, cx, |this, _, cx| {
                                                if !this.visible.is_empty() {
                                                    let index = random_other(this.visible.len(), usize::MAX);
                                                    if let Some(song) = this.visible.get(index).cloned() { this.play_key(song.key); }
                                                }
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(div().flex_1())
                            .child(div().text_size(px(11.)).text_color(rgb(MUTED)).text_ellipsis().child(description)),
                    ),
            )
            .child(
                div()
                    .px_6()
                    .pb_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.category_chips(cx))
                    .child(self.icon_button("tool-category", "plus", "新增分类", false, cx, |this, window, cx| {
                        this.open_category_dialog(window, cx);
                    })),
            )
            .child(
                div()
                    .px_6()
                    .pb_3()
                    .flex()
                    .items_end()
                    .gap_3()
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(results))
                    .child(div().pb(px(1.)).text_size(px(11.)).text_color(rgb(FAINT)).child(format!("{count} 首配乐")))
                    .child(div().flex_1())
                    .child(div().max_w(px(330.)).text_size(px(11.)).text_color(rgb(FAINT)).text_ellipsis().child(
                        if self.query.is_empty() { notice } else { format!("正在筛选“{}”", self.query) },
                    )),
            )
            .children(warning.map(|warning| {
                div()
                    .mx_6()
                    .mb_3()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .bg(fade(rgb(0xb69c59), 0.08))
                    .border_1()
                    .border_color(fade(rgb(0xb69c59), 0.2))
                    .text_size(px(11.))
                    .text_color(rgb(0xdac590))
                    .child(icon("alert", 14., rgb(0xdac590)))
                    .child(warning)
            }))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .mx_6()
                    .mb(px(118.))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(fade(rgb(LINE), 0.75))
                    .child(self.table_header(narrow))
                    .child(self.song_list(cx)),
            )
    }

    fn hero_label(&self) -> String {
        match &self.filter {
            LibraryFilter::All => self.library.config.ui.all.label_en.clone(),
            LibraryFilter::Favorites => self.library.config.ui.favorites.label_en.clone(),
            LibraryFilter::Category(id) => self
                .library
                .snapshot
                .categories
                .iter()
                .find(|category| &category.id == id)
                .map(|category| category.label_en.clone())
                .unwrap_or_else(|| id.to_uppercase()),
        }
    }

    fn hero_accent(&self) -> Rgba {
        match &self.filter {
            LibraryFilter::Favorites => rgb(FAVORITE),
            LibraryFilter::Category(id) => self
                .library
                .snapshot
                .categories
                .iter()
                .find(|category| &category.id == id)
                .map(|category| parse_color(&category.color))
                .unwrap_or_else(|| rgb(ACCENT)),
            LibraryFilter::All => rgb(ACCENT),
        }
    }

    fn hero_art(&self, accent: Rgba, height: f32) -> impl IntoElement + use<> {
        div()
            .absolute()
            .right(px(280.))
            .top(px((height - 104.) / 2.))
            .size(px(104.))
            .rounded_full()
            .bg(fade(accent, 0.06))
            .border_1()
            .border_color(fade(accent, 0.18))
            .child(
                div()
                    .absolute()
                    .left(px(11.))
                    .top(px(11.))
                    .size(px(80.))
                    .rounded_full()
                    .border_1()
                    .border_color(fade(accent, 0.14)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(22.))
                    .top(px(22.))
                    .size(px(58.))
                    .rounded_full()
                    .border_1()
                    .border_color(fade(accent, 0.2)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(35.))
                    .top(px(35.))
                    .size(px(32.))
                    .rounded_full()
                    .bg(fade(accent, 0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon("music", 15., accent)),
            )
    }

    fn hero_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &str,
        primary: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label = label.to_owned();
        div()
            .id(id)
            .h(px(33.))
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .bg(if primary {
                fade(rgb(ACCENT), 1.)
            } else {
                fade(rgb(0xffffff), 0.07)
            })
            .text_color(if primary { rgb(ACCENT_INK) } else { rgb(TEXT) })
            .border_1()
            .border_color(if primary {
                fade(rgb(0xffffff), 0.)
            } else {
                fade(rgb(0xffffff), 0.12)
            })
            .hover(move |style| {
                if primary {
                    style.bg(rgb(0x7adcb4))
                } else {
                    style.bg(fade(rgb(0xffffff), 0.12))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(
                icon_name,
                14.,
                if primary { rgb(ACCENT_INK) } else { rgb(TEXT) },
            ))
            .child(label)
            .into_any_element()
    }

    fn icon_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        _label: &'static str,
        solid: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .size(px(33.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .border_1()
            .border_color(if solid {
                fade(rgb(0xffffff), 0.)
            } else {
                fade(rgb(LINE), 1.)
            })
            .bg(if solid {
                fade(rgb(0xffffff), 0.06)
            } else {
                fade(rgb(0xffffff), 0.03)
            })
            .hover(|style| style.bg(fade(rgb(0xffffff), 0.11)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(icon_name, 15., rgb(MUTED)))
            .into_any_element()
    }

    fn tool_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &str,
        primary: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label = label.to_owned();
        div()
            .id(id)
            .h(px(33.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .bg(if primary {
                fade(rgb(ACCENT), 1.)
            } else {
                fade(rgb(0xffffff), 0.03)
            })
            .text_color(if primary { rgb(ACCENT_INK) } else { rgb(MUTED) })
            .border_1()
            .border_color(if primary {
                fade(rgb(0xffffff), 0.)
            } else {
                fade(rgb(LINE), 1.)
            })
            .hover(move |style| {
                if primary {
                    style.bg(rgb(0x7adcb4))
                } else {
                    style.bg(fade(rgb(0xffffff), 0.1)).text_color(rgb(TEXT))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(
                icon_name,
                14.,
                if primary { rgb(ACCENT_INK) } else { rgb(FAINT) },
            ))
            .child(label)
            .into_any_element()
    }

    fn category_chips(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut chips = Vec::new();
        chips.push(self.filter_chip(
            "chip-all",
            "music",
            "全部风格",
            None,
            self.filter == LibraryFilter::All,
            cx,
            |this, cx| {
                this.filter = LibraryFilter::All;
                cx.notify();
            },
        ));
        for category in self.library.snapshot.categories.clone() {
            let id = category.id.clone();
            let active = self.filter == LibraryFilter::Category(id.clone());
            let color = parse_color(&category.color);
            chips.push(self.filter_chip(
                &format!("chip-{}", category.id),
                category_icon(&category.icon),
                &category.name_zh,
                Some(color),
                active,
                cx,
                move |this, cx| {
                    this.filter = LibraryFilter::Category(id.clone());
                    cx.notify();
                },
            ));
        }
        div().flex().flex_wrap().gap_2().children(chips)
    }

    fn filter_chip(
        &mut self,
        id: &str,
        icon_name: &'static str,
        label: &str,
        color: Option<Rgba>,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let tint = color.unwrap_or_else(|| rgb(ACCENT));
        let label = label.to_owned();
        let id = SharedString::from(id.to_owned());
        div()
            .id(id)
            .h(px(30.))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .rounded_full()
            .cursor_pointer()
            .text_size(px(12.))
            .border_1()
            .border_color(if active {
                fade(tint, 0.5)
            } else {
                fade(rgb(0xffffff), 0.08)
            })
            .bg(if active {
                fade(tint, 0.16)
            } else {
                fade(rgb(0xffffff), 0.03)
            })
            .text_color(if active { rgb(TEXT) } else { rgb(MUTED) })
            .hover(move |style| {
                if active {
                    style.bg(fade(tint, 0.24))
                } else {
                    style.bg(fade(rgb(0xffffff), 0.08)).text_color(rgb(TEXT))
                }
            })
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .child(icon(icon_name, 13., if active { tint } else { rgb(FAINT) }))
            .child(label)
            .into_any_element()
    }

    fn table_header(&mut self, _narrow: bool) -> impl IntoElement + use<> {
        div()
            .h(px(33.))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .flex_none()
            .bg(fade(rgb(0xffffff), 0.015))
            .border_b_1()
            .border_color(fade(rgb(LINE), 0.8))
            .text_size(px(10.))
            .text_color(rgb(FAINT))
            .child(div().w(px(30.)).child("#"))
            .child(div().flex_1().pl(px(44.)).child("曲目"))
            .child(div().w(px(60.)).text_right().child("操作"))
    }

    fn song_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.visible.len();
        if count == 0 {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(icon("music", 30., rgb(0x3c5148)))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(MUTED))
                        .child("这个列表里还没有曲目"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(FAINT))
                        .child("换个分类、清空搜索，或导入新的 mp3"),
                )
                .into_any_element();
        }
        uniform_list(
            "songs",
            count,
            cx.processor(|this, range: Range<usize>, _window, cx| {
                let mut rows = Vec::new();
                for index in range {
                    let Some(song) = this.visible.get(index).cloned() else {
                        continue;
                    };
                    rows.push(this.song_row(index, song, cx));
                }
                rows
            }),
        )
        .h_full()
        .w_full()
        .into_any_element()
    }

    fn song_row(
        &mut self,
        index: usize,
        song: Song,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let current = self.current_key().as_deref() == Some(song.key.as_str());
        let playing = current && self.playing && !self.audio.is_paused();
        let favorite = self
            .library
            .prefs
            .favorites
            .iter()
            .any(|key| key == &song.key);
        let category = self
            .library
            .snapshot
            .categories
            .iter()
            .find(|category| category.id == song.category);
        let category_name = category
            .map(|category| category.name_zh.clone())
            .unwrap_or_else(|| song.category.clone());
        let category_color = category
            .map(|category| parse_color(&category.color))
            .unwrap_or_else(|| rgb(ACCENT));
        let category_icon_name = category
            .map(|category| category_icon(&category.icon))
            .unwrap_or("music");
        let style_label = category
            .map(|category| {
                category
                    .label_en
                    .split('/')
                    .next()
                    .unwrap_or(&category.label_en)
                    .trim()
                    .to_owned()
            })
            .unwrap_or_else(|| song.category.to_uppercase());
        let play_key = song.key.clone();
        let fav_key = song.key.clone();
        let menu_key = song.key.clone();

        div()
            .h(px(52.))
            .w_full()
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(fade(rgb(LINE), 0.45))
            .bg(if current {
                fade(rgb(ACCENT), 0.075)
            } else {
                fade(rgb(BG), 0.)
            })
            .hover(|style| style.bg(fade(rgb(0x19d98b), 0.045)))
            .child(
                div()
                    .w(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if playing {
                        icon("play-fill", 11., rgb(ACCENT)).into_any_element()
                    } else {
                        div()
                            .text_size(px(11.))
                            .text_color(if current { rgb(ACCENT) } else { rgb(FAINT) })
                            .child(format!("{:02}", index + 1))
                            .into_any_element()
                    }),
            )
            .child(
                div()
                    .id(SharedString::from(format!("play-{}", song.key)))
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.current_key().as_deref() == Some(play_key.as_str())
                            && this.audio.queued() > 0
                        {
                            this.play_pause(&PlayPause, window, cx);
                        } else {
                            this.play_key(play_key.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(32.))
                            .flex_none()
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(linear_gradient(
                                145.,
                                fade(category_color, 0.24),
                                fade(rgb(PANEL), 0.7),
                            ))
                            .border_1()
                            .border_color(fade(category_color, if current { 0.65 } else { 0.18 }))
                            .child(icon(
                                if playing {
                                    "play-fill"
                                } else {
                                    category_icon_name
                                },
                                14.,
                                category_color,
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(if current {
                                        FontWeight::MEDIUM
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(if current { rgb(0xbaf4d8) } else { rgb(TEXT) })
                                    .text_ellipsis()
                                    .child(song.title.clone()),
                            )
                            .child(
                                div()
                                    .mt(px(3.))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .text_color(category_color)
                                            .child(category_name),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(9.5))
                                            .text_color(rgb(FAINT))
                                            .text_ellipsis()
                                            .child(format!("· {style_label}")),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(60.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .child(self.row_action(
                        format!("fav-{}", song.key),
                        if favorite { "heart-fill" } else { "heart" },
                        if favorite {
                            rgb(FAVORITE)
                        } else {
                            rgb(0x6f8378)
                        },
                        cx,
                        move |this, _, cx| {
                            this.notice = match this.library.toggle_favorite(&fav_key) {
                                Ok(true) => "已收藏".into(),
                                Ok(false) => "已取消收藏".into(),
                                Err(error) => error,
                            };
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .id(SharedString::from(format!("more-{}", song.key)))
                            .size(px(26.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(|style| style.bg(fade(rgb(0xffffff), 0.09)))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.action_menu = Some((menu_key.clone(), event.position()));
                                cx.notify();
                            }))
                            .child(icon("more", 14., rgb(0x6f8378))),
                    ),
            )
    }

    fn row_action(
        &mut self,
        id: String,
        icon_name: &'static str,
        color: Rgba,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(SharedString::from(id))
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(fade(rgb(0xffffff), 0.09)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(icon_name, 13., color))
            .into_any_element()
    }

    fn player_bar(&mut self, narrow: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let title = self.current_title();
        let has_track = self.current_key().is_some();
        let category = self
            .current_key()
            .as_deref()
            .and_then(|key| self.library.song(key))
            .and_then(|song| {
                self.library
                    .snapshot
                    .categories
                    .iter()
                    .find(|category| category.id == song.category)
                    .map(|category| (category.name_zh.clone(), parse_color(&category.color)))
            });
        let ratio = if self.duration.is_zero() {
            0.0
        } else {
            (self.position.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
        };
        let volume = self.output_volume();
        let mode_icon = match self.library.prefs.play_mode {
            PlayMode::Sequence => "repeat",
            PlayMode::RepeatOne => "repeat-1",
            PlayMode::Shuffle => "shuffle",
        };
        let mode_name = match self.library.prefs.play_mode {
            PlayMode::Sequence => "顺序播放",
            PlayMode::RepeatOne => "单曲循环",
            PlayMode::Shuffle => "随机播放",
        };
        let rate = format_rate(self.library.prefs.playback_rate);
        let playing = self.playing && !self.audio.is_paused();
        let seek_view = cx.entity().downgrade();
        let volume_view = cx.entity().downgrade();
        let favorite = self
            .current_key()
            .as_deref()
            .is_some_and(|key| self.library.prefs.favorites.iter().any(|item| item == key));
        let muted = self.library.prefs.muted;

        div()
            .flex_none()
            .h(px(92.))
            .px_5()
            .flex()
            .items_center()
            .gap_5()
            .rounded_lg()
            .bg(linear_gradient(180., rgb(0x12231b), rgb(0x0d1a14)))
            .border_1()
            .border_color(rgb(LINE))
            .shadow_lg()
            .child(
                div()
                    .w(px(228.))
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .relative()
                            .size(px(48.))
                            .flex_none()
                            .rounded_lg()
                            .bg(linear_gradient(150., rgb(0x25493a), rgb(0x121e18)))
                            .border_1()
                            .border_color(fade(rgb(ACCENT), if has_track { 0.24 } else { 0.1 }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon(
                                "disc",
                                22.,
                                if has_track {
                                    rgb(0xa9ebcb)
                                } else {
                                    rgb(0x4f645a)
                                },
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(if has_track { rgb(TEXT) } else { rgb(MUTED) })
                                    .text_ellipsis()
                                    .child(title),
                            )
                            .child(match category {
                                Some((name, color)) => div()
                                    .mt(px(2.))
                                    .text_size(px(10.5))
                                    .text_color(color)
                                    .text_ellipsis()
                                    .child(name),
                                None => div()
                                    .mt(px(2.))
                                    .text_size(px(10.5))
                                    .text_color(rgb(FAINT))
                                    .child("从曲库选择一首开始"),
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(self.player_icon_button(
                                "player-mode",
                                mode_icon,
                                false,
                                cx,
                                move |this, _, cx| {
                                    let next = match this.library.prefs.play_mode {
                                        PlayMode::Sequence => PlayMode::RepeatOne,
                                        PlayMode::RepeatOne => PlayMode::Shuffle,
                                        PlayMode::Shuffle => PlayMode::Sequence,
                                    };
                                    if let Err(error) = this.library.set_mode(next) {
                                        this.notice = error;
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(self.player_icon_button(
                                "player-prev",
                                "skip-back",
                                false,
                                cx,
                                |this, _, cx| {
                                    this.previous();
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .id("player-play")
                                    .size(px(38.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(if playing { rgb(ACCENT) } else { rgb(TEXT) })
                                    .shadow_sm()
                                    .hover(|style| style.opacity(0.9))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.play_pause(&PlayPause, window, cx);
                                    }))
                                    .child(icon(
                                        if playing { "pause-fill" } else { "play-fill" },
                                        15.,
                                        rgb(ACCENT_INK),
                                    )),
                            )
                            .child(self.player_icon_button(
                                "player-next",
                                "skip-forward",
                                false,
                                cx,
                                |this, _, cx| {
                                    this.failure_streak = 0;
                                    this.advance(1);
                                    cx.notify();
                                },
                            ))
                            .child(self.player_rate_button(&rate, cx, |this, _, cx| {
                                match this.library.cycle_rate() {
                                    Ok(rate) => this.audio.set_speed(rate),
                                    Err(error) => this.notice = error,
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .w(px(38.))
                                    .text_size(px(10.))
                                    .text_color(rgb(FAINT))
                                    .child(format_time(self.position)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .on_children_prepainted(move |bounds, _, cx| {
                                        seek_view
                                            .update(cx, |this, _| {
                                                this.seek_bounds = bounds.first().copied();
                                            })
                                            .ok();
                                    })
                                    .id("seek")
                                    .h(px(14.))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .on_mouse_down(MouseButton::Left, cx.listener(Self::seek_down))
                                    .on_mouse_move(cx.listener(Self::seek_move))
                                    .on_mouse_up(MouseButton::Left, cx.listener(Self::seek_up))
                                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::seek_up))
                                    .child(
                                        div()
                                            .relative()
                                            .h(px(3.))
                                            .w_full()
                                            .rounded_full()
                                            .bg(rgb(LINE))
                                            .child(
                                                div()
                                                    .h_full()
                                                    .w(relative(ratio))
                                                    .rounded_full()
                                                    .bg(if has_track {
                                                        rgb(ACCENT)
                                                    } else {
                                                        rgb(FAINT)
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(38.))
                                    .text_size(px(10.))
                                    .text_color(rgb(FAINT))
                                    .text_right()
                                    .child(format_time(self.duration)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(!narrow, |this| {
                        this.child(self.player_icon_button(
                            "volume-mute",
                            if muted { "volume-mute" } else { "volume" },
                            false,
                            cx,
                            |this, _, cx| {
                                let muted = !this.library.prefs.muted;
                                if let Err(error) = this.library.set_muted(muted) {
                                    this.notice = error;
                                }
                                this.audio.set_volume(this.output_volume());
                                cx.notify();
                            },
                        ))
                        .child(
                            div()
                                .on_children_prepainted(move |bounds, _, cx| {
                                    volume_view
                                        .update(cx, |this, _| {
                                            this.volume_bounds = bounds.first().copied();
                                        })
                                        .ok();
                                })
                                .id("volume")
                                .w(px(74.))
                                .h(px(14.))
                                .flex()
                                .items_center()
                                .cursor_pointer()
                                .on_mouse_down(MouseButton::Left, cx.listener(Self::volume_down_at))
                                .on_mouse_move(cx.listener(Self::volume_move))
                                .on_mouse_up(MouseButton::Left, cx.listener(Self::volume_up_at))
                                .on_mouse_up_out(MouseButton::Left, cx.listener(Self::volume_up_at))
                                .child(
                                    div().h(px(3.)).w_full().rounded_full().bg(rgb(LINE)).child(
                                        div()
                                            .h_full()
                                            .w(relative(volume))
                                            .rounded_full()
                                            .bg(rgb(MUTED)),
                                    ),
                                ),
                        )
                    })
                    .child(self.player_icon_button(
                        "player-fav",
                        if favorite { "heart-fill" } else { "heart" },
                        favorite,
                        cx,
                        move |this, _, cx| {
                            if let Some(key) = this.current_key() {
                                this.notice = match this.library.toggle_favorite(&key) {
                                    Ok(true) => "已收藏".into(),
                                    Ok(false) => "已取消收藏".into(),
                                    Err(error) => error,
                                };
                            }
                            cx.notify();
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(FAINT))
                            .child(format!("{mode_name} · 空格播放")),
                    ),
            )
    }

    fn player_rate_button(
        &mut self,
        rate: &str,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let rate = rate.to_owned();
        div()
            .id("player-rate")
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .cursor_pointer()
            .text_size(px(10.))
            .text_color(rgb(MUTED))
            .border_1()
            .border_color(fade(rgb(LINE), 0.9))
            .hover(|style| style.bg(fade(rgb(0xffffff), 0.08)).text_color(rgb(TEXT)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(rate)
            .into_any_element()
    }

    fn player_icon_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(fade(rgb(0xffffff), 0.08)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(
                icon_name,
                15.,
                if active { rgb(FAVORITE) } else { rgb(MUTED) },
            ))
            .into_any_element()
    }

    fn hero_copy(&self) -> (String, String, String) {
        let names = self
            .library
            .snapshot
            .categories
            .iter()
            .map(|category| category.name_zh.clone())
            .collect::<Vec<_>>()
            .join("、");
        match &self.filter {
            LibraryFilter::All => {
                let copy = &self.library.config.ui.all;
                (
                    copy.title.clone(),
                    copy.description.replace("{categories}", &names),
                    copy.results_title.clone(),
                )
            }
            LibraryFilter::Favorites => {
                let copy = &self.library.config.ui.favorites;
                (
                    copy.title.clone(),
                    copy.description.replace("{categories}", &names),
                    copy.results_title.clone(),
                )
            }
            LibraryFilter::Category(id) => self
                .library
                .snapshot
                .categories
                .iter()
                .find(|category| &category.id == id)
                .map(|category| {
                    (
                        category.title.clone(),
                        category.description.clone(),
                        category.name_zh.clone(),
                    )
                })
                .unwrap_or_else(|| (id.clone(), String::new(), id.clone())),
        }
    }

    fn action_menu_layer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let (key, position) = self.action_menu.clone()?;
        let left = (position.x - px(180.))
            .max(px(8.))
            .min(window.viewport_size().width - px(196.));
        let top = (position.y + px(5.))
            .min(window.viewport_size().height - px(166.))
            .max(px(8.));
        let rename_key = key.clone();
        let move_key = key.clone();
        let export_key = key.clone();
        let trash_key = key;

        Some(
            div()
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.action_menu = None;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(px(188.))
                        .p_1()
                        .flex()
                        .flex_col()
                        .rounded_md()
                        .bg(rgb(0x14241d))
                        .border_1()
                        .border_color(rgb(LINE))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(self.menu_action(
                            "menu-rename",
                            "edit",
                            "重命名",
                            false,
                            cx,
                            move |this, window, cx| {
                                this.open_rename(&rename_key, window, cx);
                            },
                        ))
                        .child(self.menu_action(
                            "menu-move",
                            "folder",
                            "移动到分类",
                            false,
                            cx,
                            move |this, _, cx| {
                                this.dialog = Some(Dialog::Move {
                                    key: move_key.clone(),
                                });
                                cx.notify();
                            },
                        ))
                        .child(self.menu_action(
                            "menu-export",
                            "download",
                            "导出音频",
                            false,
                            cx,
                            move |this, _, cx| {
                                this.export_key(export_key.clone(), cx);
                            },
                        ))
                        .child(div().h(px(1.)).mx_2().my_1().bg(fade(rgb(LINE), 0.8)))
                        .child(self.menu_action(
                            "menu-trash",
                            "trash",
                            "移到回收站",
                            true,
                            cx,
                            move |this, window, cx| {
                                this.trash_key(trash_key.clone(), window, cx);
                            },
                        )),
                ),
        )
    }

    fn menu_action(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .h(px(32.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(if danger { rgb(DANGER) } else { rgb(MUTED) })
            .hover(|style| style.bg(fade(rgb(0xffffff), 0.07)).text_color(rgb(TEXT)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.action_menu = None;
                on_click(this, window, cx);
            }))
            .child(icon(
                icon_name,
                14.,
                if danger { rgb(DANGER) } else { rgb(MUTED) },
            ))
            .child(label)
            .into_any_element()
    }

    fn tiny_button(
        &mut self,
        label: &'static str,
        id: &'static str,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let primary = label == "确定";
        div()
            .id(id)
            .h(px(32.))
            .px_4()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .bg(if primary {
                fade(rgb(ACCENT), 1.)
            } else {
                fade(rgb(0xffffff), 0.05)
            })
            .text_color(if primary { rgb(ACCENT_INK) } else { rgb(MUTED) })
            .border_1()
            .border_color(if primary {
                fade(rgb(ACCENT), 0.)
            } else {
                fade(rgb(LINE), 1.)
            })
            .hover(|style| style.opacity(0.86))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(label)
            .into_any_element()
    }

    fn dialog_layer(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let dialog = self.dialog.clone()?;
        let (heading, body) = match &dialog {
            Dialog::Rename { .. } => (
                "重命名".to_owned(),
                "改的是文件名，收藏会跟着走。".to_owned(),
            ),
            Dialog::NewCategory => (
                "新增分类".to_owned(),
                "目录名使用分类名称，并在曲库里创建文件夹。".to_owned(),
            ),
            Dialog::Move { .. } => (
                "移动到分类".to_owned(),
                "文件会移动到目标分类目录。".to_owned(),
            ),
            Dialog::Import { files } => (
                "导入到分类".to_owned(),
                format!("已选择 {} 个 mp3。", files.len()),
            ),
        };
        let show_field = matches!(dialog, Dialog::Rename { .. } | Dialog::NewCategory);
        let categories = self.library.snapshot.categories.clone();
        let mut choices = Vec::new();
        if matches!(dialog, Dialog::Move { .. } | Dialog::Import { .. }) {
            for category in categories {
                let id = category.id.clone();
                let name = category.name_zh.clone();
                choices.push(
                    div()
                        .id(SharedString::from(format!("pick-{id}")))
                        .h(px(34.))
                        .px_3()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(rgb(BG))
                        .hover(|style| style.bg(rgb(LINE)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.commit_category_choice(id.clone(), window, cx);
                        }))
                        .child(name)
                        .into_any_element(),
                );
            }
        }

        Some(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(fade(rgb(0x000000), 0.55))
                .key_context("Dialog")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.dialog = None;
                        window.focus(&this.focus);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(420.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(LINE))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_lg().child(heading))
                        .child(div().text_xs().text_color(rgb(MUTED)).child(body))
                        .when(show_field, |this| this.child(self.dialog_field.clone()))
                        .children(choices)
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(self.tiny_button(
                                    "取消",
                                    "dialog-cancel",
                                    cx,
                                    |this, window, cx| {
                                        this.dialog = None;
                                        window.focus(&this.focus);
                                        cx.notify();
                                    },
                                ))
                                .when(show_field, |this| {
                                    this.child(self.tiny_button(
                                        "确定",
                                        "dialog-ok",
                                        cx,
                                        |this, window, cx| {
                                            this.confirm(&Confirm, window, cx);
                                        },
                                    ))
                                }),
                        ),
                ),
        )
    }

    fn commit_category_choice(
        &mut self,
        category: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        match dialog {
            Dialog::Move { key } => match self.library.move_song(&key, &category) {
                Ok(mutation) => self.apply_mutation(mutation),
                Err(error) => self.notice = error,
            },
            Dialog::Import { files } => {
                let mutation = self.library.import(&category, &files);
                self.notice = mutation.message;
            }
            _ => return,
        }
        self.dialog = None;
        window.focus(&self.focus);
        cx.notify();
    }
}

fn queue_index_after_removal(
    queue: &[String],
    current: Option<&str>,
    previous_index: usize,
) -> usize {
    current
        .and_then(|current| queue.iter().position(|key| key == current))
        .unwrap_or_else(|| {
            if previous_index >= queue.len() {
                0
            } else {
                previous_index
            }
        })
}

fn linear_gradient(
    angle: f32,
    from: impl Into<gpui::Hsla>,
    to: impl Into<gpui::Hsla>,
) -> gpui::Background {
    gpui::linear_gradient(
        angle,
        linear_color_stop(from, 0.),
        linear_color_stop(to, 1.),
    )
}

fn fade(color: impl Into<gpui::Hsla>, factor: f32) -> gpui::Hsla {
    color.into().opacity(factor)
}

fn parse_color(hex: &str) -> gpui::Rgba {
    let hex = hex.trim().trim_start_matches('#');
    let value = u32::from_str_radix(hex, 16).unwrap_or(ACCENT);
    rgb(value)
}

fn format_time(duration: Duration) -> String {
    let total = duration.as_secs();
    format!("{:02}:{:02}", total / 60, total % 60)
}

fn format_rate(rate: f32) -> String {
    if (rate - rate.round()).abs() < 0.01 {
        format!("{:.0}×", rate)
    } else {
        format!("{rate:.2}×")
    }
}

fn random_other(len: usize, avoid: usize) -> usize {
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as usize)
        .unwrap_or(1);
    let mut index = tick % len.max(1);
    if index == avoid && len > 1 {
        index = (index + 1) % len;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_track_follows_its_key_after_earlier_removal() {
        let queue = vec!["B".to_owned(), "C".to_owned()];
        assert_eq!(queue_index_after_removal(&queue, Some("B"), 1), 0);
        assert_eq!(queue_index_after_removal(&queue, Some("C"), 1), 1);
    }

    #[test]
    fn missing_current_resets_only_an_invalid_index() {
        let queue = vec!["B".to_owned(), "C".to_owned()];
        assert_eq!(queue_index_after_removal(&queue, Some("missing"), 1), 1);
        assert_eq!(queue_index_after_removal(&queue, Some("missing"), 2), 0);
    }
}
