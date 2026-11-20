import { invoke } from './tauri.js';
import { applyConfig } from './config.js';
import {
  ALL_CATEGORY,
  COMMANDS,
  EVENTS,
  LIBRARY_SOURCES,
  SORT_KEYS,
} from './constants.js';

const listeners = new Map();

export const state = {
  library: {
    root: '',
    source: 'bundled',
    songs: [],
    categories: [],
    warnings: [],
    scannedAtMs: 0,
  },
  prefs: null,
  favorites: new Set(),
  category: ALL_CATEGORY,
  favoritesOnly: false,
  search: '',
  sortBy: SORT_KEYS.recommended,
  selected: new Set(),
  queue: [],
  queueIndex: -1,
  currentKey: null,
  currentSong: null,
  playing: false,
};

const collator = new Intl.Collator(['zh-Hans-CN', 'zh', 'en'], {
  numeric: true,
  sensitivity: 'base',
});

let saveTimer = 0;
let saveSequence = 0;

export function on(event, handler) {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event).add(handler);
  return () => listeners.get(event)?.delete(handler);
}

export function emit(event, payload) {
  for (const handler of listeners.get(event) ?? []) {
    handler(payload);
  }
}

export function applyBootstrap(boot) {
  applyConfig(boot.config);
  state.prefs = boot.prefs;
  state.library = boot.library;
  state.favorites = new Set(boot.prefs.favorites);
  state.selected = new Set();
  state.sortBy = boot.prefs.sortBy;
  emit(EVENTS.library);
  emit(EVENTS.favorites);
  emit(EVENTS.selection);
  emit(EVENTS.view);
}

export function replaceLibrary(snapshot) {
  state.library = snapshot;

  let viewChanged = false;
  if (
    state.category !== ALL_CATEGORY &&
    !snapshot.categories.some((category) => category.id === state.category)
  ) {
    state.category = ALL_CATEGORY;
    viewChanged = true;
  }

  const queue = [];
  for (const item of state.queue) {
    const fresh = snapshot.songs.find((song) => song.key === item.key);
    if (fresh) queue.push(fresh);
  }
  state.queue = queue;

  if (state.currentKey) {
    const fresh = snapshot.songs.find((song) => song.key === state.currentKey);
    state.currentSong = fresh ?? null;
    if (!fresh) {
      state.currentKey = null;
    } else {
      state.queueIndex = queue.findIndex((song) => song.key === state.currentKey);
    }
  }

  let pruned = false;
  for (const key of [...state.selected]) {
    if (!snapshot.songs.some((song) => song.key === key)) {
      state.selected.delete(key);
      pruned = true;
    }
  }

  emit(EVENTS.library);
  emit(EVENTS.queue);
  emit(EVENTS.player);
  if (viewChanged) emit(EVENTS.view);
  if (pruned) emit(EVENTS.selection);
}

/** 应用曲库写操作结果：先迁移 key（队列/当前曲目/收藏/选择），再替换快照。 */
export function applyMutation(result) {
  const remap = new Map(
    (result.changes ?? [])
      .filter((change) => change.from !== change.to)
      .map((change) => [change.from, change.to]),
  );

  if (remap.size > 0) {
    const remapKey = (key) => remap.get(key) ?? key;
    state.queue = state.queue.map((song) => ({ ...song, key: remapKey(song.key) }));
    if (state.currentKey) state.currentKey = remapKey(state.currentKey);
    state.favorites = new Set([...state.favorites].map(remapKey));
    state.selected = new Set([...state.selected].map(remapKey));
  }
  for (const key of result.removed ?? []) {
    state.favorites.delete(key);
    state.selected.delete(key);
  }

  replaceLibrary(result.library);
  if (remap.size > 0 || (result.removed ?? []).length > 0) emit(EVENTS.favorites);
  emit(EVENTS.selection);
}

export function categoryById(id) {
  return state.library.categories.find((category) => category.id === id) ?? null;
}

export function songByKey(key) {
  if (!key) return null;
  return state.library.songs.find((song) => song.key === key) ?? null;
}

export function favoriteCount() {
  return state.library.songs.reduce(
    (count, song) => count + (state.favorites.has(song.key) ? 1 : 0),
    0,
  );
}

