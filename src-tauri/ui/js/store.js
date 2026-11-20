import { invoke } from './tauri.js';

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
  category: 'all',
  favoritesOnly: false,
  search: '',
  sortBy: 'default',
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
  state.prefs = boot.prefs;
  state.library = boot.library;
  state.favorites = new Set(boot.prefs.favorites);
  state.sortBy = boot.prefs.sortBy;
  emit('library');
  emit('favorites');
  emit('view');
}

export function replaceLibrary(snapshot) {
  state.library = snapshot;
  for (let index = 0; index < state.queue.length; index += 1) {
    const fresh = snapshot.songs.find((song) => song.key === state.queue[index].key);
    if (fresh) state.queue[index] = fresh;
  }
  if (state.currentKey) {
    const fresh = snapshot.songs.find((song) => song.key === state.currentKey);
    if (fresh) state.currentSong = fresh;
  }
  emit('library');
  emit('queue');
  emit('player');
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
  emit('favorites');
  persistPrefs(true);
}

export function setCategory(categoryId) {
  state.category = categoryId;
  state.favoritesOnly = false;
  emit('view');
}

export function toggleFavoritesOnly() {
  state.favoritesOnly = !state.favoritesOnly;
  emit('view');
}

export function resetFilters() {
  state.category = 'all';
  state.favoritesOnly = false;
  state.search = '';
  emit('view');
  emit('search');
}

export function setSearch(query) {
  state.search = query;
  emit('search');
}

export function setSortBy(sortBy) {
  if (state.sortBy === sortBy) return;
  state.sortBy = sortBy;
  persistPrefs(true);
  emit('view');
}

export function visibleSongs() {
  const list = filterSongs();
  return state.sortBy === 'name'
    ? [...list].sort((a, b) => collator.compare(a.title, b.title))
    : list;
}

function filterSongs() {
  let list = state.library.songs;

  if (state.favoritesOnly) {
    list = list.filter((song) => state.favorites.has(song.key));
  } else if (state.category !== 'all') {
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
    const saved = await invoke('save_prefs', { prefs: payload });
    if (sequence === saveSequence) state.prefs = saved;
  } catch (error) {
    emit('error', `保存偏好设置失败：${error}`);
  }
}

export function sourceLabel() {
  switch (state.library.source) {
    case 'custom':
      return '自定义目录';
    case 'env':
      return '环境变量目录';
    default:
      return '内置曲库';
  }
}