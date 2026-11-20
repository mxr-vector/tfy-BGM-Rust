import {
  DEFAULT_WALLPAPER_BLUR,
  DEFAULT_WALLPAPER_FIT,
  DEFAULT_WALLPAPER_OPACITY,
  COMMANDS,
} from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { Prefs, SortBy, WallpaperFit } from '@/lib/types';
import { state } from './state';

export interface WallpaperOverrides {
  opacity?: number;
  blur?: number;
  fit?: WallpaperFit;
}

let saveTimer = 0;
let saveSequence = 0;

export function persistPrefs(immediate = false): void {
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

async function flushPrefs(): Promise<void> {
  const prefs = state.prefs;
  if (!prefs) return;

  const sequence = (saveSequence += 1);
  const payload: Prefs = {
    ...prefs,
    favorites: [...state.favorites],
    sortBy: state.sortBy,
  };
  try {
    const saved = await invoke<Prefs>(COMMANDS.savePrefs, { prefs: payload });
    if (sequence === saveSequence) {
      // 后端会丢弃不合法的壁纸选择（例如自定义文件被删除），此时以返回值为准。
      state.prefs = saved;
    }
  } catch (error) {
    toast(`保存偏好设置失败：${normalizeError(error)}`, 'error');
  }
}

export function toggleFavorite(key: string | null): void {
  if (!key) return;
  if (state.favorites.has(key)) {
    state.favorites.delete(key);
  } else {
    state.favorites.add(key);
  }
  persistPrefs(true);
}

export function setFavorites(keys: string[], favorite: boolean): void {
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
  if (changed) persistPrefs(true);
}

/** 导出响度归一化开关与场景预设，随下一次导出生效。 */
export function setExportSettings(
  loudnessNormalization: boolean,
  exportScene: string,
): void {
  const prefs = state.prefs;
  if (!prefs) return;
  state.prefs = { ...prefs, loudnessNormalization, exportScene };
  persistPrefs(true);
}

export function setSortBy(sortBy: SortBy): void {
  if (state.sortBy === sortBy) return;
  state.sortBy = sortBy;
  if (state.prefs) state.prefs.sortBy = sortBy;
  persistPrefs(true);
}

export function setWallpaper(
  id: string,
  path: string | null = null,
  options: WallpaperOverrides = {},
): void {
  const prefs = state.prefs;
  if (!prefs) return;

  state.prefs = {
    ...prefs,
    wallpaper: id,
    customWallpaper: id === 'custom' ? path : (prefs.customWallpaper ?? null),
    wallpaperOpacity: options.opacity ?? prefs.wallpaperOpacity ?? DEFAULT_WALLPAPER_OPACITY,
    wallpaperBlur: options.blur ?? prefs.wallpaperBlur ?? DEFAULT_WALLPAPER_BLUR,
    wallpaperFit: options.fit ?? prefs.wallpaperFit ?? DEFAULT_WALLPAPER_FIT,
  };
  persistPrefs(true);
}

/** 调整界面透明度（0.0 ~ 1.0），拖动时先本地生效，停止后写盘。 */
export function setWallpaperOpacity(opacity: number): void {
  const prefs = state.prefs;
  if (!prefs) return;
  state.prefs = {
    ...prefs,
    wallpaperOpacity: Math.max(0, Math.min(1, Number(opacity) || 0)),
  };
  persistPrefs(false);
}

/** 调整内容区毛玻璃模糊度（0 ~ 20 px）。 */
export function setWallpaperBlur(blur: number): void {
  const prefs = state.prefs;
  if (!prefs) return;
  state.prefs = {
    ...prefs,
    wallpaperBlur: Math.max(0, Math.min(20, Math.round(Number(blur) || 0))),
  };
  persistPrefs(false);
}

/** 壁纸填充方式：`cover` 铺满裁剪，`contain` 完整居中展示。 */
export function setWallpaperFit(fit: WallpaperFit): void {
  const prefs = state.prefs;
  if (!prefs) return;
  state.prefs = {
    ...prefs,
    wallpaperFit: fit === 'contain' ? 'contain' : 'cover',
  };
  persistPrefs(true);
}