import { ALL_CATEGORY } from '@/lib/constants';
import { applyConfig } from '@/lib/config';
import type { Bootstrap, LibraryMutation, LibrarySnapshot } from '@/lib/types';
import { state } from './state';

export function applyBootstrap(boot: Bootstrap): void {
  applyConfig(boot.config);
  state.prefs = boot.prefs;
  state.library = boot.library;
  state.favorites = new Set(boot.prefs.favorites);
  state.selected = new Set();
  state.sortBy = boot.prefs.sortBy;
}

export function replaceLibrary(snapshot: LibrarySnapshot): void {
  state.library = snapshot;

  if (
    state.category !== ALL_CATEGORY &&
    !snapshot.categories.some((category) => category.id === state.category)
  ) {
    state.category = ALL_CATEGORY;
  }

  const songsByKey = new Map(snapshot.songs.map((song) => [song.key, song]));
  state.queue = state.queue
    .map((item) => songsByKey.get(item.key))
    .filter((song): song is NonNullable<typeof song> => Boolean(song));

  if (state.currentKey) {
    const fresh = songsByKey.get(state.currentKey) ?? null;
    state.currentSong = fresh;
    if (!fresh) {
      state.currentKey = null;
    } else {
      state.queueIndex = state.queue.findIndex((song) => song.key === state.currentKey);
    }
  }

  for (const key of [...state.selected]) {
    if (!songsByKey.has(key)) state.selected.delete(key);
  }
}

/** 应用曲库写操作结果：先迁移 key（队列/当前曲目/收藏/选择），再替换快照。 */
export function applyMutation(result: LibraryMutation): void {
  const remap = new Map(
    (result.changes ?? [])
      .filter((change) => change.from !== change.to)
      .map((change) => [change.from, change.to]),
  );

  if (remap.size > 0) {
    const remapKey = (key: string) => remap.get(key) ?? key;
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
}

export function setCategory(categoryId: string): void {
  state.category = categoryId;
  state.favoritesOnly = false;
}

export function toggleFavoritesOnly(): void {
  state.favoritesOnly = !state.favoritesOnly;
}

export function resetFilters(): void {
  state.category = ALL_CATEGORY;
  state.favoritesOnly = false;
  state.search = '';
}

export function setSearch(query: string): void {
  state.search = query;
}

export function setSelected(key: string, selected: boolean): void {
  if (!key) return;
  const has = state.selected.has(key);
  if (selected && !has) state.selected.add(key);
  else if (!selected && has) state.selected.delete(key);
}

export function selectMany(keys: string[], selected: boolean): void {
  for (const key of keys) {
    if (selected) state.selected.add(key);
    else state.selected.delete(key);
  }
}

export function clearSelection(): void {
  if (state.selected.size > 0) state.selected.clear();
}

export function selectedKeys(): string[] {
  return [...state.selected];
}

/** 拖拽排序只调整前端展示顺序，后端仍按文件名重新扫描。 */
export function reorderLibrary(fromKey: string, toKey: string): void {
  const songs = state.library.songs;
  const fromIndex = songs.findIndex((song) => song.key === fromKey);
  const targetIndex = songs.findIndex((song) => song.key === toKey);
  if (fromIndex < 0 || targetIndex < 0 || fromIndex === targetIndex) return;

  const [moved] = songs.splice(fromIndex, 1);
  if (!moved) return;
  const insertAt = songs.findIndex((song) => song.key === toKey);
  songs.splice(fromIndex < targetIndex ? insertAt + 1 : insertAt, 0, moved);
}

