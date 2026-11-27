//! 曲库窗口。曲库、节目单和歌曲页共用底部播放栏，交互走 GPUI 动作与系统文件框。

mod audio_tools;
mod controls;
mod folders;
mod playlist_tools;
mod speech_tools;

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
use crate::library::sherpa::{self, RuntimeDownload, RuntimeProgress, RuntimeStage, RuntimeStatus};
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
    },
    NewCategory,
    EditCategory {
        id: String,
    },
    Move {
        key: String,
        category: String,
    },
    MoveBatch {
        keys: Vec<String>,
        category: String,
    },
    Import {
        files: Vec<PathBuf>,
        folder_id: Option<String>,
        category: String,
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
    AudioTools,
    PlaylistMigration,
    Speech,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Wallpaper,
    Playback,
    Export,
    Models,
    Integrations,
}

#[derive(Clone)]
struct Toast {
    id: u64,
    message: String,
    is_error: bool,
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
    original: ScheduleEntry,
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
    audio_tools: audio_tools::AudioTools,
    playlist_tools: playlist_tools::PlaylistTools,
    speech_tools: speech_tools::SpeechTools,
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
    sherpa_runtime_status: sherpa::RuntimeStatus,
    sherpa_download: Option<sherpa::RuntimeDownload>,
    sherpa_download_progress: Option<sherpa::RuntimeProgress>,
    asr_url_field: Entity<TextField>,
    _observe_asr_url: Subscription,
    model_advanced_open: bool,
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
    api_reference_open: bool,
    fonts: Vec<SystemFont>,
    font_picker_open: bool,
    font_filter: Entity<TextField>,
    _observe_font_filter: Subscription,
    settings_feedback: Option<String>,
    settings_focus: FocusHandle,
    dialog_focus: FocusHandle,
    picker_focus: FocusHandle,
    close_focus: FocusHandle,
    folders_focus: FocusHandle,
    track_menu_focus: FocusHandle,
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
    queue_scroll: gpui::UniformListScrollHandle,
    schedule_scroll: gpui::ScrollHandle,
    shelf_scroll: gpui::ScrollHandle,
    filter: LibraryFilter,
    query: String,
    dialog: Option<Dialog>,
    dialog_feedback: Option<String>,
    action_menu: Option<TrackMenu>,
    folders_modal_open: bool,
    folder_menu: Option<(String, Point<Pixels>)>,
    folders_warnings_open: bool,
    keyboard_focus_visible: bool,
    toast: Option<Toast>,
    toast_seq: u64,
    toast_task: Option<gpui::Task<()>>,
    picker: Option<SongPicker>,
    ffmpeg_detected: Option<Option<PathBuf>>,
    wallpaper_request: Option<wallpaper::Request>,
    wallpaper_loading: bool,
    wallpaper_image: Option<Arc<gpui::RenderImage>>,
    wallpaper_video: Option<wallpaper::Video>,
    wallpaper_video_frame: Option<wallpaper::VideoFrame>,
    main_window: Option<gpui::AnyWindowHandle>,
    builtin_wallpapers: Vec<wallpaper::BuiltinWallpaper>,
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
    schedule_feedback: Option<String>,
    schedule_discard_pending: bool,
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