export function toggleFavorite(key) {
  if (!key) return;
  if (state.favorites.has(key)) {
    state.favorites.delete(key);
  } else {
    state.favorites.add(key);
  }
  emit(EVENTS.favorites);
  persistPrefs(true);
}

export function setFavorites(keys, favorite) {
  let changed = false;
  for (const key of keys) {
    if (!key) continue;
    if (favorite && !state.favorites.has(key)) {
      state.favorites.add(key);
      changed = true;
    } else if (!favorite && state.favorites.delete(key)) {
      changed = true;
    }
  }
  if (!changed) return;
  emit(EVENTS.favorites);
  persistPrefs(true);
}

export function setCategory(categoryId) {
  state.category = categoryId;
  state.favoritesOnly = false;
  emit(EVENTS.view);
}

export function toggleFavoritesOnly() {
  state.favoritesOnly = !state.favoritesOnly;
  emit(EVENTS.view);
}

export function resetFilters() {
  state.category = ALL_CATEGORY;
  state.favoritesOnly = false;
  state.search = '';
  emit(EVENTS.view);
  emit(EVENTS.search);
}

export function setSearch(query) {
  state.search = query;
  emit(EVENTS.search);
}

export function setSortBy(sortBy) {
  if (state.sortBy === sortBy) return;
  state.sortBy = sortBy;
  persistPrefs(true);
  emit(EVENTS.view);
}

/**
 * 切换界面壁纸。
 * `default` / `gradient-*` / `builtin-*` 只改 id；`custom` 需要同时给出本地文件路径。
 * 具体的层渲染由 wallpaper 模块监听 EVENTS.wallpaper 完成。
 */
export function setWallpaper(id, path = null, options = {}) {
  if (!state.prefs) return;
  const customWallpaper =
    id === 'custom' ? path ?? null : state.prefs.customWallpaper ?? null;
  const wallpaperOpacity =
    options.opacity !== null && options.opacity !== undefined
      ? options.opacity
      : state.prefs.wallpaperOpacity ?? 0.35;
  const wallpaperBlur =
    options.blur !== null && options.blur !== undefined
      ? options.blur
      : state.prefs.wallpaperBlur ?? 0;
  const wallpaperFit =
    options.fit !== null && options.fit !== undefined
      ? options.fit
      : state.prefs.wallpaperFit ?? 'cover';

  state.prefs = {
    ...state.prefs,
    wallpaper: id,
    customWallpaper,
    wallpaperOpacity,
    wallpaperBlur,
    wallpaperFit,
  };
  emit(EVENTS.wallpaper, {
    id,
    path: customWallpaper,
    opacity: wallpaperOpacity,
    blur: wallpaperBlur,
    fit: wallpaperFit,
  });
  persistPrefs(true);
}

/**
 * 调整界面透明度（0.0 ~ 1.0）。
 * 用户拖拽滑块时实时触发，先本地生效，停止拖拽后写盘持久化。
 */
export function setWallpaperOpacity(opacity) {
  if (!state.prefs) return;
  const clamped = Math.max(0, Math.min(1, Number(opacity) || 0));
  state.prefs = { ...state.prefs, wallpaperOpacity: clamped };
  emit(EVENTS.wallpaper, {
    id: state.prefs.wallpaper,
    path: state.prefs.customWallpaper ?? null,
    opacity: clamped,
    blur: state.prefs.wallpaperBlur ?? 0,
    fit: state.prefs.wallpaperFit ?? 'cover',
  });
  persistPrefs(false);
}

/**
 * 调整界面模糊度（0 ~ 20 px）。
 */
export function setWallpaperBlur(blur) {
  if (!state.prefs) return;
  const clamped = Math.max(0, Math.min(20, Math.round(Number(blur) || 0)));
  state.prefs = { ...state.prefs, wallpaperBlur: clamped };
  emit(EVENTS.wallpaper, {
    id: state.prefs.wallpaper,
    path: state.prefs.customWallpaper ?? null,
    opacity: state.prefs.wallpaperOpacity ?? 0.35,
    blur: clamped,
    fit: state.prefs.wallpaperFit ?? 'cover',
  });
  persistPrefs(false);
}

/**
 * 切换壁纸填充模式：`cover` 铺满裁剪，`contain` 完整居中展示。
 */
