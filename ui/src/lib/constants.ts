import type { PlayMode, SortBy } from './types';

/** Tauri 命令名；与 Rust `invoke_handler!` 注册的名称保持一致。 */
export const COMMANDS = {
  bootstrap: 'bootstrap',
  reloadConfig: 'reload_config',
  openConfigFile: 'open_config_file',
  savePrefs: 'save_prefs',
  saveSchedule: 'save_schedule',
  saveLoopPoint: 'save_loop_point',
  removeLoopPoint: 'remove_loop_point',
  rescanLibrary: 'rescan_library',
  chooseLibraryDir: 'choose_library_dir',
  resetLibraryDir: 'reset_library_dir',
  openLibraryDir: 'open_library_dir',
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
  apiInfo: 'api_info',
  apiEndpoints: 'api_endpoints',
  updateApiConfig: 'update_api_config',
  applyDesktopSettings: 'apply_desktop_settings',
  hideToTray: 'hide_to_tray',
  quitApp: 'quit_app',
  apiPublishState: 'api_publish_state',
  apiReply: 'api_reply',
} as const;

/** Rust 推给前端的事件名；与 `src-tauri/src/api.rs` 保持一致。 */
export const EVENTS = {
  libraryChanged: 'library-changed',
  apiCommand: 'api-command',
} as const;

export const PLAY_MODES = {
  sequence: 'sequence',
  repeatOne: 'repeat-one',
  shuffle: 'shuffle',
} as const satisfies Record<string, PlayMode>;

export const PLAY_MODE_CYCLE: readonly PlayMode[] = [
  PLAY_MODES.sequence,
  PLAY_MODES.repeatOne,
  PLAY_MODES.shuffle,
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

/** 内置字体栈，与 main.css 的 `--font-sans` 保持一致；字体选择为「系统默认」时使用。 */
export const DEFAULT_FONT_STACK =
  '"HarmonyOS Sans SC", "MiSans", "PingFang SC", "Noto Sans CJK SC", "Source Han Sans SC", "Microsoft YaHei", system-ui, -apple-system, "Segoe UI", sans-serif';

/** 「常规 · 字体选择」可选字体，id 与 Rust `prefs::UI_FONT_IDS` 一致。 */
export const FONT_OPTIONS = [
  { id: 'system', label: '系统默认', stack: DEFAULT_FONT_STACK },
  { id: 'harmony', label: 'HarmonyOS Sans', stack: '"HarmonyOS Sans SC", "HarmonyOS Sans", sans-serif' },
  { id: 'misans', label: 'MiSans', stack: '"MiSans", "MiSans VF", sans-serif' },
  { id: 'pingfang', label: '苹方 PingFang SC', stack: '"PingFang SC", "Hiragino Sans GB", sans-serif' },
  { id: 'yahei', label: '微软雅黑 Microsoft YaHei', stack: '"Microsoft YaHei", "PingFang SC", sans-serif' },
] as const;

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