        let watcher = LibraryWatcher::start_many(&library.watch_roots()).ok();
        if watcher.is_none() && !library.online_folders().is_empty() {
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
                        this.poll_audio_tools(cx);
                        this.poll_speech_tools(second_elapsed, cx);
                        let api_changed = this.poll_api(cx);
                        let diagnostics_changed = this.poll_diagnostics(cx);
                        let subtitles_changed = this.poll_track_lyrics(cx);
                        this.poll_desktop_lyrics(cx);
                        let model_download_changed = this.poll_model_download();
                        let sherpa_download_changed = this.poll_sherpa_download();
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
                            || sherpa_download_changed
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
        let observe_asr_url = cx.observe(&asr_url_field, |_, _, cx| cx.notify());
        let autoplay = library.prefs.autoplay_on_start;
        let mut workspace = Self {
            audio_tools: audio_tools::AudioTools::new(
                library.config_path(),
                library.indexed_roots(),
            ),
            playlist_tools: playlist_tools::PlaylistTools::new(cx),
            speech_tools: speech_tools::SpeechTools::new(&library, cx),
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
            sherpa_runtime_status: sherpa::current_runtime_status(),
            sherpa_download: None,
            sherpa_download_progress: None,
            asr_url_field,
            _observe_asr_url: observe_asr_url,
            model_advanced_open: false,
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
            api_reference_open: false,
            fonts: Vec::new(),
            font_picker_open: false,
            font_filter,
            _observe_font_filter: observe_font_filter,
            settings_feedback: None,
            settings_focus: cx.focus_handle(),
            dialog_focus: cx.focus_handle(),
            picker_focus: cx.focus_handle(),
            close_focus: cx.focus_handle(),
            folders_focus: cx.focus_handle(),
            track_menu_focus: cx.focus_handle(),
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
            queue_scroll: Default::default(),
            schedule_scroll: Default::default(),
            shelf_scroll: Default::default(),
            filter: LibraryFilter::All,
            query: String::new(),
            dialog: None,
            dialog_feedback: None,
            action_menu: None,
            folders_modal_open: false,
            folder_menu: None,
            folders_warnings_open: false,
            keyboard_focus_visible: false,
            toast: None,
            toast_seq: 0,
            toast_task: None,
            picker: None,
            ffmpeg_detected: None,
            wallpaper_request: None,
            wallpaper_loading: false,
            wallpaper_image: None,
            wallpaper_video: None,
            wallpaper_video_frame: None,
            main_window: None,
            builtin_wallpapers: wallpaper::discover_builtin(),
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
            schedule_feedback: None,
            schedule_discard_pending: false,
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
        if self.modal_open() {
            return;
        }
        self.toggle_playback();
        cx.notify();
    }

    fn microphone_active(&self) -> bool {
        self.audio_tools.recording() || self.speech_tools.recording()
    }

    fn toggle_playback(&mut self) {
        self.speech_tools.stop_preview();
        if self.microphone_active() {
            self.notice = "麦克风录音期间已暂停播放".into();
            return;
        }
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
        if self.modal_open() {
            return;
        }
        if self.view != AppView::Library {
            self.filter = LibraryFilter::All;
        }
        self.queue_open = false;
        self.diagnostics_open = false;
        self.navigate(AppView::Library, None, cx);
        let search = self.search.clone();
        cx.on_next_frame(window, move |_, window, cx| {
            window.focus(&search.focus_handle(cx))
        });
        self.search
            .update(cx, |field, cx| field.select_entire_text(cx));
    }

    fn active_surface_focus(&self) -> &FocusHandle {
        if self.close_prompt {
            &self.close_focus
        } else if self.folders_modal_open {
            &self.folders_focus
        } else if self.settings_page.is_some() {
            &self.settings_focus
        } else if self.picker.is_some() {
            &self.picker_focus
        } else if self.dialog.is_some() {
            &self.dialog_focus
        } else if self.action_menu.is_some() {
            &self.track_menu_focus
        } else {
            &self.focus
        }
    }

    pub(super) fn after_button_click(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.keyboard_focus_visible && !self.any_text_field_focused(window, cx) {
            window.focus(self.active_surface_focus());
        }
    }

    fn show_toast(&mut self, message: impl Into<String>, is_error: bool, cx: &mut Context<Self>) {
        let message = message.into();
        self.notice = message.clone();
        self.toast_seq = self.toast_seq.wrapping_add(1);
        let id = self.toast_seq;
        self.toast = Some(Toast {
            id,
            message,
            is_error,
        });
        let delay = if is_error {
            Duration::from_millis(4500)
        } else {
            Duration::from_secs(3)
        };
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|toast| toast.id == id) {
                    this.toast = None;
                    cx.notify();
                }
            });
        }));
    }

    fn action_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        icon_name: Option<&'static str>,
        tone: controls::ButtonTone,
        enabled: bool,
        compact: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        controls::ui_button(
            id,
            label,
            icon_name,
            tone,
            enabled,
            compact,
            self.keyboard_focus_visible,
            cx,
            on_click,
        )
        .into_any_element()
    }

    fn move_keyboard_focus(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.keyboard_focus_visible = true;
        let scope = if self.close_prompt {
            Some(&self.close_focus)
        } else if self.folders_modal_open {
            Some(&self.folders_focus)
        } else if self.settings_page.is_some() {
            Some(&self.settings_focus)
        } else if self.picker.is_some() {
            Some(&self.picker_focus)
        } else if self.dialog.is_some() {
            Some(&self.dialog_focus)
        } else if self.action_menu.is_some() {
            Some(&self.track_menu_focus)
        } else {
            None
        };
        let before = window.focused(cx);
        let step = |window: &mut Window| {
            if forward {
                window.focus_next();
            } else {
                window.focus_prev();
            }
        };
        step(window);
        let Some(scope) = scope else {
            cx.notify();
            return;
        };
        let first = window.focused(cx);
        while !scope.contains_focused(window, cx) {
            step(window);
            if window.focused(cx) == first {
                window.focus(
                    before
                        .as_ref()
                        .filter(|focus| scope.contains(focus, window))
                        .unwrap_or(scope),
                );
                break;
            }
        }
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
                self.model_advanced_open = false;
                window.focus(&self.focus);
            }
            cx.notify();
            return;
        }
        if self.folders_modal_open {
            if self.folder_menu.is_some() {
                self.folder_menu = None;
                window.focus(&self.folders_focus);
            } else {
                self.close_folders_modal(window, cx);
            }
            cx.notify();
            return;
        }
        if self.diagnostics_open || self.queue_open {
            self.diagnostics_open = false;
            self.queue_open = false;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if self.picker.is_some() {
            self.picker = None;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if (self.now_playing_open || self.track_transition.is_some()) && self.dialog.is_none() {
            self.set_track_page(false);
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if self.action_menu.is_some() {
            self.action_menu = None;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if self.dialog.is_some() {
            self.dialog = None;
            window.focus(&self.focus);
            cx.notify();
            return;
        }
        if self.view == AppView::Schedule && self.schedule_draft.is_some() {
            self.cancel_schedule_edit(window, cx);
            return;
        }
        if self.view == AppView::Speech && self.dismiss_speech_options(cx) {
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
                self.clear_search(cx);
            }
            cx.notify();
        }
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(picker) = &self.picker {
            if !picker.selected.is_empty() && !self.picker_search.read(cx).has_marked_text() {
                self.confirm_song_picker(window, cx);
            }
            return;
        }
        let Some(dialog) = self.dialog.clone() else {
            if self.view == AppView::PlaylistMigration && self.confirm_playlist_link(window, cx) {
                return;
            }
            if self.search.focus_handle(cx).is_focused(window)
                && !self.search.read(cx).has_marked_text()
            {
                window.focus(&self.focus);
                cx.notify();
            }
            return;
        };
        if [
            &self.dialog_field,
            &self.category_detail_field,
            &self.loop_start_field,
            &self.loop_end_field,
        ]
        .iter()
        .any(|field| field.focus_handle(cx).is_focused(window) && field.read(cx).has_marked_text())
        {
            return;
        }
        let result = match dialog {
            Dialog::EditTrack { key, category } => {
                let title = self.dialog_field.read(cx).text().trim().to_owned();
                self.library
                    .update_track(&key, &title, &category)
                    .map(|mutation| self.apply_mutation(mutation))
            }
            Dialog::NewCategory => {
                let name = self.dialog_field.read(cx).text().trim().to_owned();
                let id = self.category_detail_field.read(cx).text().trim().to_owned();
                self.library
                    .create_category(&name, (!id.is_empty()).then_some(id.as_str()))
                    .map(|id| {
                        self.filter = LibraryFilter::Category(id);
                        self.api_library_dirty = true;
                        self.notice = format!("已新增分类 {name}");
                    })
            }
            Dialog::EditCategory { id } => {
                let name = self.dialog_field.read(cx).text().trim().to_owned();
                let color = self.category_detail_field.read(cx).text().trim().to_owned();
                self.library
                    .update_category(&id, &name, (!color.is_empty()).then_some(color.as_str()))
                    .map(|_| {
                        self.api_library_dirty = true;
                        self.notice = format!("已更新分类 {name}");
                    })
            }
            Dialog::Loop { key } => self.save_loop_from_editor(&key, cx),
            Dialog::FileReport { .. } => Ok(()),
            Dialog::Move { key, category } => {
                let res = self.library.move_song(&key, &category);
                match res {
                    Ok(mutation) => {
                        self.apply_mutation(mutation);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Dialog::MoveBatch { keys, category } => {
                match self.library.move_batch(&keys, &category) {
                    Ok(mutation) => {
                        for change in &mutation.changes {
                            self.selected_songs.remove(&change.from);
                        }
                        self.apply_mutation(mutation);
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            Dialog::Import {
                files,
                folder_id,
                category,
            } => {
                let Some(folder_id) = folder_id else {
                    self.dialog_feedback = Some("请选择目标音乐文件夹".into());
                    cx.notify();
                    return;
                };
                let mutation = self.library.import_to_folder(&folder_id, &category, &files);
                self.apply_mutation(mutation);
                Ok(())
            }
        };
        if !matches!(self.dialog, Some(Dialog::FileReport { .. })) {
            match result {
                Ok(()) => {
                    self.dialog = None;
                    self.dialog_feedback = None;
                    window.focus(&self.focus);
                }
                Err(error) => self.dialog_feedback = Some(error),
            }
        }
        cx.notify();
    }

    fn clear_search(&mut self, cx: &mut Context<Self>) {
        self.search.update(cx, |field, cx| field.set_text("", cx));
        self.query.clear();
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
        let autoplay = autoplay && !self.microphone_active();
        if autoplay {
            self.speech_tools.stop_preview();
        }
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
        self.speech_tools.stop_preview();
        if self.microphone_active() {
            return;
        }
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
                task.finish(Err("请先在「设置 → 语音识别」加载 ASR 模型".into()));
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
                self.library.rescan();
                self.reconcile_snapshot();
                self.install_watcher();
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
            .update(cx, |field, cx| field.set_text(start.clone(), cx));
        self.schedule_end_field
            .update(cx, |field, cx| field.set_text(end.clone(), cx));
        self.schedule_feedback = None;
        self.schedule_discard_pending = false;
        self.schedule_draft = Some(ScheduleDraft {
            original: ScheduleEntry {
                start,
                end,
                enabled: true,
                ..Default::default()
            },
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
        self.schedule_feedback = None;
        self.schedule_discard_pending = false;
        self.schedule_draft = Some(ScheduleDraft {
            original: entry.clone(),
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

    fn schedule_form(&self, cx: &App) -> Option<ScheduleEntry> {
        let draft = self.schedule_draft.as_ref()?;
        let field_text = |field: &Entity<TextField>| field.read(cx).text().trim().to_owned();
        Some(ScheduleEntry {
            id: draft.original.id.clone(),
            name: field_text(&self.schedule_name_field),
            start: field_text(&self.schedule_start_field),
            end: field_text(&self.schedule_end_field),
            songs: draft
                .songs
                .iter()
                .enumerate()
                .map(|(index, song)| ScheduleSong {
                    key: song.key.clone(),
                    start: field_text(&draft.song_start_fields[index]),
                    end: field_text(&draft.song_end_fields[index]),
                })
                .collect(),
            volume: draft
                .volume_enabled
                .then_some((draft.volume * 100.0).round() / 100.0),
            enabled: draft.enabled,
        })
    }

    fn cancel_schedule_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.schedule_discard_pending {
            self.schedule_discard_pending = false;
            cx.notify();
            return;
        }
        let changed = self
            .schedule_form(cx)
            .zip(self.schedule_draft.as_ref())
            .is_some_and(|(entry, draft)| entry != draft.original);
        if changed {
            self.schedule_discard_pending = true;
            cx.notify();
        } else {
            self.close_schedule_draft(window, cx);
        }
    }

    fn close_schedule_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.schedule_draft = None;
        self.schedule_feedback = None;
        self.schedule_discard_pending = false;
        window.focus(&self.focus);
        cx.notify();
    }

    fn save_schedule_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut entry) = self.schedule_form(cx) else {
            return;
        };
        self.schedule_discard_pending = false;
        if entry.name.is_empty() {
            self.schedule_feedback = Some("请填写节目名称。".into());
            cx.notify();
            return;
        }
        let (Some(start), Some(end)) = (minutes_of(&entry.start), minutes_of(&entry.end)) else {
            self.schedule_feedback = Some("请用 HH:MM 输入节目时间，例如 09:30。".into());
            cx.notify();
            return;
        };
        if end <= start {
            self.schedule_feedback = Some("请选择同一天内的时间，结束时间须晚于开始时间。".into());
            cx.notify();
            return;
        }
        if entry.songs.is_empty() {
            self.schedule_feedback = Some("请先添加至少一首歌曲。".into());
            cx.notify();
            return;
        }
        entry.start = format_minutes(start);
        entry.end = format_minutes(end);

        let mut next = self.library.schedule.clone();
        if entry.id.is_empty() {
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |time| time.as_millis());
            entry.id = format!("slot-{millis}");
            next.entries.push(entry);
        } else if let Some(slot) = next.entries.iter_mut().find(|item| item.id == entry.id) {
            *slot = entry;
        } else {
            self.schedule_feedback = Some("这个节目已被删除，请重新创建。".into());
            cx.notify();
            return;
        }
        match self.library.save_schedule(next) {
            Ok(_) => {
                self.notice = "节目单已保存".into();
                self.close_schedule_draft(window, cx);
                self.sync_to_current_program();
            }
            Err(error) => self.schedule_feedback = Some(error),
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
            self.schedule_feedback = Some("请先填写有效的节目起止时间。".into());
            cx.notify();
            return;
        };
        if end <= start || draft.songs.is_empty() {
            self.schedule_feedback = Some("请先设置时段并添加曲目。".into());
            cx.notify();
            return;
        }
        self.schedule_feedback = None;
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
        if self
            .picker
            .as_ref()
            .is_none_or(|picker| picker.selected.is_empty())
        {
            return;
        }
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
        self.dialog_feedback = None;
        self.dialog = Some(Dialog::Loop {
            key: key.to_owned(),
        });
        let field = self.loop_start_field.clone();
        cx.on_next_frame(window, move |_this, window, cx| {
            window.focus(&field.focus_handle(cx));
        });
        cx.notify();
    }

    fn save_loop_from_editor(&mut self, key: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let seconds = |field: &Entity<TextField>| {
            field
                .read(cx)
                .text()
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
        };
        let (start, end) = seconds(&self.loop_start_field)
            .zip(seconds(&self.loop_end_field))
            .ok_or("请输入有效的秒数")?;
        if start < 0.0 {
            return Err("起点不能为负数".into());
        }
        if end - start < MIN_LOOP_SPAN {
            return Err(format!("循环终点至少要比起点晚 {MIN_LOOP_SPAN} 秒"));
        }
        let duration = self
            .library
            .song(key)
            .and_then(|song| song.duration_ms)
            .map(|ms| ms as f64 / 1000.0);
        if let Some(duration) = duration.filter(|duration| end > duration + 0.5) {
            return Err(format!(
                "循环终点超出曲目时长 {}",
                format_time(Duration::from_secs_f64(duration))
            ));
        }

        let enabled = self.loop_enabled_pending;
        self.library.save_loop_point(key, start, end, enabled)?;
        let title = self.song_title(key);
        self.notice = if enabled {
            format!("《{title}》已启用循环 {start:.2}–{end:.2} 秒")
        } else {
            format!("《{title}》循环已关闭")
        };
        self.refresh_loop_region();
        Ok(())
    }

    // ---------- 文件监听 ----------

    fn install_watcher(&mut self) {
        self.watcher = LibraryWatcher::start_many(&self.library.watch_roots()).ok();
        self.last_fallback = Instant::now();
        if self.watcher.is_none() && !self.library.online_folders().is_empty() {
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
        if self.watcher.is_none() && !self.library.watch_roots().is_empty() {
            self.install_watcher();
        }
        self.last_fallback = now;
        self.watch_scanning = true;
        let scan = self.library.watch_scan();
        let task = cx.background_executor().spawn(async move { scan.run() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                this.watch_scanning = false;
                if let Some((revision, snapshot)) = result
                    && let Some(change) = this.library.apply_watch_scan(revision, snapshot)
                {
                    this.apply_library_change(change, cx);
                    this.install_watcher();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn apply_library_change(&mut self, change: LibraryChange, cx: &mut Context<Self>) {
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
        let mut message = if notes.is_empty() {
            "曲库已更新".to_owned()
        } else {
            format!("曲库已更新：{}", notes.join("、"))
        };
        if !migration_warnings.is_empty() {
            message.push_str(&format!("。{}", migration_warnings.join("。")));
        }
        self.show_toast(message, !migration_warnings.is_empty(), cx);
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
            self.dialog_feedback = None;
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
        self.rescan_library(cx);
        window.set_window_title(&self.library.window_title());
        cx.notify();
    }

    fn rescan_library(&mut self, cx: &mut Context<Self>) {
        self.library.rescan();
        self.api_config = api::Config::load(self.library.config_path());
        if let Some(server) = &self.api {
            server.update_config(self.api_config.clone());
        }
        self.reconcile_snapshot();
        self.audio.set_speed(self.library.prefs.playback_rate);
        self.install_watcher();
        self.poll_audio_tools(cx);
        if !self.library.has_folders() {
            self.show_toast("请先添加音乐文件夹", false, cx);
        } else if self.library.online_folders().is_empty() {
            self.show_toast("已添加的音乐文件夹当前均离线", true, cx);
        } else {
            self.show_toast(
                format!("已重新扫描 {} 首曲目", self.library.snapshot.songs.len()),
                false,
                cx,
            );
        }
    }

    fn default_import_target(&self) -> (Option<String>, String) {
        let folder_id = self
            .library
            .online_folders()
            .first()
            .map(|folder| folder.id.clone());
        let category = match &self.filter {
            LibraryFilter::Category(id) if id != crate::library::UNCATEGORIZED_CATEGORY_ID => {
                id.clone()
            }
            _ => self
                .library
                .snapshot
                .categories
                .first()
                .map(|cat| cat.id.clone())
                .unwrap_or_else(|| "epic".to_owned()),
        };
        (folder_id, category)
    }

    fn import_files(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.library.online_folders().is_empty() {
            self.show_toast("请先添加并连接至少一个音乐文件夹", true, cx);
            self.open_folders_modal(window, cx);
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("导入音频".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(files))) = receiver.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if files.is_empty() {
                    this.show_toast("没有选中支持的音频文件", false, cx);
                } else {
                    let (folder_id, category) = this.default_import_target();
                    this.dialog_feedback = None;
                    this.dialog = Some(Dialog::Import {
                        files,
                        folder_id,
                        category,
                    });
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
            &[
                PromptButton::ok("移到曲库回收站"),
                PromptButton::cancel("取消"),
            ],
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
        self.dialog_feedback = None;
        self.dialog = Some(Dialog::EditTrack {
            key: key.to_owned(),
            category,
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
        self.dialog_feedback = None;
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
        self.dialog_feedback = None;
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
        let folders = self.folders_modal_layer(window, cx);
        let action_menu = self.action_menu_layer(window, cx);
        let dialog = self.dialog_layer(window, cx);
        let picker = self.picker_layer(cx);
        let queue = self.queue_layer(cx);
        let diagnostics = self.diagnostics_layer(cx);
        let settings = self.settings_layer(window, cx);
        let close_prompt = self.close_layer(cx);
        let toast = self.toast_layer();

        div()
            .id("workspace")
            .key_context("Library")
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.keyboard_focus_visible {
                        this.keyboard_focus_visible = false;
                        cx.notify();
                    }
                }),
            )
            .on_action(cx.listener(Self::play_pause))
            .on_action(cx.listener(Self::seek_back))
            .on_action(cx.listener(Self::seek_forward))
            .on_action(cx.listener(Self::volume_up))
            .on_action(cx.listener(Self::volume_down))
            .on_action(cx.listener(Self::toggle_mute))
            .on_action(
                cx.listener(|this, _: &crate::actions::FocusNext, window, cx| {
                    this.move_keyboard_focus(true, window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::FocusPrevious, window, cx| {
                    this.move_keyboard_focus(false, window, cx);
                }),
            )
            .on_action(cx.listener(Self::focus_search))
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenLibrary, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::Library, Some(LibraryFilter::All), cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenFavorites, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::Library, Some(LibraryFilter::Favorites), cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenSchedule, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::Schedule, None, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenSubtitles, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::Subtitles, None, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenAudioTools, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::AudioTools, None, cx);
                    }
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::actions::OpenPlaylistMigration, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::PlaylistMigration, None, cx);
                    }
                },
            ))
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenSpeech, window, cx| {
                    if !this.modal_open() {
                        window.focus(&this.focus);
                        this.navigate(AppView::Speech, None, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ImportMusic, window, cx| {
                    if !this.modal_open() {
                        this.import_files(&ClickEvent::default(), window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::RescanLibrary, window, cx| {
                    if !this.modal_open() {
                        this.rescan(window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenFolders, window, cx| {
                    this.open_folders_modal(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &crate::actions::ToggleQueue, _, cx| {
                if !this.modal_open() {
                    this.toggle_queue(cx);
                }
            }))
            .on_action(
                cx.listener(|this, _: &crate::actions::PreviousTrack, _, cx| {
                    this.skip_track(false, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &crate::actions::NextTrack, _, cx| {
                this.skip_track(true, cx);
            }))
            .on_action(
                cx.listener(|this, _: &crate::actions::OpenSettings, window, cx| {
                    this.open_settings(window, cx);
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::TrackPage, window, cx| {
                    if !this.modal_open() {
                        this.toggle_track_page(window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::actions::ToggleDesktopLyrics, _, cx| {
                    if !this.modal_open() {
                        this.toggle_desktop_lyrics(cx);
                    }
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
            .children(folders)
            .children(dialog)
            .children(picker)
            .children(queue)
            .children(diagnostics)
            .children(settings)
            .children(close_prompt)
            .children(toast)
    }
}

impl Workspace {
    fn any_text_field_focused(&self, window: &Window, cx: &App) -> bool {
        let mut focused = [
            &self.search,
            &self.asr_url_field,
            &self.api_token_field,
            &self.api_cors_field,
            &self.font_filter,
            &self.dialog_field,
            &self.category_detail_field,
            &self.loop_start_field,
            &self.loop_end_field,
            &self.schedule_name_field,
            &self.schedule_start_field,
            &self.schedule_end_field,
            &self.picker_search,
            &self.segment_fields[0],
            &self.segment_fields[1],
            &self.segment_fields[2],
        ]
        .into_iter()
        .any(|field| field.focus_handle(cx).is_focused(window));
        if !focused && let Some(draft) = &self.schedule_draft {
            focused = draft
                .song_start_fields
                .iter()
                .chain(draft.song_end_fields.iter())
                .any(|field| field.focus_handle(cx).is_focused(window));
        }
        focused
            || self.playlist_tools.any_field_focused(window, cx)
            || self.speech_tools.any_field_focused(window, cx)
    }

    fn toast_layer(&self) -> Option<gpui::AnyElement> {
        let toast = self.toast.as_ref()?;
        let accent = if toast.is_error { DANGER } else { ACCENT };
        Some(
            div()
                .absolute()
                .bottom(px(PLAYER_HEIGHT + 16.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(px(520.))
                        .px_4()
                        .py_2()
                        .rounded_full()
                        .bg(theme_color(PANEL))
                        .border_1()
                        .border_color(fade(theme_color(accent), 0.4))
                        .shadow_lg()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(
                            if toast.is_error { "alert" } else { "check" },
                            14.,
                            theme_color(accent),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme_color(TEXT))
                                .truncate()
                                .child(toast.message.clone()),
                        ),
                )
                .into_any_element(),
        )
    }

    fn interface_font(&self) -> SharedString {
        if self.library.prefs.ui_font == "system" {
            ".SystemUIFont".into()
        } else {
            self.library.prefs.ui_font.clone().into()
        }
    }

    fn modal_open(&self) -> bool {
        self.dialog.is_some()
            || self.picker.is_some()
            || self.settings_page.is_some()
            || self.folders_modal_open
            || self.close_prompt
    }

    fn navigate(&mut self, page: AppView, filter: Option<LibraryFilter>, cx: &mut Context<Self>) {
        if self.modal_open() {
            return;
        }
        if let Some(filter) = filter {
            if self.filter != filter {
                self.selecting = false;
                self.selected_songs.clear();
            }
            self.filter = filter;
        }
        self.set_track_page(false);
        self.action_menu = None;
        self.folder_menu = None;
        self.view = page;
        if page == AppView::Subtitles {
            self.refresh_sherpa_status();
            if self.asr_catalog.is_none() {
                self.scan_asr_models(cx, false);
            }
            self.reload_subtitle_catalog(cx);
        }
        cx.notify();
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
            AppView::AudioTools => self.audio_tools_page(cx),
            AppView::PlaylistMigration => self.playlist_page(cx),
            AppView::Speech => self.speech_page(cx),
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
        let folders = self.library.folders();
        let folder_count = folders.len();
        let offline_count = folders.iter().filter(|folder| !folder.online).count();
        let folder_status = if folder_count == 0 {
            "点击添加目录".to_owned()
        } else if offline_count > 0 {
            format!("{folder_count} 个目录（{offline_count} 离线）")
        } else {
            format!("{folder_count} 个目录")
        };
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
                    .pt_2()
                    .pb_1()
                    .text_size(px(11.))
                    .text_color(theme_color(FAINT))
                    .child("音乐"),
            )
            .child(
                div()
                    .id("primary-navigation")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px_3()
                    .pt_1()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(self.nav_button(
                        "nav-all",
                        "music",
                        "音乐库",
                        0,
                        all_active,
                        false,
                        cx,
                        |this, cx| {
                            this.navigate(AppView::Library, Some(LibraryFilter::All), cx);
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
                            this.navigate(AppView::Library, Some(LibraryFilter::Favorites), cx);
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
                            this.navigate(AppView::Schedule, None, cx);
                        },
                    ))
                    .child(
                        div()
                            .px_2()
                            .pt_3()
                            .pb_1()
                            .text_size(px(11.))
                            .text_color(theme_color(MUTED))
                            .child("制作工具"),
                    )
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
                                this.navigate(AppView::Subtitles, None, cx);
                            },
                        ),
                    )
                    .child(self.nav_button(
                        "nav-audio-tools",
                        "search",
                        "音频工具",
                        0,
                        self.view == AppView::AudioTools,
                        false,
                        cx,
                        |this, cx| {
                            this.navigate(AppView::AudioTools, None, cx);
                        },
                    ))
                    .child(self.nav_button(
                        "nav-playlist-migration",
                        "list-ordered",
                        "歌单迁移",
                        0,
                        self.view == AppView::PlaylistMigration,
                        false,
                        cx,
                        |this, cx| {
                            this.navigate(AppView::PlaylistMigration, None, cx);
                        },
                    ))
                    .child(self.nav_button(
                        "nav-speech",
                        "audio-lines",
                        "文生语音",
                        0,
                        self.view == AppView::Speech,
                        false,
                        cx,
                        |this, cx| {
                            this.navigate(AppView::Speech, None, cx);
                        },
                    )),
            )
            .child(
                div().mx_3().mt_2().mb(px(12.)).flex_none().child(
                    controls::keyboard_control("cmd-music-folders", self.keyboard_focus_visible)
                        .tooltip(|_, cx| cx.new(|_| Tip("管理收录的音乐文件夹".into())).into())
                        .px_3()
                        .py_2p5()
                        .rounded_lg()
                        .bg(fade(theme_color(CONTROL_TINT), 0.04 * panel))
                        .border_1()
                        .border_color(if offline_count > 0 {
                            fade(theme_color(WARNING), 0.35)
                        } else {
                            fade(theme_color(LINE), 0.8)
                        })
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .cursor_pointer()
                        .hover(|style| {
                            style
                                .bg(fade(theme_color(CONTROL_TINT), 0.09))
                                .border_color(fade(theme_color(ACCENT), 0.35))
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_folders_modal(window, cx);
                        }))
                        .child(
                            div()
                                .size(px(30.))
                                .flex_none()
                                .rounded_md()
                                .bg(fade(
                                    theme_color(if offline_count > 0 { WARNING } else { ACCENT }),
                                    0.14,
                                ))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(icon(
                                    "folder-open",
                                    15.,
                                    theme_color(if offline_count > 0 { WARNING } else { ACCENT }),
                                )),
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
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme_color(TEXT))
                                        .child("音乐文件夹"),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(if offline_count > 0 {
                                            theme_color(WARNING)
                                        } else {
                                            theme_color(MUTED)
                                        })
                                        .truncate()
                                        .child(folder_status),
                                ),
                        )
                        .child(icon("chevron-right", 13., theme_color(FAINT))),
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
        controls::keyboard_control(id, self.keyboard_focus_visible)
            .h(px(34.))
            .flex_none()
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
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, cx);
                this.after_button_click(window, cx);
            }))
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
                    .when(count > 0, |count_view| count_view.child(count.to_string())),
            )
            .into_any_element()
    }

    // ---------- 渲染：顶栏 ----------

    fn topbar(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let search_focused = self.search.focus_handle(cx).is_focused(window);
        let panel = self.panel_alpha();
        let search = self.search.clone();
        let library_page = self.view == AppView::Library;
        div()
            .h(px(66.))
            .flex_none()
            .px_6()
            .flex()
            .items_center()
            .gap_3()
            .bg(fade(theme_color(BG), panel))
            .when(library_page, |bar| {
                bar.child(
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
                                    this.clear_search(cx);
                                },
                            ))
                        })
                        .when(self.query.is_empty(), |this| {
                            this.child(
                                div()
                                    .px_1p5()
                                    .h(px(21.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(5.))
                                    .border_1()
                                    .border_color(theme_color(LINE))
                                    .text_size(px(11.))
                                    .text_color(theme_color(FAINT))
                                    .child("⌘F"),
                            )
                        }),
                )
            })
            .when(!library_page, |bar| {
                bar.child(self.tool_button(
                    "open-library-search",
                    "search",
                    "搜索曲库",
                    false,
                    cx,
                    |this, window, cx| this.focus_search(&FocusSearch, window, cx),
                ))
            })
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move()),
            )
            .child(self.action_button(
                "tool-import",
                "导入音乐",
                Some("upload"),
                controls::ButtonTone::Secondary,
                true,
                false,
                cx,
                |this, window, cx| {
                    this.import_files(&ClickEvent::default(), window, cx);
                },
            ))
            .child(self.action_button(
                "tool-config",
                "设置",
                Some("settings"),
                controls::ButtonTone::Ghost,
                true,
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
        let offline = self.library.online_folders().is_empty() && self.library.has_folders();
        let recovery_only =
            !self.library.has_folders() || (offline && self.library.snapshot.songs.is_empty());
        let show_shelf =
            !recovery_only && self.filter != LibraryFilter::Favorites && self.query.is_empty();
        let category_count = self.library.snapshot.categories.len();
        let panel = self.panel_alpha();
        let edit_category = match &self.filter {
            LibraryFilter::Category(id) if id != crate::library::UNCATEGORIZED_CATEGORY_ID => {
                Some(id.clone())
            }
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
                            .flex_1()
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
                            .when(!recovery_only, |heading| {
                                heading.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(theme_color(MUTED))
                                        .w_full()
                                        .truncate()
                                        .child(description),
                                )
                            }),
                    )
                    .when(count > 0, |header| {
                        header.child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(self.hero_button(
                                    "hero-play",
                                    "play-fill",
                                    "播放全部",
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
                                        if !this.visible.is_empty()
                                            && !this.guard_library_playback()
                                        {
                                            if let Some(key) = this.smart_pick_start() {
                                                this.play_key(key);
                                            }
                                        }
                                        cx.notify();
                                    },
                                )),
                        )
                    }),
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
            .when(!recovery_only, |page| {
                page.child(
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
                        .when(!self.query.is_empty(), |row| {
                            row.child(
                                div()
                                    .max_w(px(330.))
                                    .text_size(px(11.))
                                    .text_color(theme_color(FAINT))
                                    .truncate()
                                    .child(format!("正在筛选“{}”", self.query)),
                            )
                        })
                        .when(count > 0, |bar| {
                            bar.child(self.tool_button(
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
                        })
                        .when(count > 0 || self.selecting, |bar| {
                            bar.child(self.tiny_button(
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
                            ))
                        }),
                )
            })
            .when(offline && count > 0, |page| {
                page.child(
                    div()
                        .mx_6()
                        .mb_2()
                        .flex_none()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .gap_2()
                        .child(icon("alert", 13., theme_color(WARNING)))
                        .child(
                            div()
                                .mr_2()
                                .text_xs()
                                .text_color(theme_color(MUTED))
                                .child("目录暂时无法读取，正在显示最近的扫描结果"),
                        )
                        .child(self.library_recovery_actions(cx)),
                )
            })
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
                    .when(count > 0, |table| table.child(self.table_header()))
                    .when(self.selecting && !recovery_only, |table| {
                        table.child(self.batch_bar(cx))
                    })
                    .child(self.song_list(cx)),
            )
    }

    fn hero_button(
        &mut self,
        id: impl Into<gpui::ElementId>,
        icon_name: &'static str,
        label: &str,
        primary: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        controls::ui_button(
            id,
            label.to_owned(),
            Some(icon_name),
            if primary {
                controls::ButtonTone::Primary
            } else {
                controls::ButtonTone::Secondary
            },
            true,
            false,
            self.keyboard_focus_visible,
            cx,
            on_click,
        )
        .into_any_element()
    }

    fn icon_button(
        &mut self,
        id: impl Into<gpui::ElementId>,
        icon_name: &'static str,
        label: &'static str,
        solid: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        controls::keyboard_control(id, self.keyboard_focus_visible)
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
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, window, cx);
                this.after_button_click(window, cx);
            }))
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
        controls::keyboard_control(id, self.keyboard_focus_visible)
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
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, window, cx);
                this.after_button_click(window, cx);
            }))
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
                        this.filter = if this.filter == LibraryFilter::Category(id.clone()) {
                            LibraryFilter::All
                        } else {
                            LibraryFilter::Category(id.clone())
                        };
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
                    let default_cat = this.default_import_target().1;
                    this.dialog_feedback = None;
                    this.dialog = Some(Dialog::MoveBatch {
                        keys: this.selected_keys(),
                        category: default_cat,
                    });
                    cx.notify();
                }))
                .child(self.tiny_button("导出", "batch-export", cx, |this, _, cx| {
                    this.export_selected(cx)
                }))
                .child(self.action_button(
                    "batch-trash",
                    "移到曲库回收站",
                    None,
                    controls::ButtonTone::Danger,
                    true,
                    true,
                    cx,
                    |this, window, cx| {
                        this.trash_keys(this.selected_keys(), window, cx);
                    },
                ))
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

    fn library_recovery_actions(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let has_folders = self.library.has_folders();
        div()
            .flex()
            .gap_2()
            .items_center()
            .flex_wrap()
            .child(self.action_button(
                "library-recovery-folders",
                if has_folders {
                    "管理音乐文件夹…"
                } else {
                    "添加文件夹…"
                },
                Some(if has_folders {
                    "folder-open"
                } else {
                    "folder-plus"
                }),
                controls::ButtonTone::Primary,
                true,
                false,
                cx,
                |this, window, cx| {
                    if this.library.has_folders() {
                        this.open_folders_modal(window, cx);
                    } else {
                        this.add_library_folders(window, cx);
                    }
                },
            ))
            .when(has_folders, |row| {
                row.child(self.action_button(
                    "library-retry",
                    "重新扫描",
                    Some("refresh"),
                    controls::ButtonTone::Secondary,
                    true,
                    false,
                    cx,
                    |this, window, cx| this.rescan(window, cx),
                ))
            })
    }

    fn song_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.visible.len();
        if count == 0 {
            if !self.library.has_folders() {
                return div()
                    .size_full()
                    .p_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(icon("folder-open", 30., theme_color(MUTED)))
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("还没有音乐文件夹"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme_color(MUTED))
                            .child("添加一个或多个文件夹后，散落在不同位置的配乐都会汇聚到这里。"),
                    )
                    .child(self.library_recovery_actions(cx))
                    .into_any_element();
            }
            if self.library.online_folders().is_empty() {
                return div()
                    .size_full()
                    .p_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(icon("alert", 30., theme_color(WARNING)))
                    .child(
                        div()
                            .text_base()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("音乐文件夹暂时离线"),
                    )
                    .child(
                        div().text_sm().text_color(theme_color(MUTED)).child(
                            "请确认外置硬盘或对应目录已重新连接，原有收藏与节目单配置已保留。",
                        ),
                    )
                    .child(self.library_recovery_actions(cx))
                    .into_any_element();
            }
            let searching = !self.query.is_empty();
            let favorites = self.filter == LibraryFilter::Favorites;
            let category = matches!(self.filter, LibraryFilter::Category(_));
            let (title, detail, label, glyph) = if searching {
                (
                    "没有匹配的曲目",
                    "试试其他曲名、分类或关键词。",
                    "清除搜索",
                    "x",
                )
            } else if favorites {
                (
                    "还没有收藏",
                    "在曲库里点心形图标，就能在这里找到那首曲目。",
                    "浏览音乐库",
                    "music",
                )
            } else if category {
                (
                    "这个场景还没有曲目",
                    "可浏览全部曲目，或导入音频到这个分类。",
                    "查看全部曲目",
                    "music",
                )
            } else {
                (
                    "曲库还没有音乐",
                    "导入本地音频，或从侧栏选择已有曲库目录。",
                    "导入音乐…",
                    "upload",
                )
            };
            return div()
                .size_full()
                .p_4()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(icon(
                    if favorites { "heart" } else { "music" },
                    28.,
                    theme_color(MUTED),
                ))
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_xs().text_color(theme_color(MUTED)).child(detail))
                .child(self.tool_button(
                    "library-empty-action",
                    glyph,
                    label,
                    false,
                    cx,
                    move |this, window, cx| {
                        if searching {
                            this.clear_search(cx);
                            window.focus(&this.search.focus_handle(cx));
                        } else if favorites || category {
                            this.filter = LibraryFilter::All;
                            cx.notify();
                        } else {
                            this.import_files(&ClickEvent::default(), window, cx);
                        }
                    },
                ))
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
        let category_name = if song.category == crate::library::UNCATEGORIZED_CATEGORY_ID {
            "未分类".to_owned()
        } else {
            category
                .map(|category| category.name_zh.clone())
                .unwrap_or_else(|| song.category.clone())
        };
        let category_color = category
            .map(|category| parse_color(&category.color))
            .unwrap_or_else(|| theme_color(MUTED));
        let category_icon_name = category
            .map(|category| category_icon(&category.icon))
            .unwrap_or("folder");
        let base_sub = song
            .artist
            .as_ref()
            .or(song.album.as_ref())
            .cloned()
            .unwrap_or_else(|| song.file_name.clone());
        let subtitle = if self.library.folders().len() > 1 {
            if let Some(folder) = self.library.folder_for_song(&song.key) {
                format!("{} · {base_sub}", folder.display_name)
            } else {
                base_sub
            }
        } else {
            base_sub
        };
        let time = song
            .duration_ms
            .map(|ms| format_time(Duration::from_millis(u64::from(ms))))
            .unwrap_or_else(|| "—".into());
        let selected = self.selected_songs.contains(&song.key);
        let selecting = self.selecting;
        let play_key = song.key.clone();
        let fav_key = song.key.clone();
        let menu_key = song.key.clone();
        let right_menu_key = song.key.clone();
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
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.show_track_menu(right_menu_key.clone(), event.position, cx);
                    cx.notify();
                }),
            )
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
                    .child(if selecting {
                        div()
                            .size(px(16.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(if selected {
                                fade(theme_color(ACCENT), 1.0)
                            } else {
                                fade(theme_color(CONTROL_TINT), 0.28)
                            })
                            .bg(if selected {
                                fade(theme_color(ACCENT), 1.0)
                            } else {
                                fade(theme_color(CONTROL_TINT), 0.04)
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(selected, |box_div| {
                                box_div.child(icon("check", 11., theme_color(ACCENT_INK)))
                            })
                            .into_any_element()
                    } else if playing {
                        icon("play-fill", 11., theme_color(ACCENT)).into_any_element()
                    } else {
                        div()
                            .text_size(px(11.))
                            .text_color(if current {
                                theme_color(ACCENT)
                            } else {
                                theme_color(FAINT)
                            })
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
                            "取消收藏"
                        } else {
                            "收藏曲目"
                        },
                        if favorite {
                            theme_color(FAVORITE)
                        } else {
                            theme_color(FAINT)
                        },
                        cx,
                        move |this, _, cx| {
                            match this.library.toggle_favorite(&fav_key) {
                                Ok(true) => this.show_toast("已添加到我的收藏", false, cx),
                                Ok(false) => this.show_toast("已取消收藏", false, cx),
                                Err(error) => this.show_toast(error, true, cx),
                            }
                            cx.notify();
                        },
                    ))
                    .child(
                        controls::keyboard_control(
                            SharedString::from(format!("more-{}", song.key)),
                            self.keyboard_focus_visible,
                        )
                        .tooltip(|_, cx| cx.new(|_| Tip("曲目操作".into())).into())
                        .size(px(26.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.09)))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.show_track_menu(menu_key.clone(), event.position(), cx);
                            this.after_button_click(window, cx);
                        }))
                        .child(icon("more", 14., theme_color(FAINT))),
                    ),
            )
    }

    fn row_action(
        &mut self,
        id: String,
        icon_name: &'static str,
        label: &'static str,
        color: Rgba,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        controls::keyboard_control(SharedString::from(id), self.keyboard_focus_visible)
            .tooltip(move |_, cx| cx.new(|_| Tip(label.into())).into())
            .size(px(24.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.09)))
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, window, cx);
                this.after_button_click(window, cx);
            }))
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
        self.action_menu = None;
        self.folder_menu = None;
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
        self.refresh_sherpa_status();
        if self.asr_catalog.is_none() {
            self.scan_asr_models(cx, false);
        }
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
        let sherpa_ready = sherpa::is_runtime_available();
        self.sherpa_runtime_status = sherpa::current_runtime_status();
        self.speech_tools.sherpa_runtime_ready = sherpa_ready;
        if !sherpa_ready {
            let reason = if captions {
                "生成字幕与人声分离"
            } else {
                "人声与伴奏分离"
            };
            self.prompt_missing_sherpa_runtime(reason, window, cx);
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
                self.prompt_missing_asr_model(window, cx);
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
                            let pending_id = select_key.clone();
                            row.child(self.tiny_button(
                                "选择",
                                SharedString::from(format!("select-pending-subtitle-{pending_id}")),
                                cx,
                                move |this, window, cx| {
                                    this.open_subtitle_generation(select_key.clone(), window, cx);
                                },
                            ))
                            .child(self.tiny_button(
                                "移除",
                                SharedString::from(format!("remove-pending-subtitle-{pending_id}")),
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
                            SharedString::from(format!("preview-subtitle-{}", record.key)),
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
                            SharedString::from(format!("edit-subtitle-segments-{}", record.key)),
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
                            SharedString::from(format!("export-subtitle-{}", record.key)),
                            cx,
                            move |this, _, cx| {
                                this.export_subtitle_record(export_key.clone(), &export_title, cx)
                            },
                        ))
                        .child(self.action_button(
                            SharedString::from(format!("delete-subtitle-{}", record.key)),
                            "删除",
                            None,
                            controls::ButtonTone::Danger,
                            true,
                            true,
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
                                                    let unique_id = SharedString::from(format!(
                                                        "{id}-{}",
                                                        record.key
                                                    ));
                                                    self.tiny_button(
                                                        label,
                                                        unique_id,
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
                                                    let unique_id = SharedString::from(format!(
                                                        "{id}-{}",
                                                        record.key
                                                    ));
                                                    self.tiny_button(
                                                        label,
                                                        unique_id,
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
                            .text_size(px(PAGE_TITLE))
                            .font_weight(FontWeight::BOLD)
                            .child("字幕生成"),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .when(can_select, |row| {
                                row.child(self.tiny_button(
                                    "选择曲目…",
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
                                        if let Some((install_msg, _)) = self.sherpa_install_status() {
                                            install_msg
                                        } else if !self.sherpa_runtime_status.is_ready() {
                                            format!(
                                                "缺少通用推理库（sherpa-onnx 2.0.2，约 {:.1} MB），可一键安装或前往模型设置。",
                                                sherpa::current_package()
                                                    .map_or(9_000_000.0, |pkg| pkg.size_bytes as f64)
                                                    / 1_000_000.0
                                            )
                                        } else if self.asr_session.is_some() {
                                            "识别模型已就绪。生成时将分离人声并输出字幕。".into()
                                        } else if self.asr_loading.is_some() {
                                            "正在加载语音识别模型…".into()
                                        } else if self.default_asr_model_complete() {
                                            "本地语音识别模型已下载，启用后即可生成字幕。".into()
                                        } else if self.asr_download.is_some() {
                                            "正在下载语音识别模型…".into()
                                        } else {
                                            "尚未下载语音识别模型（Qwen3-ASR 0.6B，约 941 MiB），可在「设置 → 语音模型」中一键下载。".into()
                                        },
                                    )),
                            )
                            .when_some(track.filter(|_| can_generate), |row, song| {
                                let key = song.key.clone();
                                row.child(self.settings_button(
                                    "confirm-generate-subtitle",
                                    "生成字幕…",
                                    true,
                                    true,
                                    cx,
                                    move |this, window, cx| {
                                        this.process_track_audio(key.clone(), true, window, cx);
                                    },
                                ))
                            })
                            .when(self.sherpa_download.is_some(), |row| {
                                row.child(self.tiny_button(
                                    "取消安装",
                                    "subtitle-cancel-sherpa",
                                    cx,
                                    |this, _, cx| this.cancel_sherpa_runtime_install(cx),
                                ))
                            })
                            .when(
                                self.sherpa_download.is_none()
                                    && !self.sherpa_runtime_status.is_ready(),
                                |row| {
                                    row.child(self.settings_button(
                                        "subtitle-install-sherpa",
                                        "一键安装推理库",
                                        true,
                                        sherpa::current_package().is_some(),
                                        cx,
                                        |this, _, cx| this.start_sherpa_runtime_install(cx),
                                    ))
                                },
                            )
                            .when(
                                self.sherpa_download.is_none()
                                    && self.sherpa_runtime_status.is_ready()
                                    && self.asr_session.is_none()
                                    && self.asr_loading.is_none()
                                    && self.default_asr_model_complete(),
                                |row| {
                                    row.child(controls::ui_button(
                                        "subtitle-enable-asr",
                                        "启用识别模型",
                                        None,
                                        controls::ButtonTone::Primary,
                                        !self.asr_busy(),
                                        true,
                                        self.keyboard_focus_visible,
                                        cx,
                                        |this, _, cx| {
                                            this.load_asr_model(DEFAULT_MODEL_NAME.into(), cx);
                                        },
                                    ))
                                },
                            )
                            .when(
                                self.asr_session.is_none() && self.sherpa_download.is_none(),
                                |row| {
                                    row.child(self.tiny_button(
                                        if self.default_asr_model_complete() {
                                            "模型设置"
                                        } else {
                                            "打开模型设置"
                                        },
                                        "subtitle-model-settings",
                                        cx,
                                        |this, window, cx| {
                                            this.open_settings_page(
                                                SettingsPage::Models,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                },
                            ),
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

        let source_folder = self
            .library
            .folder_for_song(&key)
            .map(|folder| folder.display_name.clone());
        let category_label = if song.category == crate::library::UNCATEGORIZED_CATEGORY_ID {
            "未分类".to_owned()
        } else {
            name.to_owned()
        };
        let mut metadata = div().flex().flex_col().gap_3();
        for (label, value) in [
            ("艺人", song.artist.clone()),
            ("专辑", song.album.clone()),
            ("分类", Some(category_label)),
            ("来源", source_folder),
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
                                "上一首",
                                false,
                                cx,
                                |this, _, cx| this.skip_track(false, cx),
                            ))
                            .child(
                                controls::keyboard_control(
                                    "player-play",
                                    self.keyboard_focus_visible,
                                )
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
                                    this.after_button_click(window, cx);
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
                                "下一首",
                                false,
                                cx,
                                |this, _, cx| {
                                    this.skip_track(true, cx);
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
                                    .h(px(18.))
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
                                            .h(px(4.))
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
                                            )
                                            .when(has_track, |track| {
                                                track.child(
                                                    div()
                                                        .absolute()
                                                        .left(relative(ratio))
                                                        .ml(px(-5.))
                                                        .top(px(-3.))
                                                        .size(px(10.))
                                                        .rounded_full()
                                                        .bg(theme_color(ACCENT))
                                                        .shadow_sm(),
                                                )
                                            }),
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
                        if muted { "解除静音" } else { "静音" },
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
                            .tooltip(move |_, cx| {
                                cx.new(|_| {
                                    Tip(format!("音量 {}%", (volume * 100.0).round() as i32).into())
                                })
                                .into()
                            })
                            .w(px(if narrow { 52. } else { 78. }))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, cx.listener(Self::volume_down_at))
                            .on_mouse_move(cx.listener(Self::volume_move))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::volume_up_at))
                            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::volume_up_at))
                            .child(
                                div()
                                    .relative()
                                    .h(px(4.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(theme_color(LINE))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(volume))
                                            .rounded_full()
                                            .bg(theme_color(MUTED)),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .left(relative(volume))
                                            .ml(px(-4.5))
                                            .top(px(-2.5))
                                            .size(px(9.))
                                            .rounded_full()
                                            .bg(theme_color(TEXT)),
                                    ),
                            ),
                    )
                    .child(self.player_icon_button(
                        "player-queue",
                        "list-music",
                        if self.queue_open {
                            "收起播放队列"
                        } else {
                            "显示播放队列"
                        },
                        self.queue_open,
                        cx,
                        |this, _, cx| {
                            this.toggle_queue(cx);
                        },
                    ))
                    .child(div().when(!has_track, |el| el.opacity(0.4)).child(
                        self.player_icon_button(
                            "player-fav",
                            if favorite { "heart-fill" } else { "heart" },
                            if !has_track {
                                "当前未播放曲目"
                            } else if favorite {
                                "取消收藏"
                            } else {
                                "收藏当前曲目"
                            },
                            favorite,
                            cx,
                            move |this, _, cx| {
                                if let Some(key) = this.current_key() {
                                    match this.library.toggle_favorite(&key) {
                                        Ok(true) => this.show_toast("已添加到我的收藏", false, cx),
                                        Ok(false) => this.show_toast("已取消收藏", false, cx),
                                        Err(error) => this.show_toast(error, true, cx),
                                    }
                                    cx.notify();
                                }
                            },
                        ),
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
        let show_focus = self.keyboard_focus_visible;
        Some(
            controls::modal_backdrop(
                "close-backdrop",
                &self.close_focus,
                cx,
                |this, window, cx| {
                    this.close_prompt = false;
                    window.focus(&this.focus);
                    cx.notify();
                },
            )
            .child(
                controls::modal_card(410.)
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme_color(TEXT))
                                    .child("关闭听风吟"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme_color(MUTED))
                                    .child("最小化到菜单栏后可继续在后台播放配乐。"),
                            ),
                    )
                    .child(controls::checkbox_control(
                        "close-remember",
                        "记住这次选择，下次不再询问",
                        self.remember_close,
                        show_focus,
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
                            .child(controls::ui_button(
                                "close-cancel",
                                "取消",
                                None,
                                controls::ButtonTone::Ghost,
                                true,
                                false,
                                show_focus,
                                cx,
                                |this, window, cx| {
                                    this.close_prompt = false;
                                    window.focus(&this.focus);
                                    cx.notify();
                                },
                            ))
                            .child(controls::ui_button(
                                "close-quit",
                                "退出应用",
                                None,
                                controls::ButtonTone::Secondary,
                                true,
                                false,
                                show_focus,
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
                            .child(controls::ui_button(
                                "close-tray",
                                "最小化到菜单栏",
                                None,
                                controls::ButtonTone::Primary,
                                true,
                                false,
                                show_focus,
                                cx,
                                |this, window, cx| {
                                    let close_action =
                                        this.remember_close.then_some(CloseAction::Tray);
                                    match this.set_tray_settings(true, close_action, window, cx) {
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
        self.open_settings_page(SettingsPage::General, window, cx);
    }

    fn open_settings_page(
        &mut self,
        page: SettingsPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.modal_open() {
            return;
        }
        self.action_menu = None;
        self.folder_menu = None;
        cx.on_next_frame(window, |this, window, _| window.focus(&this.settings_focus));
        if self.fonts.is_empty() {
            self.fonts = desktop::system_fonts(cx);
        }
        self.settings_feedback = None;
        self.model_advanced_open = false;
        self.settings_page = Some(page);
        self.queue_open = false;
        self.diagnostics_open = false;
        if page == SettingsPage::Export {
            self.detect_ffmpeg(cx);
        } else if page == SettingsPage::Models {
            self.refresh_sherpa_status();
            self.scan_asr_models(cx, false);
        } else if page == SettingsPage::Playback {
            self.refresh_audio_outputs();
        }
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

    fn settings_layer(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let page = self.settings_page?;
        let show_focus = self.keyboard_focus_visible;
        self.settings_slider_bounds.clear();
        let body = match page {
            SettingsPage::General => self.general_settings(cx),
            SettingsPage::Wallpaper => self.wallpaper_settings(window, cx),
            SettingsPage::Playback => self.playback_settings(cx),
            SettingsPage::Export => self.export_settings(cx),
            SettingsPage::Models => self.model_settings(cx),
            SettingsPage::Integrations => self.integration_settings(cx),
        };
        let pages = [
            (SettingsPage::General, "general", "settings", "常规"),
            (SettingsPage::Wallpaper, "wallpaper", "image", "壁纸"),
            (SettingsPage::Playback, "playback", "play-fill", "播放"),
            (SettingsPage::Export, "export", "sliders", "导出"),
            (SettingsPage::Models, "models", "audio-lines", "语音模型"),
            (
                SettingsPage::Integrations,
                "integrations",
                "external",
                "本机接口",
            ),
        ];
        let (page_title, page_subtitle) = match page {
            SettingsPage::General => ("常规设置", "外观主题、字体、音乐文件夹和关闭行为"),
            SettingsPage::Wallpaper => ("界面壁纸", "预设配色、静态图片与动态 MP4 背景效果"),
            SettingsPage::Playback => ("播放与输出", "自动播放、桌面歌词、淡入淡出和音频输出设备"),
            SettingsPage::Export => ("导出与响度", "配置单首和批量导出时的响度归一化与预设目标"),
            SettingsPage::Models => ("语音模型", "管理字幕生成、文生语音与人声分离所需的本地组件"),
            SettingsPage::Integrations => ("本机接口", "HTTP 接口开关、访问令牌与跨域白名单"),
        };
        let nav_items = pages
            .into_iter()
            .map(|(target, id, icon_name, label)| {
                let active = page == target;
                controls::keyboard_control(
                    SharedString::from(format!("settings-tab-{id}")),
                    show_focus,
                )
                .h(px(34.))
                .px_3()
                .rounded_md()
                .flex()
                .items_center()
                .gap_2p5()
                .cursor_pointer()
                .text_xs()
                .font_weight(if active {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                })
                .bg(if active {
                    fade(theme_color(ACCENT), 0.14)
                } else {
                    fade(theme_color(CONTROL_TINT), 0.0)
                })
                .text_color(if active {
                    theme_color(ACCENT)
                } else {
                    theme_color(MUTED)
                })
                .hover(move |style| {
                    if active {
                        style
                    } else {
                        style
                            .bg(fade(theme_color(CONTROL_TINT), 0.08))
                            .text_color(theme_color(TEXT))
                    }
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if target == SettingsPage::Playback {
                        this.refresh_audio_outputs();
                    } else if target == SettingsPage::Export {
                        this.detect_ffmpeg(cx);
                    } else if target == SettingsPage::Models {
                        this.refresh_sherpa_status();
                        this.scan_asr_models(cx, false);
                    }
                    window.focus(&this.settings_focus);
                    this.settings_page = Some(target);
                    this.font_picker_open = false;
                    this.model_advanced_open = false;
                    this.settings_feedback = None;
                    this.settings_slider_drag = None;
                    cx.notify();
                }))
                .child(icon(
                    icon_name,
                    14.,
                    theme_color(if active { ACCENT } else { MUTED }),
                ))
                .child(label)
            })
            .collect::<Vec<_>>();
        Some(
            controls::modal_backdrop(
                "settings-backdrop",
                &self.settings_focus,
                cx,
                |this, window, cx| {
                    this.settings_page = None;
                    this.settings_slider_drag = None;
                    this.font_picker_open = false;
                    this.model_advanced_open = false;
                    window.focus(&this.focus);
                    cx.notify();
                },
            )
            .child(
                controls::modal_card(780.)
                    .h(px(560.))
                    .max_w(relative(0.92))
                    .max_h(relative(0.88))
                    .flex()
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(168.))
                            .flex_none()
                            .h_full()
                            .bg(fade(theme_color(CONTROL_TINT), 0.03))
                            .border_r_1()
                            .border_color(theme_color(LINE))
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .px_3()
                                    .pt_2()
                                    .pb_3()
                                    .text_base()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme_color(TEXT))
                                    .child("设置"),
                            )
                            .children(nav_items),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px_6()
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
                                            .gap_0p5()
                                            .child(
                                                div()
                                                    .text_base()
                                                    .font_weight(FontWeight::BOLD)
                                                    .text_color(theme_color(TEXT))
                                                    .child(page_title),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme_color(MUTED))
                                                    .truncate()
                                                    .child(page_subtitle),
                                            ),
                                    )
                                    .child(controls::ui_icon_button(
                                        "settings-close",
                                        "x",
                                        false,
                                        true,
                                        show_focus,
                                        cx,
                                        |this, _, window, cx| {
                                            this.settings_page = None;
                                            this.settings_slider_drag = None;
                                            this.font_picker_open = false;
                                            this.model_advanced_open = false;
                                            window.focus(&this.focus);
                                            cx.notify();
                                        },
                                    )),
                            )
                            .child(
                                div()
                                    .id(("settings-scroll", page as usize))
                                    .px_6()
                                    .py_3()
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_y_scroll()
                                    .child(body),
                            )
                            .when_some(self.settings_feedback.clone(), |panel, feedback| {
                                panel.child(
                                    div()
                                        .px_6()
                                        .py_2()
                                        .border_t_1()
                                        .border_color(theme_color(LINE))
                                        .text_xs()
                                        .text_color(theme_color(MUTED))
                                        .child(feedback),
                                )
                            }),
                    ),
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
                    this.refresh_sherpa_status();
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
                self.speech_tools.sync_root(&self.library);
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
                    if this.sherpa_runtime_status.is_ready() {
                        this.load_asr_model(DEFAULT_MODEL_NAME.into(), cx);
                    } else {
                        let ready_msg = "Qwen3-ASR 0.6B 模型已下载完成，安装通用推理引擎后即可启用";
                        if this.settings_page == Some(SettingsPage::Models) {
                            this.settings_feedback = Some(ready_msg.into());
                        } else {
                            this.notice = ready_msg.into();
                        }
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn poll_model_download(&mut self) -> bool {
        let Some(download) = &self.asr_download else {
            return false;
        };
        let next = download.progress();
        let changed = self.asr_download_progress != next;
        self.asr_download_progress = next;
        changed
    }

    pub(super) fn refresh_sherpa_status(&mut self) {
        self.sherpa_runtime_status = sherpa::current_runtime_status();
        self.speech_tools.sherpa_runtime_ready = self.sherpa_runtime_status.is_ready();
    }

    pub(super) fn sherpa_install_status(&self) -> Option<(String, Option<f32>)> {
        let download = self.sherpa_download.as_ref()?;
        if download.cancelling() {
            return Some(("正在取消推理库安装…".into(), None));
        }
        let progress = self.sherpa_download_progress.as_ref()?;
        let mb = |bytes: u64| bytes as f64 / 1_000_000.0;
        let result = match progress.stage {
            RuntimeStage::Downloading => {
                let ratio = (progress.total > 0)
                    .then(|| (progress.received as f32 / progress.total as f32).clamp(0.0, 1.0));
                let text = if progress.total > 0 {
                    format!(
                        "正在下载通用推理库（{:.1} / {:.1} MB）…",
                        mb(progress.received),
                        mb(progress.total)
                    )
                } else {
                    "正在下载通用推理库…".into()
                };
                (text, ratio)
            }
            RuntimeStage::Verifying => ("正在校验推理库 SHA-256 摘要…".into(), Some(1.0)),
            RuntimeStage::Extracting => ("正在解压并提取动态库…".into(), Some(1.0)),
            RuntimeStage::Checking => ("正在验证 sherpa-onnx 2.0.2 接口…".into(), Some(1.0)),
        };
        Some(result)
    }

    pub(super) fn start_sherpa_runtime_install(&mut self, cx: &mut Context<Self>) {
        if self.sherpa_download.is_some() {
            return;
        }
        let download = RuntimeDownload::default();
        let job = download.install_job();
        self.run_sherpa_install_job(download, job, cx);
    }

    pub(super) fn cancel_sherpa_runtime_install(&mut self, cx: &mut Context<Self>) {
        if let Some(download) = &self.sherpa_download {
            download.cancel();
            cx.notify();
        }
    }

    fn import_sherpa_runtime_local(&mut self, cx: &mut Context<Self>) {
        if self.sherpa_download.is_some() {
            return;
        }
        let selection = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("选择 sherpa-onnx 2.0.2 压缩包或动态库目录".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = selection.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.sherpa_download.is_some() {
                    return;
                }
                let download = RuntimeDownload::default();
                let job = download.install_from_local_job(path);
                this.run_sherpa_install_job(download, job, cx);
            });
        })
        .detach();
    }

    fn run_sherpa_install_job(
        &mut self,
        download: RuntimeDownload,
        job: impl FnOnce() -> Result<PathBuf, String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.sherpa_download = Some(download);
        self.sherpa_download_progress = Some(RuntimeProgress {
            stage: RuntimeStage::Downloading,
            received: 0,
            total: sherpa::current_package().map_or(0, |pkg| pkg.size_bytes),
        });
        if self.settings_page == Some(SettingsPage::Models) {
            self.settings_feedback = None;
        }
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_sherpa_runtime_install(result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    fn finish_sherpa_runtime_install(
        &mut self,
        result: Result<PathBuf, String>,
        cx: &mut Context<Self>,
    ) {
        self.sherpa_download = None;
        self.sherpa_download_progress = None;
        self.refresh_sherpa_status();
        match result {
            Ok(_) => {
                if self
                    .speech_tools
                    .status
                    .as_deref()
                    .is_some_and(|text| text.contains("sherpa-onnx") || text.contains("推理库"))
                {
                    self.speech_tools.status = None;
                }
                if self.subtitle_feedback.as_ref().is_some_and(|(_, text)| {
                    text.contains("sherpa-onnx") || text.contains("推理库")
                }) {
                    self.subtitle_feedback = None;
                }
                self.speech_tools.refresh_model();
                self.scan_asr_models(cx, false);
                if self.asr_session.is_none()
                    && self.asr_loading.is_none()
                    && let Some(last) = self.library.asr_model_preferences().last_model
                {
                    self.load_asr_model(last, cx);
                }
                let message =
                    "sherpa-onnx 2.0.2 推理库已安装，现在可直接用于语音识别、文生语音和人声分离";
                if self.settings_page == Some(SettingsPage::Models) {
                    self.settings_feedback = Some(message.into());
                }
                self.show_toast(message, false, cx);
            }
            Err(error) => {
                if self.settings_page == Some(SettingsPage::Models) {
                    self.settings_feedback = Some(error.clone());
                }
                self.show_toast(error, true, cx);
            }
        }
        cx.notify();
    }

    fn poll_sherpa_download(&mut self) -> bool {
        let Some(download) = &self.sherpa_download else {
            return false;
        };
        let next = download.progress();
        let changed = next.is_some() && self.sherpa_download_progress != next;
        if changed {
            self.sherpa_download_progress = next;
        }
        changed
    }

    fn prompt_missing_sherpa_runtime(
        &mut self,
        feature: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let size_mb =
            sherpa::current_package().map_or(9.0, |pkg| pkg.size_bytes as f64 / 1_000_000.0);
        let detail = format!(
            "「{feature}」需要通用推理库 sherpa-onnx 2.0.2（约 {size_mb:.1} MB，与文字转语音、字幕生成共用）。可直接一键自动下载并校验安装，或前往设置页查看与导入本地包。"
        );
        let answer = window.prompt(
            PromptLevel::Info,
            "缺少通用推理库",
            Some(&detail),
            &[
                PromptButton::ok("一键安装推理库"),
                PromptButton::new("前往模型设置"),
                PromptButton::cancel("取消"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| match answer.await {
            Ok(0) => {
                let _ = this.update(cx, |this, cx| {
                    this.start_sherpa_runtime_install(cx);
                });
            }
            Ok(1) => {
                let _ = cx.update(|window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.open_settings_page(SettingsPage::Models, window, cx);
                    });
                });
            }
            _ => {}
        })
        .detach();
    }

    fn default_asr_model_complete(&self) -> bool {
        self.asr_catalog
            .as_ref()
            .is_some_and(|catalog| catalog.default_files.missing.is_empty())
    }

    fn prompt_missing_asr_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_default_files = self.default_asr_model_complete()
            || crate::library::default_asr_model_complete(
                &self.library.asr_model_preferences().root(),
            );
        if has_default_files {
            let answer = window.prompt(
                PromptLevel::Info,
                "语音识别模型尚未启用",
                Some("本地 Qwen3-ASR 0.6B 模型已完整下载，启用后即可开始生成字幕。"),
                &[
                    PromptButton::ok("立即启用模型"),
                    PromptButton::new("打开模型设置"),
                    PromptButton::cancel("取消"),
                ],
                cx,
            );
            cx.spawn_in(window, async move |this, cx| match answer.await {
                Ok(0) => {
                    let _ = this.update(cx, |this, cx| {
                        this.load_asr_model(DEFAULT_MODEL_NAME.into(), cx);
                    });
                }
                Ok(1) => {
                    let _ = cx.update(|window, cx| {
                        let _ = this.update(cx, |this, cx| {
                            this.open_settings_page(SettingsPage::Models, window, cx);
                        });
                    });
                }
                _ => {}
            })
            .detach();
            return;
        }
        let answer = window.prompt(
            PromptLevel::Info,
            "尚未加载语音识别模型",
            Some("生成字幕需要先下载并启用本地语音识别模型，可在「设置 → 语音模型」中一键下载并启用。"),
            &[
                PromptButton::ok("前往模型设置"),
                PromptButton::cancel("取消"),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if matches!(answer.await, Ok(0)) {
                let _ = cx.update(|window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.open_settings_page(SettingsPage::Models, window, cx);
                    });
                });
            }
        })
        .detach();
    }

    fn runtime_engine_card(&self, cx: &mut Context<Self>) -> gpui::Div {
        let show_focus = self.keyboard_focus_visible;
        if let Some((detail, ratio)) = self.sherpa_install_status() {
            let cancelling = self
                .sherpa_download
                .as_ref()
                .is_some_and(RuntimeDownload::cancelling);
            let cancel_btn = controls::ui_button(
                "sherpa-runtime-cancel",
                if cancelling { "正在取消" } else { "取消" },
                None,
                controls::ButtonTone::Secondary,
                !cancelling,
                true,
                show_focus,
                cx,
                |this, _, cx| this.cancel_sherpa_runtime_install(cx),
            );
            return model_card_shell(true)
                .child(model_card_header(
                    "sliders",
                    ModelStatusTone::Ready,
                    "通用推理引擎",
                    Some(("安装中", ModelStatusTone::Ready)),
                    &detail,
                    cancel_btn,
                ))
                .when_some(ratio, |card, percent| {
                    card.child(model_progress_bar(percent))
                });
        }
        match &self.sherpa_runtime_status {
            RuntimeStatus::Ready { path, source } => {
                let reveal_path = path.clone();
                let detail = format!(
                    "sherpa-onnx {} · {}",
                    sherpa::RUNTIME_VERSION,
                    source.label(),
                );
                let actions = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .child(controls::ui_button(
                        "sherpa-runtime-reveal",
                        "显示位置",
                        None,
                        controls::ButtonTone::Secondary,
                        true,
                        true,
                        show_focus,
                        cx,
                        move |_, _, cx| cx.reveal_path(&reveal_path),
                    ))
                    .when(*source == sherpa::RuntimeSource::Managed, |row| {
                        row.child(controls::ui_button(
                            "sherpa-runtime-reinstall",
                            "重新安装",
                            None,
                            controls::ButtonTone::Secondary,
                            sherpa::current_package().is_some(),
                            true,
                            show_focus,
                            cx,
                            |this, _, cx| this.start_sherpa_runtime_install(cx),
                        ))
                    });
                model_card_shell(false).child(model_card_header(
                    "sliders",
                    ModelStatusTone::Ready,
                    "通用推理引擎",
                    Some(("已就绪", ModelStatusTone::Ready)),
                    &detail,
                    actions,
                ))
            }
            RuntimeStatus::Missing(_) => {
                let package = sherpa::current_package();
                let detail = match package {
                    Some(pkg) => format!(
                        "自动匹配 {}（约 {:.1} MB）",
                        pkg.platform_label,
                        pkg.size_bytes as f64 / 1_000_000.0
                    ),
                    None => "当前系统架构暂不支持自动安装，请导入本地动态库压缩包或目录".into(),
                };
                let actions = div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .when(package.is_some(), |row| {
                        row.child(controls::ui_button(
                            "sherpa-runtime-install",
                            "一键安装",
                            Some("download"),
                            controls::ButtonTone::Primary,
                            true,
                            true,
                            show_focus,
                            cx,
                            |this, _, cx| this.start_sherpa_runtime_install(cx),
                        ))
                    })
                    .child(controls::ui_button(
                        "sherpa-runtime-import",
                        "导入本地包…",
                        None,
                        controls::ButtonTone::Secondary,
                        true,
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| this.import_sherpa_runtime_local(cx),
                    ));
                model_card_shell(true).child(model_card_header(
                    "sliders",
                    ModelStatusTone::Warning,
                    "通用推理引擎",
                    Some(("未安装", ModelStatusTone::Warning)),
                    &detail,
                    actions,
                ))
            }
        }
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
                    if result.is_ok() {
                        if this.settings_page == Some(SettingsPage::Models) {
                            this.settings_feedback = Some("语音识别模型已启用".into());
                        }
                        this.show_toast("语音识别模型已启用", false, cx);
                    } else if this.settings_page == Some(SettingsPage::Models) {
                        this.settings_feedback = None;
                    }
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
                    let message = "已停用语音识别模型并释放内存";
                    if self.settings_page == Some(SettingsPage::Models) {
                        self.settings_feedback = Some(message.into());
                    } else {
                        self.show_toast(message, false, cx);
                    }
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

    fn asr_model_card(&self, cx: &mut Context<Self>) -> gpui::Div {
        let show_focus = self.keyboard_focus_visible;
        let sherpa_ready = self.sherpa_runtime_status.is_ready();
        let primary_tone = if sherpa_ready {
            controls::ButtonTone::Primary
        } else {
            controls::ButtonTone::Secondary
        };
        let loaded = self.asr_session.as_ref();
        let loading = self.asr_loading.as_deref();
        let files = self
            .asr_catalog
            .as_ref()
            .map(|catalog| &catalog.default_files);
        let remembered = self.library.asr_model_preferences().last_model.is_some();
        let idle = !self.asr_busy();
        let has_files = files.is_some_and(|files| files.complete > 0);
        let incomplete = files.is_some_and(|files| files.complete < files.count);

        let (highlight, tone, pill, detail, accessory, progress_ratio) = if let Some(download) =
            &self.asr_download
        {
            let progress = self.asr_download_progress.as_ref();
            let (detail, percent) = progress.map_or_else(
                || ("正在准备下载 Qwen3-ASR 0.6B 模型…".to_owned(), 0.0),
                |progress| {
                    let mib = |bytes| bytes as f64 / 1_048_576.;
                    let detail = format!(
                        "{}/{} · {} · {:.1} / {:.1} MiB",
                        progress.index,
                        progress.count,
                        progress.file,
                        mib(progress.received),
                        mib(progress.total)
                    );
                    let ratio =
                        (progress.received as f32 / progress.total.max(1) as f32).clamp(0.0, 1.0);
                    (detail, ratio)
                },
            );
            let cancel = controls::ui_button(
                "asr-download-cancel",
                if download.cancelling() {
                    "正在取消"
                } else {
                    "取消下载"
                },
                None,
                controls::ButtonTone::Secondary,
                !download.cancelling(),
                true,
                show_focus,
                cx,
                |this, _, cx| {
                    if let Some(download) = &this.asr_download {
                        download.cancel();
                    }
                    cx.notify();
                },
            )
            .into_any_element();
            (
                true,
                ModelStatusTone::Ready,
                "下载中",
                detail,
                cancel,
                Some(percent),
            )
        } else if let Some(model) = loading {
            let detail = format!("正在启动 {}，请稍候…", display_asr_model_name(model));
            let button = controls::ui_button(
                "asr-enabling",
                "启用中…",
                None,
                controls::ButtonTone::Secondary,
                false,
                true,
                show_focus,
                cx,
                |_, _, _| {},
            )
            .into_any_element();
            (true, ModelStatusTone::Ready, "启用中", detail, button, None)
        } else if let Some(session) = loaded {
            let detail = if session.id == DEFAULT_MODEL_NAME {
                "Qwen3-ASR 0.6B · 用于字幕与桌面歌词（重启恢复）".to_owned()
            } else {
                format!("当前使用「{}」· 用于字幕与桌面歌词", session.id)
            };
            let unload = controls::ui_button(
                "asr-unload",
                "停用",
                None,
                controls::ButtonTone::Secondary,
                idle,
                true,
                show_focus,
                cx,
                |this, _, cx| this.unload_asr_model(cx),
            )
            .into_any_element();
            (true, ModelStatusTone::Ready, "已启用", detail, unload, None)
        } else if files.is_some() && !incomplete {
            if sherpa_ready {
                let enable = controls::ui_button(
                    "asr-enable-default",
                    "启用模型",
                    None,
                    controls::ButtonTone::Primary,
                    idle,
                    true,
                    show_focus,
                    cx,
                    |this, _, cx| this.load_asr_model(DEFAULT_MODEL_NAME.into(), cx),
                )
                .into_any_element();
                (
                    false,
                    ModelStatusTone::Neutral,
                    "未启用",
                    "Qwen3-ASR 0.6B · 文件完整（941 MiB），启用后可用于字幕与桌面歌词".to_owned(),
                    enable,
                    None,
                )
            } else {
                (
                    false,
                    ModelStatusTone::Warning,
                    "缺推理引擎",
                    "Qwen3-ASR 0.6B · 文件完整（941 MiB），请先安装上方通用推理引擎".to_owned(),
                    div().into_any_element(),
                    None,
                )
            }
        } else if let Some(files) = files {
            let tone = if has_files {
                ModelStatusTone::Warning
            } else {
                ModelStatusTone::Neutral
            };
            let pill = if has_files { "待补齐" } else { "未下载" };
            let detail = if has_files {
                format!(
                    "Qwen3-ASR 0.6B · 已下载 {}/{} 个文件，继续补齐后即可使用",
                    files.complete, files.count
                )
            } else {
                "Qwen3-ASR 0.6B · 约 941 MiB，字幕生成与桌面歌词共用".to_owned()
            };
            let download_btn = controls::ui_button(
                "asr-download",
                if has_files {
                    "补齐下载"
                } else {
                    "下载模型"
                },
                Some("download"),
                primary_tone,
                idle,
                true,
                show_focus,
                cx,
                |this, _, cx| this.start_model_download(DownloadMode::Resume, cx),
            )
            .into_any_element();
            (false, tone, pill, detail, download_btn, None)
        } else {
            (
                false,
                ModelStatusTone::Neutral,
                "检查中",
                "正在检查本地语音识别模型文件…".to_owned(),
                div().into_any_element(),
                None,
            )
        };

        let show_notice =
            self.asr_message.is_some() || (loaded.is_none() && loading.is_none() && remembered);
        let notice_text = self
            .asr_message
            .clone()
            .unwrap_or_else(|| "上次记住的模型尚未启用，可重新启用或清除自动恢复".to_owned());
        let can_clear_remembered = loaded.is_none() && remembered;

        let maintenance_summary = files.map(|files| {
            if !incomplete {
                format!(
                    "本地文件：{} / {} 完整 · {:.0} MiB",
                    files.complete,
                    files.count,
                    files.expected_bytes as f64 / 1_048_576.
                )
            } else if has_files {
                format!(
                    "本地文件：{} / {} 完整 · 缺失 {} 个",
                    files.complete,
                    files.count,
                    files.missing.len()
                )
            } else {
                format!(
                    "共 {} 个文件 · 约 {:.0} MiB",
                    files.count,
                    files.expected_bytes as f64 / 1_048_576.
                )
            }
        });

        model_card_shell(highlight)
            .child(model_card_header(
                "audio-lines",
                tone,
                "语音识别模型",
                Some((pill, tone)),
                &detail,
                accessory,
            ))
            .when_some(progress_ratio, |card, percent| {
                card.child(model_progress_bar(percent))
            })
            .when(show_notice, |card| {
                card.child(
                    div()
                        .mx_3p5()
                        .mb_2p5()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(fade(theme_color(WARNING), 0.10))
                        .border_1()
                        .border_color(fade(theme_color(WARNING), 0.28))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_xs()
                                .text_color(theme_color(TEXT))
                                .child(notice_text),
                        )
                        .when(can_clear_remembered, |bar| {
                            bar.child(controls::ui_button(
                                "asr-clear-remembered",
                                "清除启动恢复",
                                None,
                                controls::ButtonTone::Ghost,
                                idle,
                                true,
                                show_focus,
                                cx,
                                |this, _, cx| this.unload_asr_model(cx),
                            ))
                        }),
                )
            })
            .when_some(
                maintenance_summary.filter(|_| self.asr_download.is_none()),
                |card, summary| {
                    card.child(
                        div()
                            .border_t_1()
                            .border_color(theme_color(LINE))
                            .bg(fade(theme_color(CONTROL_TINT), 0.025))
                            .px_3p5()
                            .py_2()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_xs()
                                    .text_color(theme_color(FAINT))
                                    .truncate()
                                    .child(summary),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .gap_1()
                                    .child(controls::ui_button(
                                        "asr-check-files",
                                        "检查完整性",
                                        None,
                                        controls::ButtonTone::Ghost,
                                        idle,
                                        true,
                                        show_focus,
                                        cx,
                                        |this, _, cx| this.scan_asr_models(cx, true),
                                    ))
                                    .when(has_files, |bar| {
                                        bar.child(controls::ui_button(
                                            "asr-redownload",
                                            "重新下载",
                                            None,
                                            controls::ButtonTone::Ghost,
                                            idle,
                                            true,
                                            show_focus,
                                            cx,
                                            |this, window, cx| {
                                                this.confirm_model_redownload(window, cx)
                                            },
                                        ))
                                    }),
                            ),
                    )
                },
            )
    }

    fn model_directory_card(&self, cx: &mut Context<Self>) -> gpui::Div {
        let idle = !self.asr_busy();
        let show_focus = self.keyboard_focus_visible;
        let prefs = self.library.asr_model_preferences();
        let (is_custom, root_display) = match &self.asr_catalog {
            Some(catalog) => (
                !catalog.is_default_root,
                controls::compact_display_path(&catalog.root),
            ),
            None => (
                prefs.directory.is_some(),
                controls::compact_display_path(&prefs.root()),
            ),
        };
        let (pill, tone) = if is_custom {
            ("自定义", ModelStatusTone::Ready)
        } else {
            ("默认", ModelStatusTone::Neutral)
        };
        let actions = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(controls::ui_button(
                "asr-folder-choose",
                "更改目录…",
                None,
                controls::ButtonTone::Secondary,
                idle,
                true,
                show_focus,
                cx,
                |this, _, cx| this.choose_asr_directory(cx),
            ))
            .child(controls::ui_button(
                "asr-folder-open",
                "在访达中显示",
                None,
                controls::ButtonTone::Secondary,
                true,
                true,
                show_focus,
                cx,
                |this, _, cx| this.open_asr_directory(cx),
            ))
            .when(is_custom, |row| {
                row.child(controls::ui_button(
                    "asr-folder-reset",
                    "恢复默认",
                    None,
                    controls::ButtonTone::Secondary,
                    idle,
                    true,
                    show_focus,
                    cx,
                    |this, _, cx| this.apply_asr_directory(None, cx),
                ))
            });
        model_card_shell(false).child(model_card_header(
            "folder-open",
            ModelStatusTone::Neutral,
            "模型目录",
            Some((pill, tone)),
            &root_display,
            actions,
        ))
    }

    fn model_advanced_card(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let idle = !self.asr_busy();
        let show_focus = self.keyboard_focus_visible;
        let sherpa_ready = self.sherpa_runtime_status.is_ready();
        let loaded = self.asr_session.as_ref();
        let has_warnings = self
            .asr_catalog
            .as_ref()
            .is_some_and(|catalog| !catalog.warnings.is_empty());
        let has_custom_models = self.asr_catalog.as_ref().is_some_and(|catalog| {
            catalog
                .models
                .iter()
                .any(|model| model.id != DEFAULT_MODEL_NAME)
        });
        let pill = if has_warnings {
            Some(("有扫描提示", ModelStatusTone::Warning))
        } else if has_custom_models {
            Some(("含自定义模型", ModelStatusTone::Neutral))
        } else {
            None
        };

        let header = controls::keyboard_control("model-advanced-toggle", show_focus)
            .cursor_pointer()
            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.05)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.model_advanced_open = !this.model_advanced_open;
                cx.notify();
            }))
            .child(model_card_header(
                "settings",
                ModelStatusTone::Neutral,
                "高级选项",
                pill,
                "镜像下载地址、本地模型列表与 WAV 试测",
                icon(
                    if self.model_advanced_open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    15.,
                    theme_color(MUTED),
                ),
            ));

        if !self.model_advanced_open {
            return model_card_shell(false).child(header);
        }

        let saved_url = self.library.asr_model_preferences().download_url;
        let current_url = self.asr_url_field.read(cx).text().to_owned();
        let url_dirty = current_url != saved_url;
        let url_is_default =
            saved_url == DEFAULT_ASR_DOWNLOAD_URL && current_url == DEFAULT_ASR_DOWNLOAD_URL;

        let url_section = div()
            .p_3p5()
            .border_b_1()
            .border_color(theme_color(LINE))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme_color(TEXT))
                            .child("自定义下载地址"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child("ModelScope 仓库或模型镜像目录"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .child(controls::input_box(self.asr_url_field.clone())),
                    )
                    .child(controls::ui_button(
                        "asr-url-save",
                        "保存",
                        None,
                        if url_dirty {
                            controls::ButtonTone::Primary
                        } else {
                            controls::ButtonTone::Secondary
                        },
                        idle && url_dirty,
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| {
                            let url = this.asr_url_field.read(cx).text().to_owned();
                            this.save_asr_url(&url, cx);
                        },
                    ))
                    .when(!url_is_default, |row| {
                        row.child(controls::ui_button(
                            "asr-url-reset",
                            "恢复默认",
                            None,
                            controls::ButtonTone::Secondary,
                            idle,
                            true,
                            show_focus,
                            cx,
                            |this, _, cx| this.save_asr_url(DEFAULT_ASR_DOWNLOAD_URL, cx),
                        ))
                    }),
            );

        let scan_status = match &self.asr_catalog {
            None => "正在扫描模型目录".into(),
            Some(catalog) if catalog.models.is_empty() && catalog.warnings.is_empty() => {
                "未找到 ONNX 模型文件".into()
            }
            Some(catalog) => format!("发现 {} 组 ONNX 模型文件", catalog.models.len()),
        };

        let model_rows = self
            .asr_catalog
            .iter()
            .flat_map(|catalog| catalog.models.iter())
            .map(|model| {
                let active = loaded.is_some_and(|session| session.id == model.id);
                let id = model.id.clone();
                let load_id = id.clone();
                let title = if model.id == DEFAULT_MODEL_NAME {
                    format!("{}（默认）", display_asr_model_name(&model.id))
                } else {
                    model.id.clone()
                };
                let (status_label, status_tone) = match &model.missing_components {
                    None => (
                        "根目录单文件，无法加载".to_owned(),
                        ModelStatusTone::Neutral,
                    ),
                    Some(_) if active => ("已启用".to_owned(), ModelStatusTone::Ready),
                    Some(missing) if missing.is_empty() => {
                        ("组件齐全".to_owned(), ModelStatusTone::Neutral)
                    }
                    Some(missing) => (
                        format!("缺少 {}", missing.join("、")),
                        ModelStatusTone::Warning,
                    ),
                };
                let can_load = model.missing_components.as_ref().is_some_and(Vec::is_empty);
                div()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(fade(theme_color(CONTROL_TINT), 0.03))
                    .border_1()
                    .border_color(if active {
                        fade(theme_color(ACCENT), 0.35)
                    } else {
                        theme_color(LINE).into()
                    })
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
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
                                            .truncate()
                                            .child(title),
                                    )
                                    .child(model_status_pill(status_label, status_tone)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme_color(FAINT))
                                    .truncate()
                                    .child(model.files.join(" · ")),
                            ),
                    )
                    .when(can_load && !active, |row| {
                        row.child(controls::ui_button(
                            SharedString::from(format!("asr-load-{id}")),
                            if loaded.is_some() {
                                "切换启用"
                            } else {
                                "启用"
                            },
                            None,
                            controls::ButtonTone::Secondary,
                            idle && sherpa_ready,
                            true,
                            show_focus,
                            cx,
                            move |this, _, cx| this.load_asr_model(load_id.clone(), cx),
                        ))
                    })
            })
            .collect::<Vec<_>>();

        let warnings = self
            .asr_catalog
            .iter()
            .flat_map(|catalog| catalog.warnings.iter())
            .map(|warning| {
                div()
                    .text_xs()
                    .text_color(theme_color(WARNING))
                    .child(warning.clone())
            })
            .collect::<Vec<_>>();

        let local_models_section = div()
            .p_3p5()
            .border_b_1()
            .border_color(theme_color(LINE))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme_color(TEXT))
                                    .child("本地模型列表"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme_color(MUTED))
                                    .truncate()
                                    .child(scan_status),
                            ),
                    )
                    .child(controls::ui_button(
                        "asr-scan",
                        "重新扫描",
                        None,
                        controls::ButtonTone::Secondary,
                        true,
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| this.scan_asr_models(cx, false),
                    )),
            )
            .children(warnings)
            .children(model_rows);

        let wav_detail = if self.asr_transcribing {
            "正在识别 WAV 音频…"
        } else if loaded.is_some() {
            "选择本地 WAV 文件测试当前识别效果"
        } else {
            "启用上方语音识别模型后可用"
        };

        let wav_section = div()
            .p_3p5()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme_color(TEXT))
                                    .child("WAV 识别测试"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme_color(MUTED))
                                    .truncate()
                                    .child(wav_detail),
                            ),
                    )
                    .child(controls::ui_button(
                        "asr-transcribe",
                        "选择 WAV…",
                        None,
                        controls::ButtonTone::Secondary,
                        idle && loaded.is_some(),
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| this.choose_asr_wav(cx),
                    )),
            )
            .when_some(self.asr_text.clone(), |section, text| {
                section.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .id("asr-result")
                                .max_h(px(140.))
                                .overflow_y_scroll()
                                .p_3()
                                .rounded_md()
                                .border_1()
                                .border_color(theme_color(LINE))
                                .bg(theme_color(BG))
                                .text_xs()
                                .child(text),
                        )
                        .child(div().flex().justify_end().child(controls::ui_button(
                            "asr-clear-text",
                            "清空结果",
                            None,
                            controls::ButtonTone::Ghost,
                            true,
                            true,
                            show_focus,
                            cx,
                            |this, _, cx| {
                                this.asr_text = None;
                                cx.notify();
                            },
                        ))),
                )
            });

        model_card_shell(false).child(header).child(
            div()
                .border_t_1()
                .border_color(theme_color(LINE))
                .flex()
                .flex_col()
                .child(url_section)
                .child(local_models_section)
                .child(wav_section),
        )
    }

    fn model_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .py_2()
            .child(self.runtime_engine_card(cx))
            .child(self.asr_model_card(cx))
            .child(self.model_directory_card(cx))
            .child(self.model_advanced_card(cx))
            .into_any_element()
    }

    fn general_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::settings::Appearance;

        let settings = self.library.prefs.clone();
        let show_focus = self.keyboard_focus_visible;
        let folder_count = self.library.folders().len();
        let offline_count = self
            .library
            .folders()
            .iter()
            .filter(|folder| !folder.online)
            .count();
        let folder_summary = if folder_count == 0 {
            "尚未添加任何音乐文件夹".to_owned()
        } else if offline_count > 0 {
            format!("已收录 {folder_count} 个文件夹（其中 {offline_count} 个离线）")
        } else {
            format!("已收录 {folder_count} 个音乐文件夹")
        };
        let font_name = if settings.ui_font == "system" {
            "macOS 系统字体"
        } else {
            self.fonts
                .iter()
                .find(|font| font.family == settings.ui_font)
                .map_or(settings.ui_font.as_str(), |font| font.label.as_str())
        };
        let font_button = format!("{font_name}  ▾");
        let appearance = controls::segmented_control(
            "appearance",
            &[
                (Appearance::System, "跟随系统"),
                (Appearance::Light, "浅色"),
                (Appearance::Dark, "深色"),
            ],
            self.library.appearance,
            show_focus,
            cx,
            |this, value, _, cx| {
                this.settings_feedback = this.library.set_appearance(value).err();
                cx.notify();
            },
        );
        let closing = controls::segmented_control(
            "closing",
            &[
                (CloseAction::Ask, "每次询问"),
                (CloseAction::Quit, "退出应用"),
                (CloseAction::Tray, "隐藏到菜单栏"),
            ],
            settings.close_action,
            show_focus,
            cx,
            |this, value, window, cx| this.set_close_action(value, window, cx),
        );

        div()
            .flex()
            .flex_col()
            .child(settings_section("音乐库"))
            .child(settings_row(
                "音乐文件夹",
                &folder_summary,
                controls::ui_button(
                    "settings-open-folders",
                    "管理文件夹…",
                    Some("folder-open"),
                    controls::ButtonTone::Secondary,
                    true,
                    true,
                    show_focus,
                    cx,
                    |this, window, cx| this.open_folders_modal(window, cx),
                ),
            ))
            .child(settings_section("界面"))
            .child(settings_row(
                "外观",
                "跟随 macOS 或固定使用一种外观",
                appearance,
            ))
            .child(settings_row(
                "界面字体",
                "应用到曲库、播放器和设置",
                controls::ui_button(
                    "choose-font",
                    font_button,
                    None,
                    controls::ButtonTone::Secondary,
                    true,
                    true,
                    show_focus,
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
            .child(settings_section("启动与关闭"))
            .child(settings_row(
                "登录时启动",
                "登录 macOS 后打开听风吟",
                controls::toggle_switch(
                    "autostart",
                    settings.autostart,
                    show_focus,
                    cx,
                    |this, _, cx| {
                        let previous = this.library.prefs.autostart;
                        let enabled = !previous;
                        let result = desktop::set_autostart(enabled).and_then(|()| {
                            this.library
                                .update_playback_settings(|settings| settings.autostart = enabled)
                                .map_err(|error| match desktop::set_autostart(previous) {
                                    Ok(()) => error,
                                    Err(rollback) => {
                                        format!("{error}，恢复原启动项失败：{rollback}")
                                    }
                                })
                        });
                        this.settings_feedback = result.err();
                        cx.notify();
                    },
                ),
            ))
            .child(settings_row(
                "在菜单栏显示图标",
                "点击图标恢复窗口，右键可显示或退出",
                controls::toggle_switch(
                    "tray",
                    settings.minimize_to_tray,
                    show_focus,
                    cx,
                    |this, window, cx| {
                        let enable = !this.library.prefs.minimize_to_tray;
                        this.settings_feedback =
                            this.set_tray_settings(enable, None, window, cx).err();
                        cx.notify();
                    },
                ),
            ))
            .child(settings_row(
                "关闭窗口时",
                "选择点击窗口关闭按钮后的行为",
                closing,
            ))
            .child(settings_section("配置文件"))
            .child(settings_row(
                "在访达中显示",
                "查看应用配置或曲库分类配置",
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
        cx: &mut Context<Self>,
        action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        controls::option_chip(
            id.into(),
            label.to_owned(),
            selected,
            available,
            self.keyboard_focus_visible,
            cx,
            action,
        )
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
            .child(settings_section("播放行为"))
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
                self.setting_toggle(
                    "wake-lock",
                    settings.prevent_sleep,
                    cx,
                    |this, window, cx| {
                        this.update_settings(|settings| {
                            settings.prevent_sleep = !settings.prevent_sleep
                        });
                        this.update_wake_lock(|| desktop::window_visible(window));
                        cx.notify();
                    },
                ),
            ))
            .child(settings_row(
                "桌面歌词",
                "在独立窗口显示当前曲目的歌词。实时识别需要语音识别模型。",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(self.desktop_lyrics.overlay.is_some(), |row| {
                        row.child(self.tiny_button(
                            "样式",
                            "desktop-lyrics-style",
                            cx,
                            |this, _, cx| this.toggle_lyrics_settings(cx),
                        ))
                    })
                    .child(self.setting_toggle(
                        "desktop-lyrics",
                        self.desktop_lyrics.overlay.is_some(),
                        cx,
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
        cx: &mut Context<Self>,
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
        let show_focus = self.keyboard_focus_visible;
        let volume_id = key.clone();
        let mute_id = key.clone();
        let enable_id = key.clone();
        let remove_id = key.clone();
        let muted = route.muted;
        let enabled = route.enabled;
        div()
            .my_1()
            .p_3()
            .rounded_lg()
            .bg(fade(theme_color(CONTROL_TINT), 0.035))
            .border_1()
            .border_color(theme_color(LINE))
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .truncate()
                                    .child(name.to_owned()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if connected {
                                        theme_color(MUTED)
                                    } else {
                                        theme_color(WARNING)
                                    })
                                    .child(status),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(controls::option_chip(
                                SharedString::from(format!("output-mute-{key}")),
                                if muted { "取消静音" } else { "静音" },
                                muted,
                                connected && enabled,
                                show_focus,
                                cx,
                                move |this, _, cx| {
                                    this.change_output(
                                        json!({"deviceId":mute_id,"muted":!muted}),
                                        cx,
                                    )
                                },
                            ))
                            .child(controls::ui_button(
                                SharedString::from(format!("output-enabled-{key}")),
                                if enabled { "停用" } else { "启用" },
                                None,
                                controls::ButtonTone::Secondary,
                                connected || enabled,
                                true,
                                show_focus,
                                cx,
                                move |this, _, cx| {
                                    this.change_output(
                                        json!({"deviceId":enable_id,"enabled":!enabled}),
                                        cx,
                                    )
                                },
                            ))
                            .when(!default_output, |controls| {
                                controls.child(controls::ui_button(
                                    SharedString::from(format!("output-remove-{key}")),
                                    "移除",
                                    None,
                                    controls::ButtonTone::Danger,
                                    true,
                                    true,
                                    show_focus,
                                    cx,
                                    move |this, _, cx| {
                                        let mut routes = this.library.prefs.output_routes.clone();
                                        routes.retain(|route| route.device_id != remove_id);
                                        this.change_output(json!({"routes":routes}), cx);
                                    },
                                ))
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme_color(FAINT))
                            .child("独立输出音量"),
                    )
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
                    )),
            )
            .into_any_element()
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
        let show_focus = self.keyboard_focus_visible;
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
        let cors_mode = match config.cors.as_str() {
            "any" => "any",
            "custom" => "custom",
            _ => "local",
        };
        let cors_control = controls::segmented_control(
            "api-cors",
            &[
                ("local", "仅本机"),
                ("any", "任意来源"),
                ("custom", "指定来源"),
            ],
            cors_mode,
            show_focus,
            cx,
            |this, value, _, cx| {
                let mut next = this.api_config.clone();
                next.cors = value.into();
                if this.apply_api_config(next, cx) {
                    this.settings_feedback = Some("跨域来源已更新".into());
                }
            },
        );
        let token_row =
            div()
                .py_3()
                .border_b_1()
                .border_color(theme_color(LINE))
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().font_weight(FontWeight::MEDIUM).child("访问令牌"))
                        .child(div().text_xs().text_color(theme_color(MUTED)).child(
                            if has_token {
                                "已设置 · 输入新令牌可替换"
                            } else {
                                "未设置 · 可添加令牌限制接口访问"
                            },
                        )),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .child(controls::input_box(self.api_token_field.clone())),
                        )
                        .child(controls::ui_button(
                            "api-save-token",
                            "保存令牌",
                            None,
                            controls::ButtonTone::Primary,
                            can_save_token,
                            true,
                            show_focus,
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
                            row.child(controls::ui_button(
                                "api-clear-token",
                                "清除令牌",
                                None,
                                controls::ButtonTone::Danger,
                                true,
                                true,
                                show_focus,
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
                );
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
            .child(token_row)
            .child(settings_row(
                "跨域来源",
                "选择可从网页访问本机接口的来源",
                cors_control,
            ))
            .when(config.cors == "custom", |body| {
                body.child(
                    div()
                        .py_3()
                        .border_b_1()
                        .border_color(theme_color(LINE))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(div().font_weight(FontWeight::MEDIUM).child("允许的网站"))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme_color(MUTED))
                                        .child("用逗号分隔，最多 16 个来源地址"),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .child(controls::input_box(self.api_cors_field.clone())),
                                )
                                .child(controls::ui_button(
                                    "api-save-origins",
                                    "保存来源",
                                    None,
                                    controls::ButtonTone::Primary,
                                    true,
                                    true,
                                    show_focus,
                                    cx,
                                    |this, _, cx| {
                                        let mut next = this.api_config.clone();
                                        next.cors_origins = this.read_cors_origins(cx);
                                        if this.apply_api_config(next, cx) {
                                            this.settings_feedback =
                                                Some("已保存允许的网站".into());
                                        }
                                    },
                                )),
                        ),
                )
            })
            .child(div().pt_3().flex().child(self.tiny_button(
                if self.api_reference_open {
                    "收起接口列表"
                } else {
                    "查看接口列表"
                },
                "api-reference",
                cx,
                |this, _, cx| {
                    this.api_reference_open = !this.api_reference_open;
                    cx.notify();
                },
            )))
            .when(self.api_reference_open, |body| {
                body.child(div().py_3().flex().flex_col().gap_3().children(
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
            })
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
        controls::toggle_switch(id, enabled, self.keyboard_focus_visible, cx, toggle)
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
                .id("diagnostics-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .bg(fade(theme_color(SCRIM), 0.32))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.diagnostics_open = false;
                        window.focus(&this.focus);
                        cx.notify();
                    }),
                )
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
                        .rounded(px(14.))
                        .border_1()
                        .border_color(fade(theme_color(CONTROL_TINT), 0.14))
                        .bg(theme_color(PANEL))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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

    fn skip_track(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.modal_open() || self.guard_library_playback() {
            cx.notify();
            return;
        }
        if forward {
            self.failed_tracks.clear();
            self.advance(TrackStart::Selected);
        } else {
            self.previous();
        }
        cx.notify();
    }

    fn toggle_queue(&mut self, cx: &mut Context<Self>) {
        self.queue_open = !self.queue_open;
        self.diagnostics_open = false;
        if self.queue_open && !self.queue.is_empty() {
            self.queue_scroll
                .scroll_to_item(self.queue_index, gpui::ScrollStrategy::Center);
        }
        cx.notify();
    }

    fn enqueue_track(&mut self, key: &str, play_next: bool, cx: &mut Context<Self>) {
        if self.guard_library_playback() || self.library.song(key).is_none() {
            cx.notify();
            return;
        }
        if self.queue.is_empty() {
            self.queue.push(key.to_owned());
            self.queue_index = 0;
            self.failed_tracks.clear();
            self.load_key(key, false);
        } else {
            let index = if play_next {
                self.queue_index + 1
            } else {
                self.queue.len()
            };
            self.queue.insert(index, key.to_owned());
        }
        let title = self.song_title(key);
        self.notice = if play_next {
            format!("《{title}》已排在当前曲目后")
        } else {
            format!("《{title}》已添加到队列")
        };
        self.queue_open = true;
        self.queue_scroll.scroll_to_item(
            if play_next {
                (self.queue_index + 1).min(self.queue.len() - 1)
            } else {
                self.queue.len() - 1
            },
            gpui::ScrollStrategy::Center,
        );
        cx.notify();
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
                        let remove = this.icon_button(
                            ("queue-remove", index),
                            "x",
                            "移出队列",
                            false,
                            cx,
                            move |this, window, cx| {
                                if this.queue.get(index) != Some(&remove_key) {
                                    return;
                                }
                                if this.queue_index == index {
                                    this.confirm_queue_removal(
                                        Some((index, remove_key.clone())),
                                        window,
                                        cx,
                                    );
                                } else if !this.guard_library_playback() {
                                    this.remove_from_queue(Some(index));
                                }
                                cx.notify();
                            },
                        );
                        Some(
                            div()
                                .w_full()
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
                                        .text_color(theme_color(MUTED))
                                        .child(if current {
                                            icon(
                                                if this.playing {
                                                    "audio-lines"
                                                } else {
                                                    "pause-fill"
                                                },
                                                14.,
                                                theme_color(ACCENT),
                                            )
                                            .into_any_element()
                                        } else {
                                            div().child((index + 1).to_string()).into_any_element()
                                        }),
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
                                .when(current, |row| {
                                    row.child(
                                        div()
                                            .flex_none()
                                            .text_xs()
                                            .text_color(theme_color(ACCENT))
                                            .child(if this.playing {
                                                "正在播放"
                                            } else {
                                                "当前曲目"
                                            }),
                                    )
                                })
                                .child(remove),
                        )
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .w_full()
        .track_scroll(self.queue_scroll.clone())
        .h(px((count as f32 * 38.0).clamp(38.0, 342.0)));
        Some(
            div()
                .id("queue-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .bg(fade(theme_color(SCRIM), 0.14))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.queue_open = false;
                        window.focus(&this.focus);
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .absolute()
                        .right(px(20.))
                        .bottom(px(112.))
                        .w(px(360.))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .bg(theme_color(PANEL))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(fade(theme_color(CONTROL_TINT), 0.14))
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
                                        "清空队列",
                                        "queue-clear",
                                        cx,
                                        |this, window, cx| {
                                            this.confirm_queue_removal(None, window, cx);
                                        },
                                    ))
                                })
                                .child(controls::ui_icon_button(
                                    "queue-close",
                                    "x",
                                    false,
                                    true,
                                    self.keyboard_focus_visible,
                                    cx,
                                    |this, _, window, cx| {
                                        this.queue_open = false;
                                        window.focus(&this.focus);
                                        cx.notify();
                                    },
                                )),
                        )
                        .when(count > 0, |panel| panel.child(rows))
                        .when(count == 0, |panel| {
                            panel.child(
                                div()
                                    .p_2()
                                    .text_xs()
                                    .text_color(theme_color(MUTED))
                                    .child("在曲库播放曲目，或从曲目菜单添加到队列。"),
                            )
                        }),
                )
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
        controls::keyboard_control("player-rate", self.keyboard_focus_visible)
            .tooltip(|_, cx| cx.new(|_| Tip("更改播放速度".into())).into())
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
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, window, cx);
                this.after_button_click(window, cx);
            }))
            .child(rate)
            .into_any_element()
    }

    fn player_icon_button(
        &mut self,
        id: &'static str,
        icon_name: &'static str,
        label: &'static str,
        active: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        controls::keyboard_control(id, self.keyboard_focus_visible)
            .tooltip(move |_, cx| cx.new(|_| Tip(label.into())).into())
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.08)))
            .on_click(cx.listener(move |this, _, window, cx| {
                on_click(this, window, cx);
                this.after_button_click(window, cx);
            }))
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
            LibraryFilter::Category(id) if id == crate::library::UNCATEGORIZED_CATEGORY_ID => (
                "未分类音频".to_owned(),
                "位于已添加文件夹中但不在具体场景目录下的本地音频。".to_owned(),
                "未分类".to_owned(),
            ),
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
        self.queue_open = false;
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
        let menu_height = 371.
            + if stem_count > 0 {
                32. * (stem_count + 1) as f32
            } else {
                0.
            };
        let top = (position.y + px(5.))
            .min(window.viewport_size().height - px(menu_height))
            .max(px(8.));
        let reveal_path = self
            .library
            .song(&key)
            .map(|song| PathBuf::from(&song.path));
        let rename_key = key.clone();
        let move_key = key.clone();
        let export_key = key.clone();
        let loop_key = key.clone();
        let separation_key = key.clone();
        let separation_delete_key = key.clone();
        let subtitle_key = key.clone();
        let separating = self.separating.contains(&key);
        let next_key = key.clone();
        let append_key = key.clone();
        let trash_key = key;

        Some(
            div()
                .absolute()
                .inset_0()
                .occlude()
                .track_focus(&self.track_menu_focus)
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
                            "menu-play-next",
                            "skip-forward",
                            "排在当前曲目后",
                            false,
                            cx,
                            move |this, _, cx| this.enqueue_track(&next_key, true, cx),
                        ))
                        .child(self.menu_action(
                            "menu-add-queue",
                            "list-music",
                            "添加到播放队列",
                            false,
                            cx,
                            move |this, _, cx| this.enqueue_track(&append_key, false, cx),
                        ))
                        .child(div().h(px(1.)).mx_2().my_1().bg(theme_color(LINE)))
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
                                let default_cat = this.default_import_target().1;
                                this.dialog_feedback = None;
                                this.dialog = Some(Dialog::Move {
                                    key: move_key.clone(),
                                    category: default_cat,
                                });
                                cx.notify();
                            },
                        ))
                        .child(self.menu_action(
                            "menu-loop",
                            "repeat",
                            "编辑 A/B 循环点",
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
                        .when_some(reveal_path, |menu, path| {
                            menu.child(self.menu_action(
                                "menu-reveal",
                                "folder",
                                "在 Finder 中显示",
                                false,
                                cx,
                                move |this, _, cx| {
                                    this.action_menu = None;
                                    cx.reveal_path(&path);
                                    cx.notify();
                                },
                            ))
                        })
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
                            "移到曲库回收站",
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
        controls::keyboard_control(id, self.keyboard_focus_visible)
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
                this.after_button_click(window, cx);
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
        label: impl Into<SharedString>,
        id: impl Into<gpui::ElementId>,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> gpui::AnyElement {
        let label = label.into();
        let tone = if label == "确定" || label == "保存节目" {
            controls::ButtonTone::Primary
        } else {
            controls::ButtonTone::Secondary
        };
        controls::ui_button(
            id,
            label,
            None,
            tone,
            true,
            true,
            self.keyboard_focus_visible,
            cx,
            on_click,
        )
        .into_any_element()
    }

    fn dialog_layer(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let dialog = self.dialog.clone()?;
        let show_focus = self.keyboard_focus_visible;
        let (heading, body) = match &dialog {
            Dialog::EditTrack { .. } => ("编辑曲目".to_owned(), "修改曲名和所属分类。".to_owned()),
            Dialog::NewCategory => (
                "新增分类".to_owned(),
                "创建分类，并按需在曲库目录中建立对应子文件夹。".to_owned(),
            ),
            Dialog::EditCategory { .. } => {
                ("编辑分类".to_owned(), "修改显示名称和主题色。".to_owned())
            }
            Dialog::Move { .. } => (
                "移动到分类".to_owned(),
                "将文件移动到原音乐文件夹下的目标分类目录。".to_owned(),
            ),
            Dialog::MoveBatch { keys, .. } => (
                "批量移动".to_owned(),
                format!("将已选的 {} 首曲目移动到目标分类目录。", keys.len()),
            ),
            Dialog::Import { files, .. } => (
                "导入音乐".to_owned(),
                format!(
                    "已选择 {} 个文件，请确认写入的目标文件夹与场景分类。",
                    files.len()
                ),
            ),
            Dialog::FileReport { summary, .. } => ("文件操作结果".to_owned(), summary.clone()),
            Dialog::Loop { key } => (
                format!("循环点 · {}", self.song_title(key)),
                "输入开始和结束时间（秒），播放到终点时会回到起点。".to_owned(),
            ),
        };
        let show_field = matches!(
            dialog,
            Dialog::EditTrack { .. } | Dialog::NewCategory | Dialog::EditCategory { .. }
        );
        let category_detail = match dialog {
            Dialog::NewCategory => Some("目录名（可选）"),
            Dialog::EditCategory { .. } => Some("主题颜色"),
            _ => None,
        };
        let edit_category_color = matches!(dialog, Dialog::EditCategory { .. });
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
        let online_folders = self
            .library
            .online_folders()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let selected_category = match &dialog {
            Dialog::EditTrack { category, .. }
            | Dialog::Move { category, .. }
            | Dialog::MoveBatch { category, .. }
            | Dialog::Import { category, .. } => Some(category.as_str()),
            _ => None,
        };
        let selected_folder_id = match &dialog {
            Dialog::Import { folder_id, .. } => folder_id.clone(),
            _ => None,
        };
        let is_import = matches!(dialog, Dialog::Import { .. });
        let is_move = matches!(dialog, Dialog::Move { .. } | Dialog::MoveBatch { .. });
        let primary_label = if is_import {
            "确认导入"
        } else if is_move {
            "确认移动"
        } else {
            "保存"
        };
        let edit_error = self.dialog_feedback.clone();
        let lines: Option<Vec<String>> = match &dialog {
            Dialog::FileReport { report, .. } => Some(
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
                    .cloned()
                    .collect(),
            ),
            _ => None,
        };
        let details_only = lines.is_some();

        let folder_cards = if is_import {
            let paths = controls::distinguishing_folder_paths(&online_folders);
            online_folders
                .iter()
                .map(|folder| {
                    let fid = folder.id.clone();
                    let active = selected_folder_id.as_deref() == Some(fid.as_str());
                    let path_hint = paths
                        .get(&fid)
                        .cloned()
                        .unwrap_or_else(|| controls::compact_display_path(&folder.path));
                    div()
                        .id(SharedString::from(format!("import-folder-{fid}")))
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(if active {
                            fade(theme_color(ACCENT), 0.55)
                        } else {
                            theme_color(LINE).into()
                        })
                        .bg(if active {
                            fade(theme_color(ACCENT), 0.12)
                        } else {
                            fade(theme_color(CONTROL_TINT), 0.04)
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.09)))
                        .flex()
                        .items_center()
                        .gap_2p5()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(Dialog::Import {
                                files, category, ..
                            }) = this.dialog.clone()
                            {
                                this.dialog_feedback = None;
                                this.dialog = Some(Dialog::Import {
                                    files,
                                    folder_id: Some(fid.clone()),
                                    category,
                                });
                                cx.notify();
                            }
                        }))
                        .child(icon(
                            "folder-open",
                            15.,
                            theme_color(if active { ACCENT } else { MUTED }),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme_color(TEXT))
                                        .truncate()
                                        .child(folder.display_name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme_color(FAINT))
                                        .truncate()
                                        .child(path_hint),
                                ),
                        )
                        .when(active, |row| {
                            row.child(icon("check", 14., theme_color(ACCENT)))
                        })
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        let mut choices = Vec::new();
        if selected_category.is_some() {
            for category in &categories {
                let id = category.id.clone();
                let name = category.name_zh.clone();
                let selected = selected_category == Some(id.as_str());
                choices.push(
                    controls::option_chip(
                        SharedString::from(format!("pick-{id}")),
                        name,
                        selected,
                        true,
                        show_focus,
                        cx,
                        move |this, _, cx| {
                            this.select_dialog_category(id.clone(), cx);
                        },
                    )
                    .into_any_element(),
                );
            }
        }

        let import_preview = match &dialog {
            Dialog::Import {
                files,
                folder_id,
                category,
            } => {
                let folder_name = folder_id
                    .as_deref()
                    .and_then(|id| online_folders.iter().find(|f| f.id == id))
                    .map(|f| f.display_name.as_str())
                    .unwrap_or("未选择文件夹");
                let cat_name = categories
                    .iter()
                    .find(|c| &c.id == category)
                    .map(|c| c.name_zh.as_str())
                    .unwrap_or(category.as_str());
                Some(format!(
                    "将复制 {} 个音频到「{folder_name}」下的「{cat_name}（{category}/）」目录",
                    files.len()
                ))
            }
            _ => None,
        };

        let swatches = [
            "#D97757", "#4E9F8B", "#7C83FD", "#E06C75", "#E5C07B", "#56B6C2", "#C678DD", "#34D399",
        ];
        let current_hex = self.category_detail_field.read(cx).text().trim().to_owned();

        Some(
            controls::modal_backdrop(
                "dialog-backdrop",
                &self.dialog_focus,
                cx,
                move |this, window, cx| {
                    if !show_field && !loop_dialog {
                        this.dialog = None;
                        window.focus(&this.focus);
                        cx.notify();
                    }
                },
            )
            .child(
                controls::modal_card(if is_import { 480. } else { 440. })
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3p5()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(theme_color(TEXT))
                                    .child(heading),
                            )
                            .child(div().text_xs().text_color(theme_color(MUTED)).child(body)),
                    )
                    .when_some(lines, |body, lines| {
                        body.child(
                            div()
                                .id("details-lines")
                                .max_h(px(280.))
                                .overflow_y_scroll()
                                .p_3()
                                .rounded_md()
                                .bg(fade(theme_color(CONTROL_TINT), 0.04))
                                .border_1()
                                .border_color(theme_color(LINE))
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .children(lines.into_iter().map(|line| {
                                    div().text_xs().text_color(theme_color(MUTED)).child(line)
                                })),
                        )
                    })
                    .when(show_field, |panel| {
                        panel.child(controls::input_box(self.dialog_field.clone()))
                    })
                    .when_some(category_detail, |dialog_el, label| {
                        dialog_el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme_color(MUTED))
                                                .child(label),
                                        )
                                        .when(edit_category_color, |row| {
                                            row.child(
                                                div()
                                                    .size(px(14.))
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(fade(
                                                        theme_color(CONTROL_TINT),
                                                        0.2,
                                                    ))
                                                    .bg(parse_color(&current_hex)),
                                            )
                                        }),
                                )
                                .when(edit_category_color, |col| {
                                    col.child(div().flex().flex_wrap().gap_2().children(
                                        swatches.into_iter().enumerate().map(|(i, hex)| {
                                            let picked = current_hex.eq_ignore_ascii_case(hex);
                                            let swatch_hex = hex.to_owned();
                                            div()
                                                .id(("cat-swatch", i))
                                                .size(px(24.))
                                                .rounded_full()
                                                .bg(parse_color(hex))
                                                .border_2()
                                                .border_color(if picked {
                                                    fade(theme_color(TEXT), 1.0)
                                                } else {
                                                    fade(theme_color(CONTROL_TINT), 0.12)
                                                })
                                                .cursor_pointer()
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.category_detail_field.update(
                                                        cx,
                                                        |field, cx| {
                                                            field.set_text(swatch_hex.clone(), cx);
                                                        },
                                                    );
                                                    cx.notify();
                                                }))
                                        }),
                                    ))
                                })
                                .child(controls::input_box(self.category_detail_field.clone())),
                        )
                    })
                    .when(loop_dialog, |this| {
                        this.child(
                            div().flex().gap_2().children(
                                [
                                    ("开始时间（秒）", self.loop_start_field.clone()),
                                    ("结束时间（秒）", self.loop_end_field.clone()),
                                ]
                                .map(|(label, field)| {
                                    div()
                                        .flex_1()
                                        .min_w(px(0.))
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(theme_color(MUTED))
                                                .child(label),
                                        )
                                        .child(controls::input_box(field))
                                }),
                            ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .py_1()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme_color(TEXT))
                                                .child("启用区间循环"),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(11.))
                                                .text_color(theme_color(FAINT))
                                                .child("关闭时仍会保留循环点标记"),
                                        ),
                                )
                                .child(controls::toggle_switch(
                                    "loop-enabled",
                                    loop_enabled,
                                    show_focus,
                                    cx,
                                    |this, _, cx| {
                                        this.loop_enabled_pending = !this.loop_enabled_pending;
                                        cx.notify();
                                    },
                                )),
                        )
                        .when(has_existing_loop, |this| {
                            this.child(div().flex().child(controls::ui_button(
                                "loop-clear",
                                "清除循环点标记",
                                Some("trash"),
                                controls::ButtonTone::Danger,
                                true,
                                true,
                                show_focus,
                                cx,
                                move |this, window, cx| {
                                    if let Some(key) = loop_key_for_clear.clone() {
                                        match this.library.remove_loop_point(&key) {
                                            Ok(_) => {
                                                this.show_toast("已清除循环点", false, cx);
                                                this.refresh_loop_region();
                                            }
                                            Err(error) => this.show_toast(error, true, cx),
                                        }
                                    }
                                    this.dialog = None;
                                    window.focus(&this.focus);
                                    cx.notify();
                                },
                            )))
                        })
                    })
                    .when(!folder_cards.is_empty(), |dialog_el| {
                        dialog_el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme_color(MUTED))
                                        .child("1. 目标音乐文件夹"),
                                )
                                .child(
                                    div()
                                        .id("import-folders-scroll")
                                        .max_h(px(156.))
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_col()
                                        .gap_1p5()
                                        .children(folder_cards),
                                ),
                        )
                    })
                    .when(!choices.is_empty(), |dialog_el| {
                        dialog_el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1p5()
                                .when(is_import, |col| {
                                    col.child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(theme_color(MUTED))
                                            .child("2. 目标场景分类"),
                                    )
                                })
                                .child(
                                    div()
                                        .id("dialog-category-choices")
                                        .max_h(px(150.))
                                        .overflow_y_scroll()
                                        .flex()
                                        .flex_wrap()
                                        .gap_1p5()
                                        .children(choices),
                                ),
                        )
                    })
                    .when_some(import_preview, |dialog_el, preview| {
                        dialog_el.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded_md()
                                .bg(fade(theme_color(ACCENT), 0.09))
                                .border_1()
                                .border_color(fade(theme_color(ACCENT), 0.25))
                                .text_xs()
                                .text_color(theme_color(TEXT))
                                .child(preview),
                        )
                    })
                    .when_some(edit_error, |dialog_el, error| {
                        dialog_el
                            .child(div().text_xs().text_color(theme_color(DANGER)).child(error))
                    })
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(controls::ui_button(
                                "dialog-cancel",
                                if details_only { "关闭" } else { "取消" },
                                None,
                                controls::ButtonTone::Secondary,
                                true,
                                false,
                                show_focus,
                                cx,
                                |this, window, cx| {
                                    this.dialog = None;
                                    this.dialog_feedback = None;
                                    window.focus(&this.focus);
                                    cx.notify();
                                },
                            ))
                            .when(show_field || loop_dialog || is_import || is_move, |this| {
                                let enabled = !is_import || selected_folder_id.is_some();
                                this.child(controls::ui_button(
                                    "dialog-ok",
                                    primary_label,
                                    Some("check"),
                                    controls::ButtonTone::Primary,
                                    enabled,
                                    false,
                                    show_focus,
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

    fn select_dialog_category(&mut self, category: String, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        self.dialog_feedback = None;
        self.dialog = Some(match dialog {
            Dialog::EditTrack { key, .. } => Dialog::EditTrack { key, category },
            Dialog::Move { key, .. } => Dialog::Move { key, category },
            Dialog::MoveBatch { keys, .. } => Dialog::MoveBatch { keys, category },
            Dialog::Import {
                files, folder_id, ..
            } => Dialog::Import {
                files,
                folder_id,
                category,
            },
            other => other,
        });
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
                .track_focus(&self.picker_focus)
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
                        .child(audio_tools::tool_text_input(self.picker_search.clone()))
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
                                .child(self.settings_button(
                                    "picker-ok",
                                    if captions {
                                        "选定曲目"
                                    } else if multi {
                                        "添加所选曲目"
                                    } else {
                                        "替换曲目"
                                    },
                                    true,
                                    selected_count > 0,
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
        controls::keyboard_control(
            SharedString::from(format!("picker-row-{}", song.key)),
            self.keyboard_focus_visible,
        )
        .w_full()
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

    fn export_settings(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let show_focus = self.keyboard_focus_visible;
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
            (false, _) => "当前已关闭 · 单首和批量导出时均直接复制原始文件".to_owned(),
            (true, Some(preset)) => {
                format!(
                    "已开启 · {} LUFS · 适用于「{}」",
                    preset.target_lufs, preset.label
                )
            }
            (true, None) => "已开启 · 导出时进行人耳响度归一化".to_owned(),
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .pt_2()
            .child(
                div()
                    .p_3p5()
                    .rounded_lg()
                    .bg(if enabled {
                        fade(theme_color(ACCENT), 0.1)
                    } else {
                        fade(theme_color(CONTROL_TINT), 0.03)
                    })
                    .border_1()
                    .border_color(if enabled {
                        fade(theme_color(ACCENT), 0.35)
                    } else {
                        theme_color(LINE).into()
                    })
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
                            .gap_0p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("响度归一化"),
                            )
                            .child(div().text_xs().text_color(theme_color(MUTED)).child(hint)),
                    )
                    .child(controls::toggle_switch(
                        "export-toggle",
                        enabled,
                        show_focus,
                        cx,
                        |this, _, cx| {
                            let enabled = this.library.prefs.loudness_normalization;
                            let scene = this.library.prefs.export_scene.clone();
                            if let Err(error) = this.library.set_export_settings(!enabled, &scene) {
                                this.settings_feedback = Some(error);
                            } else {
                                this.settings_feedback = None;
                            }
                            cx.notify();
                        },
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme_color(MUTED))
                    .child("导出用途预设（点击即可启用）"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1p5()
                    .children(presets.into_iter().map(|preset| {
                        let active = enabled
                            && selected
                                .as_ref()
                                .is_some_and(|current| current.id == preset.id);
                        let is_default = preset.id == bgm_core::loudness::DEFAULT_PRESET_ID;
                        controls::keyboard_control(
                            SharedString::from(format!("export-preset-{}", preset.id)),
                            show_focus,
                        )
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(if active {
                            fade(theme_color(ACCENT), 0.45)
                        } else {
                            theme_color(LINE).into()
                        })
                        .bg(if active {
                            fade(theme_color(ACCENT), 0.12)
                        } else {
                            fade(theme_color(CONTROL_TINT), 0.03)
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.08)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Err(error) = this.library.set_export_settings(true, &preset.id) {
                                this.settings_feedback = Some(error);
                            } else {
                                this.settings_feedback = None;
                            }
                            cx.notify();
                        }))
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap_0p5()
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
                                                .child(preset.label),
                                        )
                                        .when(is_default, |row| {
                                            row.child(
                                                div()
                                                    .px_1p5()
                                                    .rounded(px(4.))
                                                    .bg(fade(theme_color(ACCENT), 0.14))
                                                    .text_size(px(10.))
                                                    .text_color(theme_color(ACCENT))
                                                    .child("推荐默认"),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme_color(FAINT))
                                        .child(preset.description),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme_color(MUTED))
                                .child(format!("{} LUFS", preset.target_lufs)),
                        )
                        .when(active, |row| {
                            row.child(icon("check", 14., theme_color(ACCENT)))
                        })
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(fade(theme_color(CONTROL_TINT), 0.04))
                    .text_xs()
                    .text_color(if ffmpeg.is_some() {
                        theme_color(MUTED)
                    } else {
                        theme_color(WARNING)
                    })
                    .child(if ffmpeg.is_some() {
                        icon("check", 13., theme_color(ACCENT)).into_any_element()
                    } else {
                        icon("alert", 13., theme_color(WARNING)).into_any_element()
                    })
                    .child(match ffmpeg {
                        Some(path) => format!("ffmpeg 已就绪：{}", path.display()),
                        None if self.ffmpeg_detected.is_none() => "正在检测 ffmpeg…".to_owned(),
                        None => "未检测到 ffmpeg，导出时将回退为复制原文件".to_owned(),
                    }),
            )
            .into_any_element()
    }

    // ---------- 渲染：壁纸 ----------

    fn poll_wallpaper(&mut self, cx: &mut Context<Self>) {
        let id = self.library.prefs.wallpaper.as_str();
        let source = self.wallpaper_view().source;
        if id != "default" && matches!(source, WallpaperSource::None) {
            self.restore_default_wallpaper("找不到选定的壁纸文件".into(), cx);
            return;
        }
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
            id if id.starts_with("builtin-") => {
                let matched = self
                    .builtin_wallpapers
                    .iter()
                    .find(|item| item.id == id)
                    .or_else(|| {
                        (id == "builtin-1")
                            .then(|| self.builtin_wallpapers.iter().find(|item| item.video))
                            .flatten()
                    })
                    .or_else(|| {
                        (id == "builtin-2")
                            .then(|| {
                                self.builtin_wallpapers
                                    .iter()
                                    .find(|item| item.label == "雪山云海")
                            })
                            .flatten()
                    });
                match matched {
                    Some(item) if item.video => WallpaperSource::Video {
                        path: item.path.clone(),
                        contain,
                    },
                    Some(item) => WallpaperSource::Image {
                        asset: wallpaper::Asset::Local(item.path.clone()),
                        contain,
                    },
                    None if id == "builtin-2" => WallpaperSource::Image {
                        asset: wallpaper::Asset::Embedded,
                        contain,
                    },
                    None => WallpaperSource::None,
                }
            }
            _ => WallpaperSource::None,
        };
        WallpaperView {
            has_wallpaper: id != "default" && !matches!(source, WallpaperSource::None),
            opacity,
            source,
        }
    }

    fn wallpaper_settings(&mut self, _window: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let show_focus = self.keyboard_focus_visible;
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
            .unwrap_or_else(|| "选择图片或 MP4 视频…".into());
        let selected_custom = current == "custom";
        let opacity = self.library.prefs.wallpaper_opacity.clamp(0.0, 1.0);
        let fit = self.library.prefs.wallpaper_fit.clone();
        let fit_mode = if fit == "contain" { "contain" } else { "cover" };
        let opacity_view = cx.entity().downgrade();

        let mut options: Vec<(String, String, &'static str)> = vec![
            ("default".into(), "默认样式".into(), "当前配色"),
            ("gradient-blue".into(), "渐变蓝".into(), "深邃静谧"),
            ("gradient-green".into(), "渐变绿".into(), "翡翠流光"),
            ("gradient-red".into(), "渐变红".into(), "暖色余晖"),
        ];
        options.extend(self.builtin_wallpapers.iter().map(|item| {
            (
                item.id.clone(),
                item.label.clone(),
                if item.video {
                    "MP4 动态"
                } else {
                    "静态图片"
                },
            )
        }));

        div()
            .flex()
            .flex_col()
            .gap_3()
            .pt_2()
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme_color(MUTED))
                    .child("预设背景"),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .children(options.into_iter().map(|(id, label, hint)| {
                        let active = current == id;
                        controls::keyboard_control(
                            SharedString::from(format!("wall-{id}")),
                            show_focus,
                        )
                        .min_w(px(165.))
                        .h(px(40.))
                        .px_3()
                        .rounded_md()
                        .border_1()
                        .border_color(if active {
                            fade(theme_color(ACCENT), 0.45)
                        } else {
                            theme_color(LINE).into()
                        })
                        .bg(if active {
                            fade(theme_color(ACCENT), 0.14)
                        } else {
                            fade(theme_color(CONTROL_TINT), 0.03)
                        })
                        .cursor_pointer()
                        .hover(|style| style.bg(fade(theme_color(CONTROL_TINT), 0.08)))
                        .flex()
                        .items_center()
                        .gap_2()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Err(error) = this.library.set_wallpaper(&id, None) {
                                this.settings_feedback = Some(error);
                            } else {
                                this.settings_feedback = None;
                            }
                            cx.notify();
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .truncate()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
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
                    })),
            )
            .child(settings_row(
                "自定义壁纸",
                "支持 JPG、PNG、WebP 图片和静音循环 MP4 视频",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(controls::option_chip(
                        "wall-custom",
                        custom_name,
                        selected_custom,
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| {
                            this.choose_custom_wallpaper(cx);
                        },
                    ))
                    .when(current != "default", |row| {
                        row.child(controls::ui_button(
                            "wall-reset-default",
                            "恢复默认",
                            None,
                            controls::ButtonTone::Ghost,
                            true,
                            true,
                            show_focus,
                            cx,
                            |this, _, cx| {
                                if let Err(error) = this.library.set_wallpaper("default", None) {
                                    this.settings_feedback = Some(error);
                                }
                                cx.notify();
                            },
                        ))
                    }),
            ))
            .child(settings_row(
                "显示强度",
                "调整壁纸在主界面背景中的显露程度",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .w(px(136.))
                            .on_children_prepainted(move |bounds, _, cx| {
                                opacity_view
                                    .update(cx, |this, _| {
                                        this.wallpaper_opacity_bounds = bounds.first().copied();
                                    })
                                    .ok();
                            })
                            .id("wallpaper-opacity")
                            .h(px(24.))
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
                                    .relative()
                                    .h(px(4.))
                                    .w_full()
                                    .rounded_full()
                                    .bg(theme_color(LINE))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(opacity))
                                            .rounded_full()
                                            .bg(theme_color(ACCENT)),
                                    )
                                    .child(
                                        div()
                                            .absolute()
                                            .left(relative(opacity))
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
                            .w(px(38.))
                            .text_right()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .child(format!("{}%", (opacity * 100.0).round() as i32)),
                    ),
            ))
            .child(settings_row(
                "缩放填充",
                "选择壁纸适应窗口尺寸的方式",
                controls::segmented_control(
                    "wall-fit",
                    &[("cover", "铺满裁剪"), ("contain", "完整展示")],
                    fit_mode,
                    show_focus,
                    cx,
                    |this, mode, _, cx| {
                        if let Err(error) = this.library.set_wallpaper_fit(mode) {
                            this.settings_feedback = Some(error);
                        }
                        cx.notify();
                    },
                ),
            ))
            .child(settings_row(
                "图像模糊",
                "用于图片和视频壁纸（0–20 px）",
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(controls::ui_button(
                        "wall-blur-less",
                        "−",
                        None,
                        controls::ButtonTone::Secondary,
                        self.library.prefs.wallpaper_blur > 0,
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| this.adjust_wallpaper_blur(-2, cx),
                    ))
                    .child(
                        div()
                            .w(px(44.))
                            .text_center()
                            .text_xs()
                            .child(format!("{} px", self.library.prefs.wallpaper_blur)),
                    )
                    .child(controls::ui_button(
                        "wall-blur-more",
                        "+",
                        None,
                        controls::ButtonTone::Secondary,
                        self.library.prefs.wallpaper_blur < 20,
                        true,
                        show_focus,
                        cx,
                        |this, _, cx| this.adjust_wallpaper_blur(2, cx),
                    )),
            ))
            .into_any_element()
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
                        this.show_toast("请选择 JPG、PNG、WebP 图片或 MP4 视频", true, cx);
                    } else if let Err(error) = this
                        .library
                        .set_wallpaper("custom", Some(&path.to_string_lossy()))
                    {
                        this.show_toast(error, true, cx);
                    } else {
                        this.show_toast("已应用自定义壁纸", false, cx);
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
        if self.schedule_draft.is_some() {
            return div()
                .id("schedule-editor-view")
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .bg(fade(theme_color(BG), panel))
                .child(div().flex().px_6().py_2().child(self.tool_button(
                    "back-to-schedule",
                    "chevron-left",
                    "返回节目单",
                    false,
                    cx,
                    |this, window, cx| this.cancel_schedule_edit(window, cx),
                )))
                .child(self.schedule_editor(window, cx));
        }
        let entries = self.library.schedule.entries.clone();
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
            list = list.child(self.schedule_entry_row(entry, &current, window, cx));
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
            .child(list)
    }

    fn schedule_entry_row(
        &mut self,
        entry: &ScheduleEntry,
        current: &Option<ScheduleEntry>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let panel = self.panel_alpha();
        let is_current = current.as_ref().is_some_and(|item| item.id == entry.id);
        let expanded = self.expanded_schedule.contains(&entry.id);
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
                            .id(SharedString::from(format!("slot-expand-{}", entry.id)))
                            .cursor_pointer()
                            .child(icon(
                                if expanded {
                                    "chevron-down"
                                } else {
                                    "chevron-right"
                                },
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
            .when(expanded, |this| {
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
        controls::ui_button(
            id.into(),
            label,
            None,
            if danger {
                controls::ButtonTone::Danger
            } else {
                controls::ButtonTone::Ghost
            },
            true,
            true,
            self.keyboard_focus_visible,
            cx,
            on_click,
        )
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
            .map(|draft| !draft.original.id.is_empty())
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
            .mt_2()
            .mb_4()
            .flex_none()
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
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_base()
                    .child(draft_name_hint),
            )
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
                            .child(audio_tools::tool_text_input(
                                self.schedule_name_field.clone(),
                            )),
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
                            .child(audio_tools::tool_text_input(
                                self.schedule_start_field.clone(),
                            )),
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
                            .child(audio_tools::tool_text_input(
                                self.schedule_end_field.clone(),
                            )),
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
                    .when(song_count > 0, |row| {
                        row.child(self.slot_action(
                            "draft-autofill",
                            "按曲目时长排期",
                            false,
                            cx,
                            |this, _, cx| this.auto_fill_draft_songs(cx),
                        ))
                    })
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
                            .child("添加歌曲后，可以逐首设置时间，或按时长自动排期。"),
                    )
                },
            ))
            .children(
                self.schedule_feedback
                    .clone()
                    .map(|error| audio_tools::tool_banner(error, true)),
            )
            .when(self.schedule_discard_pending, |editor| {
                editor.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().child(audio_tools::tool_banner(
                            "节目有未保存的更改。".into(),
                            false,
                        )))
                        .child(self.tiny_button(
                            "继续编辑",
                            "keep-schedule-draft",
                            cx,
                            |this, _, cx| {
                                this.schedule_discard_pending = false;
                                cx.notify();
                            },
                        ))
                        .child(self.tiny_button(
                            "放弃更改",
                            "discard-schedule-draft",
                            cx,
                            |this, window, cx| {
                                this.close_schedule_draft(window, cx);
                            },
                        )),
                )
            })
            .when(!self.schedule_discard_pending, |editor| {
                editor.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme_color(MUTED))
                                .child("曲目的播放时间应在节目时段内。"),
                        )
                        .child(div().flex_1())
                        .child(self.tiny_button(
                            "取消",
                            "schedule-cancel",
                            cx,
                            |this, window, cx| this.cancel_schedule_edit(window, cx),
                        ))
                        .child(self.hero_button(
                            "schedule-save",
                            "check",
                            "保存节目",
                            true,
                            cx,
                            |this, window, cx| this.save_schedule_draft(window, cx),
                        )),
                )
            })
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

