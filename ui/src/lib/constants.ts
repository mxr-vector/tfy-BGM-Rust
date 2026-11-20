import type { PlayMode, SortBy } from './types';

/** Tauri 命令名；与 Rust `invoke_handler!` 注册的名称保持一致。 */
export const COMMANDS = {
  bootstrap: 'bootstrap',
  reloadConfig: 'reload_config',
  openConfigFile: 'open_config_file',
  savePrefs: 'save_prefs',
  rescanLibrary: 'rescan_library',
  chooseLibraryDir: 'choose_library_dir',
  resetLibraryDir: 'reset_library_dir',
  openLibraryDir: 'open_library_dir',
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

export const DEFAULT_WALLPAPER = 'default';
export const DEFAULT_WALLPAPER_OPACITY = 0.35;
export const DEFAULT_WALLPAPER_BLUR = 0;
export const DEFAULT_WALLPAPER_FIT = 'cover' as const;

export const WALLPAPER_OPTIONS = [
  { id: 'default', label: '默认样式', hint: '当前配色', preview: 'default' },
  { id: 'gradient-blue', label: '渐变蓝', hint: '深邃静谧', preview: 'blue' },
  { id: 'gradient-green', label: '渐变绿', hint: '翡翠流光', preview: 'green' },
  { id: 'gradient-red', label: '渐变红', hint: '暖色余晖', preview: 'red' },
  { id: 'builtin-1', label: '幻彩流光', hint: 'MP4 动态', preview: 'builtin-1' },
  { id: 'builtin-2', label: '雪山云海', hint: 'JPG 静态', preview: 'builtin-2' },
] as const;