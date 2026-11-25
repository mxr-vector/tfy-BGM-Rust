import { computed, reactive } from 'vue';
import { ALL_CATEGORY, SORT_KEYS } from '@/lib/constants';
import type {
  AppPlatform,
  CategorySummary,
  LibrarySnapshot,
  LibrarySource,
  LoopPoint,
  Prefs,
  ScheduleEntry,
  Song,
  SortBy,
} from '@/lib/types';

export type AppView = 'library' | 'schedule' | 'subtitles' | 'tts' | 'duplicates' | 'sound-search' | 'playlist-migration';

export interface AppState {
  library: LibrarySnapshot;
  prefs: Prefs | null;
  favorites: Set<string>;
  category: string;
  favoritesOnly: boolean;
  search: string;
  sortBy: SortBy;
  selected: Set<string>;
  queue: Song[];
  queueIndex: number;
  currentKey: string | null;
  currentSong: Song | null;
  playing: boolean;
  view: AppView;
  schedule: ScheduleEntry[];
  loopPoints: Record<string, LoopPoint>;
  /** 当前运行平台，bootstrap 后写入；平台特有配置项据此显示。 */
  platform: AppPlatform;
}

export function emptyLibrary(): LibrarySnapshot {
  return {
    root: '',
    source: 'bundled',
    songs: [],
    categories: [],
    warnings: [],
    scannedAtMs: 0,
  };
}

export const state = reactive<AppState>({
  library: emptyLibrary(),
  prefs: null,
  favorites: new Set<string>(),
  category: ALL_CATEGORY,
  favoritesOnly: false,
  search: '',
  sortBy: SORT_KEYS.recommended,
  selected: new Set<string>(),
  queue: [],
  queueIndex: -1,
  currentKey: null,
  currentSong: null,
  playing: false,
  view: 'library',
  schedule: [],
  loopPoints: {},
  platform: 'other',
});

const collator = new Intl.Collator(['zh-Hans-CN', 'zh', 'en'], {
  numeric: true,
  sensitivity: 'base',
});

export function setView(view: AppView): void {
  state.view = view;
}

export function categoryById(id: string): CategorySummary | null {
  return state.library.categories.find((category) => category.id === id) ?? null;
}

export function songByKey(key: string | null): Song | null {
  if (!key) return null;
  return state.library.songs.find((song) => song.key === key) ?? null;
}

export const favoriteCount = computed(() =>
  state.library.songs.reduce(
    (count, song) => count + (state.favorites.has(song.key) ? 1 : 0),
    0,
  ),
);

export const visibleSongs = computed(() => {
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

  return state.sortBy === SORT_KEYS.name
    ? [...list].sort((a, b) => collator.compare(a.title, b.title))
    : list;
});

export function sourceLabel(source: LibrarySource = state.library.source): string {
  switch (source) {
    case 'custom':
      return '自定义目录';
    case 'external':
      return '外置曲库';
    case 'env':
      return '环境变量目录';
    default:
      return '内置曲库';
  }
}