/**
 * 跨模块共享的契约字符串。
 *
 * 事件名、命令名、播放模式等一旦拼错不会抛错，只会静默失效；
 * 统一从这里引用，改后端命令名时只需改一处。
 */

export const EVENTS = Object.freeze({
  library: 'library',
  view: 'view',
  search: 'search',
  favorites: 'favorites',
  selection: 'selection',
  player: 'player',
  queue: 'queue',
  error: 'error',
});

export const COMMANDS = Object.freeze({
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
});

/** 与 Rust `PlayMode` 的 kebab-case 序列化保持一致。 */
export const PLAY_MODES = Object.freeze({
  sequence: 'sequence',
  repeatOne: 'repeat-one',
  shuffle: 'shuffle',
});

/** 与 Rust `SortBy` 保持一致。 */
export const SORT_KEYS = Object.freeze({
  recommended: 'default',
  name: 'name',
});

/** 与 Rust `LibrarySource` 保持一致。 */
export const LIBRARY_SOURCES = Object.freeze({
  bundled: 'bundled',
  custom: 'custom',
  env: 'env',
});

/** 虚拟分类「全部」的 id。 */
export const ALL_CATEGORY = 'all';

/** 配置中两个虚拟分类文案段的键。 */
export const HERO_KINDS = Object.freeze({
  all: 'all',
  favorites: 'favorites',
});

/** 未取到分类颜色时的主题绿。 */
export const DEFAULT_ACCENT = '#19d98b';
/** 收藏页主题色。 */
export const FAVORITES_ACCENT = '#ec4899';
