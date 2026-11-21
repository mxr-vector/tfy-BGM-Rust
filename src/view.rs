//! 曲库窗口。布局是壁纸层、顶栏、侧栏、曲目列表/节目单页和悬浮播放器，交互走 GPUI 动作与系统文件框。

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bgm_core::{LoopPoint, PlayMode, ScheduleEntry, ScheduleSong, Song, SortBy};
use chrono::{DateTime, Datelike, Local, NaiveDate, Timelike};
use gpui::{
    Animation, AnimationExt, App, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable,
    Font, FontFeatures, FontStyle, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ObjectFit, PathPromptOptions, Pixels, Point, PromptButton, PromptLevel, Rgba,
    SharedString, Subscription, TextRun, Window, div, img, linear_color_stop, prelude::*, px,
    relative, rgb, uniform_list,
};

use crate::actions::{
    Confirm, Dismiss, FocusSearch, Mute, PlayPause, SeekBack, SeekForward, VolumeDown, VolumeUp,
};
use crate::audio::AudioEngine;
use crate::icons::{category_icon, icon};
use crate::library::{Library, LibraryChange, LibraryFilter, Mutation};
use crate::text_input::TextField;
use crate::watch::LibraryWatcher;

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

/// 智能随机的播放历史窗口与条数上限，与主线 queue.ts 保持一致。
const HISTORY_LIMIT: usize = 48;
const HISTORY_WINDOW: Duration = Duration::from_secs(45 * 60);
/// 文件监听的兜底扫描间隔。
const WATCH_FALLBACK: Duration = Duration::from_secs(30);
/// 循环点编辑要求的最小跨度（秒），与 core 的 MIN_LOOP_SPAN 一致。
const MIN_LOOP_SPAN: f64 = 0.2;