fn settings_section(title: &'static str) -> gpui::Div {
    div()
        .pt_4()
        .pb_1()
        .font_weight(FontWeight::SEMIBOLD)
        .text_sm()
        .child(title)
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

#[derive(Clone, Copy)]
enum ModelStatusTone {
    Ready,
    Warning,
    Neutral,
}

impl ModelStatusTone {
    fn fg(self) -> Rgba {
        match self {
            Self::Ready => theme_color(ACCENT),
            Self::Warning => theme_color(WARNING),
            Self::Neutral => theme_color(MUTED),
        }
    }

    fn icon_bg(self) -> gpui::Hsla {
        match self {
            Self::Ready => fade(theme_color(ACCENT), 0.14),
            Self::Warning => fade(theme_color(WARNING), 0.16),
            Self::Neutral => fade(theme_color(CONTROL_TINT), 0.08),
        }
    }

    fn pill_bg(self) -> gpui::Hsla {
        match self {
            Self::Ready => fade(theme_color(ACCENT), 0.14),
            Self::Warning => fade(theme_color(WARNING), 0.16),
            Self::Neutral => fade(theme_color(CONTROL_TINT), 0.09),
        }
    }
}

fn display_asr_model_name(id: &str) -> String {
    if id == DEFAULT_MODEL_NAME {
        "Qwen3-ASR 0.6B".to_owned()
    } else {
        id.to_owned()
    }
}

fn model_card_shell(highlight: bool) -> gpui::Div {
    div()
        .rounded_lg()
        .bg(fade(theme_color(CONTROL_TINT), 0.035))
        .border_1()
        .border_color(if highlight {
            fade(theme_color(ACCENT), 0.35)
        } else {
            theme_color(LINE).into()
        })
        .overflow_hidden()
}

fn model_status_pill(label: impl Into<SharedString>, tone: ModelStatusTone) -> gpui::Div {
    div()
        .px_2()
        .py_0p5()
        .rounded_full()
        .flex_none()
        .bg(tone.pill_bg())
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(tone.fg())
        .child(label.into())
}

fn model_progress_bar(percent: f32) -> gpui::Div {
    div().px_3p5().pb_3().child(
        div()
            .h(px(4.))
            .rounded_full()
            .overflow_hidden()
            .bg(theme_color(LINE))
            .child(
                div()
                    .h_full()
                    .w(relative(percent.clamp(0.0, 1.0)))
                    .bg(theme_color(ACCENT)),
            ),
    )
}

fn model_card_header(
    icon_name: &'static str,
    tone: ModelStatusTone,
    title: &str,
    pill: Option<(&str, ModelStatusTone)>,
    detail: &str,
    accessory: impl IntoElement,
) -> gpui::Div {
    div()
        .px_3p5()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .size(px(32.))
                .flex_none()
                .rounded_md()
                .bg(tone.icon_bg())
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_name, 15., tone.fg())),
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
                        .min_w(px(0.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme_color(TEXT))
                                .truncate()
                                .child(title.to_owned()),
                        )
                        .when_some(pill, |row, (label, pill_tone)| {
                            row.child(model_status_pill(label.to_owned(), pill_tone))
                        }),
                )
                .when(!detail.is_empty(), |col| {
                    col.child(
                        div()
                            .text_xs()
                            .text_color(theme_color(MUTED))
                            .truncate()
                            .child(detail.to_owned()),
                    )
                }),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .child(accessory),
        )
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