export function setWallpaperFit(fit) {
  if (!state.prefs) return;
  const valid = fit === 'contain' ? 'contain' : 'cover';
  state.prefs = { ...state.prefs, wallpaperFit: valid };
  emit(EVENTS.wallpaper, {
    id: state.prefs.wallpaper,
    path: state.prefs.customWallpaper ?? null,
    opacity: state.prefs.wallpaperOpacity ?? 0.35,
    blur: state.prefs.wallpaperBlur ?? 0,
    fit: valid,
  });
  persistPrefs(true);
}

export function visibleSongs() {
  const list = filterSongs();
  return state.sortBy === SORT_KEYS.name
    ? [...list].sort((a, b) => collator.compare(a.title, b.title))
    : list;
}

export function isSelected(key) {
  return state.selected.has(key);
}

export function setSelected(key, selected) {
  const changed = selected ? !state.selected.has(key) : state.selected.has(key);
  if (!changed) return;
  if (selected) state.selected.add(key);
  else state.selected.delete(key);
  emit(EVENTS.selection);
}

export function selectMany(keys, selected) {
  let changed = false;
  for (const key of keys) {
    if (selected && !state.selected.has(key)) {
      state.selected.add(key);
      changed = true;
    } else if (!selected && state.selected.delete(key)) {
      changed = true;
    }
  }
  if (changed) emit(EVENTS.selection);
}

export function clearSelection() {
  if (state.selected.size === 0) return;
  state.selected.clear();
  emit(EVENTS.selection);
}

export function selectedKeys() {
  return [...state.selected];
}

function filterSongs() {
  let list = state.library.songs;

  if (state.favoritesOnly) {
    list = list.filter((song) => state.favorites.has(song.key));
  } else if (state.category !== ALL_CATEGORY) {
    list = list.filter((song) => song.category === state.category);
  }

  const query = state.search.trim().toLowerCase();
  if (query) {
    list = list.filter((song) => {
      const category = categoryById(song.category);
      return (
        song.title.toLowerCase().includes(query) ||
        song.category.toLowerCase().includes(query) ||
        (category?.nameZh.toLowerCase().includes(query) ?? false) ||
        (category?.nameEn.toLowerCase().includes(query) ?? false) ||
        (category?.labelEn.toLowerCase().includes(query) ?? false)
      );
    });
  }

  return list;
}

export function persistPrefs(immediate = false) {
  if (!state.prefs) return;
  window.clearTimeout(saveTimer);
  if (immediate) {
    void flushPrefs();
    return;
  }
  saveTimer = window.setTimeout(() => {
    void flushPrefs();
  }, 280);
}

async function flushPrefs() {
  const sequence = (saveSequence += 1);
  const payload = {
    ...state.prefs,
    favorites: [...state.favorites],
    sortBy: state.sortBy,
  };
  try {
    const saved = await invoke(COMMANDS.savePrefs, { prefs: payload });
    if (sequence === saveSequence) {
      // 后端会丢弃不合法的壁纸选择（例如自定义文件被删除），此时以返回值为准。
      const wallpaperChanged =
        saved.wallpaper !== state.prefs.wallpaper ||
        (saved.customWallpaper ?? null) !== (state.prefs.customWallpaper ?? null) ||
        Math.abs((saved.wallpaperOpacity ?? 0.35) - (state.prefs.wallpaperOpacity ?? 0.35)) > 0.001 ||
        (saved.wallpaperBlur ?? 0) !== (state.prefs.wallpaperBlur ?? 0) ||
        (saved.wallpaperFit ?? 'cover') !== (state.prefs.wallpaperFit ?? 'cover');
      state.prefs = saved;
      if (wallpaperChanged) {
        emit(EVENTS.wallpaper, {
          id: saved.wallpaper,
          path: saved.customWallpaper ?? null,
          opacity: saved.wallpaperOpacity ?? 0.35,
          blur: saved.wallpaperBlur ?? 0,
          fit: saved.wallpaperFit ?? 'cover',
        });
      }
    }
  } catch (error) {
    emit(EVENTS.error, `保存偏好设置失败：${error}`);
  }
}

export function sourceLabel() {
  switch (state.library.source) {
    case LIBRARY_SOURCES.custom:
      return '自定义目录';
    case LIBRARY_SOURCES.external:
      return '外置曲库';
    case LIBRARY_SOURCES.env:
      return '环境变量目录';
    default:
      return '内置曲库';
  }
}