#[derive(Clone)]
enum Dialog {
    Rename {
        key: String,
    },
    NewCategory,
    Move {
        key: String,
    },
    Import {
        files: Vec<PathBuf>,
    },
    /// A/B 循环点编辑；起点/终点单位为秒。
    Loop {
        key: String,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppView {
    Library,
    Schedule,
}

/// 节目单编辑草稿；节目名称与时间范围放在独立的 TextField 实体里，歌曲时间与列表平行。
struct ScheduleDraft {
    id: Option<String>,
    songs: Vec<ScheduleSong>,
    song_start_fields: Vec<Entity<TextField>>,
    song_end_fields: Vec<Entity<TextField>>,
    volume_enabled: bool,
    volume: f32,
    enabled: bool,
}

/// 歌曲选择弹窗：节目单添加歌曲（多选）或替换某一首（单选）。
struct SongPicker {
    multi: bool,
    replace_index: Option<usize>,
    selected: HashSet<String>,
}

/// 跑马灯的测量结果；文本超出播放器标题区宽度时滚动。
struct Marquee {
    shift: f32,
    period: Duration,
}

pub struct Workspace {
    library: Library,
    audio: AudioEngine,
    search: Entity<TextField>,
    dialog_field: Entity<TextField>,
    loop_start_field: Entity<TextField>,
    loop_end_field: Entity<TextField>,
    schedule_name_field: Entity<TextField>,
    schedule_start_field: Entity<TextField>,
    schedule_end_field: Entity<TextField>,
    picker_search: Entity<TextField>,
    focus: FocusHandle,
    _observe: Subscription,
    view: AppView,
    filter: LibraryFilter,
    query: String,
    dialog: Option<Dialog>,
    action_menu: Option<(String, Point<Pixels>)>,
    picker: Option<SongPicker>,
    export_popover: bool,
    wallpaper_popover: bool,
    schedule_draft: Option<ScheduleDraft>,
    loop_enabled_pending: bool,
    expanded_schedule: HashSet<String>,
    queue: Vec<String>,
    queue_index: usize,
    playing: bool,
    position: Duration,
    duration: Duration,
    notice: String,
    seek_bounds: Option<Bounds<Pixels>>,
    volume_bounds: Option<Bounds<Pixels>>,
    title_bounds: Option<Bounds<Pixels>>,
    draft_volume_bounds: Option<Bounds<Pixels>>,
    wallpaper_opacity_bounds: Option<Bounds<Pixels>>,
    dragging_seek: bool,
    dragging_volume: bool,
    dragging_draft_volume: bool,
    dragging_wallpaper_opacity: bool,
    seen_audio: bool,
    track_started: Instant,
    failure_streak: u32,
    play_history: Vec<(String, Instant)>,
    active_schedule_id: Option<String>,
    triggered: HashSet<String>,
    last_tick: Instant,
    scheduler_now: Option<DateTime<Local>>,
    watcher: Option<LibraryWatcher>,
    last_fallback: Instant,
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
        let mut notice = audio
            .open_error()
            .map(str::to_owned)
            .or_else(|| library.snapshot.warnings.first().cloned())
            .unwrap_or_else(|| format!("已载入 {} 首", library.snapshot.songs.len()));

        let search = cx.new(|cx| TextField::new(cx, "搜索曲名或分类，按 / 聚焦"));
        let dialog_field = cx.new(|cx| TextField::new(cx, "输入名称"));
        let loop_start_field = cx.new(|cx| TextField::new(cx, "例如 12.5"));
        let loop_end_field = cx.new(|cx| TextField::new(cx, "例如 32.5"));
        let schedule_name_field = cx.new(|cx| TextField::new(cx, "例如：午后轻快"));
        let schedule_start_field = cx.new(|cx| TextField::new(cx, "09:00"));
        let schedule_end_field = cx.new(|cx| TextField::new(cx, "10:00"));
        let picker_search = cx.new(|cx| TextField::new(cx, "搜索曲名或艺人"));
        let observe = cx.observe(&search, |this, search, cx| {
            let query = search.read(cx).text().to_owned();
            if this.query != query {
                this.query = query;
                cx.notify();
            }
        });

        // 启动文件监听；失败时只提示，用户仍可手动重新扫描。
        let watcher = LibraryWatcher::start(library.root()).ok();
        if watcher.is_none() {
            notice = "文件监听不可用，外部改动需要手动重新扫描".to_owned();
        }

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
                        let watch_changed = this.poll_watch();
                        let schedule_changed = this.tick_scheduler();
                        if this.playing
                            || this.position != position
                            || was_playing
                            || watch_changed
                            || schedule_changed
                        {
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
            loop_start_field,
            loop_end_field,
            schedule_name_field,
            schedule_start_field,
            schedule_end_field,
            picker_search,
            focus: cx.focus_handle(),
            _observe: observe,
            view: AppView::Library,
            filter: LibraryFilter::All,
            query: String::new(),
            dialog: None,
            action_menu: None,
            picker: None,
            export_popover: false,
            wallpaper_popover: false,
            schedule_draft: None,
            loop_enabled_pending: true,
            expanded_schedule: HashSet::new(),
            queue: Vec::new(),
            queue_index: 0,
            playing: false,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            notice,
            seek_bounds: None,
            volume_bounds: None,
            title_bounds: None,
            draft_volume_bounds: None,
            wallpaper_opacity_bounds: None,
            dragging_seek: false,
            dragging_volume: false,
            dragging_draft_volume: false,
            dragging_wallpaper_opacity: false,
            seen_audio: false,
            track_started: Instant::now(),
            failure_streak: 0,
            play_history: Vec::new(),
            active_schedule_id: None,
            triggered: HashSet::new(),
            last_tick: Instant::now(),
            scheduler_now: None,
            watcher,
            last_fallback: Instant::now(),
            visible: Vec::new(),
        }
    }

    pub fn window_title(&self) -> String {
        self.library.window_title()
    }

    // ---------- 播放 ----------

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
        // 节目生效时段内接管「播完」：只循环当前到点的歌曲，等下一首的起始时间再切换。
        if self.handle_schedule_ended() {
            return;
        }
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
        if let Some(song) = self
            .library
            .visible(&self.filter, &self.query)
            .first()
            .cloned()
        {
            if self.guard_library_playback() {
                cx.notify();
                return;
            }
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
        if self.picker.is_some() {
            self.picker = None;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if self.export_popover || self.wallpaper_popover {
            self.export_popover = false;
            self.wallpaper_popover = false;
            cx.notify();
            return;
        }
        if self.schedule_draft.is_some() && self.dialog.is_none() {
            self.cancel_schedule_edit(window, cx);
            return;
        }
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
            Dialog::Loop { key } => {
                self.save_loop_from_editor(&key, cx);
                self.dialog = None;
                window.focus(&self.focus);
                cx.notify();
                return;
            }
            Dialog::Move { .. } | Dialog::Import { .. } => return,
        }
        self.dialog = None;
        window.focus(&self.focus);
        cx.notify();
    }

    /// 手动点播一首歌：以当前可见列表为队列。节目时段内会被拦截。
    fn play_key(&mut self, key: String) {
        if self.guard_library_playback() {
            return;
        }
        let visible = self.library.visible(&self.filter, &self.query);
        self.queue = visible.into_iter().map(|song| song.key).collect();
        self.queue_index = self.queue.iter().position(|item| item == &key).unwrap_or(0);
        self.failure_streak = 0;
        self.start_key(&key);
    }

    /// 以给定队列开播（节目单入口），不做时段拦截。
    fn play_keys(&mut self, keys: Vec<String>, key: &str) {
        self.queue = keys;
        self.queue_index = self.queue.iter().position(|item| item == key).unwrap_or(0);
        self.failure_streak = 0;
        self.start_key(key);
    }

    fn start_key(&mut self, key: &str) {
        let Some(song) = self.library.song(key).cloned() else {
            self.fail_current("曲目已不在曲库中");
            return;
        };
        self.record_play(&song.key);
        self.audio.set_speed(self.library.prefs.playback_rate);
        self.audio.set_volume(self.output_volume());
        match self.audio.load(std::path::Path::new(&song.path)) {
            Ok(duration) => {
                self.duration = duration;
                self.position = Duration::ZERO;
                self.playing = true;
                self.seen_audio = false;
                self.track_started = Instant::now();
                self.refresh_loop_region();
            }
            Err(error) => {
                self.notice = format!("《{}》{error}", song.title);
                self.fail_current(&error);
            }
        }
    }

    fn record_play(&mut self, key: &str) {
        self.play_history.push((key.to_owned(), Instant::now()));
        if self.play_history.len() > HISTORY_LIMIT {
            self.play_history.remove(0);
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
            PlayMode::Shuffle => self
                .smart_next_index(&self.queue, self.queue_index)
                .unwrap_or(self.queue_index),
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

    fn current_song(&self) -> Option<&Song> {
        self.current_key()
            .as_deref()
            .and_then(|key| self.library.song(key))
    }

    fn current_title(&self) -> String {
        self.current_song()
            .map(|song| song.title.clone())
            .unwrap_or_else(|| "未在播放".into())
    }

    // ---------- 智能随机 ----------

    /// 智能随机下一曲：与主线 queue.ts 的 smartNextIndex 同权重。
    fn smart_next_index(&self, queue: &[String], current_index: usize) -> Option<usize> {
        if queue.len() <= 1 {
            return None;
        }
        let excluded: HashSet<usize> = HashSet::from([current_index]);
        let current_key = queue.get(current_index).map(String::as_str);
        let picked = self.smart_pick(queue, current_key, &excluded)?;
        queue.iter().position(|key| *key == picked)
    }

    /// 没有当前曲目时的加权起点（随机试听入口）。
    fn smart_pick_start(&self, list: &[String]) -> Option<String> {
        self.smart_pick(list, None, &HashSet::new())
    }

    fn smart_pick(
        &self,
        candidates: &[String],
        current_key: Option<&str>,
        excluded: &HashSet<usize>,
    ) -> Option<String> {
        if candidates.is_empty() {
            return None;
        }
        let song = |key: Option<&str>| key.and_then(|key| self.library.song(key));
        let current = song(current_key);
        let favorites: HashSet<&str> = self
            .library
            .prefs
            .favorites
            .iter()
            .map(String::as_str)
            .collect();
        let recent = self.recent_penalties();

        let mut weights: Vec<f32> =
            candidates
                .iter()
                .enumerate()
                .map(|(index, key)| {
                    if excluded.contains(&index) {
                        return 0.0;
                    }
                    let Some(item) = self.library.song(key) else {
                        return 0.0;
                    };
                    let mut weight = 1.0f32;
                    if favorites.contains(item.key.as_str()) {
                        weight *= 1.9;
                    }
                    if current.is_some_and(|c| c.category == item.category) {
                        weight *= 1.18;
                    } else {
                        weight *= 0.95;
                    }
                    if current.is_some_and(|c| c.genre.is_some())
                        && item.genre.is_some()
                        && item.genre == current.as_ref().unwrap().genre
                    {
                        weight *= 1.08;
                    }
                    let penalty = recent.get(&item.key).copied().unwrap_or(0.0);
                    weight *= (1.0 - penalty).max(0.05);

                    if let (Some(current), Some(duration_ms)) = (current, item.duration_ms) {
                        let current_ms = current.duration_ms.unwrap_or(0);
                        if current_ms >= 180_000 && duration_ms < 90_000 {
                            weight *= 0.22;
                        }
                        if current_ms < 60_000 && duration_ms < 60_000 {
                            weight *= 0.4;
                        }
                        if current_ms >= 300_000 && duration_ms >= 300_000 {
                            weight *= 0.75;
                        }
                    }

                    let same_artist = current.as_ref().zip(item.artist.as_deref()).is_some_and(
                        |(current, artist)| {
                            current.artist.as_deref().is_some_and(|current_artist| {
                                current_artist.eq_ignore_ascii_case(artist)
                            })
                        },
                    );
                    let same_album = current.as_ref().zip(item.album.as_deref()).is_some_and(
                        |(current, album)| {
                            current.album.as_deref().is_some_and(|current_album| {
                                current_album.eq_ignore_ascii_case(album)
                            })
                        },
                    );
                    if same_album {
                        weight *= 0.03;
                    } else if same_artist {
                        weight *= 0.05;
                    }
                    weight
                })
                .collect();

        // 有不同艺人/专辑的替代曲目时，同艺人/同专辑的候选直接出局。
        let has_alternative = candidates.iter().enumerate().any(|(index, key)| {
            weights[index] > 0.0
                && !matches!(
                    (current, self.library.song(key.as_str())),
                    (Some(current), Some(item))
                        if current.artist.is_some()
                            && item.artist == current.artist
                )
                && !matches!(
                    (current, self.library.song(key.as_str())),
                    (Some(current), Some(item))
                        if current.album.is_some() && item.album == current.album
                )
        });
        if has_alternative {
            for (index, key) in candidates.iter().enumerate() {
                let Some(item) = self.library.song(key) else {
                    continue;
                };
                let Some(current) = current else { continue };
                let same_artist = current.artist.is_some() && item.artist == current.artist;
                let same_album = current.album.is_some() && item.album == current.album;
                if same_artist || same_album {
                    weights[index] = 0.0;
                }
            }
        }

        let total: f32 = weights.iter().sum();
        if total <= 0.0 {
            return None;
        }
        let mut roll = rand_f32() * total;
        for (index, weight) in weights.iter().enumerate() {
            roll -= weight;
            if roll <= 0.0 && *weight > 0.0 {
                return Some(candidates[index].clone());
            }
        }
        candidates
            .iter()
            .enumerate()
            .find(|(index, _)| weights[*index] > 0.0)
            .map(|(_, key)| key.clone())
    }

    /// 近期播放惩罚：0 ~ 1，1 表示刚刚播过；45 分钟窗口内越近降得越狠。
    fn recent_penalties(&self) -> HashMap<String, f32> {
        let now = Instant::now();
        let mut penalties: HashMap<String, f32> = HashMap::new();
        let history_len = self.play_history.len().max(1);
        for (index, (key, at)) in self.play_history.iter().enumerate() {
            let age = now.duration_since(*at);
            if age > HISTORY_WINDOW {
                continue;
            }
            let recency = 1.0 - age.as_secs_f32() / HISTORY_WINDOW.as_secs_f32();
            let position = (index + 1) as f32 / history_len as f32;
            let value = recency * (0.45 + 0.55 * position);
            let slot = penalties.entry(key.clone()).or_insert(value);
            if *slot < value {
                *slot = value;
            }
        }
        penalties
    }

    // ---------- 节目单调度 ----------

    fn local_now() -> DateTime<Local> {
        Local::now()
    }

    fn minutes_of_day(now: &DateTime<Local>) -> u32 {
        now.hour() * 60 + now.minute()
    }

    /// 当前时刻生效的节目（时间范围左闭右开）。
    fn current_program(&self) -> Option<ScheduleEntry> {
        let minutes = Self::minutes_of_day(&Self::local_now());
        self.library
            .schedule
            .entries
            .iter()
            .find(|entry| {
                entry.enabled
                    && minutes_of(&entry.start).unwrap_or(0) <= minutes
                    && minutes < minutes_of(&entry.end).unwrap_or(u32::MAX)
            })
            .cloned()
    }

    /// 当前节目里最晚一首已到播放时间的歌曲。
    fn current_program_song(&self) -> Option<ScheduleSong> {
        let entry = self.current_program()?;
        let minutes = Self::minutes_of_day(&Self::local_now());
        latest_song_before(&entry, minutes)
    }

    /// 下一档节目；今天已无节目时指向明天的第一档。
    fn next_program(&self) -> Option<(ScheduleEntry, bool)> {
        let minutes = Self::minutes_of_day(&Self::local_now());
        let mut next: Option<&ScheduleEntry> = None;
        let mut next_start = u32::MAX;
        let mut first: Option<&ScheduleEntry> = None;
        let mut first_start = u32::MAX;
        for entry in &self.library.schedule.entries {
            if !entry.enabled {
                continue;
            }
            let start = minutes_of(&entry.start).unwrap_or(u32::MAX);
            if start < first_start {
                first = Some(entry);
                first_start = start;
            }
            if start > minutes && start < next_start {
                next = Some(entry);
                next_start = start;
            }
        }
        if let Some(entry) = next {
            return Some((entry.clone(), false));
        }
        first.map(|entry| (entry.clone(), true))
    }

    /// 每秒对照系统时间，在歌曲设定的开始时间应用对应节目。
    /// GPUI 的定时器不会像 WebView 那样被节流，但保留 2 分钟回看窗口；
    /// 休眠恢复（两次 tick 间隔超过 90 秒）则直接对齐到当前节目里最晚一首已开始的歌。
    fn tick_scheduler(&mut self) -> bool {
        let now = Self::local_now();
        if self.last_tick.elapsed() < Duration::from_secs(1) {
            return false;
        }
        // 墙钟间隔用于识别休眠恢复；只在真正的 tick 时更新，轮询本身不重置它。
        let gap = self
            .scheduler_now
            .map(|previous| now - previous)
            .unwrap_or_else(|| chrono::Duration::hours(24));
        self.scheduler_now = Some(now);
        self.last_tick = Instant::now();

        let day = now.date_naive();
        let day_text = day_key(day);
        let now_ms = now.timestamp_millis();
        let cutoff = now_ms - 120_000;
        let entries = self.library.schedule.entries.clone();

        struct Candidate {
            entry: ScheduleEntry,
            song_key: Option<String>,
            at: i64,
            key: String,
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        for entry in &entries {
            if !entry.enabled {
                continue;
            }
            if entry.songs.is_empty() {
                if let Some(at) = hhmm_millis(day, &entry.start)
                    && at <= now_ms
                    && at >= cutoff
                {
                    let key = format!("{}@{}@{}@", entry.id, day_text, entry.start);
                    if !self.triggered.contains(&key) {
                        candidates.push(Candidate {
                            entry: entry.clone(),
                            song_key: None,
                            at,
                            key,
                        });
                    }
                }
                continue;
            }
            for song in &entry.songs {
                let Some(at) = hhmm_millis(day, &song.start) else {
                    continue;
                };
                if at > now_ms || at < cutoff {
                    continue;
                }
                let key = format!("{}@{}@{}@{}", entry.id, day_text, song.start, song.key);
                if !self.triggered.contains(&key) {
                    candidates.push(Candidate {
                        entry: entry.clone(),
                        song_key: Some(song.key.clone()),
                        at,
                        key,
                    });
                }
            }
        }

        if !candidates.is_empty() {
            candidates.sort_by(|left, right| right.at.cmp(&left.at));
            // 同一窗口内的多条歌曲视为已过期，只应用最晚的一条。
            for candidate in &candidates {
                self.triggered.insert(candidate.key.clone());
            }
            let latest = &candidates[0];
            let switched = self.active_schedule_id.as_deref() != Some(latest.entry.id.as_str());
            if self.apply_schedule_entry(&latest.entry, latest.song_key.as_deref(), switched, false)
            {
                self.active_schedule_id = Some(latest.entry.id.clone());
            }
        } else if gap > chrono::Duration::seconds(90) || self.active_schedule_id.is_none() {
            // 启动、睡眠恢复或新节目生效：直接对齐到今天当前节目里最晚一首已开始的歌。
            let minutes = Self::minutes_of_day(&now);
            if let Some(program) = self
                .library
                .schedule
                .entries
                .iter()
                .find(|entry| {
                    entry.enabled
                        && minutes_of(&entry.start).unwrap_or(0) <= minutes
                        && minutes < minutes_of(&entry.end).unwrap_or(u32::MAX)
                })
                .cloned()
            {
                if let Some(due) = latest_song_before(&program, minutes) {
                    let key = format!("{}@{}@{}@{}", program.id, day_text, due.start, due.key);
                    if !self.triggered.contains(&key) {
                        let switched =
                            self.active_schedule_id.as_deref() != Some(program.id.as_str());
                        if self.apply_schedule_entry(&program, Some(&due.key), switched, false) {
                            self.active_schedule_id = Some(program.id.clone());
                        }
                        self.triggered.insert(key);
                    }
                } else if program.songs.is_empty() {
                    let key = format!("{}@{}@{}@", program.id, day_text, program.start);
                    if !self.triggered.contains(&key) {
                        let switched =
                            self.active_schedule_id.as_deref() != Some(program.id.as_str());
                        if self.apply_schedule_entry(&program, None, switched, false) {
                            self.active_schedule_id = Some(program.id.clone());
                        }
                        self.triggered.insert(key);
                    }
                }
            }
        }

        if self.current_program().is_none() {
            self.active_schedule_id = None;
        }
        if self.triggered.len() > 4000 {
            self.triggered.clear();
        }
        true
    }

    /// 节目生效时段内歌曲播完时：只循环「当前到点」的歌曲，不再顺着整张节目单往后跳。
    fn handle_schedule_ended(&mut self) -> bool {
        let Some(program) = self.current_program() else {
            return false;
        };
        let Some(due) = self.current_program_song() else {
            return false;
        };
        self.apply_schedule_entry(&program, Some(&due.key), false, true)
    }

    /// 处于节目单时段时拦截手动点播并提示；返回 true 表示已拦截。
    fn guard_library_playback(&mut self) -> bool {
        let Some(program) = self.current_program() else {
            return false;
        };
        self.notice = format!(
            "当前处于节目单「{}」播放时段（{}–{}），暂不能手动点播",
            program.name, program.start, program.end
        );
        true
    }

    /// 按节目内的顺序解析出仍在曲库中的歌曲 key；已移除的曲目会被跳过。
    fn resolve_schedule_songs(&self, entry: &ScheduleEntry) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut songs = Vec::new();
        for item in &entry.songs {
            if !seen.insert(item.key.clone()) {
                continue;
            }
            if self.library.song(&item.key).is_some() {
                songs.push(item.key.clone());
            }
        }
        songs
    }

    /// 应用节目：把节目歌曲设为播放队列并开始播放，同时应用该节目的音量。
    fn apply_schedule_entry(
        &mut self,
        entry: &ScheduleEntry,
        song_key: Option<&str>,
        announce: bool,
        restart: bool,
    ) -> bool {
        // 先调音量再开播，避免新节目以旧音量突然响起。
        if let Some(volume) = entry.volume {
            if self.library.set_volume(volume).is_ok() {
                self.audio.set_volume(self.output_volume());
            }
        }

        let queue = self.resolve_schedule_songs(entry);
        if queue.is_empty() {
            if announce {
                self.notice = format!("节目「{}」没有可播放的曲目", entry.name);
            }
            return false;
        }

        let mut target = queue.first().cloned();
        if let Some(song_key) = song_key {
            let order: Vec<&str> = entry.songs.iter().map(|item| item.key.as_str()).collect();
            let start = order.iter().position(|item| *item == song_key).unwrap_or(0);
            target = order[start..]
                .iter()
                .find(|item| queue.contains(&(*item).to_string()))
                .map(|item| (*item).to_owned());
        }

        // 指定歌曲缺失时顺延到后面的曲目，再不行就从第一首开始。
        let target = target.or_else(|| queue.first().cloned());
        let Some(target) = target else {
            return false;
        };
        if self.current_key().as_deref() == Some(target.as_str()) && !restart {
            self.queue = queue.clone();
            self.queue_index = queue.iter().position(|item| *item == target).unwrap_or(0);
        } else {
            self.play_keys(queue, &target);
        }

        if announce {
            let volume_note = entry
                .volume
                .map(|volume| format!(" · 音量 {}%", (volume * 100.0).round() as i32))
                .unwrap_or_default();
            self.notice = format!("已切换到节目「{}」{volume_note}", entry.name);
        }
        true
    }

    /// 立即按当前时间对齐节目，供启用/保存节目后调用。
    /// 时段内还没有歌曲到点时从节目第一首开始，避免启用后仍在播放音乐库曲目。
    fn sync_to_current_program(&mut self) {
        let Some(program) = self.current_program() else {
            return;
        };
        let minutes = Self::minutes_of_day(&Self::local_now());
        let due = latest_song_before(&program, minutes);
        let target = due
            .map(|song| song.key)
            .or_else(|| program.songs.first().map(|song| song.key.clone()));
        let Some(target) = target else {
            return;
        };
        let switched = self.active_schedule_id.as_deref() != Some(program.id.as_str());
        if self.apply_schedule_entry(&program, Some(&target), switched, false) {
            self.active_schedule_id = Some(program.id.clone());
        }
    }

    // ---------- 节目单编辑 ----------

    fn open_schedule_create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (start, end) = next_hour_range();
        self.schedule_name_field
            .update(cx, |field, cx| field.set_text("", cx));
        self.schedule_start_field
            .update(cx, |field, cx| field.set_text(start, cx));
        self.schedule_end_field
            .update(cx, |field, cx| field.set_text(end, cx));
        self.schedule_draft = Some(ScheduleDraft {
            id: None,
            songs: Vec::new(),
            song_start_fields: Vec::new(),
            song_end_fields: Vec::new(),
            volume_enabled: false,
            volume: 0.5,
            enabled: true,
        });
        let field = self.schedule_name_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn open_schedule_editor(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.library.schedule.entry(id).cloned() else {
            return;
        };
        self.expanded_schedule.remove(id);
        let mut song_start_fields = Vec::new();
        let mut song_end_fields = Vec::new();
        for song in &entry.songs {
            song_start_fields.push(cx.new(|cx| TextField::new(cx, "开始")));
            song_start_fields
                .last()
                .unwrap()
                .update(cx, |field, cx| field.set_text(song.start.clone(), cx));
            song_end_fields.push(cx.new(|cx| TextField::new(cx, "结束")));
            song_end_fields
                .last()
                .unwrap()
                .update(cx, |field, cx| field.set_text(song.end.clone(), cx));
        }
        self.schedule_name_field
            .update(cx, |field, cx| field.set_text(entry.name.clone(), cx));
        self.schedule_start_field
            .update(cx, |field, cx| field.set_text(entry.start.clone(), cx));
        self.schedule_end_field
            .update(cx, |field, cx| field.set_text(entry.end.clone(), cx));
        self.schedule_draft = Some(ScheduleDraft {
            id: Some(entry.id.clone()),
            songs: entry.songs.clone(),
            song_start_fields,
            song_end_fields,
            volume_enabled: entry.volume.is_some(),
            volume: entry.volume.unwrap_or(0.5),
            enabled: entry.enabled,
        });
        let field = self.schedule_name_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn cancel_schedule_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.schedule_draft = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn save_schedule_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 先把草稿与各输入框的值快照下来，再放开对 self 的借用做校验与保存。
        let Some(draft) = self.schedule_draft.as_ref() else {
            return;
        };
        let draft_id = draft.id.clone();
        let draft_volume_enabled = draft.volume_enabled;
        let draft_volume = draft.volume;
        let draft_enabled = draft.enabled;
        let song_keys: Vec<String> = draft.songs.iter().map(|item| item.key.clone()).collect();
        let song_times: Vec<(String, String)> = draft
            .songs
            .iter()
            .enumerate()
            .map(|(index, _)| {
                (
                    draft.song_start_fields[index]
                        .read(cx)
                        .text()
                        .trim()
                        .to_owned(),
                    draft.song_end_fields[index]
                        .read(cx)
                        .text()
                        .trim()
                        .to_owned(),
                )
            })
            .collect();
        let name = self.schedule_name_field.read(cx).text().trim().to_owned();
        let start_text = self.schedule_start_field.read(cx).text().trim().to_owned();
        let end_text = self.schedule_end_field.read(cx).text().trim().to_owned();

        if name.is_empty() {
            self.notice = "节目名称不能为空".into();
            cx.notify();
            return;
        }
        let (Some(start), Some(end)) = (minutes_of(&start_text), minutes_of(&end_text)) else {
            self.notice = "开始/结束时间需要是 HH:MM 格式".into();
            cx.notify();
            return;
        };
        if end <= start {
            self.notice = "结束时间必须晚于开始时间，且不能跨天".into();
            cx.notify();
            return;
        }
        if song_keys.is_empty() {
            self.notice = "还未添加歌曲：请先通过「添加歌曲」从歌单中选择歌曲".into();
            cx.notify();
            return;
        }

        // 校验每首歌的起止时间落在节目范围内。
        let mut songs = Vec::with_capacity(song_keys.len());
        for (index, key) in song_keys.iter().enumerate() {
            let (song_start_text, song_end_text) = &song_times[index];
            let (Some(song_start), Some(song_end)) =
                (minutes_of(song_start_text), minutes_of(song_end_text))
            else {
                self.notice = format!("《{}》的起止时间需要是 HH:MM 格式", self.song_title(key));
                cx.notify();
                return;
            };
            if song_start >= song_end || song_start < start || song_end > end {
                self.notice = format!(
                    "《{}》的起止时间必须落在 {}–{} 内，且结束晚于开始",
                    self.song_title(key),
                    start_text,
                    end_text
                );
                cx.notify();
                return;
            }
            songs.push(ScheduleSong {
                key: key.clone(),
                start: format_minutes(song_start),
                end: format_minutes(song_end),
            });
        }

        let entry = ScheduleEntry {
            id: draft_id.clone().unwrap_or_else(|| {
                format!(
                    "slot-{}",
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|time| time.as_millis() as u64)
                        .unwrap_or(0)
                        .to_string()
                )
            }),
            name,
            start: format_minutes(start),
            end: format_minutes(end),
            songs,
            volume: if draft_volume_enabled {
                Some((draft_volume * 100.0).round() / 100.0)
            } else {
                None
            },
            enabled: draft_enabled,
        };

        let mut next_entries = self.library.schedule.entries.clone();
        match &draft_id {
            Some(id) => {
                if let Some(slot) = next_entries.iter_mut().find(|item| item.id == *id) {
                    *slot = entry;
                }
            }
            None => next_entries.push(entry),
        }
        let mut next = bgm_core::Schedule::default();
        next.entries = next_entries;
        match self.library.save_schedule(next) {
            Ok(_) => {
                self.notice = "节目单已保存".into();
                self.schedule_draft = None;
                window.focus(&self.focus);
                self.sync_to_current_program();
            }
            Err(error) => self.notice = error,
        }
        cx.notify();
    }

    fn toggle_schedule_entry(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.library.schedule.entry(id).cloned() else {
            return;
        };
        let enable = !entry.enabled;
        let mut next_entries = self.library.schedule.entries.clone();
        if let Some(slot) = next_entries.iter_mut().find(|item| item.id == id) {
            slot.enabled = enable;
        }
        let mut next = bgm_core::Schedule::default();
        next.entries = next_entries;
        if self.library.save_schedule(next).is_ok() {
            self.notice = if enable {
                "节目已启用"
            } else {
                "节目已停用"
            }
            .into();
            if enable {
                self.sync_to_current_program();
            }
        }
        cx.notify();
    }

    fn remove_schedule_entry(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.library.schedule.entry(id).cloned() else {
            return;
        };
        let removed_id = id.to_owned();
        let receiver = window.prompt(
            PromptLevel::Warning,
            "删除节目",
            Some(&format!(
                "确定删除「{}」（{}–{}）吗？不会影响曲库中的音频文件。",
                entry.name, entry.start, entry.end
            )),
            &[PromptButton::ok("删除"), PromptButton::cancel("取消")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if receiver.await == Ok(0) {
                this.update(cx, |this, cx| {
                    let mut next = bgm_core::Schedule::default();
                    next.entries = this
                        .library
                        .schedule
                        .entries
                        .iter()
                        .filter(|item| item.id != removed_id)
                        .cloned()
                        .collect();
                    if this.library.save_schedule(next).is_ok() {
                        this.notice = "节目已删除".into();
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// 按歌曲时长比例把节目时间范围铺满，保证每首歌至少有 1 分钟。
    fn auto_fill_draft_songs(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.schedule_draft.as_ref() else {
            return;
        };
        let start = self.schedule_start_field.read(cx).text().trim().to_owned();
        let end = self.schedule_end_field.read(cx).text().trim().to_owned();
        let (Some(start), Some(end)) = (minutes_of(&start), minutes_of(&end)) else {
            return;
        };
        if end <= start || draft.songs.is_empty() {
            return;
        }
        let count = draft.songs.len() as u32;
        let span = end - start;
        let assignments: Vec<(u32, u32)> = if span < count {
            vec![(start, end); count as usize]
        } else {
            let weights: Vec<u32> = draft
                .songs
                .iter()
                .map(|item| {
                    let ms = self
                        .library
                        .song(&item.key)
                        .and_then(|song| song.duration_ms)
                        .unwrap_or(0);
                    if ms > 0 { (ms / 60_000).max(1) } else { 3 }
                })
                .collect();
            let total: u32 = weights.iter().sum::<u32>().max(count);
            let mut cursor = start;
            let mut slots = Vec::with_capacity(count as usize);
            for (index, weight) in weights.iter().enumerate() {
                let remaining = end - cursor;
                let remaining_songs = count - index as u32;
                let length = if index as u32 == count - 1 {
                    remaining
                } else {
                    ((span * weight) / total)
                        .clamp(1, remaining.saturating_sub(remaining_songs - 1))
                };
                slots.push((cursor, cursor + length));
                cursor = (cursor + length).min(end);
            }
            slots
        };
        let Some(draft) = self.schedule_draft.as_mut() else {
            return;
        };
        for (index, (song_start, song_end)) in assignments.into_iter().enumerate() {
            draft.songs[index].start = format_minutes(song_start);
            draft.songs[index].end = format_minutes(song_end);
            let start_text = draft.songs[index].start.clone();
            let end_text = draft.songs[index].end.clone();
            draft.song_start_fields[index].update(cx, |field, cx| field.set_text(start_text, cx));
            draft.song_end_fields[index].update(cx, |field, cx| field.set_text(end_text, cx));
        }
        cx.notify();
    }

    fn add_draft_songs(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        let start = self.schedule_start_field.read(cx).text().trim().to_owned();
        let end = self.schedule_end_field.read(cx).text().trim().to_owned();
        let Some(draft) = self.schedule_draft.as_mut() else {
            return;
        };
        let existing: HashSet<String> = draft.songs.iter().map(|item| item.key.clone()).collect();
        for key in keys {
            if existing.contains(key.as_str()) {
                continue;
            }
            let start_field = cx.new(|cx| TextField::new(cx, "开始"));
            start_field.update(cx, |field, cx| field.set_text(start.clone(), cx));
            let end_field = cx.new(|cx| TextField::new(cx, "结束"));
            end_field.update(cx, |field, cx| field.set_text(end.clone(), cx));
            draft.songs.push(ScheduleSong {
                key: key.clone(),
                start: start.clone(),
                end: end.clone(),
            });
            draft.song_start_fields.push(start_field);
            draft.song_end_fields.push(end_field);
        }
        self.auto_fill_draft_songs(cx);
    }

    fn replace_draft_song(&mut self, index: usize, key: String, cx: &mut Context<Self>) {
        let Some(draft) = self.schedule_draft.as_mut() else {
            return;
        };
        let Some(item) = draft.songs.get_mut(index) else {
            return;
        };
        item.key = key;
        cx.notify();
    }

    fn remove_draft_song(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(draft) = self.schedule_draft.as_mut() else {
            return;
        };
        if index >= draft.songs.len() {
            return;
        }
        draft.songs.remove(index);
        draft.song_start_fields.remove(index);
        draft.song_end_fields.remove(index);
        cx.notify();
    }

    /// 上移/下移会同步交换时间槽，保持列表顺序与播放时间一致。
    fn move_draft_song(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let Some(draft) = self.schedule_draft.as_mut() else {
            return;
        };
        if from >= draft.songs.len() || to >= draft.songs.len() || from == to {
            return;
        }
        let windows: Vec<(String, String)> = draft
            .songs
            .iter()
            .map(|item| (item.start.clone(), item.end.clone()))
            .collect();
        let moved = draft.songs.remove(from);
        draft.songs.insert(to, moved);
        let moved_start = draft.song_start_fields.remove(from);
        draft.song_start_fields.insert(to, moved_start);
        let moved_end = draft.song_end_fields.remove(from);
        draft.song_end_fields.insert(to, moved_end);
        for (index, (start, end)) in windows.into_iter().enumerate() {
            draft.songs[index].start = start;
            draft.songs[index].end = end;
        }
        cx.notify();
    }

    fn open_song_picker(
        &mut self,
        replace_index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker_search
            .update(cx, |field, cx| field.set_text("", cx));
        self.picker = Some(SongPicker {
            multi: replace_index.is_none(),
            replace_index,
            selected: HashSet::new(),
        });
        let field = self.picker_search.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn confirm_song_picker(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.take() else {
            return;
        };
        let keys: Vec<String> = self
            .library
            .snapshot
            .songs
            .iter()
            .filter(|song| picker.selected.contains(&song.key))
            .map(|song| song.key.clone())
            .collect();
        match picker.replace_index {
            Some(index) => {
                if let Some(key) = keys.first() {
                    self.replace_draft_song(index, key.clone(), cx);
                }
            }
            None => self.add_draft_songs(keys, cx),
        }
        cx.notify();
    }

    fn song_title(&self, key: &str) -> String {
        self.library
            .song(key)
            .map(|song| song.title.clone())
            .unwrap_or_else(|| "曲目已移除".into())
    }

    // ---------- 循环点 ----------

    /// 当前曲目启用的循环区间；终点会被实际时长裁剪，非法区间自动忽略。
    fn loop_region_for(&self, key: Option<&str>) -> Option<(f64, f64)> {
        let key = key?;
        let point: LoopPoint = self.library.loop_point(key)?;
        if !point.enabled {
            return None;
        }
        let known_duration = if self
            .current_key()
            .as_deref()
            .is_some_and(|current| current == key)
        {
            self.duration.as_secs_f64()
        } else {
            self.library
                .song(key)
                .and_then(|song| song.duration_ms)
                .map(|ms| ms as f64 / 1000.0)
                .unwrap_or(0.0)
        };
        let start = point.start.max(0.0);
        let mut end = point.end;
        if known_duration > 0.0 {
            end = end.min(known_duration);
        }
        if end - start < 0.1 {
            return None;
        }
        Some((start, end))
    }

    fn refresh_loop_region(&mut self) {
        let region = self.loop_region_for(self.current_key().as_deref());
        self.audio.set_loop_region(region);
    }

    fn open_loop_editor(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.library.loop_point(key);
        let duration = self
            .library
            .song(key)
            .and_then(|song| song.duration_ms)
            .map(|ms| ms as f64 / 1000.0);
        self.loop_start_field.update(cx, |field, cx| {
            field.set_text(
                existing
                    .map(|point| format!("{:.2}", point.start))
                    .unwrap_or_else(|| "0".into()),
                cx,
            )
        });
        self.loop_end_field.update(cx, |field, cx| {
            field.set_text(
                existing
                    .map(|point| format!("{:.2}", point.end))
                    .unwrap_or_else(|| duration.map(|d| format!("{d:.2}")).unwrap_or_default()),
                cx,
            )
        });
        self.loop_enabled_pending = existing.is_none_or(|point| point.enabled);
        self.dialog = Some(Dialog::Loop {
            key: key.to_owned(),
        });
        let field = self.loop_start_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn save_loop_from_editor(&mut self, key: &str, cx: &mut Context<Self>) {
        let start = self
            .loop_start_field
            .read(cx)
            .text()
            .trim()
            .parse::<f64>()
            .ok();
        let end = self
            .loop_end_field
            .read(cx)
            .text()
            .trim()
            .parse::<f64>()
            .ok();
        let enabled = self.loop_enabled_pending;
        let title = self.song_title(key);
        let duration = self
            .library
            .song(key)
            .and_then(|song| song.duration_ms)
            .map(|ms| ms as f64 / 1000.0);
        let Some((start, end)) = start.zip(end) else {
            self.notice = "请输入有效的秒数".into();
            cx.notify();
            return;
        };
        if start < 0.0 {
            self.notice = "起点不能为负数".into();
            cx.notify();
            return;
        }
        if end - start < MIN_LOOP_SPAN {
            self.notice = format!("循环终点至少要比起点晚 {MIN_LOOP_SPAN} 秒");
            cx.notify();
            return;
        }
        if duration.is_some_and(|duration| end > duration + 0.5) {
            self.notice = format!(
                "循环终点超出曲目时长 {}",
                format_time(Duration::from_secs_f64(duration.unwrap()))
            );
            cx.notify();
            return;
        }

        match self
            .library
            .save_loop_point(key, start.max(0.0), end, enabled)
        {
            Ok(_) => {
                self.notice = if enabled {
                    format!(
                        "《{title}》已启用循环 {}–{}",
                        format_time(Duration::from_secs_f64(start)),
                        format_time(Duration::from_secs_f64(end))
                    )
                } else {
                    format!("《{title}》循环已关闭")
                };
                self.refresh_loop_region();
            }
            Err(error) => self.notice = error,
        }
        cx.notify();
    }

    // ---------- 文件监听 ----------

    fn install_watcher(&mut self) {
        match LibraryWatcher::start(self.library.root()) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(_) => self.watcher = None,
        }
    }

    /// watch 事件（防抖后）或 30 秒兜底触发：重新扫描并计算增删/移动。
    fn poll_watch(&mut self) -> bool {
        let event_due = self
            .watcher
            .as_ref()
            .is_some_and(|watcher| watcher.pending());
        let fallback_due = self.last_fallback.elapsed() >= WATCH_FALLBACK;
        if !event_due && !fallback_due {
            return false;
        }
        if event_due && let Some(watcher) = &self.watcher {
            watcher.consume();
        }
        self.last_fallback = Instant::now();
        let Some(change) = self.library.handle_watch_event() else {
            return false;
        };
        let LibraryChange {
            added,
            removed,
            moved,
        } = change;
        self.reconcile_snapshot();
        self.refresh_loop_region();
        let mut notes = Vec::new();
        if !added.is_empty() {
            notes.push(format!("新增 {}", added.len()));
        }
        if !removed.is_empty() {
            notes.push(format!("移除 {}", removed.len()));
        }
        if !moved.is_empty() {
            notes.push(format!("移动 {}", moved.len()));
        }
        self.notice = if notes.is_empty() {
            "曲库已更新".into()
        } else {
            format!("曲库已更新：{}", notes.join("、"))
        };
        true
    }

    // ---------- 曲库写操作 ----------

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
                            this.install_watcher();
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
        if self.library.prefs.loudness_normalization {
            self.notice = "正在导出并做响度归一化，请稍候…".into();
            cx.notify();
        }
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

    // ---------- 拖动条 ----------

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

    fn draft_volume_down_at(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragging_draft_volume = true;
        self.draft_volume_at(event.position);
        cx.notify();
    }

    fn draft_volume_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dragging_draft_volume {
            self.draft_volume_at(event.position);
            cx.notify();
        }
    }

    fn draft_volume_up_at(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.dragging_draft_volume = false;
        cx.notify();
    }

    fn draft_volume_at(&mut self, position: Point<Pixels>) {
        let Some(bounds) = self.draft_volume_bounds else {
            return;
        };
        if bounds.size.width <= px(1.) {
            return;
        }
        let ratio = ((position.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        if let Some(draft) = self.schedule_draft.as_mut() {
            draft.volume = (ratio * 100.0).round() / 100.0;
        }
    }

    fn wallpaper_opacity_down_at(
        &mut self,
        event: &MouseDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragging_wallpaper_opacity = true;
        self.wallpaper_opacity_at(event.position);
        cx.notify();
    }

    fn wallpaper_opacity_move(
        &mut self,
        event: &MouseMoveEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dragging_wallpaper_opacity {
            self.wallpaper_opacity_at(event.position);
            cx.notify();
        }
    }

    fn wallpaper_opacity_up_at(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragging_wallpaper_opacity = false;
        cx.notify();
    }

    fn wallpaper_opacity_at(&mut self, position: Point<Pixels>) {
        let Some(bounds) = self.wallpaper_opacity_bounds else {
            return;
        };
        if bounds.size.width <= px(1.) {
            return;
        }
        let ratio = ((position.x - bounds.left()) / bounds.size.width).clamp(0.0, 1.0);
        if let Err(error) = self
            .library
            .set_wallpaper_opacity((ratio * 100.0).round() / 100.0)
        {
            self.notice = error;
        }
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.visible = if self.view == AppView::Library {
            self.library.visible(&self.filter, &self.query)
        } else {
            Vec::new()
        };
        let topbar = self.topbar(window, cx);
        let sidebar = self.sidebar(cx);
        let compact = window.viewport_size().height < px(750.);
        let narrow = window.viewport_size().width < px(1130.);
        let main = match self.view {
            AppView::Library => self.main_column(compact, narrow, cx).into_any_element(),
            AppView::Schedule => self.schedule_page(window, cx).into_any_element(),
        };
        let player = self.player_bar(window, narrow, cx);
        let wallpaper = self.wallpaper_layer();
        let action_menu = self.action_menu_layer(window, cx);
        let dialog = self.dialog_layer(window, cx);
        let picker = self.picker_layer(cx);
        let export_popover = self.export_popover_layer(cx);
        let wallpaper_popover = self.wallpaper_popover_layer(cx);

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
            .overflow_hidden()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family("PingFang SC")
            .text_sm()
            .children(wallpaper)
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
            .children(picker)
            .children(export_popover)
            .children(wallpaper_popover)
    }
}

impl Workspace {
    // ---------- 渲染：侧栏 ----------

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let all_count = self.library.snapshot.songs.len();
        let favorite_count = self
            .library
            .prefs
            .favorites
            .iter()
            .filter(|key| self.library.song(key).is_some())
            .count();
        let schedule_count = self.library.schedule.entries.len();
        let all_active = self.filter == LibraryFilter::All && self.view == AppView::Library;
        let fav_active = self.filter == LibraryFilter::Favorites && self.view == AppView::Library;
        let schedule_active = self.view == AppView::Schedule;
        let root = self.library.root().display().to_string();
        let source_label = self.library.source_label();
        let panel = self.panel_alpha();
        div()
            .w(px(232.))
            .h_full()
            .flex()
            .flex_col()
            .bg(fade(rgb(SIDE), panel))
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
                            this.view = AppView::Library;
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
                            this.view = AppView::Library;
                            this.filter = LibraryFilter::Favorites;
                            cx.notify();
                        },
                    ))
                    .child(self.nav_button(
                        "nav-schedule",
                        "clock",
                        "节目单",
                        schedule_count,
                        schedule_active,
                        false,
                        cx,
                        |this, cx| {
                            this.view = AppView::Schedule;
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
                                    this.install_watcher();
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
                    .bg(fade(rgb(0xffffff), 0.025 * panel))
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
                                    .child(source_label),
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

    /// 有壁纸时面板统一降到这个不透明度，让底层颜色 / 画面透出来，但仍保持文字对比度。
    fn panel_alpha(&self) -> f32 {
        let view = self.wallpaper_view();
        if view.has_wallpaper {
            (1.0 - view.opacity * 0.92).max(0.04)
        } else {
            1.0
        }
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

    // ---------- 渲染：顶栏 ----------

    fn topbar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        let brand = self.library.config.brand.clone();
        let panel = self.panel_alpha();
        div()
            .h(px(68.))
            .flex_none()
            .pl(px(92.))
            .pr_5()
            .flex()
            .items_center()
            .gap_4()
            .bg(linear_gradient(
                180.,
                fade(rgb(0x0b1713), panel),
                fade(rgb(0x08130f), panel),
            ))
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
                    .bg(fade(rgb(0x111e18), panel))
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
                "tool-export-settings",
                "sliders",
                "导出设置",
                false,
                cx,
                |this, _, cx| {
                    this.export_popover = !this.export_popover;
                    this.wallpaper_popover = false;
                    cx.notify();
                },
            ))
            .child(self.icon_button(
                "tool-wallpaper",
                "image",
                "界面壁纸",
                false,
                cx,
                |this, _, cx| {
                    this.wallpaper_popover = !this.wallpaper_popover;
                    this.export_popover = false;
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

    // ---------- 渲染：音乐库 ----------

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
        let panel = self.panel_alpha();

        div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .bg(linear_gradient(
                180.,
                fade(rgb(0x091510), panel),
                fade(rgb(BG), panel),
            ))
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
                    .bg(linear_gradient(
                        94.,
                        fade(rgb(0x0e211a), panel),
                        fade(rgb(0x0b1a14), panel),
                    ))
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
                                                if let Some(song) = this.visible.first().cloned() {
                                                    if !this.guard_library_playback() { this.play_key(song.key); }
                                                }
                                                cx.notify();
                                            }))
                                            .child(self.hero_button("hero-shuffle", "shuffle", "随机试听", false, cx, |this, _, cx| {
                                                if !this.visible.is_empty() {
                                                    if this.guard_library_playback() { cx.notify(); return; }
                                                    let list: Vec<String> = this.visible.iter().map(|song| song.key.clone()).collect();
                                                    if let Some(key) = this.smart_pick_start(&list) {
                                                        this.play_key(key);
                                                    }
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
                    .bg(fade(rgb(PANEL), panel))
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
        // 曲目带 ID3 标签时把艺人放进副标题，与主线的诊断信息保持可见性。
        let artist_label = song.artist.clone().unwrap_or_default();
        let play_key = song.key.clone();
        let fav_key = song.key.clone();
        let menu_key = song.key.clone();
        let loop_active = self.loop_region_for(Some(song.key.as_str())).is_some();

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
                        } else if !this.guard_library_playback() {
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
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(if current {
                                                FontWeight::MEDIUM
                                            } else {
                                                FontWeight::NORMAL
                                            })
                                            .text_color(if current {
                                                rgb(0xbaf4d8)
                                            } else {
                                                rgb(TEXT)
                                            })
                                            .text_ellipsis()
                                            .child(song.title.clone()),
                                    )
                                    .when(loop_active, |this| {
                                        this.child(
                                            div()
                                                .px_1()
                                                .flex_none()
                                                .rounded(px(4.))
                                                .bg(fade(rgb(ACCENT), 0.14))
                                                .border_1()
                                                .border_color(fade(rgb(ACCENT), 0.3))
                                                .text_size(px(8.5))
                                                .text_color(rgb(ACCENT))
                                                .child("循环"),
                                        )
                                    }),
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
                                            .child(if artist_label.is_empty() {
                                                format!("· {style_label}")
                                            } else {
                                                format!("· {artist_label} · {style_label}")
                                            }),
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

    // ---------- 渲染：播放器 ----------

    fn player_bar(
        &mut self,
        window: &mut Window,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let title = self.current_title();
        let has_track = self.current_key().is_some();
        let category = self.current_song().and_then(|song| {
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
        let title_view = cx.entity().downgrade();
        let favorite = self
            .current_key()
            .as_deref()
            .is_some_and(|key| self.library.prefs.favorites.iter().any(|item| item == key));
        let muted = self.library.prefs.muted;
        let panel = self.panel_alpha();

        // 播放状态与循环区间显示（对齐主线的播放状态增强）。
        let loop_region = self.loop_region_for(self.current_key().as_deref());
        let state_label = if self.playing && !self.seen_audio {
            "加载中"
        } else if playing {
            "播放中"
        } else if has_track {
            "已暂停"
        } else {
            "待机"
        };
        let loop_note = loop_region
            .map(|(start, end)| {
                format!(
                    "循环 {}–{}",
                    format_time(Duration::from_secs_f64(start)),
                    format_time(Duration::from_secs_f64(end))
                )
            })
            .unwrap_or_default();

        // 跑马灯：播放且标题超出可视宽度时滚动；宽度用文本排版精确测量。
        let font = Font {
            family: "PingFang SC".into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::MEDIUM,
            style: FontStyle::Normal,
        };
        let run = TextRun {
            len: title.len(),
            font,
            color: rgb(TEXT).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .layout_line(&title, px(13.), &[run], None);
        let host_width = self
            .title_bounds
            .map(|bounds| bounds.size.width)
            .unwrap_or(px(150.));
        let shift = (line.width - host_width - px(1.)).max(px(0.));
        let marquee = if playing && shift > px(0.) {
            let shift_px = f32::from(shift);
            Some(Marquee {
                shift: shift_px,
                period: Duration::from_secs_f64((shift_px / 26. + 4.).max(6.) as f64),
            })
        } else {
            None
        };
        let marquee_title = title.clone();

        div()
            .flex_none()
            .h(px(92.))
            .px_5()
            .flex()
            .items_center()
            .gap_5()
            .rounded_lg()
            .bg(linear_gradient(
                180.,
                fade(rgb(0x12231b), panel),
                fade(rgb(0x0d1a14), panel),
            ))
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
                            .on_children_prepainted(move |bounds, _, cx| {
                                title_view
                                    .update(cx, |this, _| {
                                        this.title_bounds = bounds.first().copied();
                                    })
                                    .ok();
                            })
                            .child(
                                // 标题区：播放时超长文本滚动（跑马灯），否则省略号。
                                div().h(px(18.)).min_w(px(0.)).overflow_hidden().child(
                                    if let Some(marquee) = marquee {
                                        div()
                                            .whitespace_nowrap()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(TEXT))
                                            .child(marquee_title)
                                            .with_animation(
                                                "player-marquee",
                                                Animation::new(marquee.period)
                                                    .repeat()
                                                    .with_easing(|delta| delta),
                                                move |el, delta| el.ml(px(-delta * marquee.shift)),
                                            )
                                            .into_any_element()
                                    } else {
                                        div()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(if has_track {
                                                rgb(TEXT)
                                            } else {
                                                rgb(MUTED)
                                            })
                                            .text_ellipsis()
                                            .child(title)
                                            .into_any_element()
                                    },
                                ),
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
                                            .children(loop_region.map(|(start, end)| {
                                                let span = self.duration.as_secs_f64();
                                                if span <= 0.0 {
                                                    return div().absolute();
                                                }
                                                let left = (start / span).clamp(0.0, 1.0);
                                                let width =
                                                    ((end - start) / span).clamp(0.0, 1.0 - left);
                                                div()
                                                    .absolute()
                                                    .top(px(0.))
                                                    .h_full()
                                                    .left(relative(left as f32))
                                                    .w(relative(width as f32))
                                                    .rounded_full()
                                                    .bg(fade(rgb(FAVORITE), 0.65))
                                            }))
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
                            .text_right()
                            .child(if loop_note.is_empty() {
                                format!("{state_label} · {mode_name}")
                            } else {
                                format!("{state_label} · {loop_note}")
                            }),
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

    // ---------- 渲染：动作菜单与对话框 ----------

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
            .min(window.viewport_size().height - px(200.))
            .max(px(8.));
        let rename_key = key.clone();
        let move_key = key.clone();
        let export_key = key.clone();
        let loop_key = key.clone();
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
                            "menu-loop",
                            "repeat",
                            "循环点",
                            false,
                            cx,
                            move |this, window, cx| {
                                this.open_loop_editor(&loop_key, window, cx);
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
        let primary = label == "确定" || label == "保存节目";
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
            Dialog::Loop { key } => (
                format!("循环点 · {}", self.song_title(key)),
                "设置 A/B 循环区间（单位：秒）。启用后播放器会在区间内采样级无缝循环。".to_owned(),
            ),
        };
        let show_field = matches!(dialog, Dialog::Rename { .. } | Dialog::NewCategory);
        let loop_dialog = matches!(dialog, Dialog::Loop { .. });
        let loop_key = match &dialog {
            Dialog::Loop { key } => Some(key.clone()),
            _ => None,
        };
        let has_existing_loop = loop_key
            .as_deref()
            .and_then(|key| self.library.loop_point(key))
            .is_some();
        let loop_key_for_clear = loop_key.clone();
        let loop_enabled = self.loop_enabled_pending;
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
                        .when(loop_dialog, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .child(self.loop_start_field.clone())
                                    .child(self.loop_end_field.clone()),
                            )
                            .child(
                                div().flex().child(
                                    div()
                                        .id("loop-enabled")
                                        .h(px(30.))
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .text_size(px(11.))
                                        .bg(if loop_enabled {
                                            fade(rgb(ACCENT), 0.14)
                                        } else {
                                            fade(rgb(0xffffff), 0.04)
                                        })
                                        .border_1()
                                        .border_color(if loop_enabled {
                                            fade(rgb(ACCENT), 0.35)
                                        } else {
                                            fade(rgb(LINE), 1.)
                                        })
                                        .text_color(if loop_enabled {
                                            rgb(ACCENT)
                                        } else {
                                            rgb(MUTED)
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.loop_enabled_pending = !this.loop_enabled_pending;
                                            cx.notify();
                                        }))
                                        .child(icon(
                                            "repeat",
                                            13.,
                                            if loop_enabled {
                                                rgb(ACCENT)
                                            } else {
                                                rgb(MUTED)
                                            },
                                        ))
                                        .child(if loop_enabled {
                                            "启用循环"
                                        } else {
                                            "关闭循环（保留标记）"
                                        }),
                                ),
                            )
                            .when(has_existing_loop, |this| {
                                this.child(div().flex().child(self.menu_action(
                                    "loop-clear",
                                    "x",
                                    "清除循环点标记",
                                    false,
                                    cx,
                                    move |this, _, cx| {
                                        if let Some(key) = loop_key_for_clear.clone() {
                                            match this.library.remove_loop_point(&key) {
                                                Ok(_) => {
                                                    this.notice = "已清除循环点".into();
                                                    this.refresh_loop_region();
                                                }
                                                Err(error) => this.notice = error,
                                            }
                                        }
                                        this.dialog = None;
                                        cx.notify();
                                    },
                                )))
                            })
                        })
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
                                .when(show_field || loop_dialog, |this| {
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

    // ---------- 渲染：歌曲选择弹窗 ----------

    fn picker_layer(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let picker = self.picker.as_ref()?;
        let multi = picker.multi;
        let selected = picker.selected.clone();
        let query = self.picker_search.read(cx).text().to_lowercase();
        let songs: Vec<Song> = self
            .library
            .snapshot
            .songs
            .iter()
            .filter(|song| {
                query.is_empty()
                    || song.title.to_lowercase().contains(&query)
                    || song
                        .artist
                        .as_deref()
                        .is_some_and(|artist| artist.to_lowercase().contains(&query))
                    || song.category.to_lowercase().contains(&query)
            })
            .cloned()
            .collect();
        let count = songs.len();
        let selected_count = selected.len();
        let existing: HashSet<String> = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.songs.iter().map(|item| item.key.clone()).collect())
            .unwrap_or_default();

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
                        this.picker = None;
                        window.focus(&this.focus);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .w(px(520.))
                        .h(px(560.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_md()
                        .bg(rgb(PANEL))
                        .border_1()
                        .border_color(rgb(LINE))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(if multi {
                            div().text_lg().child("选择歌曲")
                        } else {
                            div().text_lg().child("替换歌曲")
                        })
                        .child(div().text_xs().text_color(rgb(MUTED)).child(if multi {
                            "在列表中勾选要加入节目的歌曲；已存在的曲目会自动跳过。"
                        } else {
                            "选择一首歌替换当前条目。"
                        }))
                        .child(self.picker_search.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .overflow_hidden()
                                .rounded_md()
                                .border_1()
                                .border_color(fade(rgb(LINE), 0.7))
                                .child(if count == 0 {
                                    div()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(12.))
                                        .text_color(rgb(FAINT))
                                        .child("没有匹配的曲目")
                                        .into_any_element()
                                } else {
                                    uniform_list(
                                        "picker-songs",
                                        count,
                                        cx.processor(
                                            move |this, range: Range<usize>, _window, cx| {
                                                let picker = match &this.picker {
                                                    Some(picker) => picker.selected.clone(),
                                                    None => return Vec::new(),
                                                };
                                                let query = this
                                                    .picker_search
                                                    .read(cx)
                                                    .text()
                                                    .to_lowercase();
                                                let songs: Vec<Song> = this
                                                    .library
                                                    .snapshot
                                                    .songs
                                                    .iter()
                                                    .filter(|song| {
                                                        query.is_empty()
                                                            || song
                                                                .title
                                                                .to_lowercase()
                                                                .contains(&query)
                                                            || song.artist.as_deref().is_some_and(
                                                                |artist| {
                                                                    artist
                                                                        .to_lowercase()
                                                                        .contains(&query)
                                                                },
                                                            )
                                                            || song
                                                                .category
                                                                .to_lowercase()
                                                                .contains(&query)
                                                    })
                                                    .cloned()
                                                    .collect();
                                                let mut rows = Vec::new();
                                                for index in range {
                                                    let Some(song) = songs.get(index) else {
                                                        continue;
                                                    };
                                                    rows.push(this.picker_row(
                                                        song,
                                                        picker.contains(&song.key),
                                                        existing.contains(&song.key),
                                                        cx,
                                                    ));
                                                }
                                                rows
                                            },
                                        ),
                                    )
                                    .h_full()
                                    .w_full()
                                    .into_any_element()
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap_2()
                                .child(div().text_xs().text_color(rgb(FAINT)).child(if multi {
                                    format!("已选 {selected_count} 首")
                                } else {
                                    String::new()
                                }))
                                .child(div().flex_1())
                                .child(self.tiny_button(
                                    "取消",
                                    "picker-cancel",
                                    cx,
                                    |this, window, cx| {
                                        this.picker = None;
                                        window.focus(&this.focus);
                                        cx.notify();
                                    },
                                ))
                                .child(self.tiny_button(
                                    "确定",
                                    "picker-ok",
                                    cx,
                                    |this, _, cx| {
                                        this.confirm_song_picker(cx);
                                    },
                                )),
                        ),
                ),
        )
    }

    fn picker_row(
        &mut self,
        song: &Song,
        selected: bool,
        already_in_draft: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let key = song.key.clone();
        let category = self
            .library
            .snapshot
            .categories
            .iter()
            .find(|category| category.id == song.category)
            .map(|category| category.name_zh.clone())
            .unwrap_or_else(|| song.category.clone());
        div()
            .id(SharedString::from(format!("picker-row-{}", song.key)))
            .h(px(44.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(fade(rgb(LINE), 0.45))
            .cursor_pointer()
            .bg(if selected {
                fade(rgb(ACCENT), 0.09)
            } else {
                fade(rgb(BG), 0.)
            })
            .hover(|style| style.bg(fade(rgb(ACCENT), 0.05)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let clicked = key.clone();
                let Some(picker) = this.picker.as_mut() else {
                    return;
                };
                if picker.multi {
                    if picker.selected.contains(&clicked) {
                        picker.selected.remove(&clicked);
                    } else {
                        picker.selected.insert(clicked);
                    }
                } else {
                    picker.selected = HashSet::from([clicked]);
                }
                cx.notify();
            }))
            .child(if selected {
                icon("check", 14., rgb(ACCENT)).into_any_element()
            } else {
                div()
                    .size(px(14.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(fade(rgb(LINE), 1.))
                    .into_any_element()
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(12.5))
                            .text_color(rgb(TEXT))
                            .text_ellipsis()
                            .child(song.title.clone()),
                    )
                    .when(already_in_draft, |this| {
                        this.child(
                            div()
                                .text_size(px(9.))
                                .text_color(rgb(FAINT))
                                .child("已在节目"),
                        )
                    }),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(rgb(FAINT))
                    .child(category),
            )
            .into_any_element()
    }

    // ---------- 渲染：导出设置弹层 ----------

    fn export_popover_layer(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.export_popover {
            return None;
        }
        let (ffmpeg, presets) = self.library.export_info();
        let enabled = self.library.prefs.loudness_normalization;
        let scene = self.library.prefs.export_scene.clone();
        let selected = presets
            .iter()
            .find(|preset| preset.id == scene)
            .or_else(|| presets.first())
            .cloned();
        let hint = match (&enabled, selected.as_ref()) {
            (false, _) => "已关闭 · 仅复制原文件".to_owned(),
            (true, Some(preset)) => {
                format!(
                    "已开启 · 目标 {} LUFS · {}",
                    preset.target_lufs, preset.label
                )
            }
            (true, None) => "已开启 · 人耳响度归一化".to_owned(),
        };

        Some(
            div()
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.export_popover = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(14.))
                        .top(px(62.))
                        .w(px(320.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_lg()
                        .bg(rgb(0x14241d))
                        .border_1()
                        .border_color(rgb(LINE))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("导出设置"),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(FAINT))
                                        .child("单首导出通用"),
                                ),
                        )
                        .child(
                            div()
                                .id("export-toggle")
                                .flex()
                                .items_center()
                                .gap_3()
                                .p_3()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if enabled {
                                    fade(rgb(ACCENT), 0.1)
                                } else {
                                    fade(rgb(0xffffff), 0.03)
                                })
                                .border_1()
                                .border_color(if enabled {
                                    fade(rgb(ACCENT), 0.3)
                                } else {
                                    fade(rgb(LINE), 1.)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let enabled = this.library.prefs.loudness_normalization;
                                    let scene = this.library.prefs.export_scene.clone();
                                    if let Err(error) =
                                        this.library.set_export_settings(!enabled, &scene)
                                    {
                                        this.notice = error;
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .child(div().text_size(px(12.5)).child("响度归一化"))
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .text_color(rgb(FAINT))
                                                .child(hint),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(36.))
                                        .h(px(20.))
                                        .rounded_full()
                                        .bg(if enabled { rgb(ACCENT) } else { rgb(LINE) })
                                        .p(px(2.))
                                        .child(
                                            div()
                                                .size(px(16.))
                                                .rounded_full()
                                                .bg(rgb(TEXT))
                                                .when(enabled, |this| this.ml(relative(1.0))),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.))
                                .text_color(rgb(FAINT))
                                .child("归一化目标")
                                .child("选择导出用途，决定目标响度"),
                        )
                        .children(presets.into_iter().map(|preset| {
                            let active = enabled && preset.id == scene;
                            let is_default = preset.id == bgm_core::loudness::DEFAULT_PRESET_ID;
                            div()
                                .id(SharedString::from(format!("export-preset-{}", preset.id)))
                                .h(px(38.))
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if active {
                                    fade(rgb(ACCENT), 0.14)
                                } else {
                                    fade(rgb(0xffffff), 0.03)
                                })
                                .border_1()
                                .border_color(if active {
                                    fade(rgb(ACCENT), 0.35)
                                } else {
                                    fade(rgb(LINE), 0.8)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Err(error) =
                                        this.library.set_export_settings(true, &preset.id)
                                    {
                                        this.notice = error;
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(12.))
                                        .text_color(if active { rgb(TEXT) } else { rgb(MUTED) })
                                        .child(preset.label),
                                )
                                .when(is_default, |this| {
                                    this.child(
                                        div()
                                            .px_1()
                                            .rounded(px(4.))
                                            .bg(fade(rgb(ACCENT), 0.14))
                                            .text_size(px(9.))
                                            .text_color(rgb(ACCENT))
                                            .child("默认"),
                                    )
                                })
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(rgb(FAINT))
                                        .child(format!("{} LUFS", preset.target_lufs)),
                                )
                                .when(active, |this| this.child(icon("check", 13., rgb(ACCENT))))
                        }))
                        .when_some(selected, |this, preset| {
                            this.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(FAINT))
                                    .child(preset.description),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(fade(rgb(0xffffff), 0.03))
                                .text_size(px(10.5))
                                .text_color(if ffmpeg.is_some() {
                                    rgb(MUTED)
                                } else {
                                    rgb(0xdac590)
                                })
                                .child(if ffmpeg.is_some() {
                                    icon("check", 12., rgb(ACCENT)).into_any_element()
                                } else {
                                    icon("alert", 12., rgb(0xdac590)).into_any_element()
                                })
                                .child(match ffmpeg {
                                    Some(path) => format!("ffmpeg 已就绪：{}", path.display()),
                                    None => "未检测到 ffmpeg，导出将回退为复制原文件".to_owned(),
                                }),
                        ),
                ),
        )
    }

    // ---------- 渲染：壁纸 ----------

    /// 壁纸层：渐变或图片，绘制在最底层；面板随透明度设置变透。
    fn wallpaper_layer(&self) -> Option<impl IntoElement + use<>> {
        let view = self.wallpaper_view();
        if !view.has_wallpaper {
            return None;
        }
        let mask = (0.4 - view.opacity * 0.44).max(0.0);
        let mut layer = div().absolute().inset_0();
        match &view.source {
            WallpaperSource::Gradient(id) => {
                layer = layer.child(match id.as_str() {
                    "gradient-blue" => div().absolute().inset_0().bg(linear_gradient(
                        165.,
                        rgb(0x11406e),
                        rgb(0x050d17),
                    )),
                    "gradient-green" => div().absolute().inset_0().bg(linear_gradient(
                        165.,
                        rgb(0x0f3f2c),
                        rgb(0x040d09),
                    )),
                    _ => div().absolute().inset_0().bg(linear_gradient(
                        165.,
                        rgb(0x45151f),
                        rgb(0x0d0407),
                    )),
                });
            }
            WallpaperSource::Image {
                path,
                embedded,
                contain,
            } => {
                // 内置壁纸走编译期内嵌资源，自定义壁纸直接读磁盘文件。
                let image = if *embedded {
                    img(path.as_str())
                } else {
                    img(Path::new(path))
                };
                let fit = if *contain {
                    ObjectFit::Contain
                } else {
                    ObjectFit::Cover
                };
                layer = layer.child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(image.size_full().object_fit(fit)),
                );
            }
            WallpaperSource::None => return None,
        }
        Some(
            layer
                .child(div().absolute().inset_0().bg(fade(rgb(BG), mask)))
                .into_any_element(),
        )
    }

    fn wallpaper_view(&self) -> WallpaperView {
        let prefs = &self.library.prefs;
        let id = prefs.wallpaper.clone();
        let opacity = prefs.wallpaper_opacity.clamp(0.0, 0.95);
        let contain = prefs.wallpaper_fit == "contain";
        let source = match id.as_str() {
            "gradient-blue" | "gradient-green" | "gradient-red" => {
                WallpaperSource::Gradient(id.clone())
            }
            "builtin-2" => WallpaperSource::Image {
                path: "bg/2.jpg".into(),
                embedded: true,
                contain,
            },
            "custom" => match prefs
                .custom_wallpaper
                .as_deref()
                .filter(|path| !path.to_lowercase().ends_with(".mp4"))
            {
                Some(path) => WallpaperSource::Image {
                    path: path.to_owned(),
                    embedded: false,
                    contain,
                },
                None => WallpaperSource::None,
            },
            // builtin-1 是 mp4 动态壁纸，GPUI 暂不支持视频渲染。
            _ => WallpaperSource::None,
        };
        WallpaperView {
            has_wallpaper: id != "default" && !matches!(source, WallpaperSource::None),
            opacity,
            source,
        }
    }

    fn wallpaper_popover_layer(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.wallpaper_popover {
            return None;
        }
        let prefs = &self.library.prefs;
        let current = prefs.wallpaper.clone();
        let custom_name = prefs
            .custom_wallpaper
            .as_deref()
            .map(|path| {
                Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_owned())
            })
            .unwrap_or_else(|| "自定义壁纸".into());
        let opacity = self.library.prefs.wallpaper_opacity.clamp(0.0, 1.0);
        let fit = self.library.prefs.wallpaper_fit.clone();
        let opacity_view = cx.entity().downgrade();
        let options: Vec<(&'static str, &'static str, &'static str)> = vec![
            ("default", "默认样式", "当前配色"),
            ("gradient-blue", "渐变蓝", "深邃静谧"),
            ("gradient-green", "渐变绿", "翡翠流光"),
            ("gradient-red", "渐变红", "暖色余晖"),
            ("builtin-2", "雪山云海", "JPG 静态"),
        ];

        Some(
            div()
                .absolute()
                .inset_0()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.wallpaper_popover = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(52.))
                        .top(px(62.))
                        .w(px(300.))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_lg()
                        .bg(rgb(0x14241d))
                        .border_1()
                        .border_color(rgb(LINE))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("界面壁纸"),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(rgb(FAINT))
                                        .child("每日心情换一张"),
                                ),
                        )
                        .children(options.into_iter().map(|(id, label, hint)| {
                            let active = current == id;
                            div()
                                .id(SharedString::from(format!("wall-{id}")))
                                .h(px(34.))
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if active {
                                    fade(rgb(ACCENT), 0.14)
                                } else {
                                    fade(rgb(0xffffff), 0.03)
                                })
                                .border_1()
                                .border_color(if active {
                                    fade(rgb(ACCENT), 0.35)
                                } else {
                                    fade(rgb(LINE), 0.8)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Err(error) = this.library.set_wallpaper(id, None) {
                                        this.notice = error;
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(12.))
                                        .text_color(if active { rgb(TEXT) } else { rgb(MUTED) })
                                        .child(label),
                                )
                                .child(div().text_size(px(10.)).text_color(rgb(FAINT)).child(hint))
                                .when(active, |this| this.child(icon("check", 13., rgb(ACCENT))))
                        }))
                        .child(
                            div()
                                .id("wall-custom")
                                .h(px(34.))
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if current == "custom" {
                                    fade(rgb(ACCENT), 0.14)
                                } else {
                                    fade(rgb(0xffffff), 0.03)
                                })
                                .border_1()
                                .border_color(if current == "custom" {
                                    fade(rgb(ACCENT), 0.35)
                                } else {
                                    fade(rgb(LINE), 0.8)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.choose_custom_wallpaper(cx);
                                }))
                                .child(icon("image", 13., rgb(MUTED)))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_size(px(12.))
                                        .text_color(if current == "custom" {
                                            rgb(TEXT)
                                        } else {
                                            rgb(MUTED)
                                        })
                                        .text_ellipsis()
                                        .child(if current == "custom" {
                                            custom_name
                                        } else {
                                            "自定义图片…".to_owned()
                                        }),
                                )
                                .when(current == "custom", |this| {
                                    this.child(icon("check", 13., rgb(ACCENT)))
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.))
                                .text_color(rgb(FAINT))
                                .child("透明度")
                                .child(
                                    div()
                                        .flex_1()
                                        .on_children_prepainted(move |bounds, _, cx| {
                                            opacity_view
                                                .update(cx, |this, _| {
                                                    this.wallpaper_opacity_bounds =
                                                        bounds.first().copied();
                                                })
                                                .ok();
                                        })
                                        .id("wallpaper-opacity")
                                        .h(px(14.))
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(Self::wallpaper_opacity_down_at),
                                        )
                                        .on_mouse_move(cx.listener(Self::wallpaper_opacity_move))
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::wallpaper_opacity_up_at),
                                        )
                                        .on_mouse_up_out(
                                            MouseButton::Left,
                                            cx.listener(Self::wallpaper_opacity_up_at),
                                        )
                                        .child(
                                            div()
                                                .h(px(3.))
                                                .w_full()
                                                .rounded_full()
                                                .bg(rgb(LINE))
                                                .child(
                                                    div()
                                                        .h_full()
                                                        .w(relative(opacity))
                                                        .rounded_full()
                                                        .bg(rgb(ACCENT)),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(34.))
                                        .text_right()
                                        .text_size(px(10.))
                                        .text_color(rgb(MUTED))
                                        .child(format!("{}%", (opacity * 100.0).round() as i32)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.))
                                .text_color(rgb(FAINT))
                                .child("填充")
                                .child(
                                    div()
                                        .id("wall-fit-cover")
                                        .px_2()
                                        .h(px(22.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .text_size(px(10.5))
                                        .bg(if fit == "cover" {
                                            fade(rgb(ACCENT), 0.16)
                                        } else {
                                            fade(rgb(0xffffff), 0.04)
                                        })
                                        .text_color(if fit == "cover" {
                                            rgb(ACCENT)
                                        } else {
                                            rgb(MUTED)
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Err(error) =
                                                this.library.set_wallpaper_fit("cover")
                                            {
                                                this.notice = error;
                                            }
                                            cx.notify();
                                        }))
                                        .child("铺满裁剪"),
                                )
                                .child(
                                    div()
                                        .id("wall-fit-contain")
                                        .px_2()
                                        .h(px(22.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .text_size(px(10.5))
                                        .bg(if fit == "contain" {
                                            fade(rgb(ACCENT), 0.16)
                                        } else {
                                            fade(rgb(0xffffff), 0.04)
                                        })
                                        .text_color(if fit == "contain" {
                                            rgb(ACCENT)
                                        } else {
                                            rgb(MUTED)
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if let Err(error) =
                                                this.library.set_wallpaper_fit("contain")
                                            {
                                                this.notice = error;
                                            }
                                            cx.notify();
                                        }))
                                        .child("完整展示"),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(rgb(FAINT))
                                .child("mp4 动态壁纸暂不支持，仅 jpg / png / webp 静态图。"),
                        ),
                ),
        )
    }

    fn choose_custom_wallpaper(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择界面壁纸".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    let allowed =
                        path.extension()
                            .and_then(|ext| ext.to_str())
                            .is_some_and(|ext| {
                                ["jpg", "jpeg", "png", "webp"]
                                    .iter()
                                    .any(|item| ext.eq_ignore_ascii_case(item))
                            });
                    if !allowed {
                        this.notice =
                            "仅支持 jpg / png / webp 格式的壁纸（mp4 动态壁纸暂不支持）".into();
                    } else if let Err(error) = this
                        .library
                        .set_wallpaper("custom", Some(&path.to_string_lossy()))
                    {
                        this.notice = error;
                    } else {
                        this.wallpaper_popover = false;
                        this.notice = "已应用自定义壁纸".into();
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // ---------- 渲染：节目单页 ----------

    fn schedule_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel = self.panel_alpha();
        let entries = self.library.schedule.entries.clone();
        let editing_id = self
            .schedule_draft
            .as_ref()
            .and_then(|draft| draft.id.clone());
        let draft_open = self.schedule_draft.is_some();
        let now = self.scheduler_now.unwrap_or_else(Self::local_now);
        let current = self.current_program();
        let current_song = self.current_program_song();
        let next = self.next_program();
        let notice = self.notice.clone();

        let now_label = format!("{:02}:{:02}:{:02}", now.hour(), now.minute(), now.second());
        let current_label = match (&current, &current_song) {
            (Some(entry), Some(song)) => {
                format!("{} · {}", entry.name, self.song_title(&song.key))
            }
            (Some(entry), None) => entry.name.clone(),
            (None, _) => "未编排".to_owned(),
        };
        let next_label = next
            .map(|(entry, tomorrow)| {
                format!(
                    "{}{} {}",
                    if tomorrow { "明天 " } else { "" },
                    entry.start,
                    entry.name
                )
            })
            .unwrap_or_else(|| "暂无节目".to_owned());

        let mut list = div().flex().flex_col().gap_2().px_6().pb(px(130.));
        for entry in &entries {
            list = list.child(self.schedule_entry_row(entry, &current, &editing_id, window, cx));
        }
        if entries.is_empty() {
            list = list.child(
                div()
                    .mx_6()
                    .py_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded_lg()
                    .bg(fade(rgb(PANEL), panel))
                    .border_1()
                    .border_color(fade(rgb(LINE), 0.75))
                    .child(icon("clock", 30., rgb(0x3c5148)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(MUTED))
                            .child("还没有节目。点「新增节目」，把一天的时间轴排起来。"),
                    ),
            );
        }

        div()
            .id("schedule-page")
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .bg(linear_gradient(
                180.,
                fade(rgb(0x091510), panel),
                fade(rgb(BG), panel),
            ))
            .child(
                div()
                    .px_6()
                    .pt_5()
                    .pb_3()
                    .flex()
                    .items_start()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(10.))
                                    .text_color(rgb(FAINT))
                                    .child(icon("clock", 13., rgb(FAINT)))
                                    .child("SCENE SCHEDULE"),
                            )
                            .child(
                                div()
                                    .text_size(px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("节目单 · 场景排程"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(MUTED))
                                    .child("每天按时间段自动切换节目歌曲与音量。节目之间严禁重叠；歌曲按顺序播放，也可以为每首歌曲指定节目范围内的起止时间。"),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .px_4()
                            .py_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .rounded_lg()
                            .bg(fade(rgb(PANEL), panel))
                            .border_1()
                            .border_color(fade(rgb(LINE), 0.75))
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(rgb(ACCENT))
                                    .child(now_label),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(MUTED))
                                    .child(format!("当前：{current_label}")),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(FAINT))
                                    .child(format!("下一档：{next_label}")),
                            ),
                    ),
            )
            .child(
                div()
                    .px_6()
                    .pb_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.hero_button("schedule-create", "plus", "新增节目", true, cx, {
                        |this, window, cx| this.open_schedule_create(window, cx)
                    }))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(FAINT))
                            .child(format!(
                                "每天执行 · 共 {} 档 · 时间范围不可重叠",
                                entries.len()
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .max_w(px(360.))
                            .text_size(px(11.))
                            .text_color(rgb(FAINT))
                            .text_ellipsis()
                            .child(notice),
                    ),
            )
            .when(draft_open, |this| {
                this.child(self.schedule_editor(window, cx))
            })
            .child(list)
    }

    fn schedule_entry_row(
        &mut self,
        entry: &ScheduleEntry,
        current: &Option<ScheduleEntry>,
        editing_id: &Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel = self.panel_alpha();
        let is_current = current.as_ref().is_some_and(|item| item.id == entry.id);
        let is_editing = editing_id.as_deref() == Some(entry.id.as_str());
        let expanded = self.expanded_schedule.contains(&entry.id) || is_editing;
        let entry_clone = entry.clone();
        let entry_id = entry.id.clone();
        let headline = if entry.songs.is_empty() {
            "未添加歌曲".to_owned()
        } else {
            format!(
                "{} 等 {} 首",
                self.song_title(&entry.songs[0].key),
                entry.songs.len()
            )
        };
        let volume_label = entry
            .volume
            .map(|volume| format!("音量 {}%", (volume * 100.0).round() as i32))
            .unwrap_or_else(|| "保持当前音量".to_owned());

        let mut rows = Vec::new();
        for (index, song) in entry.songs.iter().enumerate() {
            let duration = self
                .library
                .song(&song.key)
                .and_then(|song| song.duration_ms)
                .map(|ms| format_time(Duration::from_millis(ms as u64)))
                .unwrap_or_else(|| "--:--".into());
            let category = self
                .library
                .song(&song.key)
                .map(|song| {
                    self.library
                        .snapshot
                        .categories
                        .iter()
                        .find(|category| category.id == song.category)
                        .map(|category| category.name_zh.clone())
                        .unwrap_or_else(|| song.category.clone())
                })
                .unwrap_or_else(|| "不在曲库".into());
            let playing_here = is_current
                && self
                    .current_key()
                    .as_deref()
                    .is_some_and(|key| key == song.key);
            rows.push(
                div()
                    .h(px(34.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .bg(fade(rgb(BG), 0.5))
                    .text_size(px(11.))
                    .child(
                        div()
                            .w(px(20.))
                            .text_color(rgb(FAINT))
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .w(px(92.))
                            .text_color(rgb(FAINT))
                            .child(format!("{}–{}", song.start, song.end)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(TEXT))
                                    .text_ellipsis()
                                    .child(self.song_title(&song.key)),
                            )
                            .when(playing_here, |this| {
                                this.child(
                                    div()
                                        .px_1()
                                        .rounded(px(4.))
                                        .bg(fade(rgb(ACCENT), 0.14))
                                        .text_size(px(9.))
                                        .text_color(rgb(ACCENT))
                                        .child("播放中"),
                                )
                            }),
                    )
                    .child(div().text_color(rgb(FAINT)).child(duration))
                    .child(div().text_color(rgb(FAINT)).child(category)),
            );
        }

        div()
            .mx_6()
            .mb_2()
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .bg(fade(rgb(PANEL), panel))
            .border_1()
            .border_color(if is_current {
                fade(rgb(ACCENT), 0.4)
            } else {
                fade(rgb(LINE), 0.75)
            })
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id(SharedString::from(format!("slot-toggle-{}", entry.id)))
                            .cursor_pointer()
                            .child(icon(
                                "chevron-down",
                                14.,
                                if expanded { rgb(ACCENT) } else { rgb(FAINT) },
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.expanded_schedule.contains(&entry_id) {
                                    this.expanded_schedule.remove(&entry_id);
                                } else {
                                    this.expanded_schedule.insert(entry_id.clone());
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .w(px(96.))
                            .flex_none()
                            .text_size(px(13.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(if is_current { rgb(ACCENT) } else { rgb(TEXT) })
                            .child(format!("{}–{}", entry.start, entry.end)),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("slot-body-{}", entry.id)))
                            .flex_1()
                            .min_w(px(0.))
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.expanded_schedule.contains(&entry_clone.id) {
                                    this.expanded_schedule.remove(&entry_clone.id);
                                } else {
                                    this.expanded_schedule.insert(entry_clone.id.clone());
                                }
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(13.))
                                    .child(div().child(entry.name.clone()))
                                    .when(is_current, |this| {
                                        this.child(
                                            div()
                                                .px_1()
                                                .rounded(px(4.))
                                                .bg(fade(rgb(ACCENT), 0.14))
                                                .text_size(px(9.))
                                                .text_color(rgb(ACCENT))
                                                .child("进行中"),
                                        )
                                    })
                                    .when(!entry.enabled, |this| {
                                        this.child(
                                            div()
                                                .px_1()
                                                .rounded(px(4.))
                                                .bg(fade(rgb(0xffffff), 0.06))
                                                .text_size(px(9.))
                                                .text_color(rgb(MUTED))
                                                .child("已停用"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(10.5))
                                    .text_color(rgb(FAINT))
                                    .child(format!("{headline} · {volume_label}")),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.slot_action(
                                format!("slot-edit-{}", entry.id),
                                "编辑",
                                false,
                                cx,
                                {
                                    let id = entry.id.clone();
                                    move |this, window, cx| {
                                        this.open_schedule_editor(&id, window, cx)
                                    }
                                },
                            ))
                            .child(self.slot_action(
                                format!("slot-apply-{}", entry.id),
                                "应用",
                                !entry.enabled,
                                cx,
                                {
                                    let entry = entry.clone();
                                    move |this, _, cx| {
                                        this.apply_schedule_entry(&entry, None, true, true);
                                        cx.notify();
                                    }
                                },
                            ))
                            .child(self.slot_action(
                                format!("slot-toggle-{}", entry.id),
                                if entry.enabled { "停用" } else { "启用" },
                                false,
                                cx,
                                {
                                    let id = entry.id.clone();
                                    move |this, _, cx| this.toggle_schedule_entry(&id, cx)
                                },
                            ))
                            .child(self.slot_action(
                                format!("slot-remove-{}", entry.id),
                                "删除",
                                true,
                                cx,
                                {
                                    let id = entry.id.clone();
                                    move |this, window, cx| {
                                        this.remove_schedule_entry(&id, window, cx)
                                    }
                                },
                            )),
                    ),
            )
            .when(expanded && !is_editing, |this| {
                this.child(div().px_4().pb_3().flex().flex_col().gap_1().children(rows))
            })
    }

    fn slot_action(
        &mut self,
        id: impl Into<SharedString>,
        label: &'static str,
        danger: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id.into())
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .text_size(px(11.))
            .text_color(if danger { rgb(DANGER) } else { rgb(MUTED) })
            .hover(|style| {
                style.bg(fade(rgb(0xffffff), 0.08)).text_color(if danger {
                    rgb(DANGER)
                } else {
                    rgb(TEXT)
                })
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(label)
            .into_any_element()
    }

    fn schedule_editor(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel = self.panel_alpha();
        let draft_name = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.id.is_some())
            .unwrap_or(false);
        let volume_enabled = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.volume_enabled)
            .unwrap_or(false);
        let draft_volume = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.volume)
            .unwrap_or(0.5);
        let enabled = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.enabled)
            .unwrap_or(true);
        let song_count = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.songs.len())
            .unwrap_or(0);
        let volume_view = cx.entity().downgrade();
        let draft_name_hint = if draft_name {
            "编辑节目".to_owned()
        } else {
            "新节目".to_owned()
        };

        // 歌曲行：标题 + 起止时间输入 + 替换/移除/上下移。
        let mut song_rows = Vec::new();
        if let Some(draft) = self.schedule_draft.as_ref() {
            for index in 0..draft.songs.len() {
                song_rows.push(self.draft_song_row(index, _window, cx));
            }
        }

        div()
            .mx_6()
            .mb_3()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .bg(fade(rgb(PANEL), panel))
            .border_1()
            .border_color(fade(rgb(ACCENT), 0.3))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_3()
                    .child(
                        div()
                            .w(px(200.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(FAINT))
                                    .child("节目名称"),
                            )
                            .child(self.schedule_name_field.clone()),
                    )
                    .child(
                        div()
                            .w(px(90.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(FAINT))
                                    .child("开始时间"),
                            )
                            .child(self.schedule_start_field.clone()),
                    )
                    .child(
                        div()
                            .w(px(90.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(FAINT))
                                    .child("结束时间"),
                            )
                            .child(self.schedule_end_field.clone()),
                    )
                    .child(
                        div()
                            .id("schedule-enabled")
                            .h(px(34.))
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_md()
                            .cursor_pointer()
                            .text_size(px(11.5))
                            .bg(if enabled {
                                fade(rgb(ACCENT), 0.14)
                            } else {
                                fade(rgb(0xffffff), 0.04)
                            })
                            .border_1()
                            .border_color(if enabled {
                                fade(rgb(ACCENT), 0.35)
                            } else {
                                fade(rgb(LINE), 1.)
                            })
                            .text_color(if enabled { rgb(ACCENT) } else { rgb(MUTED) })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(draft) = this.schedule_draft.as_mut() {
                                    draft.enabled = !draft.enabled;
                                }
                                cx.notify();
                            }))
                            .child(if enabled {
                                "启用这个节目"
                            } else {
                                "已停用"
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .id("draft-volume-enabled")
                            .h(px(26.))
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(6.))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .bg(if volume_enabled {
                                fade(rgb(ACCENT), 0.14)
                            } else {
                                fade(rgb(0xffffff), 0.04)
                            })
                            .text_color(if volume_enabled {
                                rgb(ACCENT)
                            } else {
                                rgb(MUTED)
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(draft) = this.schedule_draft.as_mut() {
                                    draft.volume_enabled = !draft.volume_enabled;
                                }
                                cx.notify();
                            }))
                            .child("节目开始时调整音量"),
                    )
                    .child(
                        div()
                            .w(px(160.))
                            .on_children_prepainted(move |bounds, _, cx| {
                                volume_view
                                    .update(cx, |this, _| {
                                        this.draft_volume_bounds = bounds.first().copied();
                                    })
                                    .ok();
                            })
                            .id("draft-volume")
                            .h(px(14.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(Self::draft_volume_down_at),
                            )
                            .on_mouse_move(cx.listener(Self::draft_volume_move))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::draft_volume_up_at))
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(Self::draft_volume_up_at),
                            )
                            .child(
                                div().h(px(3.)).w_full().rounded_full().bg(rgb(LINE)).child(
                                    div()
                                        .h_full()
                                        .w(relative(if volume_enabled {
                                            draft_volume
                                        } else {
                                            0.0
                                        }))
                                        .rounded_full()
                                        .bg(rgb(ACCENT)),
                                ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(MUTED))
                            .child(format!("{}%", (draft_volume * 100.0).round() as i32)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child("节目歌曲"),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(FAINT))
                            .child(format!("{song_count} 首")),
                    )
                    .child(div().flex_1())
                    .child(self.slot_action(
                        "draft-autofill",
                        "自动排期",
                        false,
                        cx,
                        |this, _, cx| this.auto_fill_draft_songs(cx),
                    ))
                    .child(self.slot_action(
                        "draft-add",
                        "添加歌曲",
                        false,
                        cx,
                        |this, window, cx| {
                            this.open_song_picker(None, window, cx);
                        },
                    )),
            )
            .child(div().flex().flex_col().gap_1().children(song_rows).when(
                song_count == 0,
                |this| {
                    this.child(
                        div()
                            .py_4()
                            .text_size(px(11.5))
                            .text_color(rgb(FAINT))
                            .child("还没有歌曲。点「添加歌曲」，在弹窗中按歌单多选后确定。"),
                    )
                },
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(FAINT))
                            .child(format!(
                                "{draft_name_hint} · 每首歌的起止时间需要落在节目范围内"
                            )),
                    )
                    .child(div().flex_1())
                    .child(
                        self.tiny_button("取消", "schedule-cancel", cx, |this, window, cx| {
                            this.cancel_schedule_edit(window, cx)
                        }),
                    )
                    .child(self.tiny_button(
                        "保存节目",
                        "schedule-save",
                        cx,
                        |this, window, cx| this.save_schedule_draft(window, cx),
                    )),
            )
    }

    fn draft_song_row(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel = self.panel_alpha();
        let (title, category, duration) = {
            let Some(draft) = self.schedule_draft.as_ref() else {
                return div().into_any_element();
            };
            let item = &draft.songs[index];
            let category = self
                .library
                .song(&item.key)
                .map(|song| {
                    self.library
                        .snapshot
                        .categories
                        .iter()
                        .find(|category| category.id == song.category)
                        .map(|category| category.name_zh.clone())
                        .unwrap_or_else(|| song.category.clone())
                })
                .unwrap_or_else(|| "不在曲库".into());
            let duration = self
                .library
                .song(&item.key)
                .and_then(|song| song.duration_ms)
                .map(|ms| format_time(Duration::from_millis(ms as u64)))
                .unwrap_or_else(|| "--:--".into());
            (self.song_title(&item.key), category, duration)
        };
        let start_field = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.song_start_fields[index].clone());
        let end_field = self
            .schedule_draft
            .as_ref()
            .map(|draft| draft.song_end_fields[index].clone());

        div()
            .h(px(46.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .bg(fade(rgb(BG), 0.5 * panel))
            .text_size(px(11.5))
            .child(
                div()
                    .w(px(18.))
                    .text_color(rgb(FAINT))
                    .child(format!("{}", index + 1)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(div().text_color(rgb(TEXT)).text_ellipsis().child(title))
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(rgb(FAINT))
                            .child(format!("{category} · {duration}")),
                    ),
            )
            .children(
                start_field
                    .map(|field| div().w(px(58.)).flex_none().child(field).into_any_element()),
            )
            .children(
                end_field.map(|field| div().w(px(58.)).flex_none().child(field).into_any_element()),
            )
            .child(self.slot_action(
                format!("draft-up-{index}"),
                "上移",
                false,
                cx,
                move |this, _, cx| {
                    if index > 0 {
                        this.move_draft_song(index, index - 1, cx);
                    }
                },
            ))
            .child(self.slot_action(
                format!("draft-down-{index}"),
                "下移",
                false,
                cx,
                move |this, _, cx| {
                    let count = this
                        .schedule_draft
                        .as_ref()
                        .map(|draft| draft.songs.len())
                        .unwrap_or(0);
                    if index + 1 < count {
                        this.move_draft_song(index, index + 1, cx);
                    }
                },
            ))
            .child(self.slot_action(
                format!("draft-replace-{index}"),
                "替换",
                false,
                cx,
                move |this, window, cx| {
                    this.open_song_picker(Some(index), window, cx);
                },
            ))
            .child(self.slot_action(
                format!("draft-remove-{index}"),
                "移除",
                true,
                cx,
                move |this, _, cx| {
                    this.remove_draft_song(index, cx);
                },
            ))
            .into_any_element()
    }
}

