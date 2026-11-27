import type { PlayMode, SortBy } from './types';

/** Tauri 命令名；与 Rust `invoke_handler!` 注册的名称保持一致。 */
export const COMMANDS = {
  bootstrap: 'bootstrap',
  reloadConfig: 'reload_config',
  openConfigFile: 'open_config_file',
  systemFonts: 'system_fonts',
  savePrefs: 'save_prefs',
  saveSchedule: 'save_schedule',
  saveLoopPoint: 'save_loop_point',
  removeLoopPoint: 'remove_loop_point',
  rescanLibrary: 'rescan_library',
  chooseLibraryDir: 'choose_library_dir',
  resetLibraryDir: 'reset_library_dir',
  openLibraryDir: 'open_library_dir',
  audioIndexStatus: 'audio_index_status',
  buildAudioIndex: 'build_audio_index',
  disableAudioIndex: 'disable_audio_index',
  audioIndexForSong: 'audio_index_for_song',
  findAudioDuplicates: 'find_audio_duplicates',
  searchLibraryBySound: 'search_library_by_sound',
  searchLibraryByRecording: 'search_library_by_recording',
  deleteAudioDuplicates: 'delete_audio_duplicates',
  parsePlaylistMigrationSource: 'parse_playlist_migration_source',
  savePlaylistMigrationText: 'save_playlist_migration_text',
  exportInfo: 'export_info',
  exportTrack: 'export_track',
  exportTracks: 'export_tracks',
  createCategory: 'create_category',
  updateCategory: 'update_category',
  openCategoriesFile: 'open_categories_file',
  importTracks: 'import_tracks',
  updateTrack: 'update_track',
  moveTracks: 'move_tracks',
  deleteTracks: 'delete_tracks',
  builtinWallpapers: 'builtin_wallpapers',
  chooseWallpaperFile: 'choose_wallpaper_file',
  wallpaperMediaUrl: 'wallpaper_media_url',
  apiInfo: 'api_info',
  apiEndpoints: 'api_endpoints',
  updateApiConfig: 'update_api_config',
  applyDesktopSettings: 'apply_desktop_settings',
  hideToTray: 'hide_to_tray',
  quitApp: 'quit_app',
  apiPublishState: 'api_publish_state',
  apiReply: 'api_reply',
  modelsInfo: 'models_info',
  chooseModelsDir: 'choose_models_dir',
  resetModelsDir: 'reset_models_dir',
  openModelsDir: 'open_models_dir',
  downloadAsrModel: 'download_asr_model',
  loadAsrModel: 'load_asr_model',
  cancelAsrLoad: 'cancel_asr_load',
  unloadAsrModel: 'unload_asr_model',
  deleteAsrModel: 'delete_asr_model',
  cancelAsrDownload: 'cancel_asr_download',
  ttsModels: 'tts_models',
  installTtsModel: 'install_tts_model',
  openModelDownloadSource: 'open_model_download_source',
  saveModelDownloadSource: 'save_model_download_source',
  loadTtsModel: 'load_tts_model',
  installTtsGpu: 'install_tts_gpu',
  removeTtsGpu: 'remove_tts_gpu',
  removeTtsModel: 'remove_tts_model',
  parseTtsScript: 'parse_tts_script',
  normalizeTtsReference: 'normalize_tts_reference',
  listTtsBuiltinVoices: 'list_tts_builtin_voices',
  loadTtsBuiltinReference: 'load_tts_builtin_reference',
  generateTts: 'generate_tts',
  cancelTtsJob: 'cancel_tts_job',
  saveTtsAudio: 'save_tts_audio',
  separationModels: 'separation_models',
  generateSubtitles: 'generate_subtitles',
  cancelSubtitleGeneration: 'cancel_subtitle_generation',
  revealSubtitleAudio: 'reveal_subtitle_audio',
  exportSubtitle: 'export_subtitle',
  subtitleForTrack: 'subtitle_for_track',
  listSubtitles: 'list_subtitles',
  deleteSubtitle: 'delete_subtitle',
  /** 保存分段编辑 / 原文编辑结果，均返回更新后的 StoredSubtitle。 */
  updateSubtitle: 'update_subtitle',
  updateSubtitleText: 'update_subtitle_text',
  setDesktopLyricsEnabled: 'set_desktop_lyrics_enabled',
  syncDesktopLyrics: 'sync_desktop_lyrics',
  desktopLyricsSnapshot: 'desktop_lyrics_snapshot',
  keepDesktopLyricsOnTop: 'keep_desktop_lyrics_on_top',
  resizeDesktopLyrics: 'resize_desktop_lyrics',
  placeDesktopLyrics: 'place_desktop_lyrics',
  setDesktopLyricsLock: 'set_desktop_lyrics_lock',
  /** 打开 / 关闭贴在歌词条上方的独立显示设置悬浮窗。 */
  toggleDesktopLyricsSettings: 'toggle_desktop_lyrics_settings',
} as const;

