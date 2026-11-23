//! 曲库窗口。曲库、节目单和歌曲页共用底部播放栏，交互走 GPUI 动作与系统文件框。

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bgm_core::{LoopPoint, PlayMode, ScheduleEntry, ScheduleSong, Song, SortBy};
use chrono::{DateTime, Local, Timelike};
use gpui::{
    Animation, AnimationExt, App, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable,
    Font, FontFeatures, FontStyle, FontWeight, HighlightStyle, ListAlignment, ListOffset,
    ListState, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    PathPromptOptions, Pixels, Point, PromptButton, PromptLevel, Rgba, SharedString, StyledText,
    Subscription, TextRun, TitlebarOptions, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions, canvas, div, fill, img, linear_color_stop, list, point, prelude::*,
    px, relative, rgb, size, uniform_list,
};
use http::StatusCode;
use serde_json::{Value, json};

use bgm_core::CloseAction;

use crate::actions::{
    Confirm, Dismiss, FocusSearch, Mute, PlayPause, SeekBack, SeekForward, VolumeDown, VolumeUp,
};
use crate::api;
use crate::audio::{AudioEngine, DecodeError, OutputDevice, PcmTrack};
use crate::desktop::{self, SystemFont, Tray, WakeLock};
use crate::desktop_lyrics::{self, DesktopLyrics};
use crate::diagnostics::{self, Analysis, Verdict};
use crate::icons::{category_icon, icon};
use crate::library::{
    AsrSession, DEFAULT_ASR_DOWNLOAD_URL, DEFAULT_MODEL_NAME, DownloadMode, DownloadProgress,
    Exporter, Library, LibraryChange, LibraryFilter, MAX_SUBTITLE_SEGMENTS, ModelCatalog,
    ModelDownload, Mutation, SeparatedAudio, SubtitleDelivery, SubtitleEdit, SubtitleFormat,
    SubtitleSegment, SubtitleSummary, delete_subtitle, export_subtitle, format_report,
    list_subtitles, load_subtitle, remap_scheduled_songs,
};
use crate::library::{separation, subtitle_generation};
use crate::lyrics::{self, PlaybackSample};
use crate::playback_queue::{self, FailedTracks, PlayHistory};
use crate::scheduler::{self, Check as ScheduleCheck, Scheduler, minutes_of};
use crate::text_input::TextField;
use crate::wallpaper;
use crate::watch::LibraryWatcher;

use crate::design::*;

/// 循环区间的最短时长，单位为秒。
const MIN_LOOP_SPAN: f64 = 0.2;

#[derive(Clone)]
struct TrackMenu {
    key: String,
    position: Point<Pixels>,
    stems: [Option<PathBuf>; 2],
}

#[derive(Clone)]
enum Dialog {
    EditTrack {
        key: String,
        category: String,
        error: Option<String>,
    },
    NewCategory,
    EditCategory {
        id: String,
    },
    Move {
        key: String,
    },
    MoveBatch {
        keys: Vec<String>,
    },
    Import {
        files: Vec<PathBuf>,
    },
    FileReport {
        summary: String,
        report: bgm_core::BatchReport,
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
    Subtitles,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Playback,
    Models,
    Integrations,
}

struct TrackLyrics {
    key: String,
    clock: lyrics::Clock,
    result: Option<Result<Option<Arc<lyrics::Timeline>>, String>>,
    scroll: ListState,
    scroll_anchor: Option<usize>,
    browsed_at: Option<Instant>,
}

impl TrackLyrics {
    fn follow_playing_line(&mut self, now: Instant) -> bool {
        if self
            .browsed_at
            .is_some_and(|since| now.duration_since(since) < Duration::from_secs(6))
        {
            return false;
        }
        let resumed = self.browsed_at.take().is_some();
        let Some(Ok(Some(timeline))) = &self.result else {
            return resumed;
        };
        let anchor = timeline
            .follow_index(self.clock.position(now))
            .saturating_sub(2);
        if self.scroll_anchor == Some(anchor) && !resumed {
            return false;
        }
        self.scroll_anchor = Some(anchor);
        self.scroll.scroll_to(ListOffset {
            item_ix: anchor,
            offset_in_item: px(0.),
        });
        true
    }
}

#[derive(Default)]
struct SubtitleCatalog {
    records: Option<Result<Vec<SubtitleSummary>, String>>,
    pending: Vec<String>,
    generation_key: Option<String>,
    preview_key: Option<String>,
    preview: Option<Result<String, String>>,
    list_task: Option<gpui::Task<()>>,
    preview_task: Option<gpui::Task<()>>,
    editor_task: Option<gpui::Task<()>>,
    editor: Option<SegmentEditor>,
    refresh: bool,
}

impl SubtitleCatalog {
    fn select_track(&mut self, key: String) {
        let saved = self
            .records
            .as_ref()
            .and_then(|records| records.as_ref().ok())
            .is_some_and(|records| records.iter().any(|record| record.key == key));
        if !saved && !self.pending.contains(&key) {
            self.pending.insert(0, key.clone());
        }
        self.generation_key = Some(key);
    }
}

struct SegmentEditor {
    edit: Arc<SubtitleEdit>,
    key: String,
    title: String,
    draft: Vec<SubtitleSegment>,
    selected: Option<usize>,
    message: Option<String>,
    saving: bool,
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

#[derive(Clone, Copy)]
enum PickerPurpose {
    AddToSchedule,
    ReplaceInSchedule(usize),
    AddSubtitleTrack,
}

struct SongPicker {
    purpose: PickerPurpose,
    selected: HashSet<String>,
}

/// 跑马灯的测量结果；文本超出播放器标题区宽度时滚动。
struct Marquee {
    shift: f32,
    period: Duration,
}

#[derive(Clone, Copy)]
struct TrackLoad {
    started: Instant,
    crossfade: f32,
    intent: TrackStart,
}

#[derive(Clone, Copy)]
enum TrackStart {
    Selected,
    AfterEnd,
    Restore {
        position: Duration,
        record_on_resume: bool,
    },
}

impl TrackStart {
    fn position(self) -> Option<Duration> {
        match self {
            Self::Restore { position, .. } => Some(position),
            _ => None,
        }
    }

    fn fade_in(self, configured: f32, transition: f32) -> f32 {
        match self {
            Self::Selected => transition,
            Self::AfterEnd => configured,
            Self::Restore { .. } => 0.0,
        }
    }

    fn record_on_resume(self) -> bool {
        match self {
            Self::Restore {
                record_on_resume, ..
            } => record_on_resume,
            _ => true,
        }
    }
}

#[derive(PartialEq)]
struct PublishedTransport {
    playing: bool,
    key: Option<String>,
    duration: Duration,
    volume: f32,
    muted: bool,
    rate: f32,
    mode: PlayMode,
    crossfade: f32,
    output_routes: Vec<crate::settings::OutputRoute>,
    output_devices: Vec<OutputDevice>,
    queue_size: usize,
    queue_index: usize,
}

struct SongDrag {
    key: String,
    title: String,
}

struct SongDragPreview(String);

impl Render for SongDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
            .px_3()
            .py_2()
            .rounded_md()
            .bg(theme_color(PANEL))
            .border_1()
            .border_color(theme_color(LINE))
            .shadow_md()
            .text_color(theme_color(TEXT))
            .text_ellipsis()
            .child(self.0.clone())
    }
}

pub struct Workspace {
    library: Library,
    audio: AudioEngine,
    api: Option<api::Server>,
    api_config: api::Config,
    api_boot_enabled: bool,
    _observe_api_token: Subscription,
    api_publication: Instant,
    published_transport: Option<PublishedTransport>,
    api_library_dirty: bool,
    api_subtitle_writes: HashSet<String>,
    output_devices: Vec<OutputDevice>,
    output_route_warnings: Vec<String>,
    last_output_refresh: Instant,
    settings_page: Option<SettingsPage>,
    asr_url_field: Entity<TextField>,
    asr_scan_generation: u64,
    asr_catalog: Option<ModelCatalog>,
    asr_download: Option<ModelDownload>,
    asr_download_progress: Option<DownloadProgress>,
    asr_session: Option<Arc<AsrSession>>,
    asr_loading: Option<String>,
    asr_load_error: Option<String>,
    asr_transcribing: bool,
    asr_message: Option<String>,
    asr_text: Option<String>,
    separating: HashSet<String>,
    subtitle_progress: Option<(String, String)>,
    subtitle_feedback: Option<(String, String)>,
    api_token_field: Entity<TextField>,
    api_cors_field: Entity<TextField>,
    fonts: Vec<SystemFont>,
    font_picker_open: bool,
    font_filter: Entity<TextField>,
    _observe_font_filter: Subscription,
    settings_feedback: Option<String>,
    settings_focus: FocusHandle,
    settings_slider_bounds: HashMap<SharedString, Bounds<Pixels>>,
    settings_slider_drag: Option<(SharedString, f32)>,
    mode_feedback: Option<gpui::Task<()>>,
    tray: Option<Tray>,
    wake_lock: Option<WakeLock>,
    wake_lock_attempted: bool,
    close_prompt: bool,
    remember_close: bool,
    search: Entity<TextField>,
    dialog_field: Entity<TextField>,
    category_detail_field: Entity<TextField>,
    loop_start_field: Entity<TextField>,
    loop_end_field: Entity<TextField>,
    schedule_name_field: Entity<TextField>,
    schedule_start_field: Entity<TextField>,
    schedule_end_field: Entity<TextField>,
    picker_search: Entity<TextField>,
    focus: FocusHandle,
    _observe: Subscription,
    view: AppView,
    now_playing_open: bool,
    track_details_key: Option<String>,
    track_transition: Option<TrackTransition>,
    desktop_lyrics: DesktopLyrics,
    _on_quit: Subscription,
    track_lyrics: Option<TrackLyrics>,
    track_lyrics_task: Option<gpui::Task<()>>,
    editing_subtitle: bool,
    subtitle_catalog: SubtitleCatalog,
    segment_fields: [Entity<TextField>; 3],
    song_scroll: gpui::UniformListScrollHandle,
    schedule_scroll: gpui::ScrollHandle,
    shelf_scroll: gpui::ScrollHandle,
    filter: LibraryFilter,
    query: String,
    dialog: Option<Dialog>,
    action_menu: Option<TrackMenu>,
    picker: Option<SongPicker>,
    export_popover: bool,
    ffmpeg_detected: Option<Option<PathBuf>>,
    wallpaper_popover: bool,
    wallpaper_request: Option<wallpaper::Request>,
    wallpaper_loading: bool,
    wallpaper_image: Option<Arc<gpui::RenderImage>>,
    wallpaper_video: Option<wallpaper::Video>,
    wallpaper_video_frame: Option<wallpaper::VideoFrame>,
    main_window: Option<gpui::AnyWindowHandle>,
    builtin_wallpaper_video: Option<PathBuf>,
    diagnostics_open: bool,
    analysed_track: Option<Weak<PcmTrack>>,
    diagnostics_loading: bool,
    analysis: Option<Analysis>,
    live_analysis: Option<diagnostics::Live>,
    last_live_analysis: Instant,
    queue_open: bool,
    selecting: bool,
    selected_songs: HashSet<String>,
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
    loading: Option<TrackLoad>,
    failed_tracks: FailedTracks,
    play_history: PlayHistory,
    record_play_on_resume: bool,
    scheduler: Scheduler,
    last_tick: Instant,
    scheduler_now: Option<DateTime<Local>>,
    watcher: Option<LibraryWatcher>,
    watch_scanning: bool,
    last_fallback: Instant,
    visible: Vec<Song>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut library = Library::open();
        let (lyrics_style, lyrics_style_error) = library.desktop_lyrics_style();
        let mut audio = AudioEngine::open();
        audio.set_volume(if library.prefs.muted {
            0.0
        } else {
            library.prefs.volume
        });
        audio.set_speed(library.prefs.playback_rate);
        let (output_devices, output_route_warnings) =
            audio.refresh_outputs(&library.prefs.output_routes);
        let mut notice = audio
            .open_error()
            .map(str::to_owned)
            .or(lyrics_style_error)
            .or_else(|| output_route_warnings.first().cloned())
            .or_else(|| library.snapshot.warnings.first().cloned())
            .unwrap_or_else(|| format!("已载入 {} 首", library.snapshot.songs.len()));
        let api_config = api::Config::load(library.config_path());
        let api = if api_config.enabled {
            match api::Server::start(api_config.clone()) {
                Ok(server) => Some(server),
                Err(error) => {
                    eprintln!("无法启动本机接口：{error}");
                    None
                }
            }
        } else {
            None
        };

        let search = cx.new(|cx| TextField::new(cx, "搜索曲名或分类"));
        let api_token_field = cx.new(|cx| TextField::new(cx, "输入新令牌"));
        let api_cors_field =
            cx.new(|cx| TextField::new(cx, "https://example.com，多个来源用逗号分隔"));
        let asr_url_field = cx.new(|cx| TextField::new(cx, "输入 HTTP(S) 下载地址"));
        let dialog_field = cx.new(|cx| TextField::new(cx, "输入名称"));
        let segment_fields = ["开始时间（秒）", "结束时间（秒）", "字幕文本"]
            .map(|placeholder| cx.new(|cx| TextField::new(cx, placeholder)));
        let category_detail_field = cx.new(|cx| TextField::new(cx, "留空使用默认值"));
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

        let watcher = LibraryWatcher::start(library.root()).ok();
        if watcher.is_none() {
            notice = "文件监听不可用，将每 30 秒检查曲库".to_owned();
        }

        let on_quit = cx.on_app_quit(|this, _| {
            this.desktop_lyrics.quitting = true;
            this.save_lyrics_window_size();
            if let Err(error) = this.library.flush_preferences() {
                eprintln!("{error}");
            }
            async {}
        });

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let was_playing = this.playing;
                        let position = this.position;
                        let was_loading = this.loading.is_some();
                        let prefs_error = this.library.poll_preferences(Instant::now()).err();
                        if let Some(error) = &prefs_error {
                            this.notice = error.clone();
                        }
                        let outputs_changed = this.poll_audio_outputs();
                        let second_elapsed = this.tick_scheduler();
                        if second_elapsed && !this.asr_busy() {
                            this.refresh_asr_session(cx);
                        }
                        this.poll_playback();
                        this.poll_watch(cx);
                        let api_changed = this.poll_api(cx);
                        let diagnostics_changed = this.poll_diagnostics(cx);
                        let subtitles_changed = this.poll_track_lyrics(cx);
                        this.poll_desktop_lyrics(cx);
                        let model_download_changed = this.poll_model_download();
                        if this.view == AppView::Subtitles && this.subtitle_catalog.refresh {
                            this.reload_subtitle_catalog(cx);
                        }
                        let window = this.main_window;
                        this.update_wake_lock(|| {
                            window.is_some_and(|window| {
                                window
                                    .update(cx, |_, window, _| desktop::window_visible(window))
                                    .unwrap_or(false)
                            })
                        });
                        this.poll_wallpaper(cx);
                        if prefs_error.is_some()
                            || this.playing
                            || this.position != position
                            || was_playing
                            || was_loading
                            || second_elapsed
                            || outputs_changed
                            || api_changed
                            || diagnostics_changed
                            || subtitles_changed
                            || model_download_changed
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