/// 界面侧的壁纸视图；`None` 表示当前选择无法渲染（例如 mp4）。
enum WallpaperSource {
    None,
    Gradient(String),
    /// `contain` 为完整展示，否则铺满裁剪。
    Image {
        path: String,
        embedded: bool,
        contain: bool,
    },
}

struct WallpaperView {
    has_wallpaper: bool,
    opacity: f32,
    source: WallpaperSource,
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

/// 简单的纳秒级伪随机数，够用于加权随机选曲。
fn rand_f32() -> f32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(1) as f32
        / u32::MAX as f32
}

fn parse_hhmm(text: &str) -> Option<(u32, u32)> {
    let (hour, minute) = text.trim().split_once(':')?;
    let hour: u32 = hour.trim().parse().ok()?;
    let minute: u32 = minute.trim().parse().ok()?;
    (hour <= 23 && minute <= 59).then_some((hour, minute))
}

fn minutes_of(time: &str) -> Option<u32> {
    parse_hhmm(time).map(|(hour, minute)| hour * 60 + minute)
}

fn format_minutes(total: u32) -> String {
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// 下一小时到再下一小时的默认节目范围。
fn next_hour_range() -> (String, String) {
    let now = Local::now();
    let start = ((now.hour() + 1) % 24) * 60;
    (
        format_minutes(start),
        format_minutes((start + 60).min(23 * 60 + 59)),
    )
}

fn day_key(date: NaiveDate) -> String {
    format!("{}-{}-{}", date.year(), date.month(), date.day())
}

fn hhmm_millis(day: NaiveDate, time: &str) -> Option<i64> {
    let (hour, minute) = parse_hhmm(time)?;
    day.and_hms_opt(hour, minute, 0)?
        .and_local_timezone(Local)
        .single()
        .map(|dt| dt.timestamp_millis())
}

fn latest_song_before(entry: &ScheduleEntry, minutes: u32) -> Option<ScheduleSong> {
    let mut best: Option<&ScheduleSong> = None;
    let mut best_start = -1i32;
    for song in &entry.songs {
        let start = minutes_of(&song.start).unwrap_or(0) as i32;
        if start <= minutes as i32 && start > best_start {
            best = Some(song);
            best_start = start;
        }
    }
    best.cloned()
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

    #[test]
    fn latest_song_before_picks_closest_start() {
        let entry = ScheduleEntry {
            songs: vec![
                ScheduleSong {
                    key: "a".into(),
                    start: "09:00".into(),
                    end: "09:05".into(),
                },
                ScheduleSong {
                    key: "b".into(),
                    start: "09:03".into(),
                    end: "09:10".into(),
                },
            ],
            ..ScheduleEntry::default()
        };
        let due = latest_song_before(&entry, 9 * 60 + 4);
        assert_eq!(due.map(|song| song.key), Some("b".into()));
    }

    #[test]
    fn minutes_of_parses_and_rejects() {
        assert_eq!(minutes_of("09:30"), Some(9 * 60 + 30));
        assert_eq!(minutes_of("24:00"), None);
        assert_eq!(minutes_of("bad"), None);
    }
}