/** Rust 推给前端的事件名；与 `src-tauri/src/api.rs`、`models.rs`、`subtitles.rs`、`lyrics.rs` 保持一致。 */
export const EVENTS = {
  libraryChanged: 'library-changed',
  audioIndexProgress: 'audio-index-progress',
  audioIndexUpdated: 'audio-index-updated',
  apiCommand: 'api-command',
  modelDownload: 'model-download-progress',
  modelLoadStatus: 'model-load-status',
  subtitleProgress: 'subtitle-progress',
  ttsDownloadProgress: 'tts-download-progress',
  ttsLoadProgress: 'tts-load-progress',
  ttsSynthesisProgress: 'tts-synthesis-progress',
  desktopLyricsControl: 'desktop-lyrics-control',
  desktopLyricsData: 'desktop-lyrics-data',
  desktopLyricsSize: 'desktop-lyrics-size',
  desktopLyricsHover: 'desktop-lyrics-hover',
  /** 歌词窗口 ↔ 设置悬浮窗：显示样式（字体 / 字号 / 配色 / 尺寸 / 锁定）实时同步。 */
  desktopLyricsStyle: 'desktop-lyrics-style',
  /** 设置悬浮窗 → 歌词窗口：锁定 / 恢复默认等只能由歌词窗口执行的动作。 */
  desktopLyricsCommand: 'desktop-lyrics-command',
  /** 设置悬浮窗开关状态，Aa 按钮据此高亮。 */
  desktopLyricsSettings: 'desktop-lyrics-settings-state',
} as const;

export const PLAY_MODES = {
  sequence: 'sequence',
  repeatOne: 'repeat-one',
  shuffle: 'shuffle',
} as const satisfies Record<string, PlayMode>;

export const PLAY_MODE_CYCLE: readonly PlayMode[] = [
  PLAY_MODES.sequence,
  PLAY_MODES.shuffle,
  PLAY_MODES.repeatOne,
];

export const SORT_KEYS = {
  recommended: 'default',
  name: 'name',
} as const satisfies Record<string, SortBy>;

/** 虚拟分类「全部」的 id。 */
export const ALL_CATEGORY = 'all';

export const HERO_KINDS = {
  all: 'all',
  favorites: 'favorites',
} as const;

export type HeroKind = (typeof HERO_KINDS)[keyof typeof HERO_KINDS];

/** 未取到分类颜色时的主题绿。 */
export const DEFAULT_ACCENT = '#19d98b';
/** 收藏页主题色。 */
export const FAVORITES_ACCENT = '#ec4899';
/** 打开响度归一化且尚未选过场景时使用的默认目标。 */
export const DEFAULT_EXPORT_SCENE = 'video-editing';

export const DEFAULT_WALLPAPER = 'default';
export const DEFAULT_WALLPAPER_OPACITY = 0.35;
export const DEFAULT_WALLPAPER_BLUR = 0;
export const DEFAULT_WALLPAPER_FIT = 'cover' as const;

/**
 * 桌面歌词窗口与文字的可自定义尺寸（逻辑像素）。
 * 与 `src-tauri/src/lyrics.rs` 的 `LYRICS_*` 常量保持一致，Rust 侧还会再做一次钳制。
 */
export const LYRICS_SIZE_LIMITS = {
  width: { min: 420, max: 1600, default: 760 },
  height: { min: 120, max: 900, default: 120 },
  fontSize: { min: 12, max: 64, default: 24 },
} as const;

/** 内置字体栈，与 main.css 的 `--font-sans` 保持一致；字体选择为「系统默认」时使用。 */
export const DEFAULT_FONT_STACK =
  '"HarmonyOS Sans SC", "MiSans", "PingFang SC", "Noto Sans CJK SC", "Source Han Sans SC", "Microsoft YaHei", system-ui, -apple-system, "Segoe UI", sans-serif';

/**
 * 旧版本 `prefs.json` 保存的字体 id → 系统字体族名。
 * 字体选择改为读取系统字体后，仅在读取旧配置时做一次映射，新配置直接存族名。
 */
export const LEGACY_UI_FONT_FAMILIES: Readonly<Record<string, string>> = {
  harmony: 'HarmonyOS Sans SC',
  misans: 'MiSans',
  pingfang: 'PingFang SC',
  yahei: 'Microsoft YaHei',
};

/** 打开「音乐淡入淡出」且此前为硬切时使用的默认时长（秒），与 Rust 默认值一致。 */
export const DEFAULT_CROSSFADE_SECONDS = 2;

export const WALLPAPER_OPTIONS = [
  { id: 'default', label: '默认样式', hint: '当前配色', preview: 'default' },
  { id: 'gradient-blue', label: '渐变蓝', hint: '深邃静谧', preview: 'blue' },
  { id: 'gradient-green', label: '渐变绿', hint: '翡翠流光', preview: 'green' },
  { id: 'gradient-red', label: '渐变红', hint: '暖色余晖', preview: 'red' },
  { id: 'builtin-1', label: '幻彩流光', hint: 'MP4 动态', preview: 'builtin-1' },
  { id: 'builtin-2', label: '雪山云海', hint: 'JPG 静态', preview: 'builtin-2' },
] as const;