        let font_filter = cx.new(|cx| TextField::new(cx, "搜索系统字体"));
        let observe_font_filter = cx.observe(&font_filter, |_, _, cx| cx.notify());
        let observe_api_token = cx.observe(&api_token_field, |_, _, cx| cx.notify());
        let autoplay = library.prefs.autoplay_on_start;
        let mut workspace = Self {
            library,
            audio,
            api,
            api_boot_enabled: api_config.enabled,
            api_config,
            _observe_api_token: observe_api_token,
            api_publication: Instant::now(),
            published_transport: None,
            api_library_dirty: true,
            api_subtitle_writes: HashSet::new(),
            output_devices,
            output_route_warnings,
            last_output_refresh: Instant::now(),
            settings_page: None,
            asr_url_field,
            asr_scan_generation: 0,
            asr_catalog: None,
            asr_download: None,
            asr_download_progress: None,
            asr_session: None,
            asr_loading: None,
            asr_load_error: None,
            asr_transcribing: false,
            asr_message: None,
            asr_text: None,
            separating: HashSet::new(),
            subtitle_progress: None,
            subtitle_feedback: None,
            api_token_field,
            api_cors_field,
            fonts: Vec::new(),
            font_picker_open: false,
            font_filter,
            _observe_font_filter: observe_font_filter,
            settings_feedback: None,
            settings_focus: cx.focus_handle(),
            settings_slider_bounds: HashMap::new(),
            settings_slider_drag: None,
            mode_feedback: None,
            tray: None,
            wake_lock: None,
            wake_lock_attempted: false,
            close_prompt: false,
            remember_close: false,
            search,
            dialog_field,
            category_detail_field,
            loop_start_field,
            loop_end_field,
            schedule_name_field,
            schedule_start_field,
            schedule_end_field,
            picker_search,
            focus: cx.focus_handle(),
            _observe: observe,
            view: AppView::Library,
            now_playing_open: false,
            track_details_key: None,
            track_transition: None,
            desktop_lyrics: DesktopLyrics::new(lyrics_style),
            _on_quit: on_quit,
            track_lyrics: None,
            track_lyrics_task: None,
            editing_subtitle: false,
            subtitle_catalog: SubtitleCatalog::default(),
            segment_fields,
            song_scroll: Default::default(),
            schedule_scroll: Default::default(),
            shelf_scroll: Default::default(),
            filter: LibraryFilter::All,
            query: String::new(),
            dialog: None,
            action_menu: None,
            picker: None,
            export_popover: false,
            ffmpeg_detected: None,
            wallpaper_popover: false,
            wallpaper_request: None,
            wallpaper_loading: false,
            wallpaper_image: None,
            wallpaper_video: None,
            wallpaper_video_frame: None,
            main_window: None,
            builtin_wallpaper_video: wallpaper::builtin_video(),
            diagnostics_open: false,
            analysed_track: None,
            diagnostics_loading: false,
            analysis: None,
            live_analysis: None,
            last_live_analysis: Instant::now(),
            queue_open: false,
            selecting: false,
            selected_songs: HashSet::new(),
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
            loading: None,
            failed_tracks: FailedTracks::default(),
            play_history: PlayHistory::default(),
            record_play_on_resume: false,
            scheduler: Scheduler::default(),
            last_tick: Instant::now(),
            scheduler_now: None,
            watcher,
            watch_scanning: false,
            last_fallback: Instant::now(),
            visible: Vec::new(),
        };
        let now = Self::local_now();
        workspace.scheduler_now = Some(now);
        workspace.check_schedule(now, ScheduleCheck::Tick { previous: None });
        if autoplay && !workspace.playing && workspace.current_program().is_none() {
            workspace.toggle_playback();
        }
        workspace.publish_api();
        if let Some(model) = workspace.library.asr_model_preferences().last_model {
            workspace.load_asr_model(model, cx);
        }
        workspace
    }

    pub fn window_title(&self) -> String {
        self.library.window_title()
    }

    // ---------- 播放 ----------

    fn poll_playback(&mut self) {
        self.audio.release_finished_fade();
        if let Some(load) = self.loading {
            if let Some(result) = self.audio.decoded_track() {
                self.loading = None;
                match result {
                    Ok(track) => {
                        self.duration = track.duration();
                        let key = self.current_key();
                        let region = self.loop_region_for(key.as_deref());
                        let fade = match load.intent {
                            TrackStart::Selected => self.audio.pending_fade(),
                            _ => load.crossfade,
                        };
                        self.audio
                            .start(track, region, fade, self.playing, load.intent.position());
                        self.position = self.audio.file_position();
                        self.failed_tracks.clear();
                        self.record_play_on_resume = load.intent.record_on_resume();
                        self.record_track_start();
                    }
                    Err(DecodeError::Track(error)) => self.fail_current(&error),
                    Err(DecodeError::Unavailable(error)) => self.stop_with_playback_error(error),
                }
            } else if load.started.elapsed().as_millis()
                >= self.library.config.player.load_timeout_ms.into()
            {
                self.fail_current("音频加载超时");
            }
            return;
        }
        if self.dragging_seek || !self.playing {
            return;
        }
        if self.audio.queued() > 0 {
            self.position = self.audio.file_position();
            return;
        }
        self.on_track_ended();
    }

    fn poll_audio_outputs(&mut self) -> bool {
        if self.audio.output_interrupted()
            || self.last_output_refresh.elapsed() >= Duration::from_secs(2)
        {
            self.refresh_audio_outputs()
        } else {
            false
        }
    }

    fn refresh_audio_outputs(&mut self) -> bool {
        let had_source = self.audio.track().is_some();
        let (devices, warnings) = self
            .audio
            .refresh_outputs(&self.library.prefs.output_routes);
        self.last_output_refresh = Instant::now();
        let changed = self.output_devices != devices || self.output_route_warnings != warnings;
        self.output_devices = devices;
        self.output_route_warnings = warnings;
        if (self.playing || self.loading.is_some() || had_source)
            && let Some(error) = self.audio.open_error().map(str::to_owned)
        {
            self.stop_with_playback_error(&error);
            return true;
        }
        if changed && let Some(warning) = self.output_route_warnings.first() {
            self.notice = warning.clone();
        }
        changed
    }

    fn on_track_ended(&mut self) {
        if self.handle_schedule_ended() {
            return;
        }
        if self.library.prefs.play_mode == PlayMode::RepeatOne {
            self.restart_current(TrackStart::AfterEnd);
            return;
        }
        if !self.advance(TrackStart::AfterEnd) && self.audio.queued() == 0 {
            self.playing = false;
            self.position = self.duration;
        }
    }

    fn play_pause(&mut self, _: &PlayPause, _window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        self.toggle_playback();
        cx.notify();
    }

    fn toggle_playback(&mut self) {
        if self.playing {
            self.audio.pause();
            self.playing = false;
            return;
        }
        if self.loading.is_some() || self.audio.queued() > 0 {
            self.audio.play();
            self.playing = true;
            self.record_track_start();
            return;
        }
        if let Some(key) = self.current_key() {
            if !self.guard_library_playback() {
                self.failed_tracks.clear();
                self.start_key(&key);
            }
            return;
        }
        if let Some(song) = self
            .library
            .visible(&self.filter, &self.query)
            .first()
            .cloned()
        {
            self.play_key(song.key.clone());
        }
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
        if self.dialog.is_some()
            || self.picker.is_some()
            || self.settings_page.is_some()
            || self.schedule_draft.is_some()
            || self.close_prompt
            || self.diagnostics_open
            || self.queue_open
            || self.wallpaper_popover
            || self.export_popover
            || self.action_menu.is_some()
        {
            return;
        }
        self.set_track_page(false);
        self.view = AppView::Library;
        window.focus(&self.search.focus_handle(cx));
        self.search
            .update(cx, |field, cx| field.select_entire_text(cx));
        cx.notify();
    }

    fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.close_prompt || self.settings_page.is_some() {
            if self.close_prompt {
                self.close_prompt = false;
                window.focus(&self.focus);
            } else if self.font_picker_open {
                self.font_picker_open = false;
                window.focus(&self.settings_focus);
            } else {
                self.settings_page = None;
                self.settings_slider_drag = None;
                window.focus(&self.focus);
            }
            cx.notify();
            return;
        }
        if self.diagnostics_open || self.queue_open {
            self.diagnostics_open = false;
            self.queue_open = false;
            cx.notify();
            return;
        }
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
        if (self.now_playing_open || self.track_transition.is_some()) && self.dialog.is_none() {
            self.set_track_page(false);
            window.focus(&self.focus);
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
        if self.selecting {
            self.selecting = false;
            self.selected_songs.clear();
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
            if self.search.focus_handle(cx).is_focused(window)
                && !self.search.read(cx).has_marked_text()
            {
                window.focus(&self.focus);
                cx.notify();
            }
            return;
        };
        match dialog {
            Dialog::EditTrack { key, category, .. } => {
                let title = self.dialog_field.read(cx).text().trim().to_owned();
                match self.library.update_track(&key, &title, &category) {
                    Ok(mutation) => self.apply_mutation(mutation),
                    Err(error) => {
                        self.dialog = Some(Dialog::EditTrack {
                            key,
                            category,
                            error: Some(error),
                        });
                        cx.notify();
                        return;
                    }
                }
            }
            Dialog::NewCategory => {
                let name = self.dialog_field.read(cx).text().trim().to_owned();
                let id = self.category_detail_field.read(cx).text().trim().to_owned();
                match self
                    .library
                    .create_category(&name, (!id.is_empty()).then_some(id.as_str()))
                {
                    Ok(id) => {
                        self.filter = LibraryFilter::Category(id);
                        self.api_library_dirty = true;
                        self.notice = format!("已新增分类 {name}");
                    }
                    Err(error) => self.notice = error,
                }
            }
            Dialog::EditCategory { id } => {
                let name = self.dialog_field.read(cx).text().trim().to_owned();
                let color = self.category_detail_field.read(cx).text().trim().to_owned();
                self.notice = match self.library.update_category(
                    &id,
                    &name,
                    (!color.is_empty()).then_some(color.as_str()),
                ) {
                    Ok(()) => {
                        self.api_library_dirty = true;
                        format!("已更新分类 {name}")
                    }
                    Err(error) => error,
                };
            }
            Dialog::Loop { key } => {
                self.save_loop_from_editor(&key, cx);
                self.dialog = None;
                window.focus(&self.focus);
                cx.notify();
                return;
            }
            Dialog::FileReport { .. } => {}
            Dialog::Move { .. } | Dialog::MoveBatch { .. } | Dialog::Import { .. } => return,
        }
        self.dialog = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn play_key(&mut self, key: String) {
        if self.guard_library_playback() {
            return;
        }
        let visible = self.library.visible(&self.filter, &self.query);
        self.queue = visible.into_iter().map(|song| song.key).collect();
        if !self.queue.contains(&key) {
            self.queue.push(key.clone());
        }
        self.queue_index = self.queue.iter().position(|item| item == &key).unwrap_or(0);
        self.failed_tracks.clear();
        self.start_key(&key);
    }

    fn play_keys(&mut self, keys: Vec<String>, key: &str) {
        self.queue = keys;
        self.queue_index = self.queue.iter().position(|item| item == key).unwrap_or(0);
        self.failed_tracks.clear();
        self.start_key(key);
    }

    fn start_key(&mut self, key: &str) {
        self.load_key(key, true);
    }

    fn load_key(&mut self, key: &str, autoplay: bool) {
        self.load_key_with(key, autoplay, TrackStart::Selected);
    }

    fn load_key_with(&mut self, key: &str, autoplay: bool, intent: TrackStart) {
        if self.audio.open_error().is_some() {
            self.refresh_audio_outputs();
        }
        let configured_fade = self.library.prefs.crossfade_seconds;
        let retained_fade = if matches!(intent, TrackStart::Selected) {
            self.audio
                .crossfade_for_load(configured_fade, self.playing && autoplay)
        } else {
            0.0
        };
        let crossfade = intent.fade_in(configured_fade, retained_fade);
        self.playing = autoplay;
        self.record_play_on_resume = false;
        let Some(path) = self.library.song(key).map(|song| PathBuf::from(&song.path)) else {
            self.fail_current("曲目已不在曲库中");
            return;
        };
        self.audio.set_speed(self.library.prefs.playback_rate);
        self.audio.set_volume(self.output_volume());
        match self.audio.request_track(path, retained_fade) {
            Ok(()) => {
                self.duration = Duration::ZERO;
                self.position = intent.position().unwrap_or_default();
                self.loading = Some(TrackLoad {
                    started: Instant::now(),
                    crossfade,
                    intent,
                });
            }
            Err(error) => self.stop_with_playback_error(&error),
        }
    }

    fn record_track_start(&mut self) {
        if self.playing
            && std::mem::take(&mut self.record_play_on_resume)
            && let Some(key) = self.current_key()
        {
            self.play_history.record(key, Instant::now());
        }
    }

    fn fail_current(&mut self, reason: &str) {
        let was_playing = self.playing;
        self.stop_with_playback_error(reason);
        let key = self.current_key();
        let max_skips = self.library.config.player.max_auto_skips;
        let can_skip = key.is_some_and(|key| self.failed_tracks.record(key, max_skips));
        if can_skip
            && was_playing
            && self.current_program().is_none()
            && let Some(next) = self.next_after_failure()
        {
            self.notice.push_str("，已自动跳到下一首");
            self.start_queue_index(next, TrackStart::Selected);
        }
    }

    fn stop_with_playback_error(&mut self, reason: &str) {
        self.stop_playback();
        self.notice = match self.current_song() {
            Some(song) => format!("《{}》{reason}", song.title),
            None => reason.to_owned(),
        };
    }

    fn next_after_failure(&self) -> Option<usize> {
        let untried = |key: &str| !self.failed_tracks.contains(key);
        if self.library.prefs.play_mode == PlayMode::Shuffle {
            self.smart_next_index(untried)
        } else {
            playback_queue::next_available_index(&self.queue, self.queue_index, |key| {
                untried(key) && self.library.song(key).is_some()
            })
        }
    }

    fn advance(&mut self, intent: TrackStart) -> bool {
        if self.queue.len() == 1 {
            self.restart_current(intent);
        } else if let Some(next) = self.next_queue_index() {
            self.start_queue_index(next, intent);
        } else {
            return false;
        }
        true
    }

    fn next_queue_index(&self) -> Option<usize> {
        if self.queue.is_empty() {
            None
        } else if self.library.prefs.play_mode == PlayMode::Shuffle {
            self.smart_next_index(|_| true)
        } else {
            Some((self.queue_index + 1) % self.queue.len())
        }
    }

    fn start_queue_index(&mut self, index: usize, intent: TrackStart) {
        if let Some(key) = self.queue.get(index).cloned() {
            self.queue_index = index;
            self.load_key_with(&key, true, intent);
        }
    }

    fn restart_current(&mut self, intent: TrackStart) {
        if self.loading.is_some() {
            self.audio.play();
            self.playing = true;
        } else if self.audio.queued() > 0 {
            self.seek_to(Duration::ZERO);
            self.audio.play();
            self.playing = true;
            self.record_track_start();
        } else if let Some(track) = self.audio.track() {
            let region = self.loop_region_for(self.current_key().as_deref());
            let fade = intent.fade_in(self.library.prefs.crossfade_seconds, 0.0);
            self.audio.start(track, region, fade, true, None);
            self.position = self.audio.file_position();
            self.playing = true;
            self.record_track_start();
        } else if let Some(key) = self.current_key() {
            self.load_key_with(&key, true, intent);
        }
    }

    fn previous(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.failed_tracks.clear();
        let threshold = self.library.config.player.restart_threshold_seconds;
        if self.position.as_secs_f64() > threshold || self.queue.len() <= 1 {
            self.restart_current(TrackStart::Selected);
            return;
        }
        let len = self.queue.len();
        self.start_queue_index((self.queue_index + len - 1) % len, TrackStart::Selected);
    }

    fn stop_playback(&mut self) {
        self.audio.stop();
        self.playing = false;
        self.position = Duration::ZERO;
        self.duration = Duration::ZERO;
        self.loading = None;
        self.record_play_on_resume = false;
    }

    fn seek_by(&mut self, delta: f64) {
        if self.duration.is_zero() {
            return;
        }
        let target = (self.position.as_secs_f64() + delta).clamp(0.0, self.duration.as_secs_f64());
        self.seek_to(Duration::from_secs_f64(target));
    }

    fn seek_to(&mut self, target: Duration) {
        if self.loading.is_some() {
            return;
        }
        if let Err(error) = self.audio.seek_file(target) {
            self.notice = error;
        } else {
            self.position = self.audio.file_position();
        }
    }

    fn nudge_volume(&mut self, delta: f32) {
        let next = (self.library.prefs.volume + delta).clamp(0.0, 1.0);
        self.library.preview_volume(next, Instant::now());
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

    fn api_state(&self) -> Value {
        json!({
            "ready": true,
            "playing": self.playing,
            "position": (self.position.as_secs_f64() * 1000.).round() / 1000.,
            "duration": (self.duration.as_secs_f64() * 1000.).round() / 1000.,
            "volume": self.library.prefs.volume,
            "muted": self.library.prefs.muted,
            "rate": self.library.prefs.playback_rate,
            "playMode": self.library.prefs.play_mode,
            "crossfadeSeconds": self.library.prefs.crossfade_seconds,
            "track": self.current_song().map(|song| json!({
                "key": song.key,
                "title": song.title,
                "category": song.category,
                "artist": song.artist,
                "album": song.album,
                "format": Path::new(&song.path).extension().and_then(|extension| extension.to_str()).map(str::to_ascii_lowercase),
                "durationMs": song.duration_ms,
                "fileName": song.file_name,
                "path": song.path,
            })),
            "queue": {
                "length": self.queue.len(),
                "index": self.current_key().map(|_| self.queue_index),
            },
            "outputs": self.library.prefs.output_routes,
            "outputDevices": self.output_devices,
            "library": {"root": self.library.root(), "count": self.library.snapshot.songs.len()},
        })
    }

    fn api_output_state(&self) -> Value {
        let routes = &self.library.prefs.output_routes;
        let devices = &self.output_devices;
        json!({
            "supported": true,
            "labelsAvailable": !devices.is_empty(),
            "routed": self.audio.open_error().is_none(),
            "error": self.audio.open_error().or_else(|| self.output_route_warnings.first().map(String::as_str)),
            "devices": devices,
            "routes": routes,
            "outputs": routes,
            "outputDevices": devices,
        })
    }

    fn transport_stamp(&self) -> PublishedTransport {
        PublishedTransport {
            playing: self.playing,
            key: self.current_key(),
            duration: self.duration,
            volume: self.library.prefs.volume,
            muted: self.library.prefs.muted,
            rate: self.library.prefs.playback_rate,
            mode: self.library.prefs.play_mode,
            crossfade: self.library.prefs.crossfade_seconds,
            output_routes: self.library.prefs.output_routes.clone(),
            output_devices: self.output_devices.clone(),
            queue_size: self.queue.len(),
            queue_index: self.queue_index,
        }
    }

    fn publish_api(&mut self) {
        if let Some(server) = &self.api {
            if self.api_library_dirty {
                server.publish_library(&self.library.snapshot);
                self.api_library_dirty = false;
            }
            server.publish(self.api_state());
            self.api_publication = Instant::now();
            self.published_transport = Some(self.transport_stamp());
        }
    }

    fn poll_api(&mut self, cx: &mut Context<Self>) -> bool {
        if self.api.is_none() {
            return false;
        }
        let mut changed = false;
        for _ in 0..8 {
            let Some(command) = self.api.as_mut().and_then(api::Server::next_command) else {
                break;
            };
            match command {
                api::Command::Playback {
                    action,
                    params,
                    reply,
                } if !reply.is_closed() => {
                    let result = self.apply_api(&action, &params);
                    if action == "library.rescan" && result.is_ok() {
                        if let Some(window) = self.main_window {
                            let title = self.library.window_title();
                            let _ =
                                window.update(cx, |_, window, _| window.set_window_title(&title));
                        }
                        self.publish_api();
                    }
                    changed |= action != "state" && action != "outputs.list";
                    let _ = reply.send(result);
                }
                api::Command::Tasks { operation, reply } if !reply.is_closed() => {
                    self.process_api_tasks(operation, reply, cx);
                }
                api::Command::Records { request, reply } if !reply.is_closed() => {
                    self.process_api_records(request, reply, cx);
                }
                _ => {}
            }
        }
        self.start_next_api_task(cx);
        if changed
            || self.api_library_dirty
            || self.api_publication.elapsed() >= Duration::from_secs(1)
            || self.published_transport.as_ref() != Some(&self.transport_stamp())
        {
            self.publish_api();
        }
        changed
    }

    fn process_api_tasks(
        &mut self,
        operation: api::tasks::Operation,
        reply: tokio::sync::oneshot::Sender<api::Reply>,
        cx: &mut Context<Self>,
    ) {
        use api::tasks::{Job, Operation};

        let preferences = self.library.asr_model_preferences();
        let busy = |message: &str| (StatusCode::CONFLICT, message.to_owned());
        match operation {
            Operation::SubmitAudio(job) => {
                let key = &job.key;
                let result = if self.library.song(key).is_none() {
                    Err((StatusCode::NOT_FOUND, format!("曲目不存在：{key}")))
                } else if self.separating.contains(key) || self.api_subtitle_writes.contains(key) {
                    Err(busy("曲目正在处理，请稍后再试"))
                } else {
                    self.submit_api_task(Job::Audio(job))
                };
                let _ = reply.send(result);
            }
            Operation::LoadAsr(id) => {
                if crate::library::api_model_id(id.strip_prefix("file:").unwrap_or(&id)).is_none() {
                    let _ = reply.send(Err((StatusCode::BAD_REQUEST, "无效的 ASR 模型 ID".into())));
                    return;
                }
                if self.asr_busy() {
                    let _ = reply.send(Err(busy("正在处理 ASR 任务，请稍候")));
                    return;
                }
                cx.spawn(async move |this, cx| {
                    let root = preferences.root();
                    let model = id.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            let catalog = ModelCatalog::scan(&preferences);
                            let entry = catalog
                                .models
                                .iter()
                                .find(|entry| entry.id == model)
                                .ok_or_else(|| {
                                    (StatusCode::NOT_FOUND, format!("模型不存在：{model}"))
                                })?;
                            match &entry.missing_components {
                                Some(missing) if missing.is_empty() => Ok(()),
                                Some(missing) => Err((
                                    StatusCode::CONFLICT,
                                    format!("模型缺少文件：{}", missing.join(" / ")),
                                )),
                                None => Err((
                                    StatusCode::CONFLICT,
                                    "所选目录没有可加载的 ASR 模型".into(),
                                )),
                            }
                        })
                        .await;
                    if reply.is_closed() {
                        return;
                    }
                    let result = result.and_then(|()| {
                        this.update(cx, |this, _| {
                            if this.asr_busy()
                                || this.library.asr_model_preferences().root() != root
                            {
                                Err(busy("模型目录或任务状态已变更"))
                            } else {
                                this.submit_api_task(Job::AsrLoad { id, root })
                            }
                        })
                        .unwrap_or_else(|_| {
                            Err((StatusCode::SERVICE_UNAVAILABLE, "界面不可用".into()))
                        })
                    });
                    let _ = reply.send(result);
                })
                .detach();
            }
            Operation::SeparationModels => {
                cx.background_executor()
                    .spawn(async move {
                        let result =
                            separation::Catalog::document(&preferences.root()).map_err(|error| {
                                (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
                            });
                        let _ = reply.send(result);
                    })
                    .detach();
            }
            Operation::AsrModels | Operation::AsrUnload => {
                let unload = matches!(operation, Operation::AsrUnload);
                let mut unloaded = false;
                if unload {
                    if self.asr_busy() {
                        let _ = reply.send(Err(busy("正在处理 ASR 任务，请稍候")));
                        return;
                    }
                    if let Err(error) = self.library.set_last_asr_model(None) {
                        let _ = reply.send(Err((StatusCode::INTERNAL_SERVER_ERROR, error)));
                        return;
                    }
                    unloaded = self.asr_session.is_some();
                    self.release_asr_session(cx);
                    self.close_desktop_lyrics(false, cx);
                    self.asr_load_error = None;
                    self.asr_message = None;
                    cx.notify();
                }
                let loaded = self.asr_session.as_ref().map(|session| session.id.clone());
                let loading = self.asr_loading.clone();
                let error = self.asr_load_error.clone();
                let downloading = self.asr_download.is_some();
                cx.background_executor()
                    .spawn(async move {
                        let info = api::tasks::models_info(
                            preferences,
                            loaded.as_deref(),
                            loading.as_deref(),
                            error.as_deref(),
                            downloading,
                        );
                        let result = if unload {
                            json!({"unloaded":unloaded,"info":info})
                        } else {
                            info
                        };
                        let _ = reply.send(Ok(result));
                    })
                    .detach();
            }
        }
    }

    fn submit_api_task(&self, job: api::tasks::Job) -> api::Reply {
        self.api
            .as_ref()
            .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "本机接口不可用".into()))?
            .submit_task(job)
    }

    fn start_next_api_task(&mut self, cx: &mut Context<Self>) {
        if self.asr_busy() || !self.separating.is_empty() || !self.api_subtitle_writes.is_empty() {
            return;
        }
        let Some((task, job)) = self.api.as_ref().and_then(api::Server::next_task) else {
            return;
        };
        match job {
            api::tasks::Job::Audio(api::tasks::AudioJob {
                key,
                model,
                target,
                captions,
            }) => {
                self.start_api_audio_task(task, key, model, target, captions, cx);
            }
            api::tasks::Job::AsrLoad { id, root } => {
                if self.library.asr_model_preferences().root() == root {
                    self.start_asr_load(root, id, Some(task), cx);
                } else {
                    task.finish(Err("模型目录已变更".into()));
                }
            }
        }
    }

    fn start_api_audio_task(
        &mut self,
        task: api::RunningTask,
        key: String,
        model: String,
        target: Option<separation::Target>,
        captions: bool,
        cx: &mut Context<Self>,
    ) {
        enum Output {
            Separation(separation::Prepared),
            Subtitles(subtitle_generation::Generated),
        }
        let session = if captions {
            let Some(session) = self.asr_session.clone() else {
                task.finish(Err("请先在「设置 → 模型」加载 ASR 模型".into()));
                return;
            };
            Some(session)
        } else {
            None
        };
        let Some(song) = self.library.song(&key).cloned() else {
            task.finish(Err(format!("曲目不存在：{key}")));
            return;
        };
        let models = self.library.asr_model_preferences().root();
        let directory = self.library.subtitle_directory();
        self.separating.insert(key.clone());
        if captions {
            self.subtitle_feedback = None;
            self.subtitle_progress = Some((key.clone(), "正在生成字幕".into()));
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let task = Arc::new(task);
            let reporter = task.clone();
            let model_session = session.clone();
            let records = directory.clone();
            let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
            let processing = cx.background_executor().spawn(async move {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let (model, target, accompaniment) =
                        separation::Catalog::discover(&models)?.for_task(&model, target)?;
                    let request = separation::Request {
                        subtitles: records,
                        key: song.key.clone(),
                        source: PathBuf::from(&song.path),
                        model,
                        target,
                        accompaniment,
                    };
                    if let Some(session) = &model_session {
                        subtitle_generation::generate(request, song, session, |stage| {
                            let (phase, current, total) = match stage {
                                subtitle_generation::Stage::Separating => ("separating", 0, 0),
                                subtitle_generation::Stage::Normalizing => ("normalizing", 0, 0),
                                subtitle_generation::Stage::Transcribing { current, total } => {
                                    ("transcribing", current, total)
                                }
                                subtitle_generation::Stage::Saving => ("saving", 0, 0),
                            };
                            reporter.progress(phase, stage.label(), current, total);
                            let _ = progress_tx.send(stage);
                        })
                        .map(Output::Subtitles)
                    } else {
                        reporter.progress(
                            "separating",
                            subtitle_generation::Stage::Separating.label(),
                            0,
                            0,
                        );
                        request.generate().map(Output::Separation)
                    }
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("任务处理提前终止")));
                (
                    result,
                    model_session
                        .as_ref()
                        .is_some_and(|session| !session.is_running()),
                )
            });
            while let Some(stage) = progress_rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.subtitle_progress = Some((key.clone(), stage.label()));
                    cx.notify();
                });
            }
            let (prepared, exited) = processing.await;
            let result: anyhow::Result<Value> = async {
                let prepared = prepared?;
                let current = this
                    .update(cx, |this, _| this.library.song(&key).cloned())?
                    .ok_or_else(|| anyhow::anyhow!("曲目已移出曲库：{key}"))?;
                if captions {
                    task.progress("saving", subtitle_generation::Stage::Saving.label(), 0, 0);
                }
                cx.background_executor()
                    .spawn(async move {
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let subtitle = match prepared {
                                Output::Subtitles(generated) => {
                                    generated.publish(&current)?;
                                    Some(
                                        SubtitleEdit::open(&directory, &current.key)
                                            .map_err(anyhow::Error::msg)?,
                                    )
                                }
                                Output::Separation(prepared) => {
                                    prepared.publish(Path::new(&current.path))?;
                                    None
                                }
                            };
                            let record = separation::result_document(&directory, &current.key)
                                .map_err(anyhow::Error::msg)?
                                .ok_or_else(|| anyhow::anyhow!("分离结果已移除"))?;
                            if let Some(subtitle) = subtitle {
                                let mut value = subtitle.document(true);
                                value["sourcePath"] = json!(current.path);
                                value["sourceFormat"] = json!(
                                    Path::new(&current.file_name)
                                        .extension()
                                        .and_then(|ext| ext.to_str())
                                        .map(str::to_ascii_lowercase)
                                );
                                value["separation"] = record["separation"].clone();
                                value["duration"] = json!(
                                    subtitle
                                        .segments()
                                        .iter()
                                        .map(|segment| segment.end)
                                        .fold(0., f64::max)
                                );
                                value["savedPath"] = json!(subtitle.record_path());
                                Ok(value)
                            } else {
                                Ok(json!({"key":current.key,"separation":record["separation"]}))
                            }
                        }));
                        result.unwrap_or_else(|_| Err(anyhow::anyhow!("任务保存提前终止")))
                    })
                    .await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.separating.remove(&key);
                if captions {
                    this.subtitle_progress = None;
                    let message = match &result {
                        Ok(_) => format!("已生成《{}》字幕", this.song_title(&key)),
                        Err(error) => format!("字幕生成失败：{error:#}"),
                    };
                    this.subtitle_feedback = Some((key.clone(), message));
                    if result.is_ok() {
                        this.subtitle_catalog.refresh = true;
                        if this
                            .track_lyrics
                            .as_ref()
                            .is_some_and(|preview| preview.key == key)
                        {
                            this.track_lyrics_task = None;
                            this.track_lyrics = None;
                            this.poll_track_lyrics(cx);
                        }
                    }
                }
                if exited
                    && this
                        .asr_session
                        .as_ref()
                        .zip(session.as_ref())
                        .is_some_and(|(current, task)| Arc::ptr_eq(current, task))
                {
                    this.release_asr_session(cx);
                }
                cx.notify();
            });
            task.finish(result.map_err(|error| format!("{error:#}")));
        })
        .detach();
    }

    fn process_api_records(
        &mut self,
        request: api::records::Operation,
        reply: tokio::sync::oneshot::Sender<api::records::Reply>,
        cx: &mut Context<Self>,
    ) {
        let write_key = request.write_key().map(str::to_owned);
        if let Some(key) = &write_key
            && (self.separating.contains(key) || !self.api_subtitle_writes.insert(key.clone()))
        {
            let _ = reply.send(Err((
                StatusCode::CONFLICT,
                "曲目正在处理，请稍后再试".into(),
            )));
            return;
        }
        let song = request
            .track_key()
            .and_then(|key| self.library.song(key))
            .cloned();
        let directory = self.library.subtitle_directory();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { request.execute(&directory, song.as_ref()) })
                .await;
            if let Some(key) = write_key {
                let _ = this.update(cx, |this, cx| {
                    this.api_subtitle_writes.remove(&key);
                    if result.is_ok() {
                        this.subtitle_catalog.refresh = true;
                        if this
                            .track_lyrics
                            .as_ref()
                            .is_some_and(|preview| preview.key == key)
                        {
                            this.track_lyrics_task = None;
                            this.track_lyrics = None;
                            this.poll_track_lyrics(cx);
                        }
                        cx.notify();
                    }
                });
            }
            let _ = reply.send(result);
        })
        .detach();
    }

    fn apply_api(&mut self, action: &str, params: &Value) -> api::Reply {
        let bad = |message: &str| (StatusCode::BAD_REQUEST, message.to_owned());
        let storage = |error: String| (StatusCode::INTERNAL_SERVER_ERROR, error);
        let number = |fields: &[&str]| {
            fields
                .iter()
                .find_map(|field| params.get(*field).and_then(Value::as_f64))
                .ok_or_else(|| bad("缺少有效的数值参数"))
        };
        match action {
            "state" => {}
            "library.rescan" => {
                self.rescan_library();
                return Ok(api::library_payload(&self.library.snapshot));
            }
            "play" => {
                let key = params
                    .get("key")
                    .or_else(|| params.get("trackKey"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("需要曲目 key"))?;
                if self.library.song(key).is_none() {
                    return Err((StatusCode::NOT_FOUND, format!("曲目不存在：{key}")));
                }
                if self.guard_library_playback() {
                    return Err((StatusCode::CONFLICT, self.notice.clone()));
                }
                let mut keys = params["queue"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|key| self.library.song(key).is_some())
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .filter(|keys| !keys.is_empty())
                    .unwrap_or_else(|| {
                        self.library
                            .snapshot
                            .songs
                            .iter()
                            .map(|song| song.key.clone())
                            .collect()
                    });
                if !keys.iter().any(|item| item == key) {
                    keys.insert(0, key.into());
                }
                self.play_keys(keys, key);
            }
            "pause" if self.playing => self.toggle_playback(),
            "resume" if !self.playing => self.toggle_playback(),
            "pause" | "resume" => {}
            "toggle" => self.toggle_playback(),
            "next" | "previous" => {
                if self.guard_library_playback() {
                    return Err((StatusCode::CONFLICT, self.notice.clone()));
                }
                self.failed_tracks.clear();
                if action == "next" {
                    self.advance(TrackStart::Selected);
                } else {
                    self.previous();
                }
            }
            "seek" => {
                let value = number(&["position", "seconds"])?;
                if self.duration.is_zero() {
                    return Err((StatusCode::CONFLICT, "当前曲目无法定位".into()));
                }
                self.seek_to(Duration::from_secs_f64(
                    value.clamp(0.0, self.duration.as_secs_f64()),
                ));
            }
            "volume" => {
                let volume = params
                    .get("volume")
                    .map(|_| number(&["volume"]))
                    .transpose()?
                    .map(|value| value.clamp(0.0, 1.0) as f32);
                let muted = params
                    .get("muted")
                    .map(|value| value.as_bool().ok_or_else(|| bad("静音参数须为布尔值")))
                    .transpose()?;
                if volume.is_none() && muted.is_none() {
                    return Err(bad("需要音量或静音参数"));
                }
                self.library
                    .set_volume_state(volume, muted)
                    .map_err(storage)?;
                self.audio.set_volume(self.output_volume());
            }
            "crossfade" => {
                self.library
                    .set_crossfade(number(&["seconds", "crossfadeSeconds"])? as f32)
                    .map_err(storage)?;
            }
            "outputs.list" => return Ok(self.api_output_state()),
            "outputs.unlock" | "outputs.refresh" => {
                self.refresh_audio_outputs();
                if self.settings_page == Some(SettingsPage::Playback) {
                    self.settings_feedback = self
                        .output_route_warnings
                        .is_empty()
                        .then(|| "设备列表已更新".into());
                }
                return Ok(self.api_output_state());
            }
            "outputs.set" => {
                let routes = api::update_output_routes(&self.library.prefs.output_routes, params)?;
                self.library.set_outputs(routes).map_err(storage)?;
                self.refresh_audio_outputs();
                if self.settings_page == Some(SettingsPage::Playback) {
                    self.settings_feedback = None;
                }
                return Ok(self.api_output_state());
            }
            _ => return Err((StatusCode::NOT_FOUND, "命令不存在".into())),
        }
        Ok(self.api_state())
    }

    // ---------- 智能随机 ----------

    fn smart_next_index(&self, eligible: impl Fn(&str) -> bool) -> Option<usize> {
        let songs: HashMap<_, _> = self
            .library
            .snapshot
            .songs
            .iter()
            .map(|song| (song.key.as_str(), song))
            .collect();
        let current = self
            .queue
            .get(self.queue_index)
            .and_then(|key| songs.get(key.as_str()).copied());
        let candidates = self.queue.iter().enumerate().map(|(index, key)| {
            if index == self.queue_index || !eligible(key) {
                None
            } else {
                songs.get(key.as_str()).copied()
            }
        });
        playback_queue::pick(
            candidates,
            current,
            &self.library.prefs.favorites,
            &self.play_history,
        )
    }

    fn smart_pick_start(&self) -> Option<String> {
        let index = playback_queue::pick(
            self.visible.iter().map(Some),
            self.current_song(),
            &self.library.prefs.favorites,
            &self.play_history,
        )?;
        self.visible.get(index).map(|song| song.key.clone())
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
        self.current_program_at(Self::minutes_of_day(&Self::local_now()))
            .cloned()
    }

    fn current_program_at(&self, minute: u32) -> Option<&ScheduleEntry> {
        scheduler::active_program(&self.library.schedule.entries, minute)
    }

    fn current_program_song(&self) -> Option<ScheduleSong> {
        let now = Self::minutes_of_day(&Self::local_now());
        self.due_program_song(self.current_program_at(now)?, now)
            .cloned()
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

    fn tick_scheduler(&mut self) -> bool {
        if self.last_tick.elapsed() < Duration::from_secs(1) {
            return false;
        }
        let now = Self::local_now();
        let previous = self.scheduler_now.replace(now);
        self.last_tick = Instant::now();
        self.check_schedule(now, ScheduleCheck::Tick { previous });
        true
    }

    fn check_schedule(&mut self, now: DateTime<Local>, check: ScheduleCheck) {
        let Some(update) =
            self.scheduler
                .check(&self.library.schedule.entries, now, check, |key| {
                    self.library.song(key).is_some()
                })
        else {
            return;
        };
        let entry = update.entry.clone();
        let song = update.song.map(|song| song.key.clone());
        self.apply_schedule_entry(&entry, song.as_deref(), update.switched, false);
        if song.is_none() {
            self.remove_from_queue(None);
        }
    }

    fn due_program_song<'a>(
        &self,
        program: &'a ScheduleEntry,
        minute: u32,
    ) -> Option<&'a ScheduleSong> {
        scheduler::latest_due_song(program, minute, |key| self.library.song(key).is_some())
    }

    /// 在节目时段只循环已到点且仍在曲库里的曲目。
    fn handle_schedule_ended(&mut self) -> bool {
        let minute = Self::minutes_of_day(&Self::local_now());
        let Some(program) = self.current_program_at(minute).cloned() else {
            return false;
        };
        if let Some(song) = self.due_program_song(&program, minute) {
            self.apply_schedule_entry(&program, Some(&song.key), false, true);
        } else {
            self.remove_from_queue(None);
        }
        true
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

    /// 节目中当前可用的曲目键，按节目顺序排列。
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

    fn apply_schedule_entry(
        &mut self,
        entry: &ScheduleEntry,
        song_key: Option<&str>,
        announce: bool,
        restart: bool,
    ) {
        let volume_error = entry
            .volume
            .filter(|&volume| {
                volume != self.library.prefs.volume || (volume <= 0.0) != self.library.prefs.muted
            })
            .and_then(|volume| match self.library.set_volume(volume) {
                Ok(()) => {
                    self.audio.set_volume(self.output_volume());
                    None
                }
                Err(error) => Some(error),
            });

        let queue = self.resolve_schedule_songs(entry);
        let Some(index) = song_key.and_then(|key| queue.iter().position(|item| item == key)) else {
            if let Some(error) = volume_error {
                self.notice = error;
            } else if announce {
                self.notice = if queue.is_empty() {
                    format!("节目「{}」没有可播放的曲目", entry.name)
                } else {
                    format!("节目「{}」正在等待曲目到点", entry.name)
                };
            }
            return;
        };
        let target = queue[index].clone();
        if self.current_key().as_deref() == Some(target.as_str())
            && (self.loading.is_some() || self.audio.track().is_some())
        {
            self.queue = queue;
            self.queue_index = index;
            if restart {
                self.restart_current(TrackStart::Selected);
            }
        } else {
            self.play_keys(queue, &target);
        }

        if let Some(error) = volume_error {
            self.notice = error;
        } else if announce {
            let volume_note = entry
                .volume
                .map(|volume| format!(" · 音量 {}%", (volume * 100.0).round() as i32))
                .unwrap_or_default();
            self.notice = format!("已切换到节目「{}」{volume_note}", entry.name);
        }
    }

    fn sync_to_current_program(&mut self) {
        self.check_schedule(Self::local_now(), ScheduleCheck::Edited);
    }

    fn recover_removed_program_track(&mut self) -> bool {
        let now = Self::local_now();
        if !self.playing
            || self
                .current_program_at(Self::minutes_of_day(&now))
                .is_none()
        {
            return false;
        }
        self.stop_playback();
        self.check_schedule(now, ScheduleCheck::TrackRemoved);
        true
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
        let Some(draft) = self.schedule_draft.as_ref() else {
            return;
        };
        let draft_id = draft.id.clone();
        let draft_volume_enabled = draft.volume_enabled;
        let draft_volume = draft.volume;
        let draft_enabled = draft.enabled;
        let songs: Vec<_> = draft
            .songs
            .iter()
            .enumerate()
            .map(|(index, song)| ScheduleSong {
                key: song.key.clone(),
                start: draft.song_start_fields[index].read(cx).text().to_owned(),
                end: draft.song_end_fields[index].read(cx).text().to_owned(),
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
        if songs.is_empty() {
            self.notice = "还未添加歌曲：请先通过「添加歌曲」从歌单中选择歌曲".into();
            cx.notify();
            return;
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

    /// 时间槽属于节目中的位置，曲目移动时沿用目标位置的起止时间。
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
        purpose: PickerPurpose,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker_search
            .update(cx, |field, cx| field.set_text("", cx));
        self.picker = Some(SongPicker {
            purpose,
            selected: HashSet::new(),
        });
        let field = self.picker_search.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn confirm_song_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        match picker.purpose {
            PickerPurpose::ReplaceInSchedule(index) => {
                if let Some(key) = keys.first() {
                    self.replace_draft_song(index, key.clone(), cx);
                }
            }
            PickerPurpose::AddToSchedule => self.add_draft_songs(keys, cx),
            PickerPurpose::AddSubtitleTrack => {
                window.focus(&self.focus);
                if let Some(key) = keys.into_iter().next() {
                    self.open_subtitle_generation(key, window, cx);
                }
            }
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
        if end - start < MIN_LOOP_SPAN {
            return None;
        }
        Some((start, end))
    }

    fn refresh_loop_region(&mut self) {
        if self.loading.is_some() {
            return;
        }
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
        self.watcher = LibraryWatcher::start(self.library.root()).ok();
        self.last_fallback = Instant::now();
        if self.watcher.is_none() {
            self.notice = "文件监听不可用，将每 30 秒检查曲库".into();
        }
    }

    fn poll_watch(&mut self, cx: &mut Context<Self>) {
        if self.watch_scanning {
            return;
        }
        let now = Instant::now();
        let fallback_due = crate::watch::fallback_due(now, self.last_fallback);
        let scan_due = self.watcher.as_ref().map_or(fallback_due, |watcher| {
            watcher.take_scan_due(now, fallback_due)
        });
        if !scan_due {
            return;
        }
        if self.watcher.is_none() && self.library.root().is_dir() {
            self.install_watcher();
        }
        self.last_fallback = now;
        self.watch_scanning = true;
        let scan = self.library.watch_scan();
        let revision = scan.revision();
        let task = cx.background_executor().spawn(async move { scan.run() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.watch_scanning = false;
                match result {
                    Some((revision, snapshot)) => {
                        if let Some(change) = this.library.apply_watch_scan(revision, snapshot) {
                            this.apply_library_change(change);
                            cx.notify();
                        }
                    }
                    None if this.library.is_current_watch_scan(revision) => {
                        this.watcher = None;
                        if this.library.mark_root_offline() {
                            if let Some(message) = this.library.snapshot.warnings.first() {
                                this.notice = message.clone();
                            }
                            this.api_library_dirty = true;
                            cx.notify();
                        }
                    }
                    None => {}
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_library_change(&mut self, change: LibraryChange) {
        let LibraryChange {
            added,
            removed,
            moved,
            migration_warnings,
        } = change;
        let removed_count = removed.len();
        let moved_count = moved.len();
        self.apply_mutation(Mutation {
            message: String::new(),
            changes: moved,
            removed,
            report: None,
        });
        self.reconcile_snapshot();
        self.refresh_loop_region();
        let mut notes = Vec::new();
        if !added.is_empty() {
            notes.push(format!("新增 {}", added.len()));
        }
        if removed_count > 0 {
            notes.push(format!("移除 {removed_count}"));
        }
        if moved_count > 0 {
            notes.push(format!("移动 {moved_count}"));
        }
        self.notice = if notes.is_empty() {
            "曲库已更新".into()
        } else {
            format!("曲库已更新：{}", notes.join("、"))
        };
        if !migration_warnings.is_empty() {
            self.notice
                .push_str(&format!("。{}", migration_warnings.join("。")));
        }
    }

    // ---------- 曲库写操作 ----------

    fn apply_mutation(&mut self, mutation: Mutation) {
        self.api_library_dirty = true;
        if let Some(report) = mutation.report {
            self.show_file_report(mutation.message, report);
        } else {
            self.notice = mutation.message;
        }
        self.subtitle_catalog.refresh |=
            !mutation.changes.is_empty() || !mutation.removed.is_empty();
        if let Some(draft) = &mut self.schedule_draft {
            remap_scheduled_songs(&mut draft.songs, &mutation.changes);
        }
        self.play_history.remap(&mutation.changes);
        let subtitles = &mut self.subtitle_catalog;
        subtitles
            .pending
            .retain(|key| !mutation.removed.contains(key));
        for change in &mutation.changes {
            if let Some(pending) = subtitles
                .pending
                .iter_mut()
                .find(|key| **key == change.from)
            {
                pending.clone_from(&change.to);
            }
            if subtitles.generation_key.as_ref() == Some(&change.from) {
                subtitles.generation_key = Some(change.to.clone());
            }
            if subtitles.preview_key.as_ref() == Some(&change.from) {
                subtitles.preview_key = Some(change.to.clone());
            }
            if change.from == change.to {
                continue;
            }
            if self.selected_songs.remove(&change.from) {
                self.selected_songs.insert(change.to.clone());
            }
        }
        self.selected_songs
            .retain(|key| !mutation.removed.contains(key));
        match update_queue_keys(
            &mut self.queue,
            &mut self.queue_index,
            &mutation.changes,
            &mutation.removed,
        ) {
            QueueChange::CurrentRemoved => {
                if !self.recover_removed_program_track() {
                    self.stop_playback();
                }
            }
            QueueChange::CurrentMoved if self.loading.is_some() => {
                self.refresh_source_path();
            }
            QueueChange::CurrentMoved => {
                if let Some(path) = self.current_song().map(|song| PathBuf::from(&song.path)) {
                    self.audio.relocate_source(path);
                }
                self.refresh_loop_region();
            }
            _ => self.refresh_loop_region(),
        }
    }

    fn show_file_report(&mut self, summary: String, report: bgm_core::BatchReport) {
        self.notice = summary.clone();
        if report.total > 1 || report.skipped > 0 || report.failed > 0 {
            self.dialog = Some(Dialog::FileReport { summary, report });
        }
    }

    fn reconcile_snapshot(&mut self) {
        self.api_library_dirty = true;
        self.subtitle_catalog
            .pending
            .retain(|key| self.library.song(key).is_some());
        self.selected_songs
            .retain(|key| self.library.song(key).is_some());
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
        let removed: Vec<_> = self
            .queue
            .iter()
            .filter(|key| self.library.song(key).is_none())
            .cloned()
            .collect();
        if update_queue_keys(&mut self.queue, &mut self.queue_index, &[], &removed)
            == QueueChange::CurrentRemoved
        {
            if !self.recover_removed_program_track() {
                self.remove_from_queue(None);
                self.notice = "当前曲目已不在曲库中，播放已停止".into();
            }
        } else {
            self.refresh_source_path();
        }
    }

    fn refresh_source_path(&mut self) {
        let Some(song) = self.current_song() else {
            return;
        };
        if self
            .audio
            .source_path()
            .is_some_and(|path| path != Path::new(&song.path))
        {
            let key = song.key.clone();
            let intent = self.loading.map_or_else(
                || TrackStart::Restore {
                    position: self.audio.file_position(),
                    record_on_resume: self.record_play_on_resume,
                },
                |load| load.intent,
            );
            self.load_key_with(&key, self.playing, intent);
        }
    }

    fn rescan(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rescan_library();
        window.set_window_title(&self.library.window_title());
        cx.notify();
    }

    fn rescan_library(&mut self) {
        let warnings = self.library.rescan();
        self.api_config = api::Config::load(self.library.config_path());
        if let Some(server) = &self.api {
            server.update_config(self.api_config.clone());
        }
        self.notice = warnings
            .first()
            .cloned()
            .unwrap_or_else(|| format!("已重新扫描 {} 首", self.library.snapshot.songs.len()));
        self.reconcile_snapshot();
        self.audio.set_speed(self.library.prefs.playback_rate);
        self.install_watcher();
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
            prompt: Some("导入音频".into()),
        });
        let category = match &self.filter {
            LibraryFilter::Category(id) => Some(id.clone()),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(files))) = receiver.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if files.is_empty() {
                    this.notice = "没有选中支持的音频文件".into();
                } else if let Some(category) = category {
                    let mutation = this.library.import(&category, &files);
                    this.apply_mutation(mutation);
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
        let Some(song) = self.library.song(&key).cloned() else {
            self.notice = "曲目已不在曲库中".into();
            cx.notify();
            return;
        };
        let directory = dirs::download_dir().unwrap_or_else(|| self.library.root().to_path_buf());
        let settings = self.library.export_settings();
        cx.spawn(async move |this, cx| {
            let export = cx
                .background_executor()
                .spawn(async move { Exporter::new(settings) })
                .await;
            let suggested = match export.suggested_name(&song) {
                Ok(suggested) => suggested,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.notice = error;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            let Ok(receiver) = this.update(cx, |_, cx| {
                cx.prompt_for_new_path(&directory, Some(&suggested))
            }) else {
                return;
            };
            if let Ok(Ok(Some(path))) = receiver.await {
                this.update(cx, |this, cx| {
                    this.notice = "正在导出曲目…".into();
                    cx.notify();
                })
                .ok();
                let job = cx.background_executor().spawn(async move {
                    export
                        .single(Path::new(&song.path), &path)
                        .unwrap_or_else(|error| error)
                });
                let message = job.await;
                this.update(cx, |this, cx| {
                    this.notice = message;
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn export_selected(&mut self, cx: &mut Context<Self>) {
        let batch = match self.library.prepare_batch_export(&self.selected_keys()) {
            Ok(batch) => batch,
            Err(error) => {
                self.notice = error;
                cx.notify();
                return;
            }
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择批量导出的文件夹".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                this.update(cx, |this, cx| {
                    this.notice = "正在批量导出曲目…".into();
                    cx.notify();
                })
                .ok();
                let job = cx
                    .background_executor()
                    .spawn(async move { batch.run(&path) });
                let result = job.await;
                this.update(cx, |this, cx| {
                    match result {
                        Ok(report) => this.show_file_report(format_report(&report), report),
                        Err(error) => this.notice = error,
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn selected_keys(&self) -> Vec<String> {
        self.library
            .snapshot
            .songs
            .iter()
            .filter(|song| self.selected_songs.contains(&song.key))
            .map(|song| song.key.clone())
            .collect()
    }

    fn trash_key(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.trash_keys(vec![key], window, cx);
    }

    fn trash_keys(&mut self, keys: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let summary = if keys.len() == 1 {
            format!("《{}》", self.song_title(&keys[0]))
        } else {
            format!("{} 首曲目", keys.len())
        };
        let receiver = window.prompt(
            PromptLevel::Warning,
            "删除曲目",
            Some(&format!(
                "把{summary}移到曲库的 .tfy-trash，之后可以在访达里恢复。"
            )),
            &[PromptButton::ok("移到回收站"), PromptButton::cancel("取消")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if receiver.await == Ok(0) {
                this.update(cx, |this, cx| {
                    match this.library.trash_batch(&keys) {
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

    fn open_track_editor(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(song) = self.library.song(key) else {
            return;
        };
        let title = song.title.clone();
        let category = song.category.clone();
        self.dialog_field
            .update(cx, |field, cx| field.set_text(title, cx));
        self.dialog = Some(Dialog::EditTrack {
            key: key.to_owned(),
            category,
            error: None,
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
        self.category_detail_field
            .update(cx, |field, cx| field.set_text("", cx));
        self.dialog = Some(Dialog::NewCategory);
        let field = self.dialog_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn open_category_edit(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(category) = self
            .library
            .snapshot
            .categories
            .iter()
            .find(|category| category.id == id)
        else {
            return;
        };
        self.dialog_field
            .update(cx, |field, cx| field.set_text(category.name_zh.clone(), cx));
        self.category_detail_field
            .update(cx, |field, cx| field.set_text(category.color.clone(), cx));
        self.dialog = Some(Dialog::EditCategory { id: id.to_owned() });
        let field = self.dialog_field.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            window.focus(&field.focus_handle(cx))
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
        self.library.preview_volume(ratio, Instant::now());
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
        self.library
            .set_wallpaper_opacity((ratio * 100.0).round() / 100.0, Instant::now());
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        set_workspace_dark(self.library.appearance.dark(window.appearance()));
        if self.current_song().is_none() {
            self.now_playing_open = false;
            self.track_transition = None;
        }
        let viewport = window.viewport_size();
        let narrow = viewport.width < px(1130.);
        let now = Instant::now();
        if let Some(transition) = &self.track_transition {
            if transition.finished(now) {
                self.track_transition = None;
            } else {
                window.request_animation_frame();
            }
        }
        let progress = self
            .track_transition
            .as_ref()
            .map(|transition| transition.position(now));
        let browse =
            (!self.now_playing_open || progress.is_some()).then(|| self.browse_page(window, cx));
        let track = (self.now_playing_open || progress.is_some()).then(|| {
            let progress = progress.unwrap_or(1.);
            div()
                .absolute()
                .inset_0()
                .top(px(TRACK_PAGE_TRAVEL * (1. - progress)))
                .bottom(px(-TRACK_PAGE_TRAVEL * (1. - progress)))
                .occlude()
                .flex()
                .flex_col()
                .opacity(progress)
                .child(self.now_playing_page(window, cx))
        });
        let content = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .children(browse)
            .children(track);
        let player = self.player_bar(window, narrow, cx);
        let wallpaper = self.wallpaper_layer();
        let action_menu = self.action_menu_layer(window, cx);
        let dialog = self.dialog_layer(window, cx);
        let picker = self.picker_layer(cx);
        let export_popover = self.export_popover_layer(cx);
        let wallpaper_popover = self.wallpaper_popover_layer(window, cx);
        let queue = self.queue_layer(cx);
        let diagnostics = self.diagnostics_layer(cx);
        let settings = self.settings_layer(cx);
        let close_prompt = self.close_layer(cx);

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
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenSettings, window, cx| {
                    this.open_settings(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::TrackPage, window, cx| {
                    this.toggle_track_page(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ToggleDesktopLyrics, _, cx| {
                    this.toggle_desktop_lyrics(cx);
                }),
            )
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::confirm))
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme_color(BG))
            .text_color(theme_color(TEXT))
            .font_family(self.interface_font())
            .text_sm()
            .children(wallpaper)
            .child(content)
            .child(player)
            .children(action_menu)
            .children(dialog)
            .children(picker)
            .children(export_popover)
            .children(wallpaper_popover)
            .children(queue)
            .children(diagnostics)
            .children(settings)
            .children(close_prompt)
    }
}

impl Workspace {
    fn interface_font(&self) -> SharedString {
        if self.library.prefs.ui_font == "system" {
            ".SystemUIFont".into()
        } else {
            self.library.prefs.ui_font.clone().into()
        }
    }

    fn browse_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let viewport = window.viewport_size();
        let topbar = self.topbar(window, cx);
        let main = match self.view {
            AppView::Library => {
                self.visible = self.library.visible(&self.filter, &self.query);
                self.main_column(viewport.height < px(750.), f32::from(viewport.width), cx)
                    .into_any_element()
            }
            AppView::Schedule => self.schedule_page(window, cx).into_any_element(),
            AppView::Subtitles => self.subtitle_page(cx),
        };
        div()
            .flex()
            .flex_1()
            .min_h(px(0.))
            .child(self.sidebar(cx))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(topbar)
                    .child(main),
            )
    }

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
        let all_active = self.filter != LibraryFilter::Favorites && self.view == AppView::Library;
        let fav_active = self.filter == LibraryFilter::Favorites && self.view == AppView::Library;
        let schedule_active = self.view == AppView::Schedule;
        let root = self.library.root().display().to_string();
        let source_label = self.library.source_label();
        let panel = self.panel_alpha();
        let brand = &self.library.config.brand;
        div()
            .w(px(SIDEBAR_WIDTH))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(fade(theme_color(SIDE), panel))
            .border_r_1()
            .border_color(fade(theme_color(LINE), 0.7))
            .child(
                div()
                    .h(px(52.))
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()),
            )
            .child(
                div()
                    .px_4()
                    .pb_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.brand_mark())
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(17.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_ellipsis()
                                    .child(brand.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme_color(FAINT))
                                    .text_ellipsis()
                                    .child(brand.subtitle.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .pt_4()
                    .pb_2()
                    .text_size(px(10.))
                    .text_color(theme_color(FAINT))
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
                    ))
                    .child(
                        self.nav_button(
                            "nav-subtitles",
                            "list-music",
                            "字幕生成",
                            self.subtitle_catalog
                                .records
                                .as_ref()
                                .and_then(|result| result.as_ref().ok())
                                .map_or(0, Vec::len)
                                + self.subtitle_catalog.pending.len(),
                            self.view == AppView::Subtitles,
                            false,
                            cx,
                            |this, cx| {
                                this.view = AppView::Subtitles;
                                this.reload_subtitle_catalog(cx);
                            },
                        ),
                    ),
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
                            .text_color(theme_color(FAINT))
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
                    .mb(px(14.))
                    .p_3()
                    .rounded_md()
                    .bg(fade(theme_color(CONTROL_TINT), 0.025 * panel))
                    .border_1()
                    .border_color(fade(theme_color(LINE), 0.65))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().size(px(5.)).rounded_full().bg(theme_color(ACCENT)))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme_color(ACCENT))
                                    .child(source_label),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme_color(MUTED))
                                    .child(format!("{all_count} 首")),
                            ),
                    )
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(10.))
                            .text_color(theme_color(FAINT))
                            .text_ellipsis()
                            .child(root),
                    ),
            )
    }

    /// 有壁纸时面板统一降到这个不透明度，让底层颜色 / 画面透出来，但仍保持文字对比度。
    fn panel_alpha(&self) -> f32 {
        let view = self.wallpaper_view();
        if view.has_wallpaper {
            1.0 - view.opacity * 0.62
        } else {
            1.0
        }
    }

    fn brand_mark(&self) -> impl IntoElement + use<> {
        img("brand/mark.png").size(px(39.)).flex_none()
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
        let ink = if active { ACCENT_INK } else { MUTED };
        div()
            .id(id)
            .h(px(39.))
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .bg(if active {
                theme_color(ACCENT).into()
            } else {
                fade(theme_color(SIDE), 0.0)
            })
            .border_1()
            .border_color(if active {
                fade(theme_color(ACCENT), 1.)
            } else {
                fade(theme_color(SIDE), 0.)
            })
            .text_color(theme_color(ink))
            .hover(move |style| {
                style.bg(if active {
                    theme_color(ACCENT_HOVER)
                } else {
                    theme_color(PANEL)
                })
            })
            .on_click(cx.listener(move |this, _, _, cx| on_click(this, cx)))
            .child(icon(
                if favorite && active {
                    "heart-fill"
                } else {
                    icon_name
                },
                17.,
                theme_color(ink),
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
                    .text_color(theme_color(ink))
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
            .text_color(theme_color(MUTED))
            .hover(|style| style.bg(theme_color(PANEL)).text_color(theme_color(TEXT)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(icon_name, 14., theme_color(FAINT)))
            .child(label)
            .into_any_element()
    }

    // ---------- 渲染：顶栏 ----------

    fn topbar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        let panel = self.panel_alpha();
        let search = self.search.clone();
        div()
            .h(px(66.))
            .flex_none()
            .px_6()
            .flex()
            .items_center()
            .gap_3()
            .bg(fade(theme_color(BG), panel))
            .child(
                div()
                    .flex_1()
                    .max_w(px(410.))
                    .h(px(36.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_md()
                    .bg(fade(theme_color(PANEL), panel))
                    .border_1()
                    .border_color(if search_focused {
                        fade(theme_color(ACCENT), 0.55)
                    } else {
                        fade(theme_color(LINE), 0.8)
                    })
                    .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                        window.focus(&search.focus_handle(cx));
                    })
                    .child(icon(
                        "search",
                        15.,
                        if search_focused {
                            theme_color(ACCENT)
                        } else {
                            theme_color(FAINT)
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
                                .border_color(theme_color(LINE))
                                .text_size(px(11.))
                                .text_color(theme_color(FAINT))
                                .child("/"),
                        )
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()),
            )
            .child(self.tool_button(
                "tool-import",
                "upload",
                "导入音乐",
                false,
                cx,
                |this, window, cx| {
                    this.import_files(&ClickEvent::default(), window, cx);
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
                    if this.export_popover {
                        this.detect_ffmpeg(cx);
                    }
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
                "设置（⌘,）",
                false,
                cx,
                |this, window, cx| {
                    this.open_settings(window, cx);
                },
            ))
    }

    // ---------- 渲染：音乐库 ----------

    fn main_column(
        &mut self,
        compact: bool,
        viewport_width: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (title, description, results) = self.hero_copy();
        let count = self.visible.len();
        let card_width = ((viewport_width - SIDEBAR_WIDTH - 48. - 5. * 12.) / 6.).floor();
        let card_height = if compact {
            78.
        } else {
            (card_width * 0.78).clamp(100., 156.)
        };
        let show_shelf = self.filter != LibraryFilter::Favorites && self.query.is_empty();
        let category_count = self.library.snapshot.categories.len();
        let warning = self.library.snapshot.warnings.first().cloned();
        let notice = self.notice.clone();
        let panel = self.panel_alpha();
        let edit_category = match &self.filter {
            LibraryFilter::Category(id) => Some(id.clone()),
            _ => None,
        };

        div()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .flex()
            .flex_col()
            .bg(fade(theme_color(BG), panel))
            .child(
                div()
                    .px_6()
                    .pt_2()
                    .pb_4()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .w(px(viewport_width - SIDEBAR_WIDTH - 290.))
                            .flex_grow()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(PAGE_TITLE))
                                    .font_weight(FontWeight::BOLD)
                                    .w_full()
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme_color(MUTED))
                                    .w_full()
                                    .truncate()
                                    .child(description),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(self.hero_button(
                                "hero-play",
                                "play-fill",
                                &format!("全部播放 ({count})"),
                                true,
                                cx,
                                |this, _, cx| {
                                    if let Some(song) = this.visible.first() {
                                        this.play_key(song.key.clone());
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(self.hero_button(
                                "hero-shuffle",
                                "shuffle",
                                "随机试听",
                                false,
                                cx,
                                |this, _, cx| {
                                    if !this.visible.is_empty() && !this.guard_library_playback() {
                                        if let Some(key) = this.smart_pick_start() {
                                            this.play_key(key);
                                        }
                                    }
                                    cx.notify();
                                },
                            )),
                    ),
            )
            .when(show_shelf, |page| {
                page.child(
                    div()
                        .mx_6()
                        .mb_4()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(15.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("按场景浏览"),
                                )
                                .child(div().flex_1())
                                .when(self.filter != LibraryFilter::All, |bar| {
                                    bar.child(self.tiny_button(
                                        "全部场景",
                                        "all-scenes",
                                        cx,
                                        |this, _, cx| {
                                            this.filter = LibraryFilter::All;
                                            cx.notify();
                                        },
                                    ))
                                })
                                .when_some(edit_category, |bar, id| {
                                    bar.child(self.icon_button(
                                        "tool-category-edit",
                                        "edit",
                                        "编辑当前分类",
                                        false,
                                        cx,
                                        move |this, window, cx| {
                                            this.open_category_edit(&id, window, cx)
                                        },
                                    ))
                                })
                                .when(category_count > 6, |bar| {
                                    let step = px((card_width + 12.) * 3.);
                                    bar.child(self.icon_button(
                                        "scenes-prev",
                                        "chevron-left",
                                        "上一组分类",
                                        false,
                                        cx,
                                        move |this, _, cx| {
                                            let offset = this.shelf_scroll.offset();
                                            this.shelf_scroll.set_offset(point(
                                                (offset.x + step).min(px(0.)),
                                                px(0.),
                                            ));
                                            cx.notify();
                                        },
                                    ))
                                    .child(self.icon_button(
                                        "scenes-next",
                                        "chevron-right",
                                        "下一组分类",
                                        false,
                                        cx,
                                        move |this, _, cx| {
                                            let offset = this.shelf_scroll.offset();
                                            this.shelf_scroll.set_offset(point(
                                                (offset.x - step)
                                                    .max(-this.shelf_scroll.max_offset().width),
                                                px(0.),
                                            ));
                                            cx.notify();
                                        },
                                    ))
                                })
                                .child(self.icon_button(
                                    "tool-category",
                                    "plus",
                                    "新增分类",
                                    false,
                                    cx,
                                    |this, window, cx| this.open_category_dialog(window, cx),
                                )),
                        )
                        .child(self.category_shelf(card_width, card_height, cx)),
                )
            })
            .child(
                div()
                    .px_6()
                    .pb_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(results),
                    )
                    .child(
                        div()
                            .pb(px(1.))
                            .text_size(px(11.))
                            .text_color(theme_color(FAINT))
                            .child(format!("{count} 首配乐")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .max_w(px(330.))
                            .text_size(px(11.))
                            .text_color(theme_color(FAINT))
                            .text_ellipsis()
                            .child(if self.query.is_empty() {
                                notice
                            } else {
                                format!("正在筛选“{}”", self.query)
                            }),
                    )
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
                    .child(self.tiny_button(
                        if self.selecting {
                            "退出多选"
                        } else {
                            "多选"
                        },
                        "select-songs",
                        cx,
                        |this, _, cx| {
                            this.selecting = !this.selecting;
                            this.selected_songs.clear();
                            cx.notify();
                        },
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
                    .bg(fade(theme_color(WARNING), 0.08))
                    .border_1()
                    .border_color(fade(theme_color(WARNING), 0.2))
                    .text_size(px(11.))
                    .text_color(theme_color(WARNING))
                    .child(icon("alert", 14., theme_color(WARNING)))
                    .child(warning)
            }))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .mx_6()
                    .mb(px(14.))
                    .flex()
                    .flex_col()
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(fade(theme_color(PANEL), panel))
                    .border_1()
                    .border_color(fade(theme_color(LINE), 0.75))
                    .child(self.table_header())
                    .when(self.selecting, |table| table.child(self.batch_bar(cx)))
                    .child(self.song_list(cx)),
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
                fade(theme_color(ACCENT), 1.)
            } else {
                fade(theme_color(CONTROL_TINT), 0.07)
            })
            .text_color(if primary {
                theme_color(ACCENT_INK)
            } else {
                theme_color(TEXT)
            })
            .border_1()
            .border_color(if primary {
                fade(theme_color(CONTROL_TINT), 0.)
            } else {
                fade(theme_color(CONTROL_TINT), 0.12)
            })
            .hover(move |style| {
                if primary {
                    style.bg(theme_color(ACCENT_HOVER))
                } else {
                    style.bg(fade(theme_color(CONTROL_TINT), 0.12))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(
                icon_name,
                14.,
                if primary {
                    theme_color(ACCENT_INK)
                } else {
                    theme_color(TEXT)
                },
            ))
            .child(label)
            .into_any_element()
    }

    fn icon_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &'static str,
        solid: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id)
            .tooltip(move |_, cx| cx.new(|_| Tip(label.into())).into())
            .size(px(32.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .border_1()
            .border_color(if solid {
                fade(theme_color(CONTROL_TINT), 0.)
            } else {
                fade(theme_color(LINE), 0.)
            })
            .bg(if solid {
                fade(theme_color(CONTROL_TINT), 0.06)
            } else {
                fade(theme_color(CONTROL_TINT), 0.)
            })
            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.11)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(icon_name, 15., theme_color(MUTED)))
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
                fade(theme_color(ACCENT), 1.)
            } else {
                fade(theme_color(CONTROL_TINT), 0.03)
            })
            .text_color(if primary {
                theme_color(ACCENT_INK)
            } else {
                theme_color(MUTED)
            })
            .border_1()
            .border_color(if primary {
                fade(theme_color(CONTROL_TINT), 0.)
            } else {
                fade(theme_color(LINE), 1.)
            })
            .hover(move |style| {
                if primary {
                    style.bg(theme_color(ACCENT_HOVER))
                } else {
                    style
                        .bg(fade(theme_color(CONTROL_TINT), 0.1))
                        .text_color(theme_color(TEXT))
                }
            })
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(
                icon_name,
                14.,
                if primary {
                    theme_color(ACCENT_INK)
                } else {
                    theme_color(FAINT)
                },
            ))
            .child(label)
            .into_any_element()
    }

    fn category_shelf(
        &self,
        width: f32,
        height: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .id("scene-shelf")
            .track_scroll(&self.shelf_scroll)
            .overflow_x_scroll()
            .flex()
            .gap_3()
            .children(self.library.snapshot.categories.iter().map(|category| {
                let id = category.id.clone();
                let active = self.filter == LibraryFilter::Category(id.clone());
                let ink = if active { ACCENT } else { TEXT };
                div()
                    .id(SharedString::from(format!("scene-{id}")))
                    .flex_none()
                    .w(px(width))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = LibraryFilter::Category(id.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .w_full()
                            .h(px(height))
                            .rounded(px(9.))
                            .overflow_hidden()
                            .relative()
                            .child(sleeve(
                                &category.id,
                                category_icon(&category.icon),
                                parse_color(&category.color),
                                28.,
                                9.,
                            ))
                            .when(active, |cover| {
                                cover.child(
                                    div()
                                        .absolute()
                                        .top_1()
                                        .right_1()
                                        .size(px(21.))
                                        .rounded_full()
                                        .bg(theme_color(ACCENT))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(icon("check", 13., theme_color(PANEL))),
                                )
                            }),
                    )
                    .child(
                        div()
                            .h(px(25.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_ellipsis()
                                    .text_color(theme_color(ink))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(category.name_zh.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(theme_color(FAINT))
                                    .child(format!("{} 首", category.count)),
                            ),
                    )
            }))
    }

    fn batch_bar(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let count = self.selected_songs.len();
        div()
            .flex()
            .flex_none()
            .gap_2()
            .items_center()
            .px_4()
            .py_2()
            .bg(theme_color(PANEL_LIGHT))
            .child(
                div()
                    .min_w(px(62.))
                    .text_xs()
                    .text_color(theme_color(ACCENT))
                    .child(format!("已选 {count} 首")),
            )
            .child(self.tiny_button(
                "选中当前列表",
                "batch-select-visible",
                cx,
                |this, _, cx| {
                    this.selected_songs
                        .extend(this.visible.iter().map(|song| song.key.clone()));
                    cx.notify();
                },
            ))
            .when(count > 0, |bar| {
                bar.child(self.tiny_button("移动", "batch-move", cx, |this, _, cx| {
                    this.dialog = Some(Dialog::MoveBatch {
                        keys: this.selected_keys(),
                    });
                    cx.notify();
                }))
                .child(self.tiny_button("导出", "batch-export", cx, |this, _, cx| {
                    this.export_selected(cx)
                }))
                .child(
                    self.tiny_button("删除", "batch-trash", cx, |this, window, cx| {
                        this.trash_keys(this.selected_keys(), window, cx);
                    }),
                )
                .child(self.tiny_button(
                    "取消选择",
                    "batch-clear",
                    cx,
                    |this, _, cx| {
                        this.selected_songs.clear();
                        cx.notify();
                    },
                ))
            })
            .into_any_element()
    }

    fn table_header(&self) -> impl IntoElement + use<> {
        div()
            .h(px(33.))
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .flex_none()
            .bg(fade(theme_color(CONTROL_TINT), 0.015))
            .border_b_1()
            .border_color(fade(theme_color(LINE), 0.8))
            .text_size(px(10.))
            .text_color(theme_color(FAINT))
            .child(div().w(px(30.)).child("#"))
            .child(div().flex_1().pl(px(44.)).child("曲目"))
            .child(div().w(px(82.)).child("分类"))
            .child(div().w(px(52.)).child("时长"))
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
                .child(icon("music", 30., theme_color(FAINT)))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme_color(MUTED))
                        .child("这个列表里还没有曲目"),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme_color(FAINT))
                        .child("换个分类、清空搜索，或导入新的音频"),
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
        .track_scroll(self.song_scroll.clone())
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
            .unwrap_or_else(|| theme_color(ACCENT));
        let category_icon_name = category
            .map(|category| category_icon(&category.icon))
            .unwrap_or("music");
        let subtitle = song
            .artist
            .as_ref()
            .or(song.album.as_ref())
            .cloned()
            .unwrap_or_else(|| song.file_name.clone());
        let time = song
            .duration_ms
            .map(|ms| format_time(Duration::from_millis(u64::from(ms))))
            .unwrap_or_else(|| "—".into());
        let selected = self.selected_songs.contains(&song.key);
        let selecting = self.selecting;
        let play_key = song.key.clone();
        let fav_key = song.key.clone();
        let menu_key = song.key.clone();
        let loop_active = self.loop_region_for(Some(song.key.as_str())).is_some();

        let can_reorder = self.library.prefs.sort_by == SortBy::Default && !selecting;
        let drag = SongDrag {
            key: song.key.clone(),
            title: song.title.clone(),
        };
        let drop_key = song.key.clone();
        div()
            .id(SharedString::from(format!("library-song-{}", song.key)))
            .when(can_reorder, |row| {
                row.on_drag(drag, |song, _, _, cx| {
                    cx.new(|_| SongDragPreview(song.title.clone()))
                })
                .drag_over::<SongDrag>(|style, _, _, _| style.bg(theme_color(PANEL_LIGHT)))
                .on_drop(cx.listener(move |this, song: &SongDrag, _, cx| {
                    this.library.reorder_song(&song.key, &drop_key);
                    cx.notify();
                }))
            })
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(fade(theme_color(LINE), 0.45))
            .bg(if current {
                fade(theme_color(ACCENT), 0.075)
            } else {
                fade(theme_color(BG), 0.)
            })
            .hover(|style| style.bg(fade(theme_color(ACCENT), 0.045)))
            .child(
                div()
                    .w(px(30.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if playing {
                        icon("play-fill", 11., theme_color(ACCENT)).into_any_element()
                    } else {
                        div()
                            .text_size(px(11.))
                            .text_color(if current {
                                theme_color(ACCENT)
                            } else {
                                theme_color(FAINT)
                            })
                            .child(if selecting {
                                if selected { "✓".into() } else { "□".into() }
                            } else {
                                format!("{:02}", index + 1)
                            })
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
                        if selecting {
                            if !this.selected_songs.remove(&play_key) {
                                this.selected_songs.insert(play_key.clone());
                            }
                        } else if this.current_key().as_deref() == Some(play_key.as_str())
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
                            .rounded(px(5.))
                            .overflow_hidden()
                            .child(sleeve(
                                &song.category,
                                category_icon_name,
                                category_color,
                                15.,
                                5.,
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
                                                theme_color(ACCENT)
                                            } else {
                                                theme_color(TEXT)
                                            })
                                            .min_w(px(0.))
                                            .truncate()
                                            .child(song.title.clone()),
                                    )
                                    .when(loop_active, |this| {
                                        this.child(
                                            div()
                                                .px_1()
                                                .flex_none()
                                                .rounded(px(4.))
                                                .bg(fade(theme_color(ACCENT), 0.14))
                                                .border_1()
                                                .border_color(fade(theme_color(ACCENT), 0.3))
                                                .text_size(px(8.5))
                                                .text_color(theme_color(ACCENT))
                                                .child("循环"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(theme_color(FAINT))
                                    .text_ellipsis()
                                    .child(subtitle),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(82.))
                    .text_size(px(12.))
                    .text_color(theme_color(MUTED))
                    .text_ellipsis()
                    .child(category_name),
            )
            .child(
                div()
                    .w(px(52.))
                    .text_size(px(12.))
                    .text_color(theme_color(FAINT))
                    .child(time),
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
                            theme_color(FAVORITE)
                        } else {
                            theme_color(FAINT)
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
                            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.09)))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                this.show_track_menu(menu_key.clone(), event.position(), cx);
                            }))
                            .child(icon("more", 14., theme_color(FAINT))),
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
            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.09)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(icon_name, 13., color))
            .into_any_element()
    }

    // ---------- 渲染：播放器 ----------

    fn toggle_track_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.current_song().is_none()
            || self.dialog.is_some()
            || self.picker.is_some()
            || self.settings_page.is_some()
            || self.close_prompt
            || self.diagnostics_open
        {
            return;
        }
        self.set_track_page(!self.now_playing_open);
        self.queue_open = false;
        self.export_popover = false;
        self.wallpaper_popover = false;
        self.action_menu = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn set_track_page(&mut self, open: bool) {
        if self.now_playing_open == open {
            return;
        }
        let now = Instant::now();
        let from = self
            .track_transition
            .as_ref()
            .map_or(if self.now_playing_open { 1. } else { 0. }, |transition| {
                transition.position(now)
            });
        self.now_playing_open = open;
        if open {
            self.track_details_key = None;
        }
        self.track_transition = if desktop::reduce_motion() {
            None
        } else {
            TrackTransition::new(from, open, now)
        };
    }

    fn lyrics_event_handler(cx: &Context<Self>) -> desktop_lyrics::Handler {
        let owner = cx.weak_entity();
        std::rc::Rc::new(move |event, cx| {
            let owner = owner.clone();
            cx.defer(move |cx| {
                let _ = owner.update(cx, |this, cx| match event {
                    desktop_lyrics::Event::Close => this.close_desktop_lyrics(true, cx),
                    desktop_lyrics::Event::Settings => this.toggle_lyrics_settings(cx),
                    desktop_lyrics::Event::CloseSettings => this.close_lyrics_settings(cx),
                    desktop_lyrics::Event::Style(style) => {
                        this.apply_desktop_lyrics_style(style, cx)
                    }
                });
            });
        })
    }

    fn toggle_desktop_lyrics(&mut self, cx: &mut Context<Self>) {
        if self.desktop_lyrics.overlay.is_some() {
            self.close_desktop_lyrics(true, cx);
        } else {
            self.open_desktop_lyrics(cx);
        }
    }

    fn open_desktop_lyrics(&mut self, cx: &mut Context<Self>) {
        if !cfg!(target_os = "macos") || self.asr_session.is_none() {
            self.notice = "桌面歌词需要在 macOS 上加载 ASR 模型".into();
            cx.notify();
            return;
        }
        if self.desktop_lyrics.overlay.is_some() {
            return;
        }
        let handler = Self::lyrics_event_handler(cx);
        let style = self.desktop_lyrics.style.clone();
        let bounds = Bounds::centered(
            None,
            size(px(f32::from(style.width)), px(f32::from(style.height))),
            cx,
        );
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                kind: WindowKind::PopUp,
                window_background: WindowBackgroundAppearance::Transparent,
                window_min_size: Some(size(px(420.), px(120.))),
                focus: false,
                is_minimizable: false,
                ..Default::default()
            },
            move |window, cx| {
                desktop::configure_lyrics_window(window);
                desktop::lyrics_pointer(window, style.locked);
                let close = handler.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    close(desktop_lyrics::Event::Close, cx);
                    false
                });
                cx.new(|_| desktop_lyrics::Overlay::new(style, handler))
            },
        );
        match result {
            Ok(window) => {
                self.desktop_lyrics.overlay = Some(window);
                if let Err(error) = self.library.set_desktop_lyrics_enabled(true) {
                    self.notice = error;
                }
                self.poll_track_lyrics(cx);
                self.poll_desktop_lyrics(cx);
            }
            Err(error) => self.notice = format!("无法打开桌面歌词：{error}"),
        }
        cx.notify();
    }

    fn close_desktop_lyrics(&mut self, explicit: bool, cx: &mut Context<Self>) {
        self.save_lyrics_window_size();
        if let Some(window) = self.desktop_lyrics.overlay.take() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        self.close_lyrics_settings(cx);
        if explicit
            && !self.desktop_lyrics.quitting
            && let Err(error) = self.library.set_desktop_lyrics_enabled(false)
        {
            self.notice = error;
        }
        cx.notify();
    }

    fn close_lyrics_settings(&mut self, cx: &mut Context<Self>) {
        if let Some(window) = self.desktop_lyrics.settings.take() {
            let _ = window.update(cx, |_, window, _| window.remove_window());
        }
        cx.notify();
    }

    fn toggle_lyrics_settings(&mut self, cx: &mut Context<Self>) {
        if self.desktop_lyrics.settings.is_some() {
            self.close_lyrics_settings(cx);
            return;
        }
        if self.desktop_lyrics.overlay.is_none() {
            return;
        }
        let handler = Self::lyrics_event_handler(cx);
        let style = self.desktop_lyrics.style.clone();
        let bounds = Bounds::centered(None, size(px(398.), px(392.)), cx);
        let result = cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("桌面歌词设置".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                kind: WindowKind::Floating,
                is_resizable: false,
                is_minimizable: false,
                ..Default::default()
            },
            move |window, cx| {
                let close = handler.clone();
                window.on_window_should_close(cx, move |_, cx| {
                    close(desktop_lyrics::Event::CloseSettings, cx);
                    false
                });
                cx.new(|_| desktop_lyrics::Settings::new(style, handler))
            },
        );
        match result {
            Ok(window) => {
                self.desktop_lyrics.settings = Some(window);
                self.poll_desktop_lyrics(cx);
            }
            Err(error) => self.notice = format!("无法打开歌词设置：{error}"),
        }
        cx.notify();
    }

    fn apply_desktop_lyrics_style(&mut self, style: desktop_lyrics::Style, cx: &mut Context<Self>) {
        let resized = (
            self.desktop_lyrics.style.width,
            self.desktop_lyrics.style.height,
        ) != (style.width, style.height);
        let size = size(px(f32::from(style.width)), px(f32::from(style.height)));
        self.desktop_lyrics.style = style;
        if resized {
            self.desktop_lyrics.size_to_save = None;
        }
        if let Err(error) = self
            .library
            .set_desktop_lyrics_style(&self.desktop_lyrics.style)
        {
            self.notice = error;
        }
        if resized && let Some(window) = self.desktop_lyrics.overlay {
            let _ = window.update(cx, |_, window, _| {
                window.resize(size);
                desktop::fit_lyrics_bounds(window);
            });
        }
        self.poll_desktop_lyrics(cx);
        cx.notify();
    }

    fn save_lyrics_window_size(&mut self) {
        if let Some(((width, height), _)) = self.desktop_lyrics.size_to_save.take() {
            self.desktop_lyrics.style.width = width;
            self.desktop_lyrics.style.height = height;
            if let Err(error) = self
                .library
                .set_desktop_lyrics_style(&self.desktop_lyrics.style)
            {
                self.notice = error;
            }
        }
    }

    fn poll_desktop_lyrics(&mut self, cx: &mut Context<Self>) {
        let Some(overlay) = self.desktop_lyrics.overlay else {
            return;
        };
        if self.asr_session.is_none() {
            self.close_desktop_lyrics(false, cx);
            return;
        }
        let style = &self.desktop_lyrics.style;
        let now = Instant::now();
        let lines = match self
            .track_lyrics
            .as_ref()
            .and_then(|track| track.result.as_ref().map(|result| (track, result)))
        {
            Some((track, Ok(Some(timeline)))) => timeline
                .frame(track.clock.position(now) + f64::from(style.lead_seconds))
                .into(),
            Some((_, Ok(None))) => desktop_lyrics::Lines::Message("当前曲目暂无字幕".into()),
            Some((_, Err(error))) => desktop_lyrics::Lines::Message(error.clone().into()),
            None if self.current_key().is_some() => {
                desktop_lyrics::Lines::Message("正在读取字幕…".into())
            }
            None => desktop_lyrics::Lines::Message("等待播放".into()),
        };
        let settings = self.desktop_lyrics.settings;
        let dimensions = overlay.update(cx, |view, window, cx| {
            if let Some(settings) = settings {
                let _ = settings.update(cx, |_, settings, _| {
                    desktop::anchor_lyrics_settings(settings, window)
                });
            }
            let hot = desktop::lyrics_pointer(window, style.locked);
            view.update_content(style, lines, hot, cx);
            desktop::lyrics_window_size(window)
        });
        let Ok((width, height)) = dimensions else {
            self.close_desktop_lyrics(false, cx);
            return;
        };
        let size = (width.clamp(420, 1600), height.clamp(120, 900));
        if (style.width, style.height) == size {
            self.desktop_lyrics.size_to_save = None;
        } else {
            match self.desktop_lyrics.size_to_save {
                Some((pending, since)) if pending == size => {
                    if now.duration_since(since) >= Duration::from_millis(450) {
                        self.save_lyrics_window_size();
                        cx.notify();
                    }
                }
                _ => self.desktop_lyrics.size_to_save = Some((size, now)),
            }
        }
        if let Some(settings) = self.desktop_lyrics.settings {
            let style = &self.desktop_lyrics.style;
            let updated = settings.update(cx, |view, _, cx| {
                if &view.style != style {
                    view.style = style.clone();
                    cx.notify();
                }
            });
            if updated.is_err() {
                self.desktop_lyrics.settings = None;
            }
        }
    }

    fn poll_track_lyrics(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(key) = self
            .queue
            .get(self.queue_index)
            .filter(|_| self.now_playing_open || self.desktop_lyrics.overlay.is_some())
        else {
            self.track_lyrics_task = None;
            return self.track_lyrics.take().is_some();
        };
        let sample = PlaybackSample {
            position: self.position.as_secs_f64(),
            duration: self.duration.as_secs_f64(),
            rate: self.library.prefs.playback_rate,
            playing: self.playing && self.loading.is_none() && !self.audio.is_paused(),
        };
        let now = Instant::now();
        if let Some(preview) = &mut self.track_lyrics
            && preview.key == *key
        {
            let changed = preview.clock.sync(sample, now);
            let follow_changed = self.now_playing_open
                && self.track_details_key.as_ref() != Some(key)
                && preview.follow_playing_line(now);
            return changed || follow_changed;
        }

        if self.track_details_key.as_ref() != Some(key) {
            self.track_details_key = None;
        }
        let scroll = ListState::new(0, ListAlignment::Top, px(180.));
        let owner = cx.weak_entity();
        let scroll_key = key.clone();
        scroll.set_scroll_handler(move |_, _, cx| {
            let _ = owner.update(cx, |this, cx| {
                if let Some(state) = &mut this.track_lyrics
                    && state.key == scroll_key
                {
                    state.browsed_at = Some(Instant::now());
                    cx.notify();
                }
            });
        });
        self.track_lyrics = Some(TrackLyrics {
            key: key.clone(),
            clock: lyrics::Clock::new(sample, now),
            result: None,
            scroll,
            scroll_anchor: None,
            browsed_at: None,
        });
        let directory = self.library.subtitle_directory();
        let key = key.clone();
        self.track_lyrics_task = Some(cx.spawn(async move |this, cx| {
            let requested_key = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    load_subtitle(&directory, &key)
                        .map(|record| record.map(|lines| Arc::new(lyrics::Timeline::new(lines))))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(preview) = &mut this.track_lyrics
                    && preview.key == requested_key
                {
                    if let Ok(Some(timeline)) = &result {
                        preview.scroll.reset(timeline.len());
                    }
                    preview.result = Some(result);
                    if this.now_playing_open
                        && this.track_details_key.as_ref() != Some(&requested_key)
                    {
                        preview.follow_playing_line(Instant::now());
                    }
                    cx.notify();
                }
            });
        }));
        true
    }

    fn edit_track_subtitle(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.editing_subtitle {
            return;
        }
        let Some(song) = self.current_song() else {
            return;
        };
        let duration = song.duration_ms;
        let key = song.key.clone();
        let directory = self.library.subtitle_directory();
        let format = self.library.subtitle_format();
        let title = format!(
            "编辑 {} · {}",
            format.extension().to_ascii_uppercase(),
            song.title
        );
        let mut window = window.to_async(cx);
        self.editing_subtitle = true;
        cx.spawn(async move |this, cx| {
            let saved_key = key.clone();
            let result = async {
                let editor = cx
                    .background_executor()
                    .spawn(async move { SubtitleEdit::open(&directory, &key).map(Arc::new) })
                    .await?;
                let initial = editor.text(format);
                let mut draft = initial.clone();
                let mut feedback: Option<String> = None;
                loop {
                    let response = window
                        .update(|window, _| {
                            desktop::edit_subtitle_text(window, &title, &draft, feedback.as_deref())
                        })
                        .map_err(|error| error.to_string())??;
                    let Some(next) = response.await.ok().flatten() else {
                        return Ok(false);
                    };
                    if next == initial {
                        return Ok(false);
                    }
                    let edit = editor.clone();
                    let submitted = next.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move { edit.save(format, &submitted, duration) })
                        .await;
                    match result {
                        Ok(()) => return Ok::<_, String>(true),
                        Err(error) => {
                            draft = next;
                            feedback = Some(error);
                        }
                    }
                }
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.editing_subtitle = false;
                match result {
                    Ok(true) => {
                        this.subtitle_catalog.refresh = true;
                        if this
                            .track_lyrics
                            .as_ref()
                            .is_some_and(|preview| preview.key == saved_key)
                        {
                            this.track_lyrics_task = None;
                            this.track_lyrics = None;
                            this.poll_track_lyrics(cx);
                        }
                        this.notice = "已保存字幕".into();
                    }
                    Ok(false) => {}
                    Err(error) => this.notice = error,
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn export_track_subtitle(&self, cx: &mut Context<Self>) {
        let Some(song) = self.current_song() else {
            return;
        };
        self.export_subtitle_record(song.key.clone(), &song.title, cx);
    }

    fn export_subtitle_record(&self, key: String, title: &str, cx: &mut Context<Self>) {
        let directory = self.library.subtitle_directory();
        let format = self.library.subtitle_format();
        let name = format!(
            "{}.{}",
            bgm_core::export::safe_file_name(title),
            format.extension()
        );
        let downloads = dirs::download_dir().unwrap_or_else(|| self.library.root().to_path_buf());
        let response = cx.prompt_for_new_path(&downloads, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = response.await else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move {
                    let editor = SubtitleEdit::open(&directory, &key)?;
                    export_subtitle(&path, &editor.text(format))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.notice = result.err().unwrap_or_else(|| "已导出字幕".into());
                cx.notify();
            });
        })
        .detach();
    }

    fn open_subtitle_generation(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.action_menu = None;
        self.set_track_page(false);
        self.view = AppView::Subtitles;
        self.queue_open = false;
        self.diagnostics_open = false;
        window.focus(&self.focus);
        if self.subtitle_progress.is_none()
            && self.subtitle_catalog.editor.is_none()
            && !self.editing_subtitle
            && self.library.song(&key).is_some()
        {
            self.subtitle_catalog.select_track(key);
        }
        self.reload_subtitle_catalog(cx);
    }

    fn reload_subtitle_catalog(&mut self, cx: &mut Context<Self>) {
        let directory = self.library.subtitle_directory();
        self.subtitle_catalog.refresh = false;
        self.subtitle_catalog.list_task = Some(cx.spawn(async move |this, cx| {
            let records = cx
                .background_executor()
                .spawn(async move { list_subtitles(&directory) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(records) = &records {
                    let saved: HashSet<_> = records.iter().map(|record| &record.key).collect();
                    let catalog = &mut this.subtitle_catalog;
                    catalog.pending.retain(|key| !saved.contains(key));
                    let selection = catalog
                        .generation_key
                        .as_ref()
                        .or(catalog.preview_key.as_ref());
                    if let Some(key) = selection.filter(|key| saved.contains(*key)).cloned() {
                        this.preview_subtitle_record(key, cx);
                    } else {
                        catalog.preview_task = None;
                        catalog.preview_key = None;
                        catalog.preview = None;
                    }
                }
                this.subtitle_catalog.records = Some(records);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn preview_subtitle_record(&mut self, key: String, cx: &mut Context<Self>) {
        let directory = self.library.subtitle_directory();
        let format = self.library.subtitle_format();
        self.subtitle_catalog.preview_key = Some(key.clone());
        self.subtitle_catalog.preview = None;
        self.subtitle_catalog.preview_task = Some(cx.spawn(async move |this, cx| {
            let requested_key = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    SubtitleEdit::open(&directory, &key).map(|editor| editor.text(format))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.subtitle_catalog.preview_key.as_ref() == Some(&requested_key) {
                    this.subtitle_catalog.preview = Some(result);
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn remove_subtitle_record(&self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        let confirm = window.prompt(
            PromptLevel::Warning,
            "删除字幕记录",
            Some("同时删除该曲目在本应用中的分离产物。原始音频保留。"),
            &[PromptButton::ok("删除"), PromptButton::cancel("取消")],
            cx,
        );
        let directory = self.library.subtitle_directory();
        cx.spawn(async move |this, cx| {
            if confirm.await != Ok(0) {
                return;
            }
            let removed_key = key.clone();
            let result = cx
                .background_executor()
                .spawn(async move { delete_subtitle(&directory, &key) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this
                    .track_lyrics
                    .as_ref()
                    .is_some_and(|preview| preview.key == removed_key)
                {
                    this.track_lyrics_task = None;
                    this.track_lyrics = None;
                }
                if result.is_ok()
                    && this.subtitle_catalog.generation_key.as_ref() == Some(&removed_key)
                {
                    this.subtitle_catalog.generation_key = None;
                }
                this.notice = result.err().unwrap_or_else(|| "已删除字幕记录".into());
                this.reload_subtitle_catalog(cx);
            });
        })
        .detach();
    }

    fn open_segment_editor(&mut self, key: String, title: String, cx: &mut Context<Self>) {
        let directory = self.library.subtitle_directory();
        self.subtitle_catalog.editor_task = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    SubtitleEdit::open(&directory, &key).map(|edit| {
                        let draft = edit.segments().to_vec();
                        SegmentEditor {
                            edit: Arc::new(edit),
                            key,
                            title,
                            draft,
                            selected: Some(0),
                            message: None,
                            saving: false,
                        }
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(editor) => {
                        this.subtitle_catalog.editor = Some(editor);
                        this.load_segment_fields(cx);
                    }
                    Err(error) => this.notice = error,
                }
                cx.notify();
            });
        }));
    }

    fn load_segment_fields(&mut self, cx: &mut Context<Self>) {
        let line = self
            .subtitle_catalog
            .editor
            .as_ref()
            .and_then(|editor| editor.selected.and_then(|index| editor.draft.get(index)));
        let values = line.map_or_else(
            || [String::new(), String::new(), String::new()],
            |line| {
                [
                    line.start.to_string(),
                    line.end.to_string(),
                    line.text.clone(),
                ]
            },
        );
        for (field, value) in self.segment_fields.iter().zip(values) {
            field.update(cx, |field, cx| field.set_text(value, cx));
        }
    }

    fn update_segment_draft(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(editor) = &mut self.subtitle_catalog.editor else {
            return false;
        };
        if editor.saving {
            return false;
        }
        let Some(index) = editor.selected else {
            return true;
        };
        let [start, end, text] = self
            .segment_fields
            .each_ref()
            .map(|field| field.read(cx).text());
        match SubtitleSegment::from_fields(start, end, text) {
            Ok(line) => {
                editor.draft[index] = line;
                editor.message = None;
                true
            }
            Err(error) => {
                editor.message = Some(format!("第 {} 段 · {}", index + 1, error));
                cx.notify();
                false
            }
        }
    }

    fn select_segment(&mut self, index: usize, cx: &mut Context<Self>) {
        if self
            .subtitle_catalog
            .editor
            .as_ref()
            .is_some_and(|editor| editor.selected == Some(index))
            || !self.update_segment_draft(cx)
        {
            return;
        }
        if let Some(editor) = &mut self.subtitle_catalog.editor
            && index < editor.draft.len()
        {
            editor.selected = Some(index);
            self.load_segment_fields(cx);
            cx.notify();
        }
    }

    fn add_segment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.update_segment_draft(cx) {
            return;
        }
        let Some(editor) = &mut self.subtitle_catalog.editor else {
            return;
        };
        if editor.draft.len() >= MAX_SUBTITLE_SEGMENTS {
            editor.message = Some(format!("字幕最多包含 {MAX_SUBTITLE_SEGMENTS} 个分段"));
        } else {
            let start = editor.draft.iter().map(|line| line.end).fold(0.0, f64::max);
            editor.selected = Some(editor.draft.len());
            editor.draft.push(SubtitleSegment {
                start,
                end: start + 5.0,
                text: String::new(),
            });
            self.load_segment_fields(cx);
            window.focus(&self.segment_fields[2].focus_handle(cx));
        }
        cx.notify();
    }

    fn remove_segment(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.subtitle_catalog.editor else {
            return;
        };
        if editor.saving {
            return;
        }
        if let Some(index) = editor.selected {
            editor.draft.remove(index);
            editor.selected = editor
                .draft
                .len()
                .checked_sub(1)
                .map(|last| last.min(index));
            editor.message = None;
            self.load_segment_fields(cx);
            cx.notify();
        }
    }

    fn save_subtitle_segments(&mut self, cx: &mut Context<Self>) {
        if !self.update_segment_draft(cx) {
            return;
        }
        let Some(editor) = &mut self.subtitle_catalog.editor else {
            return;
        };
        if editor.draft == editor.edit.segments() {
            self.subtitle_catalog.editor = None;
            cx.notify();
            return;
        }
        editor.saving = true;
        let (edit, draft) = (editor.edit.clone(), editor.draft.clone());
        let key = editor.key.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { edit.save_segments(draft) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.subtitle_catalog.editor = None;
                        this.subtitle_catalog.refresh = true;
                        if this
                            .track_lyrics
                            .as_ref()
                            .is_some_and(|preview| preview.key == key)
                        {
                            this.track_lyrics_task = None;
                            this.track_lyrics = None;
                        }
                        this.notice = "已保存字幕分段".into();
                    }
                    Err(error) => {
                        if let Some(editor) = &mut this.subtitle_catalog.editor {
                            editor.saving = false;
                            editor.message = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn segment_editor_page(
        &self,
        editor: &SegmentEditor,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let list =
            uniform_list(
                "subtitle-segment-list",
                editor.draft.len(),
                cx.processor(|this, range: Range<usize>, _, cx| {
                    let Some(editor) = &this.subtitle_catalog.editor else {
                        return Vec::new();
                    };
                    range
                        .filter_map(|index| {
                            let line = editor.draft.get(index)?;
                            Some(
                                div()
                                    .id(("subtitle-segment", index))
                                    .h(px(36.))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .bg(if editor.selected == Some(index) {
                                        theme_color(PANEL_LIGHT)
                                    } else {
                                        theme_color(PANEL)
                                    })
                                    .cursor_pointer()
                                    .hover(|row| row.bg(theme_color(PANEL_LIGHT)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.select_segment(index, cx)
                                    }))
                                    .child(
                                        div()
                                            .w(px(36.))
                                            .text_xs()
                                            .text_color(theme_color(MUTED))
                                            .child((index + 1).to_string()),
                                    )
                                    .child(
                                        div()
                                            .w(px(135.))
                                            .text_xs()
                                            .truncate()
                                            .text_color(theme_color(MUTED))
                                            .child(format!("{:.3} – {:.3}", line.start, line.end)),
                                    )
                                    .child(
                                        div()
                                            .min_w(px(0.))
                                            .flex_1()
                                            .truncate()
                                            .child(line.text.clone()),
                                    ),
                            )
                        })
                        .collect()
                }),
            )
            .flex_1()
            .min_h(px(0.));
        let start = self.segment_fields[0].clone();
        let end = self.segment_fields[1].clone();
        let content = self.segment_fields[2].clone();
        let field = |label: &'static str, entity: Entity<TextField>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().text_color(theme_color(MUTED)).child(label))
                .child(entity)
        };
        div()
            .px_5()
            .pb_4()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .gap_3()
            .child(
                div()
                    .py_3()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("字幕分段 · {}", editor.title)),
                    )
                    .when(!editor.saving, |row| {
                        row.child(self.tiny_button(
                            "取消",
                            "cancel-segment-edit",
                            cx,
                            |this, window, cx| {
                                this.subtitle_catalog.editor = None;
                                window.focus(&this.focus);
                                cx.notify();
                            },
                        ))
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme_color(LINE))
                    .child(list),
            )
            .when(editor.selected.is_some() && !editor.saving, |panel| {
                panel.child(
                    div()
                        .flex()
                        .gap_3()
                        .child(field("开始（秒）", start).w(px(114.)))
                        .child(field("结束（秒）", end).w(px(114.)))
                        .child(field("字幕文本", content).flex_1().min_w(px(0.))),
                )
            })
            .when_some(editor.message.clone(), |panel, message| {
                panel.child(
                    div()
                        .text_xs()
                        .text_color(theme_color(MUTED))
                        .child(message),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .when(editor.saving, |row| row.child("正在保存分段"))
                    .when(!editor.saving, |row| {
                        row.child(self.tiny_button(
                            "添加分段",
                            "add-subtitle-segment",
                            cx,
                            |this, window, cx| this.add_segment(window, cx),
                        ))
                    })
                    .when(!editor.saving && editor.selected.is_some(), |row| {
                        row.child(self.tiny_button(
                            "删除此段",
                            "remove-subtitle-segment",
                            cx,
                            |this, _, cx| this.remove_segment(cx),
                        ))
                    })
                    .child(div().flex_1())
                    .when(!editor.saving, |row| {
                        row.child(self.tiny_button(
                            "保存分段",
                            "save-subtitle-segments",
                            cx,
                            |this, _, cx| this.save_subtitle_segments(cx),
                        ))
                    }),
            )
            .into_any_element()
    }

    fn open_separated_audio(&self, key: String, kind: SeparatedAudio, cx: &mut Context<Self>) {
        let directory = self.library.subtitle_directory();
        cx.spawn(async move |this, cx| {
            let path = cx
                .background_executor()
                .spawn(async move { SubtitleEdit::open(&directory, &key)?.separated_audio(kind) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = path.and_then(|path| desktop::open_audio_file(&path)) {
                    this.notice = error;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn reveal_subtitle_delivery(
        &self,
        key: String,
        delivery: SubtitleDelivery,
        cx: &mut Context<Self>,
    ) {
        let directory = self.library.subtitle_directory();
        cx.spawn(async move |this, cx| {
            let path = cx
                .background_executor()
                .spawn(async move { SubtitleEdit::open(&directory, &key)?.delivery_path(delivery) })
                .await;
            let _ = this.update(cx, |this, cx| match path {
                Ok(path) => cx.reveal_path(&path),
                Err(error) => {
                    this.notice = error;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn process_track_audio(
        &mut self,
        key: String,
        captions: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        enum Output {
            Stems(separation::Prepared),
            Subtitles(subtitle_generation::Generated),
        }
        self.action_menu = None;
        if self.separating.contains(&key) || self.api_subtitle_writes.contains(&key) {
            self.notice = "曲目正在处理，请稍后再试".into();
            if captions {
                self.subtitle_feedback = Some((key, self.notice.clone()));
            }
            cx.notify();
            return;
        }
        let session = if captions {
            if self.asr_busy() {
                self.notice = "正在处理 ASR 任务，请稍候".into();
                self.subtitle_feedback = Some((key, self.notice.clone()));
                cx.notify();
                return;
            }
            let Some(session) = self.asr_session.clone() else {
                self.notice = "请先在「设置 → 模型」加载 ASR 模型".into();
                self.subtitle_feedback = Some((key, self.notice.clone()));
                cx.notify();
                return;
            };
            Some(session)
        } else {
            None
        };
        let Some(song) = self.library.song(&key).cloned() else {
            if captions {
                self.notice = "曲目已移出曲库，请重新选择曲目".into();
                self.subtitle_feedback = Some((key, self.notice.clone()));
            }
            cx.notify();
            return;
        };
        let source = PathBuf::from(&song.path);
        let title = song.title.clone();
        let models_root = self.library.asr_model_preferences().root();
        let subtitles = self.library.subtitle_directory();
        let previous_model = captions
            .then(|| self.library.prefs.separation_model.clone())
            .flatten();
        self.separating.insert(key.clone());
        if captions {
            self.subtitle_feedback = None;
            self.subtitle_progress = Some((key.clone(), "正在查找人声分离模型…".into()));
        }
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let mut asr_exited = false;
            let result: anyhow::Result<Option<Output>> = async {
                let mut catalog = cx
                    .background_executor()
                    .spawn(async move { separation::Catalog::discover(&models_root) })
                    .await?;
                if let Some(index) = previous_model.as_deref().and_then(|id| {
                    catalog.models.iter().position(|model| model.id() == id)
                }) {
                    catalog.models[..=index].rotate_right(1);
                }
                let mut choices: Vec<_> = catalog
                    .models
                    .iter()
                    .map(|model| PromptButton::new(model.name.clone()))
                    .collect();
                choices.push(PromptButton::cancel("取消"));
                if captions {
                    this.update(cx, |this, cx| {
                        this.subtitle_progress = Some((key.clone(), "请选择人声分离模型".into()));
                        cx.notify();
                    })?;
                }
                let selection = cx.update(|window, cx| {
                    window.prompt(
                        PromptLevel::Info,
                        "选择人声分离模型",
                        Some(if captions {
                            "伴奏固定使用 Inst HQ 3。识别完成后会替换这首曲目的音轨和已有字幕。"
                        } else {
                            "伴奏固定使用 Inst HQ 3。重新分离会替换这首曲目的两条音轨。"
                        }),
                        &choices,
                        cx,
                    )
                })?;
                let Ok(index) = selection.await else {
                    return Ok(None);
                };
                if index >= catalog.models.len() {
                    return Ok(None);
                }
                let model = catalog.models.remove(index);
                let target = if let Some(target) = model.target {
                    target
                } else {
                    let selection = cx.update(|window, cx| {
                        window.prompt(
                            PromptLevel::Info,
                            &format!("「{}」的主输出是哪一轨？", model.name),
                            Some("选择伴奏时，人声取该模型输出的残差轨。"),
                            &[
                                PromptButton::new("人声"),
                                PromptButton::new("伴奏"),
                                PromptButton::cancel("取消"),
                            ],
                            cx,
                        )
                    })?;
                    match selection.await {
                        Ok(0) => separation::Target::Vocals,
                        Ok(1) => separation::Target::Instrumental,
                        _ => return Ok(None),
                    }
                };
                if captions {
                    let detail = format!(
                        "人声：{}。伴奏：{}。识别仅使用分离后的人声，生成结果会替换这首曲目的已有字幕和分离音轨。",
                        model.name,
                        separation::ACCOMPANIMENT,
                    );
                    let confirm = cx.update(|window, cx| {
                        window.prompt(
                            PromptLevel::Info,
                            &format!("生成《{title}》的字幕"),
                            Some(&detail),
                            &[
                                PromptButton::ok("开始分离并识别"),
                                PromptButton::cancel("取消"),
                            ],
                            cx,
                        )
                    })?;
                    if confirm.await != Ok(0) {
                        return Ok(None);
                    }
                    this.update(cx, |this, _| {
                        let model = model.id();
                        if this.library.prefs.separation_model.as_deref() != Some(model) {
                            this.update_settings(|prefs| prefs.separation_model = Some(model.into()));
                        }
                    })?;
                }
                let request = separation::Request {
                    subtitles,
                    key: key.clone(),
                    source,
                    model,
                    target,
                    accompaniment: catalog.accompaniment,
                };
                if let Some(session) = session.clone() {
                    let (sender, mut stages) =
                        tokio::sync::mpsc::unbounded_channel::<subtitle_generation::Stage>();
                    let task = cx.background_executor().spawn(async move {
                        let result =
                            subtitle_generation::generate(request, song, &session, |stage| {
                                let _ = sender.send(stage);
                            });
                        (result, !session.is_running())
                    });
                    while let Some(stage) = stages.recv().await {
                        let _ = this.update(cx, |this, cx| {
                            let message = format!("《{title}》{}", stage.label());
                            this.subtitle_progress = Some((key.clone(), message.clone()));
                            this.notice = message;
                            cx.notify();
                        });
                    }
                    let (result, exited) = task.await;
                    asr_exited = exited;
                    if result.is_ok() {
                        let _ = this.update(cx, |this, cx| {
                            this.subtitle_progress =
                                Some((key.clone(), subtitle_generation::Stage::Saving.label()));
                            cx.notify();
                        });
                    }
                    result.map(Output::Subtitles).map(Some)
                } else {
                    this.update(cx, |this, cx| {
                        this.notice = format!("正在分离《{title}》的人声与伴奏");
                        cx.notify();
                    })?;
                    cx.background_executor()
                        .spawn(async move { request.generate().map(Output::Stems).map(Some) })
                        .await
                }
            }
            .await;
            let result: anyhow::Result<Option<(String, bool)>> = async {
                let Some(output) = result? else {
                    return Ok(None);
                };
                let current = this
                    .update(cx, |this, _| this.library.song(&key).cloned())?
                    .ok_or_else(|| anyhow::anyhow!("曲目已移出曲库，请重新选择曲目"))?;
                cx.background_executor()
                    .spawn(async move {
                        let output = match output {
                            Output::Stems(prepared) => {
                                prepared.publish(Path::new(&current.path))?;
                                (format!("已生成《{}》的人声与伴奏", current.title), false)
                            }
                            Output::Subtitles(generated) => {
                                let count = generated.publish(&current)?;
                                (
                                    format!("已生成《{}》字幕，共 {count} 段", current.title),
                                    true,
                                )
                            }
                        };
                        Ok(Some(output))
                    })
                    .await
            }
            .await;
            let _ = this.update(cx, |this, cx| {
                this.separating.remove(&key);
                if captions {
                    this.subtitle_progress = None;
                }
                if asr_exited
                    && this
                        .asr_session
                        .as_ref()
                        .zip(session.as_ref())
                        .is_some_and(|(current, task)| Arc::ptr_eq(current, task))
                {
                    this.release_asr_session(cx);
                }
                match result {
                    Ok(Some((message, captions_saved))) => {
                        if captions_saved {
                            this.subtitle_feedback = Some((key.clone(), message.clone()));
                        }
                        this.notice = message;
                        if captions_saved {
                            this.subtitle_catalog.refresh = true;
                            if this
                                .track_lyrics
                                .as_ref()
                                .is_some_and(|preview| preview.key == key)
                            {
                                this.track_lyrics_task = None;
                                this.track_lyrics = None;
                                this.poll_track_lyrics(cx);
                            }
                        }
                    }
                    Err(error) => {
                        this.notice = format!("{error:#}");
                        if captions {
                            this.subtitle_feedback = Some((key, this.notice.clone()));
                        }
                    }
                    Ok(None) => {
                        if captions {
                            this.subtitle_feedback = Some((key, "已取消字幕生成".into()));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn subtitle_page(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if let Some(editor) = &self.subtitle_catalog.editor {
            return self.segment_editor_page(editor, cx);
        }
        let state = &self.subtitle_catalog;
        let selected = state.generation_key.as_deref();
        let track = selected.and_then(|key| self.library.song(key));
        let can_select = self.subtitle_progress.is_none() && !self.editing_subtitle;
        let can_generate = can_select
            && !self.asr_busy()
            && self.asr_session.is_some()
            && track.is_some_and(|song| {
                !self.separating.contains(&song.key)
                    && !self.api_subtitle_writes.contains(&song.key)
            });
        let status = self.subtitle_progress.as_ref().or_else(|| {
            self.subtitle_feedback
                .as_ref()
                .filter(|(key, _)| selected == Some(key.as_str()))
        });
        let pending = state
            .pending
            .iter()
            .filter_map(|key| {
                let song = self.library.song(key)?;
                let select_key = key.clone();
                let remove_key = key.clone();
                Some(
                    div()
                        .id(SharedString::from(format!("pending-subtitle-{key}")))
                        .p_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .border_b_1()
                        .border_color(theme_color(LINE))
                        .when(selected == Some(key.as_str()), |row| {
                            row.bg(theme_color(PANEL_LIGHT))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(div().truncate().child(song.title.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme_color(MUTED))
                                        .child("待生成"),
                                ),
                        )
                        .when(can_select, |row| {
                            row.child(self.tiny_button(
                                "选择",
                                "select-pending-subtitle",
                                cx,
                                move |this, window, cx| {
                                    this.open_subtitle_generation(select_key.clone(), window, cx);
                                },
                            ))
                            .child(self.tiny_button(
                                "移除",
                                "remove-pending-subtitle",
                                cx,
                                move |this, _, cx| {
                                    let catalog = &mut this.subtitle_catalog;
                                    catalog.pending.retain(|item| item != &remove_key);
                                    if catalog.generation_key.as_ref() == Some(&remove_key) {
                                        catalog.generation_key = None;
                                    }
                                    cx.notify();
                                },
                            ))
                        }),
                )
            })
            .collect::<Vec<_>>();
        let data = match &state.records {
            None => div().p_4().child("正在读取字幕记录").into_any_element(),
            Some(Err(message)) => div().p_4().child(message.clone()).into_any_element(),
            Some(Ok(records)) if records.is_empty() => div()
                .p_4()
                .text_color(theme_color(MUTED))
                .child("暂无已生成字幕")
                .into_any_element(),
            Some(Ok(records)) => {
                let rows = records.iter().map(|record| {
                    let title = record.title.as_deref().unwrap_or(&record.key);
                    let record_status = if self.library.song(&record.key).is_some() {
                        "已生成"
                    } else {
                        "已生成 · 曲目已移出曲库"
                    };
                    let preview_key = record.key.clone();
                    let editor_key = record.key.clone();
                    let editor_title = title.to_owned();
                    let export_key = record.key.clone();
                    let delete_key = record.key.clone();
                    let export_title = title.to_owned();
                    let row = div()
                        .p_3()
                        .border_b_1()
                        .border_color(theme_color(LINE))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(div().truncate().child(title.to_owned()))
                                .child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .text_color(theme_color(MUTED))
                                        .child(record_status),
                                ),
                        )
                        .child(self.tiny_button(
                            "预览",
                            "preview-subtitle",
                            cx,
                            move |this, _, cx| {
                                if this.subtitle_progress.is_none() {
                                    this.subtitle_catalog.generation_key =
                                        Some(preview_key.clone());
                                    this.preview_subtitle_record(preview_key.clone(), cx);
                                }
                            },
                        ))
                        .child(self.tiny_button(
                            "分段编辑",
                            "edit-subtitle-segments",
                            cx,
                            move |this, _, cx| {
                                this.open_segment_editor(
                                    editor_key.clone(),
                                    editor_title.clone(),
                                    cx,
                                )
                            },
                        ))
                        .child(self.tiny_button(
                            "导出",
                            "export-subtitle",
                            cx,
                            move |this, _, cx| {
                                this.export_subtitle_record(export_key.clone(), &export_title, cx)
                            },
                        ))
                        .child(self.tiny_button(
                            "删除",
                            "delete-subtitle",
                            cx,
                            move |this, window, cx| {
                                this.remove_subtitle_record(delete_key.clone(), window, cx)
                            },
                        ));
                    div()
                        .id(SharedString::from(record.key.clone()))
                        .when(selected == Some(record.key.as_str()), |row| {
                            row.bg(theme_color(PANEL_LIGHT))
                        })
                        .child(row)
                        .when(
                            selected == Some(record.key.as_str())
                                && state.preview_key.as_deref() == selected,
                            |row| {
                                let raw = match &self.subtitle_catalog.preview {
                                    Some(Ok(text)) | Some(Err(text)) => text.clone(),
                                    None => "正在读取字幕".into(),
                                };
                                row.when(cfg!(target_os = "macos"), |row| {
                                    row.child(
                                        div().px_3().pt_2().flex().gap_2().children(
                                            [
                                                (SeparatedAudio::Vocals, "打开人声", "open-vocals"),
                                                (
                                                    SeparatedAudio::Accompaniment,
                                                    "打开伴奏",
                                                    "open-accompaniment",
                                                ),
                                            ]
                                            .into_iter()
                                            .map(
                                                |(kind, label, id)| {
                                                    let key = record.key.clone();
                                                    self.tiny_button(
                                                        label,
                                                        id,
                                                        cx,
                                                        move |this, _, cx| {
                                                            this.open_separated_audio(
                                                                key.clone(),
                                                                kind,
                                                                cx,
                                                            )
                                                        },
                                                    )
                                                },
                                            ),
                                        ),
                                    )
                                })
                                .child(
                                    div()
                                        .px_3()
                                        .pt_2()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme_color(MUTED))
                                                .child("在访达显示"),
                                        )
                                        .children(
                                            [
                                                (
                                                    SubtitleDelivery::Audio(SeparatedAudio::Vocals),
                                                    "人声",
                                                    "reveal-vocals",
                                                ),
                                                (
                                                    SubtitleDelivery::Audio(
                                                        SeparatedAudio::Accompaniment,
                                                    ),
                                                    "伴奏",
                                                    "reveal-accompaniment",
                                                ),
                                                (
                                                    SubtitleDelivery::Text(SubtitleFormat::Srt),
                                                    "SRT",
                                                    "reveal-srt",
                                                ),
                                                (
                                                    SubtitleDelivery::Text(SubtitleFormat::Lrc),
                                                    "LRC",
                                                    "reveal-lrc",
                                                ),
                                            ]
                                            .into_iter()
                                            .map(
                                                |(delivery, label, id)| {
                                                    let key = record.key.clone();
                                                    self.tiny_button(
                                                        label,
                                                        id,
                                                        cx,
                                                        move |this, _, cx| {
                                                            this.reveal_subtitle_delivery(
                                                                key.clone(),
                                                                delivery,
                                                                cx,
                                                            );
                                                        },
                                                    )
                                                },
                                            ),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("subtitle-record-preview")
                                        .ml_3()
                                        .mr_3()
                                        .mt_1()
                                        .max_h(px(260.))
                                        .overflow_y_scroll()
                                        .p_3()
                                        .bg(theme_color(BG))
                                        .text_xs()
                                        .child(raw),
                                )
                            },
                        )
                });
                div().children(rows).into_any_element()
            }
        };
        div()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .px_5()
            .child(
                div()
                    .py_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("字幕生成"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .when(can_select, |row| {
                                row.child(self.tiny_button(
                                    "选择曲目",
                                    "generate-selected-subtitle",
                                    cx,
                                    |this, window, cx| {
                                        this.open_song_picker(
                                            PickerPurpose::AddSubtitleTrack,
                                            window,
                                            cx,
                                        )
                                    },
                                ))
                            })
                            .child(self.tiny_button(
                                "刷新",
                                "refresh-subtitles",
                                cx,
                                |this, _, cx| this.reload_subtitle_catalog(cx),
                            )),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .mb_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme_color(LINE))
                    .bg(theme_color(PANEL))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .child(
                                        div().truncate().font_weight(FontWeight::SEMIBOLD).child(
                                            track
                                                .map_or(
                                                    "请选择要生成字幕的曲目",
                                                    |song| song.title.as_str(),
                                                )
                                                .to_owned(),
                                        ),
                                    )
                                    .child(div().text_xs().text_color(theme_color(MUTED)).child(
                                        if self.asr_session.is_some() {
                                            "ASR 模型已加载 · 仅识别人声轨"
                                        } else if self.asr_loading.is_some() {
                                            "正在加载 ASR 模型…"
                                        } else {
                                            "请先加载 ASR 模型"
                                        },
                                    )),
                            )
                            .when_some(track.filter(|_| can_generate), |row, song| {
                                let key = song.key.clone();
                                row.child(self.tiny_button(
                                    "选择模型并生成字幕",
                                    "confirm-generate-subtitle",
                                    cx,
                                    move |this, window, cx| {
                                        this.process_track_audio(key.clone(), true, window, cx);
                                    },
                                ))
                            })
                            .when(self.asr_session.is_none(), |row| {
                                row.child(self.tiny_button(
                                    "打开模型设置",
                                    "subtitle-model-settings",
                                    cx,
                                    |this, window, cx| {
                                        this.open_settings(window, cx);
                                        this.settings_page = Some(SettingsPage::Models);
                                        this.scan_asr_models(cx, false);
                                    },
                                ))
                            }),
                    )
                    .when_some(status, |row, (_, message)| {
                        row.child(
                            div()
                                .pt_2()
                                .text_sm()
                                .text_color(theme_color(TEXT))
                                .child(message.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .id("subtitle-records")
                    .overflow_y_scroll()
                    .min_h(px(0.))
                    .flex_1()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme_color(LINE))
                    .bg(theme_color(PANEL))
                    .children(pending)
                    .child(data),
            )
            .into_any_element()
    }

    fn track_lyrics_panel(&self, key: &str, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.track_lyrics.as_ref().filter(|state| state.key == key);
        let Some((lyrics, timeline)) = state.and_then(|state| match &state.result {
            Some(Ok(Some(timeline))) if timeline.len() > 0 => Some((state, timeline)),
            _ => None,
        }) else {
            let message = match state.and_then(|state| state.result.as_ref()) {
                Some(Ok(_)) => "这首歌还没有歌词",
                Some(Err(message)) => message.as_str(),
                None => "正在读取歌词…",
            };
            return div()
                .w_full()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .items_center()
                .text_sm()
                .text_color(fade(gpui::white(), 0.66))
                .child(message.to_owned())
                .into_any_element();
        };
        let selected = self.library.subtitle_format();
        let controls = div()
            .flex()
            .items_center()
            .gap_2()
            .children(
                [SubtitleFormat::Srt, SubtitleFormat::Lrc]
                    .into_iter()
                    .map(|format| {
                        div()
                            .id(format.extension())
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .text_xs()
                            .cursor_pointer()
                            .bg(fade(
                                gpui::white(),
                                if format == selected { 0.23 } else { 0.08 },
                            ))
                            .hover(|style| style.bg(fade(gpui::white(), 0.23)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Err(error) = this.library.set_subtitle_format(format) {
                                    this.notice = error;
                                }
                                cx.notify();
                            }))
                            .child(format.extension().to_ascii_uppercase())
                    }),
            )
            .when(cfg!(target_os = "macos"), |row| {
                row.child(
                    div()
                        .id("subtitle-edit")
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .text_xs()
                        .when(!self.editing_subtitle, |button| {
                            button
                                .cursor_pointer()
                                .hover(|style| style.bg(fade(gpui::white(), 0.18)))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.edit_track_subtitle(window, cx)
                                }))
                        })
                        .child("编辑"),
                )
            })
            .child(
                div()
                    .id("subtitle-export")
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .text_xs()
                    .cursor_pointer()
                    .hover(|style| style.bg(fade(gpui::white(), 0.18)))
                    .on_click(cx.listener(|this, _, _, cx| this.export_track_subtitle(cx)))
                    .child("导出"),
            )
            .when(lyrics.browsed_at.is_some(), |row| {
                row.child(
                    div()
                        .id("follow-current-lyric")
                        .ml_auto()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .text_xs()
                        .cursor_pointer()
                        .bg(fade(gpui::white(), 0.12))
                        .hover(|style| style.bg(fade(gpui::white(), 0.22)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(lyrics) = &mut this.track_lyrics {
                                lyrics.browsed_at = None;
                                lyrics.scroll_anchor = None;
                                lyrics.follow_playing_line(Instant::now());
                                cx.notify();
                            }
                        }))
                        .child("回到当前"),
                )
            });

        let timeline = timeline.clone();
        let active = timeline.active_line(lyrics.clock.position(Instant::now()));
        let key = key.to_owned();
        let workspace = cx.weak_entity();
        let rows = list(lyrics.scroll.clone(), move |index, _, _| {
            let (start, text) = timeline.line(index);
            let sung = active
                .filter(|(current, _)| *current == index)
                .map(|(_, sung)| sung);
            let highlight = HighlightStyle {
                color: Some(rgb(0xf8c078).into()),
                ..Default::default()
            };
            let text = StyledText::new(text).with_highlights(
                sung.filter(|&bytes| bytes > 0)
                    .map(|bytes| (0..bytes, highlight)),
            );
            let workspace = workspace.clone();
            let key = key.clone();
            div()
                .id(("playing-lyric-line", index))
                .w_full()
                .min_w(px(0.))
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .text_size(px(20.))
                .line_height(px(27.))
                .text_color(if sung.is_some() {
                    gpui::white()
                } else {
                    fade(gpui::white(), 0.52)
                })
                .when(sung.is_some(), |row| {
                    row.font_weight(FontWeight::SEMIBOLD)
                        .bg(fade(gpui::white(), 0.09))
                })
                .hover(|row| row.bg(fade(gpui::white(), 0.14)))
                .on_click(move |_, _, cx| {
                    let _ = workspace.update(cx, |this, cx| {
                        if this.loading.is_none()
                            && this.current_key().as_deref() == Some(key.as_str())
                        {
                            if let Some(lyrics) = &mut this.track_lyrics {
                                lyrics.browsed_at = None;
                                lyrics.scroll_anchor = None;
                            }
                            let position = start.clamp(0.0, this.duration.as_secs_f64());
                            this.seek_to(Duration::from_secs_f64(position));
                            this.poll_track_lyrics(cx);
                            cx.notify();
                        }
                    });
                })
                .child(text)
                .into_any_element()
        })
        .w_full()
        .flex_1()
        .min_h(px(0.));

        div()
            .w_full()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .gap_2()
            .child(rows)
            .child(controls.flex_none())
            .into_any_element()
    }

    fn now_playing_page(&self, window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(song) = self.current_song() else {
            return div().into_any_element();
        };
        let category = self
            .library
            .snapshot
            .categories
            .iter()
            .find(|cat| cat.id == song.category);
        let name = category.map_or(song.category.as_str(), |cat| cat.name_zh.as_str());
        let symbol = category.map_or("music", |cat| category_icon(&cat.icon));
        let tint = category.map_or(theme_color(ACCENT), |cat| parse_color(&cat.color));
        let size = window.viewport_size();
        let diameter = (f32::from(size.width) * 0.32)
            .min(f32::from(size.height) * 0.52)
            .clamp(280., 480.);
        let duration = if self.duration.is_zero() {
            song.duration_ms
                .map(|ms| Duration::from_millis(u64::from(ms)))
        } else {
            Some(self.duration)
        };
        let key = song.key.clone();
        let show_details = self.track_details_key.as_ref() == Some(&key);
        let tabs = div()
            .flex()
            .gap_2()
            .children([(false, "歌词"), (true, "详情")].map(|(details, label)| {
                let key = key.clone();
                div()
                    .id(label)
                    .px_3()
                    .py_1()
                    .rounded_full()
                    .text_size(px(12.))
                    .cursor_pointer()
                    .text_color(if details == show_details {
                        gpui::white()
                    } else {
                        fade(gpui::white(), 0.66)
                    })
                    .when(details == show_details, |tab| {
                        tab.bg(fade(gpui::white(), 0.15))
                    })
                    .hover(|tab| tab.bg(fade(gpui::white(), 0.21)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.track_details_key = details.then(|| key.clone());
                        if !details {
                            this.poll_track_lyrics(cx);
                        }
                        cx.notify();
                    }))
                    .child(label)
            }));
        let subtitle_message = self
            .subtitle_progress
            .as_ref()
            .into_iter()
            .chain(self.subtitle_feedback.as_ref())
            .find(|(track, _)| track == &key)
            .map(|(_, message)| message);

        let mut metadata = div().flex().flex_col().gap_3();
        for (label, value) in [
            ("艺人", song.artist.clone()),
            ("专辑", song.album.clone()),
            ("分类", Some(name.to_owned())),
            ("时长", duration.map(format_time)),
            ("文件", Some(song.file_name.clone())),
        ] {
            if let Some(value) = value.filter(|text| !text.trim().is_empty()) {
                metadata = metadata.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(
                            div()
                                .w(px(38.))
                                .flex_none()
                                .text_size(px(12.))
                                .text_color(rgb(0xc1c6cf))
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(div().w_full().truncate().text_size(px(14.)).child(value)),
                        ),
                );
            }
        }
        let back = div()
            .id("close-track-page")
            .px_3()
            .h(px(34.))
            .rounded_full()
            .cursor_pointer()
            .flex()
            .items_center()
            .gap_2()
            .text_size(px(13.))
            .bg(fade(gpui::white(), 0.1))
            .hover(|style| style.bg(fade(gpui::white(), 0.18)))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_track_page(window, cx)))
            .child(icon("chevron-down", 16., gpui::white()))
            .child("收起播放页");
        div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .flex()
            .flex_col()
            .text_color(gpui::white())
            .bg(tint)
            .when_some(crate::assets::sleeve_path(&song.category), |page, path| {
                page.child(
                    img(path)
                        .absolute()
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                )
            })
            .child(div().absolute().inset_0().bg(linear_gradient(
                104.,
                gpui::rgba(0x131822ae),
                gpui::rgba(0x101524f9),
            )))
            .child(
                div()
                    .relative()
                    .pl(px(92.))
                    .pr_6()
                    .h(px(64.))
                    .flex_none()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(back)
                    .child(
                        div()
                            .h_full()
                            .flex_1()
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.start_window_move()
                            }),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .max_w(px(1300.))
                    .mx_auto()
                    .pb(px(44.))
                    .px(px(48.))
                    .flex()
                    .items_center()
                    .gap(px(40.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(record(diameter, &song.category, symbol, tint)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .pt_2()
                            .flex()
                            .flex_col()
                            .items_start()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(12.))
                                    .text_color(rgb(0xe2e4ea))
                                    .child(icon("list-music", 15., rgb(0xe2e4ea)))
                                    .child(format!(
                                        "播放队列   {} / {}",
                                        self.queue_index + 1,
                                        self.queue.len()
                                    )),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .w_full()
                                    .flex_none()
                                    .text_size(px(if size.width < px(1130.) { 32. } else { 40. }))
                                    .line_height(px(if size.width < px(1130.) { 42. } else { 51. }))
                                    .line_clamp(2)
                                    .text_ellipsis()
                                    .font_weight(FontWeight::BOLD)
                                    .child(song.title.clone()),
                            )
                            .when_some(song.artist.clone(), |column, artist| {
                                column.child(
                                    div()
                                        .w_full()
                                        .flex_none()
                                        .truncate()
                                        .text_sm()
                                        .text_color(rgb(0xc1c6cf))
                                        .child(artist),
                                )
                            })
                            .child(div().mt_4().mb_3().flex_none().child(tabs))
                            .child(
                                div()
                                    .w_full()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .flex()
                                    .flex_col()
                                    .border_t_1()
                                    .border_color(fade(gpui::white(), 0.2))
                                    .when(show_details, |body| body.pt_4().child(metadata))
                                    .when(!show_details, |body| {
                                        body.pt_2().child(self.track_lyrics_panel(&key, cx))
                                    }),
                            )
                            .child(
                                div()
                                    .mt_3()
                                    .flex_none()
                                    .flex()
                                    .gap_3()
                                    .child(
                                        detail_button("track-diagnostics", "sliders", "音频诊断")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.diagnostics_open = true;
                                                this.queue_open = false;
                                                cx.notify();
                                            })),
                                    )
                                    .child({
                                        let key = song.key.clone();
                                        detail_button(
                                            "track-generate-subtitle",
                                            "music",
                                            "生成字幕",
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, window, cx| {
                                                this.open_subtitle_generation(
                                                    key.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }),
                                        )
                                    })
                                    .child(
                                        detail_button("track-loop", "repeat-1", "A/B 循环")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.open_loop_editor(&key, window, cx)
                                            })),
                                    ),
                            )
                            .when_some(subtitle_message, |page, message| {
                                page.child(
                                    div()
                                        .mt_2()
                                        .max_w(px(440.))
                                        .text_sm()
                                        .text_color(rgb(0xe2e4ea))
                                        .child(message.clone()),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn player_bar(
        &mut self,
        window: &mut Window,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let title = self.current_title();
        let has_track = self.current_song().is_some();
        let track_page_open = self.now_playing_open;
        let current = self.current_song();
        let category = current.and_then(|song| {
            self.library
                .snapshot
                .categories
                .iter()
                .find(|category| category.id == song.category)
        });
        let cover = match current {
            Some(song) => sleeve(
                &song.category,
                category.map_or("music", |c| category_icon(&c.icon)),
                category.map_or(theme_color(ACCENT), |c| parse_color(&c.color)),
                20.,
                7.,
            ),
            None => icon("disc", 24., theme_color(FAINT)).into_any_element(),
        };
        let subtitle = if self.loading.is_some() {
            "正在加载音频…".into()
        } else {
            current
                .and_then(|song| song.artist.clone())
                .or_else(|| category.map(|c| c.name_zh.clone()))
                .unwrap_or_else(|| "从曲库选择一首开始".into())
        };
        let ratio = if self.duration.is_zero() {
            0.0
        } else {
            (self.position.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
        };
        let volume = self.output_volume();
        let (mode_icon, mode_name) = match self.library.prefs.play_mode {
            PlayMode::Sequence => ("list-ordered", "顺序播放"),
            PlayMode::RepeatOne => ("repeat-1", "单曲循环"),
            PlayMode::Shuffle => ("shuffle", "随机播放"),
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

        let loop_region = self.loop_region_for(self.current_key().as_deref());
        let font = Font {
            family: self.interface_font(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight::MEDIUM,
            style: FontStyle::Normal,
        };
        let run = TextRun {
            len: title.len(),
            font,
            color: theme_color(TEXT).into(),
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
            .h(px(PLAYER_HEIGHT))
            .px_5()
            .flex()
            .items_center()
            .gap_5()
            .bg(fade(theme_color(PANEL), panel.max(0.86)))
            .border_t_1()
            .border_color(theme_color(LINE))
            .child(
                div()
                    .id("player-track")
                    .w(px(if narrow { 202. } else { 240. }))
                    .min_w(px(0.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded_md()
                    .when(has_track, |entry| {
                        entry
                            .cursor_pointer()
                            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.05)))
                            .tooltip(move |_, cx| {
                                cx.new(|_| {
                                    Tip(if track_page_open {
                                        "收起播放页  ⌘↵"
                                    } else {
                                        "打开播放页  ⌘↵"
                                    }
                                    .into())
                                })
                                .into()
                            })
                            .on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.toggle_track_page(window, cx)
                                }),
                            )
                    })
                    .child(
                        div()
                            .size(px(48.))
                            .flex_none()
                            .rounded(px(7.))
                            .bg(theme_color(SIDE))
                            .overflow_hidden()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(cover),
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
                            .child(div().h(px(18.)).min_w(px(0.)).overflow_hidden().child(
                                if let Some(marquee) = marquee {
                                    div()
                                        .whitespace_nowrap()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme_color(TEXT))
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
                                            theme_color(TEXT)
                                        } else {
                                            theme_color(MUTED)
                                        })
                                        .text_ellipsis()
                                        .child(title)
                                        .into_any_element()
                                },
                            ))
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(11.))
                                    .text_color(theme_color(FAINT))
                                    .text_ellipsis()
                                    .child(subtitle),
                            ),
                    )
                    .when(has_track, |entry| {
                        entry.child(icon(
                            if track_page_open {
                                "chevron-down"
                            } else {
                                "chevron-up"
                            },
                            15.,
                            theme_color(FAINT),
                        ))
                    }),
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
                                "player-prev",
                                "skip-back",
                                false,
                                cx,
                                |this, _, cx| {
                                    if !this.guard_library_playback() {
                                        this.previous();
                                    }
                                    cx.notify();
                                },
                            ))
                            .child(
                                div()
                                    .id("player-play")
                                    .tooltip(move |_, cx| {
                                        cx.new(|_| {
                                            Tip(if playing {
                                                "暂停".into()
                                            } else {
                                                "播放".into()
                                            })
                                        })
                                        .into()
                                    })
                                    .size(px(38.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(theme_color(ACCENT))
                                    .shadow_sm()
                                    .hover(|style| style.opacity(0.9))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.play_pause(&PlayPause, window, cx);
                                    }))
                                    .child(icon(
                                        if playing { "pause-fill" } else { "play-fill" },
                                        15.,
                                        theme_color(ACCENT_INK),
                                    )),
                            )
                            .child(self.player_icon_button(
                                "player-next",
                                "skip-forward",
                                false,
                                cx,
                                |this, _, cx| {
                                    if !this.guard_library_playback() {
                                        this.failed_tracks.clear();
                                        this.advance(TrackStart::Selected);
                                    }
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
                                    .text_color(theme_color(FAINT))
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
                                            .bg(theme_color(LINE))
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
                                                    .bg(fade(theme_color(FAVORITE), 0.65))
                                            }))
                                            .child(
                                                div()
                                                    .h_full()
                                                    .w(relative(ratio))
                                                    .rounded_full()
                                                    .bg(if has_track {
                                                        theme_color(ACCENT)
                                                    } else {
                                                        theme_color(FAINT)
                                                    }),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(38.))
                                    .text_size(px(10.))
                                    .text_color(theme_color(FAINT))
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
                    .child(self.player_icon_button(
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
                            .w(px(if narrow { 48. } else { 74. }))
                            .h(px(14.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::volume_down_at))
                            .on_mouse_move(cx.listener(Self::volume_move))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::volume_up_at))
                            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::volume_up_at))
                            .child(
                                div()
                                    .h(px(3.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(theme_color(LINE))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(volume))
                                            .rounded_full()
                                            .bg(theme_color(MUTED)),
                                    ),
                            ),
                    )
                    .child(self.player_icon_button(
                        "player-queue",
                        "list-music",
                        self.queue_open,
                        cx,
                        |this, _, cx| {
                            this.queue_open = !this.queue_open;
                            this.diagnostics_open = false;
                            cx.notify();
                        },
                    ))
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
                            .relative()
                            .child(
                                div()
                                    .id("player-mode")
                                    .size(px(32.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.08)))
                                    .tooltip(move |_, cx| cx.new(|_| Tip(mode_name.into())).into())
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.cycle_play_mode(cx)),
                                    )
                                    .child(icon(
                                        mode_icon,
                                        17.,
                                        if mode_icon == "list-ordered" {
                                            theme_color(MUTED)
                                        } else {
                                            theme_color(ACCENT)
                                        },
                                    )),
                            )
                            .when(self.mode_feedback.is_some(), |control| {
                                control.child(
                                    div()
                                        .absolute()
                                        .right_0()
                                        .bottom(px(66.))
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .bg(theme_color(TEXT))
                                        .text_color(theme_color(PANEL))
                                        .text_xs()
                                        .whitespace_nowrap()
                                        .child(mode_name),
                                )
                            }),
                    ),
            )
    }

    fn cycle_play_mode(&mut self, cx: &mut Context<Self>) {
        let next = match self.library.prefs.play_mode {
            PlayMode::Sequence => PlayMode::RepeatOne,
            PlayMode::RepeatOne => PlayMode::Shuffle,
            PlayMode::Shuffle => PlayMode::Sequence,
        };
        match self.library.set_mode(next) {
            Ok(()) => {
                self.mode_feedback = Some(cx.spawn(async |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(950))
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.mode_feedback = None;
                        cx.notify();
                    });
                }));
            }
            Err(error) => self.notice = error,
        }
        cx.notify();
    }

    pub fn install_desktop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.main_window = Some(window.window_handle());
        cx.observe_window_appearance(window, |_, _, cx| cx.notify())
            .detach();
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| match this.library.prefs.close_action {
                CloseAction::Quit => cx.quit(),
                CloseAction::Tray if this.tray.is_some() => this.hide_to_tray(cx),
                _ => {
                    this.close_prompt = true;
                    this.remember_close = false;
                    window.focus(&this.focus);
                    cx.notify();
                }
            })
            .is_err()
        });
        if self.library.prefs.minimize_to_tray
            && let Err(error) = self.ensure_tray(window, cx)
        {
            self.notice = match self.library.update_playback_settings(|settings| {
                crate::settings::set_minimize_to_tray(settings, false);
            }) {
                Ok(()) => format!("{error}，已关闭菜单栏隐藏功能"),
                Err(save_error) => {
                    crate::settings::set_minimize_to_tray(&mut self.library.prefs, false);
                    format!("{error}，{save_error}")
                }
            };
        }
    }

    fn ensure_tray(&mut self, window: &Window, cx: &mut Context<Self>) -> Result<(), String> {
        if self.tray.is_none() {
            let context = window.to_async(cx);
            let weak = cx.entity().downgrade();
            let minimize_context = context.clone();
            let quit_context = context.clone();
            let minimize_view = weak.clone();
            let executor = cx.foreground_executor().clone();
            self.tray = Some(Tray::new(
                window,
                move || {
                    context
                        .clone()
                        .update(|window, cx| {
                            let _ = weak.update(cx, |_, cx| {
                                cx.activate(true);
                                window.activate_window();
                                cx.notify();
                            });
                        })
                        .ok();
                },
                move || {
                    let mut context = minimize_context.clone();
                    let weak = minimize_view.clone();
                    executor
                        .spawn(async move {
                            let _ = context.update(|_, cx| {
                                weak.update(cx, |this, cx| {
                                    if this.library.prefs.minimize_to_tray {
                                        this.hide_to_tray(cx);
                                    }
                                })
                                .ok();
                            });
                        })
                        .detach();
                },
                move || {
                    let _ = quit_context.clone().update(|_, cx| cx.quit());
                },
            )?);
        }
        Ok(())
    }

    fn hide_to_tray(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.library.flush_preferences() {
            self.notice = error;
        }
        self.wake_lock = None;
        self.wake_lock_attempted = false;
        cx.hide();
    }

    fn update_wake_lock(&mut self, window_visible: impl FnOnce() -> bool) {
        let keep_awake = self.playing && self.library.prefs.prevent_sleep && window_visible();
        if !keep_awake {
            self.wake_lock = None;
            self.wake_lock_attempted = false;
        } else if !self.wake_lock_attempted {
            self.wake_lock_attempted = true;
            match WakeLock::acquire() {
                Ok(lock) => self.wake_lock = Some(lock),
                Err(error) if self.settings_page.is_some() => self.settings_feedback = Some(error),
                Err(error) => self.notice = error,
            }
        }
    }

    fn close_layer(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.close_prompt {
            return None;
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .bg(fade(theme_color(SCRIM), 0.26))
                .flex()
                .justify_center()
                .items_center()
                .child(
                    div()
                        .w(px(390.))
                        .p_5()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme_color(LINE))
                        .bg(theme_color(PANEL))
                        .shadow_lg()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("关闭听风吟"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme_color(MUTED))
                                .child("最小化到菜单栏可以继续播放。"),
                        )
                        .child(self.setting_toggle(
                            "close-remember",
                            self.remember_close,
                            cx,
                            |this, _, cx| {
                                this.remember_close = !this.remember_close;
                                cx.notify();
                            },
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(self.tiny_button(
                                    "取消",
                                    "close-cancel",
                                    cx,
                                    |this, _, cx| {
                                        this.close_prompt = false;
                                        cx.notify();
                                    },
                                ))
                                .child(self.tiny_button(
                                    "退出应用",
                                    "close-quit",
                                    cx,
                                    |this, _, cx| {
                                        if this.remember_close
                                            && let Err(error) =
                                                this.library.update_playback_settings(|settings| {
                                                    settings.close_action = CloseAction::Quit;
                                                })
                                        {
                                            this.notice = error;
                                        }
                                        cx.quit();
                                    },
                                ))
                                .child(self.tiny_button(
                                    "最小化到菜单栏",
                                    "close-tray",
                                    cx,
                                    |this, window, cx| {
                                        let close_action =
                                            this.remember_close.then_some(CloseAction::Tray);
                                        match this.set_tray_settings(true, close_action, window, cx)
                                        {
                                            Ok(()) => this.hide_to_tray(cx),
                                            Err(error) => this.notice = error,
                                        }
                                        this.close_prompt = false;
                                        cx.notify();
                                    },
                                )),
                        ),
                )
                .into_any_element(),
        )
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.on_next_frame(window, |this, window, _| window.focus(&this.settings_focus));
        if self.fonts.is_empty() {
            self.fonts = desktop::system_fonts(cx);
        }
        self.settings_feedback = None;
        self.settings_page = Some(SettingsPage::General);
        self.wallpaper_popover = false;
        self.export_popover = false;
        self.queue_open = false;
        self.diagnostics_open = false;
        self.api_token_field
            .update(cx, |field, cx| field.set_text("", cx));
        let origins = self.api_config.cors_origins.join(", ");
        self.api_cors_field
            .update(cx, |field, cx| field.set_text(origins, cx));
        let download_url = self.library.asr_model_preferences().download_url;
        self.asr_url_field
            .update(cx, |field, cx| field.set_text(download_url, cx));
        cx.notify();
    }

    fn update_settings(&mut self, update: impl FnOnce(&mut bgm_core::Prefs)) {
        let error = self.library.update_playback_settings(update).err();
        if self.settings_page.is_some() {
            self.settings_feedback = error;
        } else if let Some(error) = error {
            self.notice = error;
        }
    }

    fn settings_layer(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let page = self.settings_page?;
        self.settings_slider_bounds.clear();
        let body = match page {
            SettingsPage::General => self.general_settings(cx),
            SettingsPage::Playback => self.playback_settings(cx),
            SettingsPage::Models => self.model_settings(cx),
            SettingsPage::Integrations => self.integration_settings(cx),
        };
        let pages = [
            (SettingsPage::General, "general", "常规"),
            (SettingsPage::Playback, "playback", "播放"),
            (SettingsPage::Models, "models", "模型"),
            (SettingsPage::Integrations, "integrations", "第三方接入"),
        ];
        let tabs = pages
            .into_iter()
            .map(|(target, id, label)| {
                div()
                    .id(SharedString::from(format!("settings-tab-{id}")))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(if page == target {
                        theme_color(PANEL_LIGHT)
                    } else {
                        theme_color(PANEL)
                    })
                    .text_color(if page == target {
                        theme_color(ACCENT)
                    } else {
                        theme_color(MUTED)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if target == SettingsPage::Playback {
                            this.refresh_audio_outputs();
                        }
                        window.focus(&this.settings_focus);
                        this.settings_page = Some(target);
                        this.font_picker_open = false;
                        this.settings_feedback = None;
                        this.settings_slider_drag = None;
                        if target == SettingsPage::Models {
                            this.scan_asr_models(cx, false);
                        }
                        cx.notify();
                    }))
                    .child(label)
            })
            .collect::<Vec<_>>();
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .key_context("Dialog")
                .track_focus(&self.settings_focus)
                .bg(fade(theme_color(SCRIM), 0.26))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(690.))
                        .h(px(515.))
                        .max_w(relative(0.94))
                        .max_h(relative(0.9))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .rounded_lg()
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(theme_color(LINE))
                        .shadow_lg()
                        .child(
                            div()
                                .px_5()
                                .pt_4()
                                .pb_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .text_lg()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child("设置"),
                                )
                                .child(self.tiny_button(
                                    "关闭",
                                    "settings-close",
                                    cx,
                                    |this, window, cx| {
                                        this.settings_page = None;
                                        this.settings_slider_drag = None;
                                        this.font_picker_open = false;
                                        window.focus(&this.focus);
                                        cx.notify();
                                    },
                                )),
                        )
                        .child(div().px_5().pb_3().flex().gap_2().children(tabs))
                        .child(
                            div()
                                .id(("settings-scroll", page as usize))
                                .px_5()
                                .pb_4()
                                .flex_1()
                                .min_h(px(0.))
                                .border_t_1()
                                .border_color(theme_color(LINE))
                                .overflow_y_scroll()
                                .child(body),
                        )
                        .when_some(self.settings_feedback.clone(), |panel, feedback| {
                            panel.child(
                                div()
                                    .px_5()
                                    .py_2()
                                    .border_t_1()
                                    .border_color(theme_color(LINE))
                                    .text_xs()
                                    .child(feedback),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn scan_asr_models(&mut self, cx: &mut Context<Self>, report_check: bool) {
        let preferences = self.library.asr_model_preferences();
        self.asr_scan_generation = self.asr_scan_generation.wrapping_add(1);
        let generation = self.asr_scan_generation;
        self.asr_catalog = None;
        if report_check {
            self.settings_feedback = Some("正在按清单核对模型文件名称和大小…".into());
        }
        let task = cx
            .background_executor()
            .spawn(async move { ModelCatalog::scan(&preferences) });
        cx.spawn(async move |this, cx| {
            let catalog = task.await;
            this.update(cx, |this, cx| {
                if this.asr_scan_generation == generation {
                    if report_check && this.settings_page == Some(SettingsPage::Models) {
                        let files = &catalog.default_files;
                        this.settings_feedback = Some(if files.missing.is_empty() {
                            format!("检查完成：{} 个文件的名称和大小均符合清单", files.count)
                        } else {
                            format!("缺失或大小不符：{}", files.missing.join("、"))
                        });
                    }
                    this.asr_catalog = Some(catalog);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn choose_asr_directory(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择 ASR 模型目录".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.first()
            {
                this.update(cx, |this, cx| this.apply_asr_directory(Some(path), cx))
                    .ok();
            }
        })
        .detach();
    }

    fn apply_asr_directory(&mut self, path: Option<&Path>, cx: &mut Context<Self>) {
        if self.asr_busy() {
            return;
        }
        let previous = self.library.asr_model_preferences().root();
        match self.library.set_asr_model_directory(path) {
            Ok(()) => {
                if previous != self.library.asr_model_preferences().root() {
                    self.release_asr_session(cx);
                    self.asr_load_error = None;
                }
                self.settings_feedback = None;
                self.scan_asr_models(cx, false);
            }
            Err(error) => self.settings_feedback = Some(error),
        }
        cx.notify();
    }

    fn open_asr_directory(&mut self, cx: &mut Context<Self>) {
        let root = self.library.asr_model_preferences().root();
        let task = cx.background_executor().spawn(async move {
            std::fs::create_dir_all(&root)
                .map(|()| root)
                .map_err(|error| format!("无法打开模型目录：{error}"))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(root) => cx.reveal_path(&root),
                    Err(error) => this.settings_feedback = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn save_asr_url(&mut self, url: &str, cx: &mut Context<Self>) {
        self.settings_feedback = Some(match self.library.set_asr_download_url(url) {
            Ok(()) => {
                self.asr_url_field
                    .update(cx, |field, cx| field.set_text(url, cx));
                "已保存模型下载地址".into()
            }
            Err(error) => error,
        });
        cx.notify();
    }

    fn confirm_model_redownload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.asr_busy() {
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "重新下载 Qwen3-ASR 模型？",
            Some("将重新获取全部九个文件（约 941 MiB）。每个文件下载并校验成功后替换本地副本。"),
            &[PromptButton::new("重新下载"), PromptButton::cancel("取消")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if matches!(answer.await, Ok(0)) {
                let _ = this.update(cx, |this, cx| {
                    this.start_model_download(DownloadMode::Replace, cx)
                });
            }
        })
        .detach();
    }

    fn start_model_download(&mut self, mode: DownloadMode, cx: &mut Context<Self>) {
        if self.asr_busy() {
            return;
        }
        let download = ModelDownload::default();
        let job = download.job(self.library.asr_model_preferences(), mode);
        self.asr_download = Some(download);
        self.asr_download_progress = None;
        self.settings_feedback = None;
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |this, cx| {
                this.asr_download = None;
                this.asr_download_progress = None;
                let downloaded = result.is_ok();
                let message = result
                    .err()
                    .unwrap_or_else(|| "Qwen3-ASR 模型文件已下载".into());
                if this.settings_page == Some(SettingsPage::Models) {
                    this.settings_feedback = Some(message);
                } else {
                    this.notice = message;
                }
                this.scan_asr_models(cx, false);
                if downloaded {
                    if mode == DownloadMode::Replace
                        && this
                            .asr_session
                            .as_ref()
                            .is_some_and(|session| session.id == DEFAULT_MODEL_NAME)
                    {
                        this.release_asr_session(cx);
                    }
                    this.load_asr_model(DEFAULT_MODEL_NAME.into(), cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn poll_model_download(&mut self) -> bool {
        if self.settings_page != Some(SettingsPage::Models) {
            return false;
        }
        let next = self.asr_download.as_ref().and_then(ModelDownload::progress);
        let changed = self.asr_download_progress != next;
        self.asr_download_progress = next;
        changed
    }

    fn asr_busy(&self) -> bool {
        self.asr_download.is_some()
            || self.asr_loading.is_some()
            || self.asr_transcribing
            || self.subtitle_progress.is_some()
    }

    fn load_asr_model(&mut self, model: String, cx: &mut Context<Self>) {
        if self.asr_busy() {
            return;
        }
        self.start_asr_load(self.library.asr_model_preferences().root(), model, None, cx);
    }

    fn start_asr_load(
        &mut self,
        root: PathBuf,
        model: String,
        task: Option<api::RunningTask>,
        cx: &mut Context<Self>,
    ) {
        self.refresh_asr_session(cx);
        if self
            .asr_session
            .as_ref()
            .is_some_and(|session| session.id == model)
        {
            if let Some(task) = task {
                task.finish(Ok(
                    json!({"loaded":api::tasks::loaded_model(&root, &model)}),
                ));
            }
            return;
        }
        self.asr_loading = Some(model.clone());
        self.asr_load_error = None;
        self.asr_message = None;
        if let Some(task) = &task {
            task.progress("loading", "正在加载 ASR 模型…".into(), 0, 0);
        }
        let path = root.clone();
        let worker_model = model.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    std::panic::catch_unwind(|| AsrSession::load(&path, worker_model).map(Arc::new))
                        .unwrap_or_else(|_| Err("模型加载提前终止".into()))
                })
                .await;
            let result = this
                .update(cx, |this, cx| {
                    this.asr_loading = None;
                    let result = result.and_then(|session| {
                        if this.library.asr_model_preferences().last_model.as_deref()
                            != Some(&session.id)
                            && let Err(error) = this.library.set_last_asr_model(Some(&session.id))
                        {
                            cx.background_executor()
                                .spawn(async move { drop(session) })
                                .detach();
                            return Err(error);
                        }
                        this.release_asr_session(cx);
                        this.asr_session = Some(session);
                        if cfg!(target_os = "macos") && this.library.desktop_lyrics_enabled() {
                            this.open_desktop_lyrics(cx);
                        }
                        Ok(())
                    });
                    this.asr_load_error = result.as_ref().err().cloned();
                    this.asr_message = this.asr_load_error.clone();
                    cx.notify();
                    result
                })
                .unwrap_or_else(|_| Err("界面不可用".into()));
            if let Some(task) = task {
                task.finish(
                    result.map(|()| json!({"loaded":api::tasks::loaded_model(&root, &model)})),
                );
            }
        })
        .detach();
        cx.notify();
    }

    fn refresh_asr_session(&mut self, cx: &mut Context<Self>) {
        if self
            .asr_session
            .as_ref()
            .is_some_and(|session| !session.is_running())
        {
            self.release_asr_session(cx);
            self.asr_load_error = Some("ASR 进程已退出，请重新加载模型".into());
            self.asr_message.clone_from(&self.asr_load_error);
        }
    }

    fn release_asr_session(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.asr_session.take() {
            cx.background_executor()
                .spawn(async move { drop(session) })
                .detach();
        }
        self.asr_text = None;
    }

    fn unload_asr_model(&mut self, cx: &mut Context<Self>) {
        if !self.asr_busy() {
            match self.library.set_last_asr_model(None) {
                Ok(()) => {
                    self.release_asr_session(cx);
                    self.close_desktop_lyrics(false, cx);
                    self.asr_message = None;
                    self.asr_load_error = None;
                }
                Err(error) => self.asr_message = Some(error),
            }
            cx.notify();
        }
    }

    fn choose_asr_wav(&mut self, cx: &mut Context<Self>) {
        if self.asr_busy() || self.asr_session.is_none() {
            return;
        }
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 WAV 音频".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = selection.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.transcribe_asr_wav(path, cx));
            }
        })
        .detach();
    }

    fn transcribe_asr_wav(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.asr_busy() {
            return;
        }
        let Some(session) = self.asr_session.clone() else {
            return;
        };
        if !path
            .extension()
            .is_some_and(|format| format.eq_ignore_ascii_case("wav"))
        {
            self.asr_message = Some("请选择 WAV 音频文件".into());
            cx.notify();
            return;
        }
        self.asr_transcribing = true;
        self.asr_message = None;
        self.asr_text = None;
        cx.spawn(async move |this, cx| {
            let (result, running) = cx
                .background_executor()
                .spawn(async move {
                    let result = session.transcribe(path).map(|segments| {
                        segments
                            .into_iter()
                            .map(|segment| segment.text)
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                    (result, session.is_running())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.asr_transcribing = false;
                match result {
                    Ok(text) => this.asr_text = Some(text),
                    Err(error) => this.asr_message = Some(error),
                }
                if !running {
                    this.release_asr_session(cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn default_model_row(&self, cx: &Context<Self>) -> gpui::AnyElement {
        let files = self
            .asr_catalog
            .as_ref()
            .map(|catalog| &catalog.default_files);
        if let Some(download) = &self.asr_download {
            let progress = self.asr_download_progress.as_ref();
            let (detail, percent) = progress.map_or(("正在准备下载".into(), 0.), |progress| {
                let mib = |bytes| bytes as f64 / 1_048_576.;
                let detail = format!(
                    "{}/{}  {}  ·  {:.1} / {:.1} MiB",
                    progress.index,
                    progress.count,
                    progress.file,
                    mib(progress.received),
                    mib(progress.total)
                );
                (
                    detail,
                    (progress.received as f32 / progress.total.max(1) as f32).clamp(0., 1.),
                )
            });
            let cancel = self.settings_button(
                "asr-download-cancel",
                if download.cancelling() {
                    "正在取消"
                } else {
                    "取消下载"
                },
                false,
                !download.cancelling(),
                cx,
                |this, _, cx| {
                    if let Some(download) = &this.asr_download {
                        download.cancel();
                    }
                    cx.notify();
                },
            );
            return div()
                .flex()
                .flex_col()
                .child(settings_row("Qwen3-ASR 0.6B-int8", &detail, cancel))
                .child(
                    div()
                        .h(px(3.))
                        .rounded_full()
                        .overflow_hidden()
                        .bg(theme_color(LINE))
                        .child(div().h_full().w(relative(percent)).bg(theme_color(ACCENT))),
                )
                .into_any_element();
        }
        let status = files.map_or_else(
            || "正在检查模型文件".into(),
            |files| {
                format!(
                    "{} / {} 个文件名称及大小符合清单 · {:.0} MiB",
                    files.complete,
                    files.count,
                    files.expected_bytes as f64 / 1_048_576.
                )
            },
        );
        let idle = files.is_some() && !self.asr_busy();
        let has_files = files.is_some_and(|files| files.complete > 0);
        let incomplete = files.is_some_and(|files| files.complete < files.count);
        settings_row(
            "Qwen3-ASR 0.6B-int8",
            &status,
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(self.settings_button(
                    "asr-check-files",
                    "检查完整性",
                    false,
                    idle,
                    cx,
                    |this, _, cx| this.scan_asr_models(cx, true),
                ))
                .when(incomplete, |actions| {
                    actions.child(self.settings_button(
                        "asr-download",
                        if has_files {
                            "补齐下载"
                        } else {
                            "下载模型"
                        },
                        false,
                        idle,
                        cx,
                        |this, _, cx| this.start_model_download(DownloadMode::Resume, cx),
                    ))
                })
                .when(has_files, |actions| {
                    actions.child(self.settings_button(
                        "asr-redownload",
                        "重新下载",
                        false,
                        idle,
                        cx,
                        |this, window, cx| this.confirm_model_redownload(window, cx),
                    ))
                }),
        )
    }

    fn model_settings(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let idle = !self.asr_busy();
        let path = self.asr_catalog.as_ref().map(|catalog| {
            let root = catalog.root.display();
            if catalog.is_default_root {
                format!("默认 · {root}")
            } else {
                root.to_string()
            }
        });
        let controls = div()
            .flex()
            .gap_2()
            .child(self.settings_button(
                "asr-folder-choose",
                "选择目录",
                false,
                idle,
                cx,
                |this, _, cx| this.choose_asr_directory(cx),
            ))
            .child(self.settings_button(
                "asr-folder-reset",
                "恢复默认",
                false,
                idle,
                cx,
                |this, _, cx| this.apply_asr_directory(None, cx),
            ))
            .child(self.settings_button(
                "asr-folder-open",
                "在 Finder 中显示",
                false,
                true,
                cx,
                |this, _, cx| this.open_asr_directory(cx),
            ));
        let url_field = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(245.))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme_color(BG))
                    .border_1()
                    .border_color(theme_color(LINE))
                    .child(self.asr_url_field.clone()),
            )
            .child(self.settings_button(
                "asr-url-save",
                "保存",
                false,
                idle,
                cx,
                |this, _, cx| {
                    let url = this.asr_url_field.read(cx).text().to_owned();
                    this.save_asr_url(&url, cx);
                },
            ))
            .child(self.settings_button(
                "asr-url-reset",
                "恢复默认",
                false,
                idle,
                cx,
                |this, _, cx| this.save_asr_url(DEFAULT_ASR_DOWNLOAD_URL, cx),
            ));
        let scan_status = match &self.asr_catalog {
            None => "正在扫描模型目录".into(),
            Some(catalog) if catalog.models.is_empty() && catalog.warnings.is_empty() => {
                "未找到 ONNX 模型文件".into()
            }
            Some(catalog) => format!("发现 {} 组 ONNX 模型文件", catalog.models.len()),
        };
        let rows = self
            .asr_catalog
            .iter()
            .flat_map(|catalog| catalog.models.iter())
            .map(|model| {
                let active = self
                    .asr_session
                    .as_ref()
                    .is_some_and(|session| session.id == model.id);
                let id = model.id.clone();
                let status = match &model.missing_components {
                    None => "根目录单文件，无法加载".into(),
                    Some(_) if active => "已加载".into(),
                    Some(missing) if missing.is_empty() => "组件齐全".into(),
                    Some(missing) => format!("缺少 {}", missing.join("、")),
                };
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme_color(LINE))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(model.id.clone()),
                    )
                    .child(div().text_xs().text_color(theme_color(MUTED)).child(status))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child(model.files.join(" · ")),
                    )
                    .when(
                        model.missing_components.as_ref().is_some_and(Vec::is_empty) && !active,
                        |row| {
                            row.child(div().flex().child(self.settings_button(
                                format!("asr-load-{id}"),
                                "加载",
                                false,
                                idle,
                                cx,
                                move |this, _, cx| this.load_asr_model(id.clone(), cx),
                            )))
                        },
                    )
            });
        let runtime = match &self.asr_catalog {
            None => "正在查找 sherpa-onnx 2.0.2",
            Some(catalog) if catalog.runtime_error.is_some() => {
                "未找到 sherpa-onnx 2.0.2。应用包需要随附推理库才能加载模型。"
            }
            Some(_) => "已找到库文件，加载模型时校验版本与依赖",
        };
        let loaded = self.asr_session.as_ref();
        let remembered = self.library.asr_model_preferences().last_model.is_some();
        let engine_detail = if let Some(model) = &self.asr_loading {
            format!("正在加载 {model}")
        } else if self.asr_transcribing {
            "正在识别 WAV".into()
        } else if let Some(session) = loaded {
            format!("已加载 {}", session.id)
        } else {
            "尚未加载模型".into()
        };
        div()
            .flex()
            .flex_col()
            .px_5()
            .pb_4()
            .child(settings_row(
                "模型目录",
                path.as_deref().unwrap_or("正在读取目录"),
                controls,
            ))
            .child(settings_row(
                "下载地址",
                "ModelScope 仓库或模型镜像目录",
                url_field,
            ))
            .child(self.default_model_row(cx))
            .child(settings_row(
                "推理库",
                runtime,
                self.settings_button(
                    "asr-runtime-release",
                    "获取推理库",
                    false,
                    true,
                    cx,
                    |_, _, cx| {
                        cx.open_url("https://github.com/k2-fsa/sherpa-onnx/releases/tag/v2.0.2")
                    },
                ),
            ))
            .child(settings_row(
                "ASR 推理",
                &engine_detail,
                div()
                    .flex()
                    .gap_2()
                    .child(self.settings_button(
                        "asr-transcribe",
                        "选择 WAV 识别",
                        false,
                        idle && loaded.is_some(),
                        cx,
                        |this, _, cx| this.choose_asr_wav(cx),
                    ))
                    .child(self.settings_button(
                        "asr-unload",
                        if loaded.is_some() {
                            "释放模型"
                        } else {
                            "清除启动恢复"
                        },
                        false,
                        idle && (loaded.is_some() || remembered),
                        cx,
                        |this, _, cx| this.unload_asr_model(cx),
                    )),
            ))
            .when_some(self.asr_message.clone(), |body, message| {
                body.child(
                    div()
                        .py_2()
                        .text_xs()
                        .text_color(theme_color(MUTED))
                        .child(message),
                )
            })
            .when_some(self.asr_text.clone(), |body, text| {
                body.child(
                    div()
                        .id("asr-result")
                        .max_h(px(160.))
                        .overflow_y_scroll()
                        .p_3()
                        .bg(theme_color(BG))
                        .rounded_md()
                        .child(text),
                )
            })
            .child(settings_row(
                "本地模型",
                &scan_status,
                self.settings_button(
                    "asr-scan",
                    "重新扫描",
                    false,
                    true,
                    cx,
                    |this, _, cx| this.scan_asr_models(cx, false),
                ),
            ))
            .children(
                self.asr_catalog
                    .iter()
                    .flat_map(|catalog| catalog.warnings.iter())
                    .map(|warning| {
                        div()
                            .py_2()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child(warning.clone())
                    }),
            )
            .children(rows)
            .into_any_element()
    }

    fn general_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::settings::Appearance;

        let settings = self.library.prefs.clone();
        let font_name = if settings.ui_font == "system" {
            "macOS 系统字体"
        } else {
            self.fonts
                .iter()
                .find(|font| font.family == settings.ui_font)
                .map_or(settings.ui_font.as_str(), |font| font.label.as_str())
        };
        let font_button = format!("{font_name}  ▾");
        let appearance = [
            (Appearance::System, "跟随系统", "appearance-system"),
            (Appearance::Light, "浅色", "appearance-light"),
            (Appearance::Dark, "深色", "appearance-dark"),
        ]
        .into_iter()
        .map(|(value, label, id)| {
            self.settings_button(
                id,
                label,
                self.library.appearance == value,
                true,
                cx,
                move |this, _, cx| {
                    this.settings_feedback = this.library.set_appearance(value).err();
                    cx.notify();
                },
            )
        })
        .collect::<Vec<_>>();
        let closing = [
            (CloseAction::Ask, "每次询问", "closing-ask"),
            (CloseAction::Quit, "退出应用", "closing-quit"),
            (CloseAction::Tray, "隐藏到菜单栏", "closing-tray"),
        ]
        .into_iter()
        .map(|(value, label, id)| {
            self.settings_button(
                id,
                label,
                settings.close_action == value,
                true,
                cx,
                move |this, window, cx| this.set_close_action(value, window, cx),
            )
        })
        .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .child(settings_row(
                "外观",
                "跟随 macOS 或固定使用一种外观",
                div().flex().gap_1().children(appearance),
            ))
            .child(settings_row(
                "界面字体",
                "应用到曲库、播放器和设置",
                self.settings_button(
                    "choose-font",
                    &font_button,
                    self.font_picker_open,
                    true,
                    cx,
                    |this, window, cx| {
                        this.font_picker_open = !this.font_picker_open;
                        if this.font_picker_open {
                            this.font_filter
                                .update(cx, |field, cx| field.set_text("", cx));
                            let filter = this.font_filter.clone();
                            cx.on_next_frame(window, move |_, window, cx| {
                                window.focus(&filter.focus_handle(cx))
                            });
                        } else {
                            window.focus(&this.settings_focus);
                        }
                        cx.notify();
                    },
                ),
            ))
            .when(self.font_picker_open, |body| {
                body.child(self.font_picker(cx))
            })
            .child(settings_row(
                "登录时启动",
                "登录 macOS 后打开听风吟",
                self.setting_toggle("autostart", settings.autostart, cx, |this, _, cx| {
                    let previous = this.library.prefs.autostart;
                    let enabled = !previous;
                    let result = desktop::set_autostart(enabled).and_then(|()| {
                        this.library
                            .update_playback_settings(|settings| settings.autostart = enabled)
                            .map_err(|error| match desktop::set_autostart(previous) {
                                Ok(()) => error,
                                Err(rollback) => format!("{error}，恢复原启动项失败：{rollback}"),
                            })
                    });
                    this.settings_feedback = result.err();
                    cx.notify();
                }),
            ))
            .child(settings_row(
                "在菜单栏显示图标",
                "点击图标恢复窗口，右键可显示或退出",
                self.setting_toggle("tray", settings.minimize_to_tray, cx, |this, window, cx| {
                    let enable = !this.library.prefs.minimize_to_tray;
                    this.settings_feedback = this.set_tray_settings(enable, None, window, cx).err();
                    cx.notify();
                }),
            ))
            .child(settings_row(
                "关闭窗口时",
                "选择点击窗口关闭按钮后的行为",
                div().flex().gap_1().children(closing),
            ))
            .child(settings_row(
                "图形渲染",
                "macOS 由 GPUI 和系统管理 GPU 加速",
                div(),
            ))
            .child(settings_row(
                "配置文件",
                "在访达定位应用和曲库分类配置",
                div()
                    .flex()
                    .gap_2()
                    .child(
                        self.tiny_button("应用配置", "reveal-config", cx, |this, _, cx| {
                            cx.reveal_path(this.library.config_path());
                        }),
                    )
                    .child(self.tiny_button(
                        "分类配置",
                        "reveal-categories",
                        cx,
                        |this, _, cx| {
                            cx.reveal_path(this.library.categories_path());
                        },
                    )),
            ))
            .into_any_element()
    }

    fn set_close_action(&mut self, value: CloseAction, window: &Window, cx: &mut Context<Self>) {
        let enable = self.library.prefs.minimize_to_tray || value == CloseAction::Tray;
        self.settings_feedback = self
            .set_tray_settings(enable, Some(value), window, cx)
            .err();
        cx.notify();
    }

    fn set_tray_settings(
        &mut self,
        enabled: bool,
        close_action: Option<CloseAction>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if enabled {
            self.ensure_tray(window, cx)?;
        }
        let result = self.library.update_playback_settings(|settings| {
            if let Some(action) = close_action {
                settings.close_action = action;
            }
            crate::settings::set_minimize_to_tray(settings, enabled);
        });
        if !self.library.prefs.minimize_to_tray {
            self.tray = None;
        }
        result
    }

    fn font_picker(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let query = self.font_filter.read(cx).text().trim().to_lowercase();
        let mut options: Vec<(String, String)> = Vec::new();
        if query.is_empty() || "系统默认 macos 系统字体".contains(&query) {
            options.push(("system".into(), "系统默认 · macOS 系统字体".into()));
        }
        options.extend(
            self.fonts
                .iter()
                .filter(|font| {
                    font.label.to_lowercase().contains(&query)
                        || font.family.to_lowercase().contains(&query)
                })
                .map(|font| (font.family.clone(), font.label.clone())),
        );
        let count = options.len();
        let selected = self.library.prefs.ui_font.clone();
        let list = uniform_list(
            "system-fonts",
            count,
            cx.processor(move |_, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let (key, label) = options.get(index)?.clone();
                        let active = key == selected;
                        Some(
                            div()
                                .id(("ui-font", index))
                                .h(px(33.))
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .cursor_pointer()
                                .when(active, |row| {
                                    row.bg(theme_color(PANEL_LIGHT))
                                        .text_color(theme_color(ACCENT))
                                })
                                .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.09)))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.update_settings(|settings| settings.ui_font = key.clone());
                                    this.font_picker_open = false;
                                    window.focus(&this.settings_focus);
                                    cx.notify();
                                }))
                                .child(div().flex_1().min_w(px(0.)).truncate().child(label))
                                .when(active, |row| {
                                    row.child(icon("check", 14., theme_color(ACCENT)))
                                }),
                        )
                    })
                    .collect()
            }),
        );
        div()
            .w_full()
            .p_2()
            .my_2()
            .rounded_md()
            .border_1()
            .border_color(theme_color(LINE))
            .bg(theme_color(BG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .mb_1()
                    .child(icon("search", 14., theme_color(FAINT)))
                    .child(div().flex_1().min_w(px(0.)).child(self.font_filter.clone())),
            )
            .when(count == 0, |panel| {
                panel.child(
                    div()
                        .px_2()
                        .py_3()
                        .text_xs()
                        .text_color(theme_color(FAINT))
                        .child("没有匹配的字体"),
                )
            })
            .when(count > 0, |panel| {
                panel.child(list.h(px((count.min(5) * 33) as f32)))
            })
            .into_any_element()
    }

    fn settings_button(
        &self,
        id: impl Into<SharedString>,
        label: &str,
        selected: bool,
        available: bool,
        cx: &Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        div()
            .id(id.into())
            .h(px(30.))
            .max_w(px(235.))
            .px_2()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_xs()
            .bg(if selected {
                theme_color(PANEL_LIGHT)
            } else {
                theme_color(BG)
            })
            .text_color(if selected {
                theme_color(ACCENT)
            } else {
                theme_color(MUTED)
            })
            .border_1()
            .border_color(if selected {
                fade(theme_color(ACCENT), 0.3)
            } else {
                theme_color(LINE).into()
            })
            .when(available, |button| {
                button
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .focus(|style| style.border_color(theme_color(ACCENT)))
                    .hover(|style| style.bg(theme_color(PANEL_LIGHT)))
                    .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx)))
            })
            .when(!available, |button| button.opacity(0.5))
            .child(div().min_w(px(0.)).truncate().child(label.to_owned()))
            .into_any_element()
    }

    fn playback_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::settings::MAX_OUTPUT_ROUTES;
        let settings = self.library.prefs.clone();
        let count = settings.output_routes.len();
        let available = self
            .output_devices
            .iter()
            .filter(|device| {
                !settings
                    .output_routes
                    .iter()
                    .any(|route| route.device_id == device.device_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        let available_count = available.len();
        let route_rows = settings
            .output_routes
            .iter()
            .map(|route| self.output_row(route, cx))
            .collect::<Vec<_>>();
        let available_rows = available.into_iter().map(|device| {
            let label = device.label.clone();
            settings_row(&label, if count == MAX_OUTPUT_ROUTES { "已达到输出上限" } else { "" },
                self.settings_button(format!("output-add-{}", device.device_id), "添加", false, count < MAX_OUTPUT_ROUTES, cx,
                    move |this,_,cx| this.change_output(json!({"deviceId": device.device_id, "label": device.label, "enabled": true}), cx)))
        }).collect::<Vec<_>>();
        let crossfade = self.settings_slider(
            "crossfade",
            settings.crossfade_seconds,
            12.,
            0.5,
            true,
            cx,
            |value| {
                if value == 0. {
                    "关闭".into()
                } else {
                    format!("{value:.1} 秒")
                }
            },
            |this, value, cx| {
                this.settings_feedback = this.library.set_crossfade(value).err();
                cx.notify();
            },
        );
        div()
            .flex()
            .flex_col()
            .child(settings_row(
                "启动时自动播放",
                "打开听风吟后开始播放",
                self.setting_toggle("autoplay", settings.autoplay_on_start, cx, |this, _, cx| {
                    this.update_settings(|settings| {
                        settings.autoplay_on_start = !settings.autoplay_on_start
                    });
                    cx.notify();
                }),
            ))
            .child(settings_row(
                "播放时保持屏幕唤醒",
                "播放且窗口可见时保持唤醒，隐藏后释放",
                self.setting_toggle("wake-lock", settings.prevent_sleep, cx, |this, window, cx| {
                    this.update_settings(|settings| {
                        settings.prevent_sleep = !settings.prevent_sleep
                    });
                    this.update_wake_lock(|| desktop::window_visible(window));
                    cx.notify();
                }),
            ))
            .child(settings_row(
                "平台专属播放功能",
                "Windows 支持媒体控件、空间音效和音频增强，Linux 支持 MPRIS2。此 macOS 版本不提供这些功能。",
                div(),
            ))
            .child(settings_row(
                "桌面歌词",
                "加载 ASR 模型后可启用独立歌词窗口",
                div().flex().items_center().gap_2()
                    .when(self.desktop_lyrics.overlay.is_some(), |row| row.child(
                        self.tiny_button("样式", "desktop-lyrics-style", cx, |this, _, cx| this.toggle_lyrics_settings(cx)),
                    ))
                    .child(self.setting_toggle(
                        "desktop-lyrics", self.desktop_lyrics.overlay.is_some(), cx,
                        |this, _, cx| this.toggle_desktop_lyrics(cx),
                    )),
            ))
            .child(settings_row(
                "交叉淡入淡出",
                "设置切歌时两首曲目重叠的时间，0–12 秒",
                crossfade,
            ))
            .child(
                div()
                    .pt_4()
                    .pb_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("已配置输出 · {count}/{MAX_OUTPUT_ROUTES}")),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme_color(MUTED))
                                    .child("启用的设备可以同时播放，每路独立调整音量。"),
                            ),
                    )
                    .child(self.settings_button(
                        "refresh-devices",
                        "刷新设备",
                        false,
                        true,
                        cx,
                        |this, _, cx| {
                            let _ = this.apply_api("outputs.refresh", &Value::Null);
                            cx.notify();
                        },
                    )),
            )
            .children(self.output_route_warnings.iter().map(|warning| {
                div()
                    .py_1()
                    .text_xs()
                    .text_color(theme_color(WARNING))
                    .child(warning.clone())
            }))
            .children(route_rows)
            .child(
                div()
                    .pt_4()
                    .pb_1()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("可添加设备 · {available_count}")),
            )
            .when(available_count == 0, |panel| {
                panel.child(
                    div()
                        .py_2()
                        .text_xs()
                        .text_color(theme_color(MUTED))
                        .child("没有其他可添加的输出设备"),
                )
            })
            .children(available_rows)
            .into_any_element()
    }

    fn change_output(&mut self, payload: Value, cx: &mut Context<Self>) {
        self.settings_feedback = None;
        if let Err((_, error)) = self.apply_api("outputs.set", &payload) {
            self.settings_feedback = Some(error);
        }
        cx.notify();
    }

    fn output_row(
        &self,
        route: &crate::settings::OutputRoute,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let key = &route.device_id;
        let device = self
            .output_devices
            .iter()
            .find(|device| device.device_id == *key);
        let default_output = key == "default";
        let connected = device.is_some();
        let name = device.map_or(route.label.as_str(), |device| device.label.as_str());
        let status = match (default_output, connected, route.enabled, route.muted) {
            (true, _, true, false) => "已启用 · 跟随 macOS 声音设置",
            (true, _, true, true) => "已静音 · 跟随 macOS 声音设置",
            (true, _, false, _) => "已停用 · 跟随 macOS 声音设置",
            (_, false, true, _) => "离线 · 重新连接后自动恢复",
            (_, false, false, _) => "离线 · 已停用",
            (_, true, false, _) => "已停用",
            (_, true, true, true) => "已启用 · 已静音",
            _ => "已启用",
        };
        let volume_id = key.clone();
        let mute_id = key.clone();
        let enable_id = key.clone();
        let remove_id = key.clone();
        let muted = route.muted;
        let enabled = route.enabled;
        let controls = div()
            .flex()
            .items_center()
            .gap_2()
            .child(self.settings_slider(
                &format!("output-volume-{key}"),
                route.volume,
                1.,
                0.01,
                connected && enabled && !muted,
                cx,
                |value| format!("{:.0}%", value * 100.),
                move |this, value, cx| {
                    this.change_output(json!({"deviceId":volume_id,"volume":value}), cx)
                },
            ))
            .child(self.settings_button(
                format!("output-mute-{key}"),
                if muted { "取消静音" } else { "静音" },
                muted,
                connected && enabled,
                cx,
                move |this, _, cx| {
                    this.change_output(json!({"deviceId":mute_id,"muted":!muted}), cx)
                },
            ))
            .child(self.settings_button(
                format!("output-enabled-{key}"),
                if enabled { "停用" } else { "启用" },
                false,
                connected || enabled,
                cx,
                move |this, _, cx| {
                    this.change_output(json!({"deviceId":enable_id,"enabled":!enabled}), cx)
                },
            ))
            .when(!default_output, |controls| {
                controls.child(self.settings_button(
                    format!("output-remove-{key}"),
                    "移除",
                    false,
                    true,
                    cx,
                    move |this, _, cx| {
                        let mut routes = this.library.prefs.output_routes.clone();
                        routes.retain(|route| route.device_id != remove_id);
                        this.change_output(json!({"routes":routes}), cx);
                    },
                ))
            });
        settings_row(name, status, controls)
    }

    fn settings_slider(
        &self,
        id: &str,
        value: f32,
        max: f32,
        step: f32,
        enabled: bool,
        cx: &Context<Self>,
        label: impl Fn(f32) -> String,
        commit: impl Fn(&mut Self, f32, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let id: SharedString = id.to_owned().into();
        let mut value = value;
        if let Some((active, preview)) = &self.settings_slider_drag
            && active == &id
        {
            value = *preview;
        }
        let bounds_id = id.clone();
        let down_id = id.clone();
        let move_id = id.clone();
        let up_id = id.clone();
        let outside_id = id.clone();
        let weak = cx.entity().downgrade();
        let commit = std::rc::Rc::new(commit);
        let outside_commit = commit.clone();
        let key_commit = commit.clone();
        div()
            .flex()
            .items_center()
            .gap_2()
            .opacity(if enabled { 1. } else { 0.5 })
            .child(
                div()
                    .on_children_prepainted(move |bounds, _, cx| {
                        if let Some(track) = bounds.first() {
                            let _ = weak.update(cx, |this, _| {
                                this.settings_slider_bounds
                                    .insert(bounds_id.clone(), *track);
                            });
                        }
                    })
                    .id(id)
                    .w(px(102.))
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .rounded_md()
                    .when(enabled, |slider| {
                        slider
                            .cursor_pointer()
                            .focusable()
                            .tab_stop(true)
                            .focus(|style| style.border_1().border_color(theme_color(ACCENT)))
                            .on_key_down(cx.listener(
                                move |this, event: &gpui::KeyDownEvent, _, cx| {
                                    let next = match event.keystroke.key.as_str() {
                                        "left" | "down" => value - step,
                                        "right" | "up" => value + step,
                                        "home" => 0.,
                                        "end" => max,
                                        _ => return,
                                    };
                                    key_commit(this, next.clamp(0., max), cx);
                                    cx.stop_propagation();
                                },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.preview_settings_slider(
                                        &down_id,
                                        event.position,
                                        max,
                                        step,
                                    );
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(
                                move |this, event: &MouseMoveEvent, _, cx| {
                                    if this
                                        .settings_slider_drag
                                        .as_ref()
                                        .is_some_and(|(id, _)| id == &move_id)
                                    {
                                        this.preview_settings_slider(
                                            &move_id,
                                            event.position,
                                            max,
                                            step,
                                        );
                                        cx.notify();
                                    }
                                },
                            ))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(next) = this.finish_settings_slider(&up_id) {
                                        commit(this, next, cx);
                                    }
                                }),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    if let Some(next) = this.finish_settings_slider(&outside_id) {
                                        outside_commit(this, next, cx);
                                    }
                                }),
                            )
                    })
                    .child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .relative()
                            .rounded_full()
                            .bg(theme_color(LINE))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(value / max))
                                    .rounded_full()
                                    .bg(theme_color(ACCENT)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(relative(value / max))
                                    .ml(px(-5.))
                                    .top(px(-3.))
                                    .size(px(10.))
                                    .rounded_full()
                                    .bg(theme_color(ACCENT)),
                            ),
                    ),
            )
            .child(
                div()
                    .w(px(42.))
                    .text_right()
                    .text_size(px(11.))
                    .child(label(value)),
            )
            .into_any_element()
    }

    fn preview_settings_slider(
        &mut self,
        id: &SharedString,
        position: Point<Pixels>,
        max: f32,
        step: f32,
    ) {
        if let Some(bounds) = self.settings_slider_bounds.get(id) {
            let fraction =
                f32::from((position.x - bounds.left()) / bounds.size.width).clamp(0., 1.);
            let value = ((fraction * max / step).round() * step).clamp(0., max);
            self.settings_slider_drag = Some((id.clone(), value));
        }
    }

    fn finish_settings_slider(&mut self, id: &SharedString) -> Option<f32> {
        if self
            .settings_slider_drag
            .as_ref()
            .is_some_and(|(active, _)| active == id)
        {
            return self.settings_slider_drag.take().map(|(_, value)| value);
        }
        None
    }

    fn integration_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let config = self.api_config.clone();
        let endpoint = format!("http://127.0.0.1:{}", config.port);
        let state = if config.enabled != self.api_boot_enabled {
            "重启后生效"
        } else if self.api.is_some() {
            "运行中"
        } else if config.enabled {
            "启动失败"
        } else {
            "已关闭"
        };
        let has_token = !config.token.is_empty();
        let can_save_token = !self.api_token_field.read(cx).text().trim().is_empty();
        let cors_buttons = [
            ("local", "仅本机"),
            ("any", "任意来源"),
            ("custom", "指定来源"),
        ]
        .into_iter()
        .map(|(value, label)| {
            self.settings_button(
                format!("api-cors-{value}"),
                label,
                config.cors == value,
                true,
                cx,
                move |this, _, cx| {
                    let mut next = this.api_config.clone();
                    next.cors = value.into();
                    if this.apply_api_config(next, cx) {
                        this.settings_feedback = Some("跨域来源已更新".into());
                    }
                },
            )
        })
        .collect::<Vec<_>>();
        div()
            .flex()
            .flex_col()
            .child(settings_row(
                "本机接口",
                &format!("{state} · {endpoint}"),
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(self.settings_button(
                        "api-copy-url",
                        "复制地址",
                        false,
                        true,
                        cx,
                        move |this, _, cx| {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                endpoint.clone(),
                            ));
                            this.settings_feedback = Some("已复制本机地址".into());
                            cx.notify();
                        },
                    ))
                    .child(self.setting_toggle(
                        "api-enabled",
                        config.enabled,
                        cx,
                        |this, _, cx| {
                            let mut next = this.api_config.clone();
                            next.enabled = !next.enabled;
                            if this.apply_api_config(next, cx)
                                && this.api_config.enabled != this.api_boot_enabled
                            {
                                this.settings_feedback = Some("重启后生效".into());
                            }
                        },
                    )),
            ))
            .child(settings_row(
                "访问令牌",
                if has_token {
                    "已设置 · 输入新令牌可替换"
                } else {
                    "未设置 · 可添加令牌限制接口访问"
                },
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(155.))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme_color(BG))
                            .border_1()
                            .border_color(theme_color(LINE))
                            .child(self.api_token_field.clone()),
                    )
                    .child(self.settings_button(
                        "api-save-token",
                        "保存令牌",
                        false,
                        can_save_token,
                        cx,
                        |this, _, cx| {
                            let mut next = this.api_config.clone();
                            next.token = this
                                .api_token_field
                                .read(cx)
                                .text()
                                .trim()
                                .chars()
                                .take(128)
                                .collect();
                            if !next.token.is_empty() && this.apply_api_config(next, cx) {
                                this.api_token_field
                                    .update(cx, |field, cx| field.set_text("", cx));
                                this.settings_feedback = Some("已保存令牌".into());
                            }
                        },
                    ))
                    .when(has_token, |row| {
                        row.child(self.settings_button(
                            "api-clear-token",
                            "清除令牌",
                            false,
                            true,
                            cx,
                            |this, _, cx| {
                                let mut next = this.api_config.clone();
                                next.token.clear();
                                if this.apply_api_config(next, cx) {
                                    this.api_token_field
                                        .update(cx, |field, cx| field.set_text("", cx));
                                    this.settings_feedback = Some("已清除令牌".into());
                                }
                            },
                        ))
                    }),
            ))
            .child(settings_row(
                "跨域来源",
                "选择可从网页访问本机接口的来源",
                div().flex().items_center().gap_1().children(cors_buttons),
            ))
            .when(config.cors == "custom", |body| {
                body.child(settings_row(
                    "允许的网站",
                    "用逗号分隔，最多 16 个",
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(245.))
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(theme_color(BG))
                                .border_1()
                                .border_color(theme_color(LINE))
                                .child(self.api_cors_field.clone()),
                        )
                        .child(self.settings_button(
                            "api-save-origins",
                            "保存来源",
                            false,
                            true,
                            cx,
                            |this, _, cx| {
                                let mut next = this.api_config.clone();
                                next.cors_origins = this.read_cors_origins(cx);
                                if this.apply_api_config(next, cx) {
                                    this.settings_feedback = Some("已保存允许的网站".into());
                                }
                            },
                        )),
                ))
            })
            .child(div().py_3().flex().flex_col().gap_3().children(
                api::catalog::GROUPS.iter().map(|group| {
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(group.title))
                        .children(group.endpoints.iter().map(|(method, path, summary)| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .w(px(48.))
                                        .flex_shrink_0()
                                        .text_color(theme_color(MUTED))
                                        .child(*method),
                                )
                                .child(div().flex_1().child(*path))
                                .child(div().text_color(theme_color(MUTED)).child(*summary))
                        }))
                }),
            ))
            .into_any_element()
    }

    fn read_cors_origins(&self, cx: &Context<Self>) -> Vec<String> {
        self.api_cors_field
            .read(cx)
            .text()
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .take(16)
            .map(str::to_owned)
            .collect()
    }

    fn apply_api_config(&mut self, next: api::Config, cx: &mut Context<Self>) -> bool {
        let saved = match self.library.save_api_config(&next) {
            Ok(()) => {
                self.api_config = next;
                if let Some(server) = &self.api {
                    server.update_config(self.api_config.clone());
                }
                self.settings_feedback = None;
                true
            }
            Err(error) => {
                self.settings_feedback = Some(error);
                false
            }
        };
        cx.notify();
        saved
    }

    fn setting_toggle(
        &mut self,
        id: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
        toggle: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label = if id == "close-remember" {
            if enabled {
                "已记住选择"
            } else {
                "记住这次选择"
            }
        } else if enabled {
            "已开启"
        } else {
            "已关闭"
        };
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(fade(theme_color(ACCENT), 0.09)))
            .text_xs()
            .text_color(if enabled {
                theme_color(ACCENT)
            } else {
                theme_color(MUTED)
            })
            .on_click(cx.listener(move |this, _, window, cx| toggle(this, window, cx)))
            .child(
                div()
                    .w(px(33.))
                    .h(px(19.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .when(enabled, |track| track.justify_end())
                    .rounded_full()
                    .bg(if enabled {
                        theme_color(ACCENT)
                    } else {
                        theme_color(LINE)
                    })
                    .child(div().size(px(13.)).rounded_full().bg(rgb(0xffffff))),
            )
            .child(label)
            .into_any_element()
    }

    fn poll_diagnostics(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.diagnostics_open || self.loading.is_some() {
            return false;
        }
        let Some(track) = self.audio.track() else {
            return false;
        };
        let track_identity = Arc::downgrade(&track);
        let new_track = self
            .analysed_track
            .as_ref()
            .is_none_or(|old| !old.ptr_eq(&track_identity));
        if new_track {
            self.analysed_track = Some(track_identity.clone());
            self.analysis = None;
            self.live_analysis = None;
        }
        if self.analysis.is_none() && !self.diagnostics_loading {
            self.diagnostics_loading = true;
            let source = track.clone();
            let job = cx
                .background_executor()
                .spawn(async move { diagnostics::analyse(&source) });
            cx.spawn(async move |this, cx| {
                let report = job.await;
                this.update(cx, |this, cx| {
                    this.diagnostics_loading = false;
                    if this
                        .audio
                        .track()
                        .is_some_and(|track| Arc::as_ptr(&track) == track_identity.as_ptr())
                        && this
                            .analysed_track
                            .as_ref()
                            .is_some_and(|track| track.ptr_eq(&track_identity))
                    {
                        this.analysis = Some(report);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        if new_track || self.last_live_analysis.elapsed() >= Duration::from_millis(80) {
            self.live_analysis = Some(diagnostics::live(
                &track,
                self.position.as_secs_f64(),
                self.live_analysis.as_ref(),
            ));
            self.last_live_analysis = Instant::now();
            return true;
        }
        false
    }

    fn diagnostics_layer(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.diagnostics_open {
            return None;
        }
        let track = self.audio.track();
        let active = self.loading.is_none()
            && self
                .analysed_track
                .as_ref()
                .zip(track.as_ref())
                .is_some_and(|(source, track)| source.as_ptr() == Arc::as_ptr(track));
        let live = self.live_analysis.as_ref().filter(|_| active);
        let wave = self
            .analysis
            .as_ref()
            .filter(|_| active)
            .map(|analysis| analysis.waveform.clone())
            .unwrap_or_default();
        let spectrum = live.map(|live| live.bands.to_vec()).unwrap_or_default();
        let duration = self.duration.as_secs_f32();
        let progress = if duration > 0.0 {
            self.position.as_secs_f32() / duration
        } else {
            0.0
        };
        let loop_region = self
            .loop_region_for(self.current_key().as_deref())
            .filter(|_| duration > 0.0)
            .map(|(start, end)| (start as f32 / duration, end as f32 / duration));
        let live_loudness = format_metric(live.and_then(|live| live.lufs), "LUFS");
        let live_peak = format_metric(live.and_then(|live| live.peak_db), "dBFS");
        let target = self.library.export_settings().preset.target_lufs;
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("音频诊断"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child(self.current_title()),
                    ),
            )
            .child(
                self.tiny_button("关闭", "diagnostics-close", cx, |this, _, cx| {
                    this.diagnostics_open = false;
                    cx.notify();
                }),
            );
        let body = match self.analysis.as_ref().filter(|_| active) {
            Some(report) => {
                let (label, color) = match report.verdict() {
                    Verdict::Good => (
                        "可直接使用",
                        rgb(if is_dark() { 0x7ac9a3 } else { 0x227651 }),
                    ),
                    Verdict::Warning => ("可用，建议留意", theme_color(WARNING)),
                    Verdict::Bad => ("建议处理", theme_color(DANGER)),
                };
                let quiet = report
                    .silence
                    .iter()
                    .map(|(start, end)| end - start)
                    .sum::<f64>()
                    .max(0.0);
                let phase = report
                    .min_correlation
                    .zip(report.average_correlation)
                    .map_or_else(
                        || "—".to_owned(),
                        |(min, average)| format!("{min:.2} / {average:.2}"),
                    );
                div().flex().flex_col().gap_2()
                    .child(div().flex().justify_between().items_center()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(color).child(label))
                        .child(div().text_xs().text_color(theme_color(FAINT)).child(format!("导出目标 {target:.0} LUFS"))))
                    .child(div().text_xs().text_color(theme_color(MUTED)).child(format!(
                        "综合响度 {}   ·   采样峰值 {}   ·   RMS {}",
                        format_metric(report.integrated_lufs, "LUFS"),
                        format_metric(report.peak_db, "dBFS"),
                        format_metric(report.rms_db, "dBFS"),
                    )))
                    .child(div().text_xs().text_color(theme_color(MUTED)).child(format!(
                        "削波 {} 个 / {} 段   ·   瞬态跳变 {} 次（最大 {:.2}）   ·   静音 {:.1} 秒   ·   相位最低 / 平均 {phase}",
                        report.clipped, report.clip_runs, report.pops, report.max_delta, quiet,
                    )))
                    .children(report.issues.iter().map(|issue| {
                        div().text_xs().text_color(theme_color(MUTED)).child(issue.message.clone())
                    }))
            }
            None => {
                div()
                    .text_xs()
                    .text_color(theme_color(MUTED))
                    .child(if self.loading.is_some() {
                        "正在加载曲目…"
                    } else if track.is_none() {
                        "当前没有可分析的源音频"
                    } else {
                        "正在分析源音频…"
                    })
            }
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .bg(fade(theme_color(CONTROL_TINT), 0.28))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .w(px(720.))
                        .max_w(relative(0.9))
                        .p_5()
                        .gap_4()
                        .flex()
                        .flex_col()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme_color(LINE))
                        .bg(theme_color(PANEL))
                        .shadow_lg()
                        .child(header)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme_color(MUTED))
                                        .child("整曲波形"),
                                )
                                .child(
                                    div()
                                        .rounded_md()
                                        .bg(theme_color(BG))
                                        .overflow_hidden()
                                        .child(audio_chart(wave, true, progress, loop_region)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .justify_between()
                                        .text_xs()
                                        .text_color(theme_color(MUTED))
                                        .child("实时频谱")
                                        .child(format!("{live_loudness}  ·  {live_peak}")),
                                )
                                .child(
                                    div()
                                        .rounded_md()
                                        .bg(theme_color(BG))
                                        .overflow_hidden()
                                        .child(audio_chart(spectrum, false, 0., None)),
                                ),
                        )
                        .child(body),
                )
                .into_any_element(),
        )
    }

    fn confirm_queue_removal(
        &mut self,
        row: Option<(usize, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.guard_library_playback() || self.queue.is_empty() {
            cx.notify();
            return;
        }
        let message = row
            .as_ref()
            .and_then(|(_, key)| self.library.song(key))
            .map_or_else(
                || "清空播放队列？".to_owned(),
                |song| format!("将《{}》移出播放队列？", song.title),
            );
        let prompt = window.prompt(
            PromptLevel::Warning,
            &message,
            None,
            &[PromptButton::ok("移除"), PromptButton::cancel("取消")],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if prompt.await == Ok(0) {
                this.update(cx, |this, cx| {
                    if !this.guard_library_playback() {
                        match row {
                            Some((index, key)) if this.queue.get(index) == Some(&key) => {
                                this.remove_from_queue(Some(index))
                            }
                            None => this.remove_from_queue(None),
                            _ => this.notice = "队列已更新，请重新选择要移除的曲目".into(),
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn remove_from_queue(&mut self, index: Option<usize>) {
        let Some(index) = index else {
            self.stop_playback();
            self.queue.clear();
            self.queue_index = 0;
            return;
        };
        let Some(was_current) = detach_queue_item(&mut self.queue, &mut self.queue_index, index)
        else {
            return;
        };
        if self.queue.is_empty() {
            self.stop_playback();
        } else if was_current && let Some(next) = self.current_key() {
            self.failed_tracks.clear();
            self.load_key(&next, self.playing);
        }
    }

    fn queue_layer(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.queue_open {
            return None;
        }
        let count = self.queue.len();
        let rows = uniform_list(
            "queue-songs",
            count,
            cx.processor(|this, range: Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        let key = this.queue.get(index)?.clone();
                        let song = this.library.song(&key)?;
                        let title = song.title.clone();
                        let current = this.queue_index == index;
                        let remove_key = key.clone();
                        let remove = this.row_action(
                            format!("queue-remove-{index}"),
                            "x",
                            theme_color(MUTED),
                            cx,
                            move |this, window, cx| {
                                this.confirm_queue_removal(
                                    Some((index, remove_key.clone())),
                                    window,
                                    cx,
                                )
                            },
                        );
                        Some(
                            div()
                                .h(px(38.))
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_md()
                                .bg(if current {
                                    theme_color(PANEL_LIGHT)
                                } else {
                                    theme_color(PANEL)
                                })
                                .child(
                                    div()
                                        .w(px(28.))
                                        .text_xs()
                                        .text_color(theme_color(FAINT))
                                        .child((index + 1).to_string()),
                                )
                                .child(
                                    div()
                                        .id(("queue-play", index))
                                        .flex_1()
                                        .min_w(px(0.))
                                        .truncate()
                                        .cursor_pointer()
                                        .text_color(if current {
                                            theme_color(ACCENT)
                                        } else {
                                            theme_color(TEXT)
                                        })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !this.guard_library_playback()
                                                && this.queue.get(index) == Some(&key)
                                            {
                                                this.queue_index = index;
                                                this.failed_tracks.clear();
                                                this.start_key(&key);
                                            }
                                            cx.notify();
                                        }))
                                        .child(title),
                                )
                                .child(remove),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .h(px((count as f32 * 38.0).clamp(38.0, 342.0)));
        Some(
            div()
                .absolute()
                .occlude()
                .right(px(20.))
                .bottom(px(112.))
                .w(px(360.))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .bg(theme_color(PANEL))
                .rounded_lg()
                .border_1()
                .border_color(theme_color(LINE))
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_1()
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("播放队列 · {count} 首")),
                        )
                        .when(count > 0, |header| {
                            header.child(self.tiny_button(
                                "清空",
                                "queue-clear",
                                cx,
                                |this, window, cx| {
                                    this.confirm_queue_removal(None, window, cx);
                                },
                            ))
                        })
                        .child(self.tiny_button("关闭", "queue-close", cx, |this, _, cx| {
                            this.queue_open = false;
                            cx.notify();
                        })),
                )
                .when(count > 0, |panel| panel.child(rows))
                .when(count == 0, |panel| {
                    panel.child(
                        div()
                            .p_2()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child("选择一首曲目开始播放"),
                    )
                })
                .into_any_element(),
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
            .text_color(theme_color(MUTED))
            .border_1()
            .border_color(fade(theme_color(LINE), 0.9))
            .hover(|style| {
                style
                    .bg(fade(theme_color(CONTROL_TINT), 0.08))
                    .text_color(theme_color(TEXT))
            })
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
            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.08)))
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx)))
            .child(icon(
                icon_name,
                15.,
                if active {
                    theme_color(FAVORITE)
                } else {
                    theme_color(MUTED)
                },
            ))
            .into_any_element()
    }

    fn hero_copy(&self) -> (String, String, String) {
        static DEFAULT_COPY: std::sync::LazyLock<bgm_core::AppConfig> =
            std::sync::LazyLock::new(bgm_core::AppConfig::default);
        match &self.filter {
            LibraryFilter::All | LibraryFilter::Favorites => {
                let (copy, default, title, count) = if self.filter == LibraryFilter::Favorites {
                    (
                        &self.library.config.ui.favorites,
                        &DEFAULT_COPY.ui.favorites,
                        "我的收藏",
                        self.library
                            .prefs
                            .favorites
                            .iter()
                            .filter(|key| self.library.song(key).is_some())
                            .count(),
                    )
                } else {
                    (
                        &self.library.config.ui.all,
                        &DEFAULT_COPY.ui.all,
                        "音乐库",
                        self.library.snapshot.songs.len(),
                    )
                };
                let description = if copy.description == default.description {
                    if self.filter == LibraryFilter::Favorites {
                        format!("已收藏 {count} 首配乐")
                    } else {
                        format!(
                            "{} 个场景  ·  {count} 首本地配乐",
                            self.library.snapshot.categories.len()
                        )
                    }
                } else {
                    let names = self
                        .library
                        .snapshot
                        .categories
                        .iter()
                        .map(|cat| cat.name_zh.as_str())
                        .collect::<Vec<_>>()
                        .join("、");
                    copy.description.replace("{categories}", &names)
                };
                (
                    if copy.title == default.title {
                        title.to_owned()
                    } else {
                        copy.title.clone()
                    },
                    description,
                    if copy.results_title == default.results_title {
                        "曲目".to_owned()
                    } else {
                        copy.results_title.clone()
                    },
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

    fn show_track_menu(&mut self, key: String, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.action_menu = Some(TrackMenu {
            key: key.clone(),
            position,
            stems: [None, None],
        });
        let directory = self.library.subtitle_directory();
        cx.spawn(async move |this, cx| {
            let track = key.clone();
            let stems = cx
                .background_executor()
                .spawn(async move { separation::audio_paths(&directory, &track) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(menu) = &mut this.action_menu
                    && menu.key == key
                {
                    menu.stems = stems;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn remove_separated_audio(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.action_menu = None;
        if self.separating.contains(&key) {
            self.notice = "请等待这首曲目的分离任务结束".into();
            cx.notify();
            return;
        }
        let confirmation = window.prompt(
            PromptLevel::Warning,
            "删除分离产物？",
            Some("移除本应用生成的人声和伴奏文件。原始音频保留。"),
            &[PromptButton::ok("删除"), PromptButton::cancel("取消")],
            cx,
        );
        let directory = self.library.subtitle_directory();
        cx.spawn(async move |this, cx| {
            if confirmation.await != Ok(0) {
                return;
            }
            let result = cx
                .background_executor()
                .spawn(async move { separation::delete_standalone(&directory, &key) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.notice = result.err().unwrap_or_else(|| "已删除分离产物".into());
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn action_menu_layer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let TrackMenu {
            key,
            position,
            stems,
        } = self.action_menu.clone()?;
        let left = (position.x - px(180.))
            .max(px(8.))
            .min(window.viewport_size().width - px(196.));
        let stem_count = stems.iter().flatten().count();
        let menu_height = 264.
            + if stem_count > 0 {
                32. * (stem_count + 1) as f32
            } else {
                0.
            };
        let top = (position.y + px(5.))
            .min(window.viewport_size().height - px(menu_height))
            .max(px(8.));
        let rename_key = key.clone();
        let move_key = key.clone();
        let export_key = key.clone();
        let loop_key = key.clone();
        let separation_key = key.clone();
        let separation_delete_key = key.clone();
        let subtitle_key = key.clone();
        let separating = self.separating.contains(&key);
        let trash_key = key;

        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
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
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(theme_color(LINE))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(self.menu_action(
                            "menu-rename",
                            "edit",
                            "编辑曲目",
                            false,
                            cx,
                            move |this, window, cx| {
                                this.open_track_editor(&rename_key, window, cx);
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
                        .child(self.menu_action(
                            "menu-separate",
                            "music",
                            if separating {
                                "分离中…"
                            } else {
                                "分离人声与伴奏"
                            },
                            false,
                            cx,
                            move |this, window, cx| {
                                this.process_track_audio(separation_key.clone(), false, window, cx);
                            },
                        ))
                        .child(self.menu_action(
                            "menu-generate-subtitle",
                            "music",
                            "生成字幕",
                            false,
                            cx,
                            move |this, window, cx| {
                                this.open_subtitle_generation(subtitle_key.clone(), window, cx)
                            },
                        ))
                        .children(
                            stems
                                .into_iter()
                                .zip([
                                    ("menu-open-vocals", "打开人声"),
                                    ("menu-open-accompaniment", "打开伴奏"),
                                ])
                                .filter_map(|(path, (id, label))| {
                                    let path = path?;
                                    Some(self.menu_action(
                                        id,
                                        "music",
                                        label,
                                        false,
                                        cx,
                                        move |this, _, cx| {
                                            this.action_menu = None;
                                            if let Err(error) = desktop::open_audio_file(&path) {
                                                this.notice = error;
                                            }
                                            cx.notify();
                                        },
                                    ))
                                }),
                        )
                        .when(stem_count > 0, |menu| {
                            menu.child(self.menu_action(
                                "menu-delete-stems",
                                "trash",
                                "删除分离产物",
                                true,
                                cx,
                                move |this, window, cx| {
                                    this.remove_separated_audio(
                                        separation_delete_key.clone(),
                                        window,
                                        cx,
                                    );
                                },
                            ))
                        })
                        .child(
                            div()
                                .h(px(1.))
                                .mx_2()
                                .my_1()
                                .bg(fade(theme_color(LINE), 0.8)),
                        )
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
            .text_color(if danger {
                theme_color(DANGER)
            } else {
                theme_color(MUTED)
            })
            .hover(|style| {
                style
                    .bg(fade(theme_color(CONTROL_TINT), 0.07))
                    .text_color(theme_color(TEXT))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.action_menu = None;
                on_click(this, window, cx);
            }))
            .child(icon(
                icon_name,
                14.,
                if danger {
                    theme_color(DANGER)
                } else {
                    theme_color(MUTED)
                },
            ))
            .child(label)
            .into_any_element()
    }

    fn tiny_button(
        &self,
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
                fade(theme_color(ACCENT), 1.)
            } else {
                fade(theme_color(CONTROL_TINT), 0.05)
            })
            .text_color(if primary {
                theme_color(ACCENT_INK)
            } else {
                theme_color(MUTED)
            })
            .border_1()
            .border_color(if primary {
                fade(theme_color(ACCENT), 0.)
            } else {
                fade(theme_color(LINE), 1.)
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
            Dialog::EditTrack { .. } => ("编辑曲目".to_owned(), "修改曲名和所属分类。".to_owned()),
            Dialog::NewCategory => (
                "新增分类".to_owned(),
                "创建分类和对应的曲库目录。".to_owned(),
            ),
            Dialog::EditCategory { .. } => {
                ("编辑分类".to_owned(), "修改显示名称和主题色。".to_owned())
            }
            Dialog::Move { .. } => (
                "移动到分类".to_owned(),
                "文件会移动到目标分类目录。".to_owned(),
            ),
            Dialog::MoveBatch { keys } => (
                "批量移动".to_owned(),
                format!("将 {} 首曲目移动到目标分类。", keys.len()),
            ),
            Dialog::Import { files } => (
                "导入到分类".to_owned(),
                format!("已选择 {} 个文件。", files.len()),
            ),
            Dialog::FileReport { summary, .. } => ("文件操作结果".to_owned(), summary.clone()),
            Dialog::Loop { key } => (
                format!("循环点 · {}", self.song_title(key)),
                "设置 A/B 循环区间（单位：秒）。启用后播放器会在区间内采样级无缝循环。".to_owned(),
            ),
        };
        let show_field = matches!(
            dialog,
            Dialog::EditTrack { .. } | Dialog::NewCategory | Dialog::EditCategory { .. }
        );
        let category_detail = match dialog {
            Dialog::NewCategory => Some("目录名（可选）"),
            Dialog::EditCategory { .. } => Some("主题颜色（#RRGGBB）"),
            _ => None,
        };
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
        let edit_category = match &dialog {
            Dialog::EditTrack { category, .. } => Some(category.as_str()),
            _ => None,
        };
        let edit_error = match &dialog {
            Dialog::EditTrack { error, .. } => error.clone(),
            _ => None,
        };
        let report = match &dialog {
            Dialog::FileReport { report, .. } => Some(report),
            _ => None,
        };
        let mut choices = Vec::new();
        if edit_category.is_some()
            || matches!(
                dialog,
                Dialog::Move { .. } | Dialog::MoveBatch { .. } | Dialog::Import { .. }
            )
        {
            for category in categories {
                let id = category.id.clone();
                let name = category.name_zh.clone();
                let selected = edit_category == Some(id.as_str());
                choices.push(
                    div()
                        .id(SharedString::from(format!("pick-{id}")))
                        .h(px(34.))
                        .flex_shrink_0()
                        .px_3()
                        .flex()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .bg(if selected {
                            fade(theme_color(ACCENT), 0.14)
                        } else {
                            theme_color(BG).into()
                        })
                        .when(selected, |row| row.text_color(theme_color(ACCENT)))
                        .hover(|style| style.bg(theme_color(LINE)))
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
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(fade(theme_color(CONTROL_TINT), 0.28))
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
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(theme_color(LINE))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_lg().child(heading))
                        .child(div().text_xs().text_color(theme_color(MUTED)).child(body))
                        .when_some(report, |body, report| {
                            body.child(
                                div()
                                    .id("file-report-lines")
                                    .max_h(px(280.))
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .children(
                                        report
                                            .notice
                                            .iter()
                                            .chain(&report.messages)
                                            .chain(
                                                report
                                                    .failures
                                                    .iter()
                                                    .filter(|line| !report.messages.contains(line)),
                                            )
                                            .map(|line| {
                                                div()
                                                    .text_sm()
                                                    .text_color(theme_color(MUTED))
                                                    .child(line.clone())
                                            }),
                                    ),
                            )
                        })
                        .when(show_field, |this| this.child(self.dialog_field.clone()))
                        .when_some(category_detail, |dialog, label| {
                            dialog.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div().text_xs().text_color(theme_color(MUTED)).child(label),
                                    )
                                    .child(
                                        div()
                                            .px_3()
                                            .py_2()
                                            .rounded_md()
                                            .bg(theme_color(BG))
                                            .border_1()
                                            .border_color(theme_color(LINE))
                                            .child(self.category_detail_field.clone()),
                                    ),
                            )
                        })
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
                                            fade(theme_color(ACCENT), 0.14)
                                        } else {
                                            fade(theme_color(CONTROL_TINT), 0.04)
                                        })
                                        .border_1()
                                        .border_color(if loop_enabled {
                                            fade(theme_color(ACCENT), 0.35)
                                        } else {
                                            fade(theme_color(LINE), 1.)
                                        })
                                        .text_color(if loop_enabled {
                                            theme_color(ACCENT)
                                        } else {
                                            theme_color(MUTED)
                                        })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.loop_enabled_pending = !this.loop_enabled_pending;
                                            cx.notify();
                                        }))
                                        .child(icon(
                                            "repeat",
                                            13.,
                                            if loop_enabled {
                                                theme_color(ACCENT)
                                            } else {
                                                theme_color(MUTED)
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
                        .when(!choices.is_empty(), |dialog| {
                            dialog.child(
                                div()
                                    .id("dialog-category-choices")
                                    .max_h(px(178.))
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .children(choices),
                            )
                        })
                        .when_some(edit_error, |dialog, error| {
                            dialog
                                .child(div().text_xs().text_color(theme_color(DANGER)).child(error))
                        })
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(self.tiny_button(
                                    if report.is_some() { "关闭" } else { "取消" },
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
            Dialog::EditTrack { key, .. } => {
                self.dialog = Some(Dialog::EditTrack {
                    key,
                    category,
                    error: None,
                });
                cx.notify();
                return;
            }
            Dialog::Move { key } => match self.library.move_song(&key, &category) {
                Ok(mutation) => self.apply_mutation(mutation),
                Err(error) => self.notice = error,
            },
            Dialog::MoveBatch { keys } => match self.library.move_batch(&keys, &category) {
                Ok(mutation) => {
                    for change in &mutation.changes {
                        self.selected_songs.remove(&change.from);
                    }
                    self.apply_mutation(mutation);
                }
                Err(error) => self.notice = error,
            },
            Dialog::Import { files } => {
                let mutation = self.library.import(&category, &files);
                self.apply_mutation(mutation);
            }
            _ => return,
        }
        if !matches!(self.dialog, Some(Dialog::FileReport { .. })) {
            self.dialog = None;
            window.focus(&self.focus);
        }
        cx.notify();
    }

    // ---------- 渲染：歌曲选择弹窗 ----------

    fn picker_layer(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let picker = self.picker.as_ref()?;
        let multi = matches!(picker.purpose, PickerPurpose::AddToSchedule);
        let captions = matches!(picker.purpose, PickerPurpose::AddSubtitleTrack);
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
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .bg(fade(theme_color(CONTROL_TINT), 0.28))
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
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(theme_color(LINE))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(div().text_lg().child(if captions {
                            "加入字幕生成的曲目"
                        } else if multi {
                            "选择歌曲"
                        } else {
                            "替换歌曲"
                        }))
                        .when(!captions, |panel| {
                            panel.child(div().text_xs().text_color(theme_color(MUTED)).child(
                                if multi {
                                    "在列表中勾选要加入节目的歌曲；已存在的曲目会自动跳过。"
                                } else {
                                    "选择一首歌替换当前条目。"
                                },
                            ))
                        })
                        .child(self.picker_search.clone())
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.))
                                .overflow_hidden()
                                .rounded_md()
                                .border_1()
                                .border_color(fade(theme_color(LINE), 0.7))
                                .child(if count == 0 {
                                    div()
                                        .size_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .text_size(px(12.))
                                        .text_color(theme_color(FAINT))
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
                                                        !captions && existing.contains(&song.key),
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
                                .child(div().text_xs().text_color(theme_color(FAINT)).child(
                                    if multi {
                                        format!("已选 {selected_count} 首")
                                    } else {
                                        String::new()
                                    },
                                ))
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
                                    if captions {
                                        "加入待生成"
                                    } else {
                                        "确定"
                                    },
                                    "picker-ok",
                                    cx,
                                    |this, window, cx| {
                                        this.confirm_song_picker(window, cx);
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
            .border_color(fade(theme_color(LINE), 0.45))
            .cursor_pointer()
            .bg(if selected {
                fade(theme_color(ACCENT), 0.09)
            } else {
                fade(theme_color(BG), 0.)
            })
            .hover(|style| style.bg(fade(theme_color(ACCENT), 0.05)))
            .on_click(cx.listener(move |this, _, _, cx| {
                let clicked = key.clone();
                let Some(picker) = this.picker.as_mut() else {
                    return;
                };
                if matches!(picker.purpose, PickerPurpose::AddToSchedule) {
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
                icon("check", 14., theme_color(ACCENT)).into_any_element()
            } else {
                div()
                    .size(px(14.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(fade(theme_color(LINE), 1.))
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
                            .text_color(theme_color(TEXT))
                            .min_w(px(0.))
                            .truncate()
                            .child(song.title.clone()),
                    )
                    .when(already_in_draft, |this| {
                        this.child(
                            div()
                                .flex_none()
                                .text_size(px(9.))
                                .text_color(theme_color(FAINT))
                                .child("已在节目"),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(96.))
                    .flex_none()
                    .truncate()
                    .text_size(px(10.5))
                    .text_color(theme_color(FAINT))
                    .child(category),
            )
            .into_any_element()
    }

    // ---------- 渲染：导出设置弹层 ----------

    fn detect_ffmpeg(&mut self, cx: &mut Context<Self>) {
        self.ffmpeg_detected = None;
        let job = cx
            .background_executor()
            .spawn(async { bgm_core::loudness::ffmpeg_path() });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.ffmpeg_detected = Some(result);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn export_popover_layer(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        if !self.export_popover {
            return None;
        }
        let ffmpeg = self
            .ffmpeg_detected
            .as_ref()
            .and_then(|detected| detected.as_ref());
        let presets = bgm_core::loudness::EXPORT_PRESETS.to_vec();
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
                .occlude()
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
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(theme_color(LINE))
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
                                        .text_color(theme_color(FAINT))
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
                                    fade(theme_color(ACCENT), 0.1)
                                } else {
                                    fade(theme_color(CONTROL_TINT), 0.03)
                                })
                                .border_1()
                                .border_color(if enabled {
                                    fade(theme_color(ACCENT), 0.3)
                                } else {
                                    fade(theme_color(LINE), 1.)
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
                                                .text_color(theme_color(FAINT))
                                                .child(hint),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(36.))
                                        .h(px(20.))
                                        .rounded_full()
                                        .bg(if enabled {
                                            theme_color(ACCENT)
                                        } else {
                                            theme_color(LINE)
                                        })
                                        .p(px(2.))
                                        .child(
                                            div()
                                                .size(px(16.))
                                                .rounded_full()
                                                .bg(theme_color(TEXT))
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
                                .text_color(theme_color(FAINT))
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
                                    fade(theme_color(ACCENT), 0.14)
                                } else {
                                    fade(theme_color(CONTROL_TINT), 0.03)
                                })
                                .border_1()
                                .border_color(if active {
                                    fade(theme_color(ACCENT), 0.35)
                                } else {
                                    fade(theme_color(LINE), 0.8)
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
                                        .text_color(if active {
                                            theme_color(TEXT)
                                        } else {
                                            theme_color(MUTED)
                                        })
                                        .child(preset.label),
                                )
                                .when(is_default, |this| {
                                    this.child(
                                        div()
                                            .px_1()
                                            .rounded(px(4.))
                                            .bg(fade(theme_color(ACCENT), 0.14))
                                            .text_size(px(9.))
                                            .text_color(theme_color(ACCENT))
                                            .child("默认"),
                                    )
                                })
                                .child(
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(theme_color(FAINT))
                                        .child(format!("{} LUFS", preset.target_lufs)),
                                )
                                .when(active, |this| {
                                    this.child(icon("check", 13., theme_color(ACCENT)))
                                })
                        }))
                        .when_some(selected, |this, preset| {
                            this.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(theme_color(FAINT))
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
                                .bg(fade(theme_color(CONTROL_TINT), 0.03))
                                .text_size(px(10.5))
                                .text_color(if ffmpeg.is_some() {
                                    theme_color(MUTED)
                                } else {
                                    theme_color(WARNING)
                                })
                                .child(if ffmpeg.is_some() {
                                    icon("check", 12., theme_color(ACCENT)).into_any_element()
                                } else {
                                    icon("alert", 12., theme_color(WARNING)).into_any_element()
                                })
                                .child(match ffmpeg {
                                    Some(path) => format!("ffmpeg 已就绪：{}", path.display()),
                                    None if self.ffmpeg_detected.is_none() => {
                                        "正在检测 ffmpeg…".to_owned()
                                    }
                                    None => "未检测到 ffmpeg，导出将回退为复制原文件".to_owned(),
                                }),
                        ),
                ),
        )
    }

    // ---------- 渲染：壁纸 ----------

    fn poll_wallpaper(&mut self, cx: &mut Context<Self>) {
        if self.library.prefs.wallpaper == "builtin-1" && self.builtin_wallpaper_video.is_none() {
            self.restore_default_wallpaper("找不到内置视频壁纸".into(), cx);
            return;
        }
        let source = self.wallpaper_view().source;
        let request = source.image_request(self.library.prefs.wallpaper_blur);
        if self
            .wallpaper_request
            .as_ref()
            .map(|request| &request.asset)
            != request.as_ref().map(|request| &request.asset)
        {
            self.clear_wallpaper_image(cx);
        }
        if let WallpaperSource::Video { path, .. } = source {
            if let Err(error) =
                self.poll_wallpaper_video(path, self.library.prefs.wallpaper_blur, cx)
            {
                self.restore_default_wallpaper(error, cx);
            }
            return;
        }
        self.clear_wallpaper_video(cx);
        if self.wallpaper_loading || self.wallpaper_request == request {
            return;
        }
        let Some(request) = request else {
            return;
        };
        self.wallpaper_loading = true;
        let work = request.clone();
        let job = cx
            .background_executor()
            .spawn(async move { work.prepare() });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                this.wallpaper_loading = false;
                let current = this
                    .wallpaper_view()
                    .source
                    .image_request(this.library.prefs.wallpaper_blur);
                if current.as_ref() != Some(&request) {
                    return;
                }
                match result {
                    Ok(image) => {
                        this.wallpaper_request = Some(request);
                        if let Some(previous) = this.wallpaper_image.replace(image) {
                            cx.drop_image(previous, None);
                        }
                    }
                    Err(error) => this.restore_default_wallpaper(error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn poll_wallpaper_video(
        &mut self,
        path: PathBuf,
        blur: u32,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let active = self.main_window.is_some_and(|window| {
            window
                .update(cx, |_, window, _| desktop::wallpaper_window_active(window))
                .unwrap_or(false)
        });
        if self
            .wallpaper_video
            .as_ref()
            .is_some_and(|video| video.path() != path)
        {
            self.clear_wallpaper_video(cx);
        }
        if self.wallpaper_video.is_none() && active {
            self.wallpaper_video = Some(wallpaper::Video::open(path, blur)?);
        }
        if let Some(video) = &mut self.wallpaper_video
            && let Some(frame) = video.poll(active, blur)?
        {
            if let Some(previous) = self.wallpaper_video_frame.replace(frame) {
                previous.release(cx);
            }
            cx.notify();
        }
        Ok(())
    }

    fn restore_default_wallpaper(&mut self, error: String, cx: &mut Context<Self>) {
        self.clear_wallpaper_video(cx);
        self.clear_wallpaper_image(cx);
        self.notice = match self.library.set_wallpaper("default", None) {
            Ok(()) => format!("{error}，已切换回默认样式"),
            Err(save_error) => format!("{error}，{save_error}"),
        };
        cx.notify();
    }

    fn clear_wallpaper_image(&mut self, cx: &mut Context<Self>) {
        self.wallpaper_request = None;
        if let Some(previous) = self.wallpaper_image.take() {
            cx.drop_image(previous, None);
        }
    }

    fn clear_wallpaper_video(&mut self, cx: &mut Context<Self>) {
        self.wallpaper_video = None;
        if let Some(previous) = self.wallpaper_video_frame.take() {
            previous.release(cx);
        }
    }

    /// 渐变、图片和视频壁纸位于面板下方。
    fn wallpaper_layer(&self) -> Option<impl IntoElement + use<>> {
        let view = self.wallpaper_view();
        if !view.has_wallpaper {
            return None;
        }
        let mask = 0.62 - view.opacity * 0.20;
        let mut layer = div().absolute().inset_0();
        match &view.source {
            WallpaperSource::Gradient(id) => {
                layer = layer.child(match id.as_str() {
                    "gradient-blue" => div().absolute().inset_0().bg(linear_gradient(
                        165.,
                        rgb(if is_dark() { 0x1d2e53 } else { 0xe7efff }),
                        rgb(if is_dark() { 0x171c30 } else { 0xf5f8ff }),
                    )),
                    "gradient-green" => div().absolute().inset_0().bg(linear_gradient(
                        165.,
                        rgb(if is_dark() { 0x1b3935 } else { 0xe0f4ed }),
                        rgb(if is_dark() { 0x16251f } else { 0xf7fbf9 }),
                    )),
                    _ => div().absolute().inset_0().bg(linear_gradient(
                        165.,
                        rgb(if is_dark() { 0x452537 } else { 0xffe5eb }),
                        rgb(if is_dark() { 0x261d2b } else { 0xfff6f8 }),
                    )),
                });
            }
            WallpaperSource::Image { asset, contain } => {
                let image = self
                    .wallpaper_request
                    .as_ref()
                    .filter(|request| &request.asset == asset)
                    .and(self.wallpaper_image.as_ref())?;
                let fit = if *contain {
                    ObjectFit::Contain
                } else {
                    ObjectFit::Cover
                };
                layer = layer.child(
                    div()
                        .absolute()
                        .inset_0()
                        .child(img(image.clone()).size_full().object_fit(fit)),
                );
            }
            WallpaperSource::Video { path, contain } => {
                let fit = if *contain {
                    ObjectFit::Contain
                } else {
                    ObjectFit::Cover
                };
                self.wallpaper_video
                    .as_ref()
                    .filter(|video| video.path() == path)?;
                let frame = self.wallpaper_video_frame.as_ref()?;
                layer = layer.child(
                    div()
                        .absolute()
                        .inset_0()
                        .overflow_hidden()
                        .child(frame.element(fit)),
                );
            }
            WallpaperSource::None => return None,
        }
        Some(
            layer
                .child(div().absolute().inset_0().bg(fade(theme_color(BG), mask)))
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
            "builtin-1" => {
                self.builtin_wallpaper_video
                    .clone()
                    .map_or(WallpaperSource::None, |path| WallpaperSource::Video {
                        path,
                        contain,
                    })
            }
            "builtin-2" => WallpaperSource::Image {
                asset: wallpaper::Asset::Embedded,
                contain,
            },
            "custom" => match prefs.custom_wallpaper.as_deref() {
                Some(path) if wallpaper::is_video(Path::new(path)) => WallpaperSource::Video {
                    path: PathBuf::from(path),
                    contain,
                },
                Some(path) => WallpaperSource::Image {
                    asset: wallpaper::Asset::Local(path.into()),
                    contain,
                },
                None => WallpaperSource::None,
            },
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.wallpaper_popover {
            return None;
        }
        const PANEL_WIDTH: f32 = 300.;
        const PANEL_PADDING: f32 = 16.;
        const ROW_PADDING: f32 = 12.;
        const ROW_GAP: f32 = 8.;
        const ICON_SIZE: f32 = 13.;
        const BORDER: f32 = 1.;
        const NAME_SIZE: f32 = 12.;
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
        let full_name: SharedString = custom_name.into();
        let selected = current == "custom";
        let name = if selected {
            let width = PANEL_WIDTH
                - 2. * (PANEL_PADDING + ROW_PADDING + ROW_GAP + ICON_SIZE + 2. * BORDER);
            cx.text_system()
                .line_wrapper(gpui::font(self.interface_font()), px(NAME_SIZE))
                .truncate_line(full_name.clone(), px(width), "…", &mut Vec::new())
        } else {
            "自定义壁纸…".into()
        };
        let opacity = self.library.prefs.wallpaper_opacity.clamp(0.0, 1.0);
        let fit = self.library.prefs.wallpaper_fit.clone();
        let opacity_view = cx.entity().downgrade();
        let options = [
            ("default", "默认样式", "当前配色"),
            ("gradient-blue", "渐变蓝", "深邃静谧"),
            ("gradient-green", "渐变绿", "翡翠流光"),
            ("gradient-red", "渐变红", "暖色余晖"),
            ("builtin-1", "内置壁纸一", "MP4 动态"),
            ("builtin-2", "雪山云海", "JPG 静态"),
        ]
        .into_iter()
        .filter(|(id, _, _)| *id != "builtin-1" || self.builtin_wallpaper_video.is_some());

        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.wallpaper_popover = false;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .id("wallpaper-options")
                        .absolute()
                        .right(px(52.))
                        .top(px(62.))
                        .w(px(PANEL_WIDTH))
                        .max_h(window.viewport_size().height - px(74.))
                        .overflow_y_scroll()
                        .p(px(PANEL_PADDING))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_lg()
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(theme_color(LINE))
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
                                        .text_color(theme_color(FAINT))
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
                                    fade(theme_color(ACCENT), 0.14)
                                } else {
                                    fade(theme_color(CONTROL_TINT), 0.03)
                                })
                                .border_1()
                                .border_color(if active {
                                    fade(theme_color(ACCENT), 0.35)
                                } else {
                                    fade(theme_color(LINE), 0.8)
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
                                        .text_color(if active {
                                            theme_color(TEXT)
                                        } else {
                                            theme_color(MUTED)
                                        })
                                        .child(label),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .text_color(theme_color(FAINT))
                                        .child(hint),
                                )
                                .when(active, |this| {
                                    this.child(icon("check", 13., theme_color(ACCENT)).flex_none())
                                })
                        }))
                        .child(
                            div()
                                .id("wall-custom")
                                .when(selected, |row| {
                                    row.tooltip(move |_, cx| {
                                        cx.new(|_| Tip(full_name.clone())).into()
                                    })
                                })
                                .h(px(34.))
                                .px(px(ROW_PADDING))
                                .flex()
                                .items_center()
                                .gap(px(ROW_GAP))
                                .rounded_md()
                                .cursor_pointer()
                                .bg(if selected {
                                    fade(theme_color(ACCENT), 0.14)
                                } else {
                                    fade(theme_color(CONTROL_TINT), 0.03)
                                })
                                .border_1()
                                .border_color(if selected {
                                    fade(theme_color(ACCENT), 0.35)
                                } else {
                                    fade(theme_color(LINE), 0.8)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.choose_custom_wallpaper(cx);
                                }))
                                .child(icon("image", ICON_SIZE, theme_color(MUTED)).flex_none())
                                .child(
                                    div().flex_1().min_w(px(0.)).child(
                                        div()
                                            .w_full()
                                            .truncate()
                                            .text_size(px(NAME_SIZE))
                                            .text_color(if selected {
                                                theme_color(TEXT)
                                            } else {
                                                theme_color(MUTED)
                                            })
                                            .child(name),
                                    ),
                                )
                                .when(selected, |this| {
                                    this.child(
                                        icon("check", ICON_SIZE, theme_color(ACCENT)).flex_none(),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.))
                                .text_color(theme_color(FAINT))
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
                                                .bg(theme_color(LINE))
                                                .child(
                                                    div()
                                                        .h_full()
                                                        .w(relative(opacity))
                                                        .rounded_full()
                                                        .bg(theme_color(ACCENT)),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(34.))
                                        .text_right()
                                        .text_size(px(10.))
                                        .text_color(theme_color(MUTED))
                                        .child(format!("{}%", (opacity * 100.0).round() as i32)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.))
                                .text_color(theme_color(FAINT))
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
                                            fade(theme_color(ACCENT), 0.16)
                                        } else {
                                            fade(theme_color(CONTROL_TINT), 0.04)
                                        })
                                        .text_color(if fit == "cover" {
                                            theme_color(ACCENT)
                                        } else {
                                            theme_color(MUTED)
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
                                            fade(theme_color(ACCENT), 0.16)
                                        } else {
                                            fade(theme_color(CONTROL_TINT), 0.04)
                                        })
                                        .text_color(if fit == "contain" {
                                            theme_color(ACCENT)
                                        } else {
                                            theme_color(MUTED)
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
                        .child(settings_row(
                            "图像模糊",
                            "适用于图片和动态视频",
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(self.tiny_button(
                                    "−",
                                    "wall-blur-less",
                                    cx,
                                    |this, _, cx| this.adjust_wallpaper_blur(-2, cx),
                                ))
                                .child(
                                    div()
                                        .w(px(34.))
                                        .text_center()
                                        .child(format!("{} px", self.library.prefs.wallpaper_blur)),
                                )
                                .child(self.tiny_button(
                                    "+",
                                    "wall-blur-more",
                                    cx,
                                    |this, _, cx| this.adjust_wallpaper_blur(2, cx),
                                )),
                        ))
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(theme_color(FAINT))
                                .child("支持 JPG、PNG、WebP 和 MP4。视频静音循环。"),
                        ),
                ),
        )
    }

    fn adjust_wallpaper_blur(&mut self, delta: i32, cx: &mut Context<Self>) {
        let blur = (self.library.prefs.wallpaper_blur as i32 + delta).clamp(0, 20);
        self.library.set_wallpaper_blur(blur as u32, Instant::now());
        cx.notify();
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
                    let allowed = bgm_core::prefs::is_wallpaper_path(&path.to_string_lossy());
                    if !allowed {
                        this.notice = "请选择 JPG、PNG、WebP 图片或 MP4 视频".into();
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

        let mut list = div().flex().flex_col().gap_2().px_6().pb_6();
        for entry in &entries {
            list = list.child(self.schedule_entry_row(entry, &current, &editing_id, window, cx));
        }
        if entries.is_empty() {
            list = list.child(
                div()
                    .py_10()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .rounded_lg()
                    .bg(fade(theme_color(PANEL), panel))
                    .border_1()
                    .border_color(fade(theme_color(LINE), 0.75))
                    .child(icon("clock", 30., theme_color(FAINT)))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(theme_color(MUTED))
                            .child("还没有节目，新增一档即可设置播放时段和配乐。"),
                    ),
            );
        }

        div()
            .id("schedule-page")
            .track_scroll(&self.schedule_scroll)
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .bg(fade(theme_color(BG), panel))
            .child(
                div()
                    .px_6()
                    .pt_2()
                    .pb_4()
                    .flex()
                    .items_center()
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
                                    .text_size(px(PAGE_TITLE))
                                    .font_weight(FontWeight::BOLD)
                                    .child("节目单"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme_color(MUTED))
                                    .child("按本地时间自动切换曲目与音量，每天重复。"),
                            ),
                    )
                    .child(self.hero_button(
                        "schedule-create",
                        "plus",
                        "新增节目",
                        true,
                        cx,
                        |this, window, cx| this.open_schedule_create(window, cx),
                    )),
            )
            .child(
                div()
                    .mx_6()
                    .mb_5()
                    .px_5()
                    .py_4()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .bg(fade(theme_color(PANEL), panel))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme_color(LINE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(icon("clock", 17., theme_color(ACCENT)))
                            .child(
                                div()
                                    .text_size(px(20.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(now_label),
                            )
                            .child(
                                div()
                                    .w(relative(0.38))
                                    .flex_grow()
                                    .min_w(px(0.))
                                    .text_size(px(12.))
                                    .text_color(theme_color(MUTED))
                                    .truncate()
                                    .child(format!("当前 · {current_label}")),
                            )
                            .child(
                                div()
                                    .w(relative(0.32))
                                    .text_right()
                                    .text_size(px(12.))
                                    .text_color(theme_color(MUTED))
                                    .truncate()
                                    .child(format!("下一档 · {next_label}")),
                            ),
                    )
                    .child(
                        div()
                            .relative()
                            .h(px(8.))
                            .w_full()
                            .rounded_full()
                            .bg(theme_color(SIDE))
                            .children(entries.iter().filter_map(|entry| {
                                let start = minutes_of(&entry.start)? as f32 / 1440.;
                                let end = minutes_of(&entry.end)? as f32 / 1440.;
                                Some(
                                    div()
                                        .absolute()
                                        .left(relative(start))
                                        .w(relative((end - start).max(0.)))
                                        .h_full()
                                        .rounded_full()
                                        .bg(if entry.enabled {
                                            theme_color(ACCENT).into()
                                        } else {
                                            fade(theme_color(FAINT), 0.4)
                                        }),
                                )
                            }))
                            .child(
                                div()
                                    .absolute()
                                    .left(relative(Self::minutes_of_day(&now) as f32 / 1440.))
                                    .top(px(-3.))
                                    .w(px(2.))
                                    .h(px(14.))
                                    .bg(theme_color(TEXT)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(10.))
                            .text_color(theme_color(FAINT))
                            .children(["00:00", "06:00", "12:00", "18:00", "24:00"]),
                    ),
            )
            .child(
                div()
                    .px_6()
                    .pb_3()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("每日节目"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme_color(FAINT))
                            .child(format!("{} 档", entries.len())),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .max_w(px(320.))
                            .text_size(px(11.))
                            .text_color(theme_color(FAINT))
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
                    .bg(fade(theme_color(BG), 0.5))
                    .text_size(px(11.))
                    .child(
                        div()
                            .w(px(20.))
                            .text_color(theme_color(FAINT))
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .w(px(92.))
                            .text_color(theme_color(FAINT))
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
                                    .text_color(theme_color(TEXT))
                                    .min_w(px(0.))
                                    .truncate()
                                    .child(self.song_title(&song.key)),
                            )
                            .when(playing_here, |this| {
                                this.child(
                                    div()
                                        .flex_none()
                                        .px_1()
                                        .rounded(px(4.))
                                        .bg(fade(theme_color(ACCENT), 0.14))
                                        .text_size(px(9.))
                                        .text_color(theme_color(ACCENT))
                                        .child("播放中"),
                                )
                            }),
                    )
                    .child(div().text_color(theme_color(FAINT)).child(duration))
                    .child(
                        div()
                            .w(px(96.))
                            .flex_none()
                            .truncate()
                            .text_color(theme_color(FAINT))
                            .child(category),
                    ),
            );
        }

        div()
            .mx_6()
            .mb_2()
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .bg(fade(theme_color(PANEL), panel))
            .border_1()
            .border_color(if is_current {
                fade(theme_color(ACCENT), 0.4)
            } else {
                fade(theme_color(LINE), 0.75)
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
                                if expanded {
                                    theme_color(ACCENT)
                                } else {
                                    theme_color(FAINT)
                                },
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
                            .text_color(if is_current {
                                theme_color(ACCENT)
                            } else {
                                theme_color(TEXT)
                            })
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
                                    .child(div().min_w(px(0.)).truncate().child(entry.name.clone()))
                                    .when(is_current, |this| {
                                        this.child(
                                            div()
                                                .flex_none()
                                                .px_1()
                                                .rounded(px(4.))
                                                .bg(fade(theme_color(ACCENT), 0.14))
                                                .text_size(px(9.))
                                                .text_color(theme_color(ACCENT))
                                                .child("进行中"),
                                        )
                                    })
                                    .when(!entry.enabled, |this| {
                                        this.child(
                                            div()
                                                .flex_none()
                                                .px_1()
                                                .rounded(px(4.))
                                                .bg(fade(theme_color(CONTROL_TINT), 0.06))
                                                .text_size(px(9.))
                                                .text_color(theme_color(MUTED))
                                                .child("已停用"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(10.5))
                                    .text_color(theme_color(FAINT))
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
                                        let first = entry
                                            .songs
                                            .iter()
                                            .find(|song| this.library.song(&song.key).is_some());
                                        this.apply_schedule_entry(
                                            &entry,
                                            first.map(|song| song.key.as_str()),
                                            true,
                                            true,
                                        );
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
            .text_color(if danger {
                theme_color(DANGER)
            } else {
                theme_color(MUTED)
            })
            .hover(|style| {
                style
                    .bg(fade(theme_color(CONTROL_TINT), 0.08))
                    .text_color(if danger {
                        theme_color(DANGER)
                    } else {
                        theme_color(TEXT)
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
            .bg(fade(theme_color(PANEL), panel))
            .border_1()
            .border_color(fade(theme_color(ACCENT), 0.3))
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
                                    .text_color(theme_color(FAINT))
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
                                    .text_color(theme_color(FAINT))
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
                                    .text_color(theme_color(FAINT))
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
                                fade(theme_color(ACCENT), 0.14)
                            } else {
                                fade(theme_color(CONTROL_TINT), 0.04)
                            })
                            .border_1()
                            .border_color(if enabled {
                                fade(theme_color(ACCENT), 0.35)
                            } else {
                                fade(theme_color(LINE), 1.)
                            })
                            .text_color(if enabled {
                                theme_color(ACCENT)
                            } else {
                                theme_color(MUTED)
                            })
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
                                fade(theme_color(ACCENT), 0.14)
                            } else {
                                fade(theme_color(CONTROL_TINT), 0.04)
                            })
                            .text_color(if volume_enabled {
                                theme_color(ACCENT)
                            } else {
                                theme_color(MUTED)
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
                                div()
                                    .h(px(3.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(theme_color(LINE))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(if volume_enabled {
                                                draft_volume
                                            } else {
                                                0.0
                                            }))
                                            .rounded_full()
                                            .bg(theme_color(ACCENT)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme_color(MUTED))
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
                            .text_color(theme_color(FAINT))
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
                            this.open_song_picker(PickerPurpose::AddToSchedule, window, cx);
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
                            .text_color(theme_color(FAINT))
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
                            .text_color(theme_color(FAINT))
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
            .bg(fade(theme_color(BG), 0.5 * panel))
            .text_size(px(11.5))
            .child(
                div()
                    .w(px(18.))
                    .text_color(theme_color(FAINT))
                    .child(format!("{}", index + 1)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(div().text_color(theme_color(TEXT)).truncate().child(title))
                    .child(
                        div()
                            .text_size(px(9.5))
                            .text_color(theme_color(FAINT))
                            .truncate()
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
                    this.open_song_picker(PickerPurpose::ReplaceInSchedule(index), window, cx);
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

enum WallpaperSource {
    None,
    Gradient(String),
    Image {
        asset: wallpaper::Asset,
        contain: bool,
    },
    Video {
        path: PathBuf,
        contain: bool,
    },
}

impl WallpaperSource {
    fn image_request(&self, blur: u32) -> Option<wallpaper::Request> {
        let Self::Image { asset, .. } = self else {
            return None;
        };
        Some(wallpaper::Request {
            asset: asset.clone(),
            blur,
        })
    }
}

struct WallpaperView {
    has_wallpaper: bool,
    opacity: f32,
    source: WallpaperSource,
}

struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme_color(TEXT))
            .text_color(theme_color(PANEL))
            .text_xs()
            .max_w(px(380.))
            .child(self.0.clone())
    }
}

fn detail_button(
    id: &'static str,
    glyph: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(36.))
        .px_4()
        .flex()
        .items_center()
        .gap_2()
        .rounded_full()
        .border_1()
        .border_color(fade(gpui::white(), 0.28))
        .bg(fade(gpui::white(), 0.09))
        .cursor_pointer()
        .hover(|style| style.bg(fade(gpui::white(), 0.2)))
        .text_size(px(13.))
        .child(icon(glyph, 14., gpui::white()))
        .child(label)
}

fn settings_row(title: &str, detail: &str, accessory: impl IntoElement) -> gpui::AnyElement {
    div()
        .min_h(px(54.))
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(theme_color(LINE))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_ellipsis()
                        .line_clamp(1)
                        .child(title.to_owned()),
                )
                .when(!detail.is_empty(), |row| {
                    row.child(
                        div()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child(detail.to_owned()),
                    )
                }),
        )
        .child(accessory)
        .into_any_element()
}

fn format_metric(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(|| "—".to_owned(), |value| format!("{value:.1} {unit}"))
}

fn audio_chart(
    values: Vec<f32>,
    waveform: bool,
    progress: f32,
    loop_region: Option<(f32, f32)>,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let width = bounds.size.width;
            let height = bounds.size.height;
            if let Some((start, end)) = loop_region {
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + width * start, bounds.top()),
                        size(width * (end - start), height),
                    ),
                    fade(theme_color(FAVORITE), 0.21),
                ));
            }
            let step = width / values.len().max(1) as f32;
            for (index, value) in values.into_iter().enumerate() {
                let bar = (height * value.clamp(0.0, 1.0)).max(px(1.));
                let y = if waveform {
                    (height - bar) / 2.0
                } else {
                    height - bar
                };
                let x = step * index as f32;
                let active = !waveform || x < width * progress;
                window.paint_quad(fill(
                    Bounds::new(
                        point(bounds.left() + x, bounds.top() + y),
                        size((step - px(1.)).max(px(1.)), bar),
                    ),
                    if active {
                        theme_color(ACCENT)
                    } else {
                        rgb(0xa7afbf)
                    },
                ));
            }
            if waveform && progress > 0.0 {
                window.paint_quad(fill(
                    Bounds::new(
                        point(
                            bounds.left() + width * progress.clamp(0.0, 1.0),
                            bounds.top(),
                        ),
                        size(px(1.), height),
                    ),
                    theme_color(TEXT),
                ));
            }
        },
    )
    .w_full()
    .h(if waveform { px(106.) } else { px(64.) })
}

#[derive(Debug, PartialEq, Eq)]
enum QueueChange {
    Unchanged,
    CurrentMoved,
    CurrentRemoved,
}

fn update_queue_keys(
    queue: &mut Vec<String>,
    index: &mut usize,
    changes: &[bgm_core::KeyChange],
    removed: &[String],
) -> QueueChange {
    let replacements: HashMap<_, _> = changes
        .iter()
        .filter(|change| change.from != change.to)
        .map(|change| (change.from.as_str(), change.to.as_str()))
        .collect();
    let removed: HashSet<_> = removed.iter().map(String::as_str).collect();
    let old_index = *index;
    let mut cursor = 0;
    let mut removed_before = 0;
    let mut status = QueueChange::Unchanged;
    queue.retain_mut(|key| {
        let previous = cursor;
        cursor += 1;
        if removed.contains(key.as_str()) {
            removed_before += usize::from(previous < old_index);
            if previous == old_index {
                status = QueueChange::CurrentRemoved;
            }
            return false;
        }
        if let Some(replacement) = replacements.get(key.as_str()) {
            *key = (*replacement).to_owned();
            if previous == old_index {
                status = QueueChange::CurrentMoved;
            }
        }
        true
    });
    *index = old_index
        .saturating_sub(removed_before)
        .min(queue.len().saturating_sub(1));
    status
}

fn detach_queue_item(
    queue: &mut Vec<String>,
    current_index: &mut usize,
    index: usize,
) -> Option<bool> {
    queue.get(index)?;
    let was_current = *current_index == index;
    queue.remove(index);
    if index < *current_index {
        *current_index -= 1;
    }
    if *current_index >= queue.len() {
        *current_index = 0;
    }
    Some(was_current)
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
    let mut color = u32::from_str_radix(hex, 16).map_or_else(|_| theme_color(ACCENT), rgb);
    let luminance = |color: gpui::Rgba| {
        let linear = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    };
    while luminance(color) > 0.18 {
        color.r *= 0.94;
        color.g *= 0.94;
        color.b *= 0.94;
    }
    color
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_selection_keeps_pending_tracks_unique_and_reuses_saved_records() {
        let mut catalog = SubtitleCatalog::default();
        catalog.select_track("music/new".into());
        catalog.select_track("music/new".into());
        assert_eq!(catalog.pending, ["music/new"]);

        let saved =
            serde_json::from_value(json!({"key": "music/saved", "createdAtMs": 0})).unwrap();
        catalog.records = Some(Ok(vec![saved]));
        catalog.select_track("music/saved".into());
        assert_eq!(catalog.pending, ["music/new"]);
        assert_eq!(catalog.generation_key.as_deref(), Some("music/saved"));
    }

    #[test]
    fn natural_end_fades_in_without_an_outgoing_voice_and_reload_uses_transport() {
        assert_eq!(TrackStart::AfterEnd.fade_in(2.0, 0.0), 2.0);
        assert_eq!(TrackStart::AfterEnd.fade_in(0.0, 0.0), 0.0);
        assert_eq!(TrackStart::Selected.fade_in(2.0, 0.0), 0.0);
        assert_eq!(TrackStart::Selected.fade_in(2.0, 2.0), 2.0);
        let restore = TrackStart::Restore {
            position: Duration::from_secs(15),
            record_on_resume: false,
        };
        assert_eq!(restore.fade_in(2.0, 2.0), 0.0);
        assert_eq!(restore.position(), Some(Duration::from_secs(15)));
        assert!(!restore.record_on_resume());
    }

    #[test]
    fn current_track_follows_its_key_after_earlier_removal() {
        let mut queue = ["A", "B", "C"].map(str::to_owned).to_vec();
        let mut index = 1;
        let status = update_queue_keys(&mut queue, &mut index, &[], &["A".into()]);
        assert_eq!(status, QueueChange::Unchanged);
        assert_eq!(index, 0);
        assert_eq!(queue[index], "B");
    }

    #[test]
    fn renamed_current_keeps_position_and_removed_current_selects_successor() {
        let mut queue = ["A", "B", "C", "D"].map(str::to_owned).to_vec();
        let mut index = 1;
        let changes = [bgm_core::KeyChange {
            from: "B".into(),
            to: "renamed".into(),
        }];
        assert_eq!(
            update_queue_keys(&mut queue, &mut index, &changes, &["A".into()]),
            QueueChange::CurrentMoved,
        );
        assert_eq!(queue[index], "renamed");
        assert_eq!(
            update_queue_keys(&mut queue, &mut index, &[], &["renamed".into()]),
            QueueChange::CurrentRemoved,
        );
        assert_eq!(queue[index], "C");
    }

    #[test]
    fn queue_removal_keeps_the_current_key_or_selects_its_successor() {
        let mut queue = ["A", "B", "C", "D"].map(str::to_owned).to_vec();
        let mut index = 1;
        assert_eq!(detach_queue_item(&mut queue, &mut index, 0), Some(false));
        assert_eq!(queue[index], "B");
        assert_eq!(detach_queue_item(&mut queue, &mut index, 0), Some(true));
        assert_eq!(queue[index], "C");
        index = 1;
        assert_eq!(detach_queue_item(&mut queue, &mut index, 1), Some(true));
        assert_eq!(queue[index], "C");
        assert_eq!(detach_queue_item(&mut queue, &mut index, 9), None);
        assert_eq!(detach_queue_item(&mut queue, &mut index, 0), Some(true));
        assert!(queue.is_empty());
        assert_eq!(index, 0);
    }

    #[test]
    fn queue_removes_the_selected_occurrence_of_a_repeated_song() {
        let mut queue = ["A", "B", "A", "C"].map(str::to_owned).to_vec();
        let mut index = 2;
        assert_eq!(detach_queue_item(&mut queue, &mut index, 2), Some(true));
        assert_eq!(queue, ["A", "B", "C"]);
        assert_eq!(queue[index], "C");
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
        let due = scheduler::latest_due_song(&entry, 9 * 60 + 4, |_| true).cloned();
        assert_eq!(due.map(|song| song.key), Some("b".into()));
    }

    #[test]
    fn minutes_of_parses_and_rejects() {
        assert_eq!(minutes_of("09:30"), Some(9 * 60 + 30));
        assert_eq!(minutes_of("24:00"), None);
        assert_eq!(minutes_of("bad"), None);
    }
}
