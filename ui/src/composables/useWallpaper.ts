import { computed, ref, shallowRef } from 'vue';
import {
  COMMANDS,
  DEFAULT_WALLPAPER,
  DEFAULT_WALLPAPER_BLUR,
  DEFAULT_WALLPAPER_FIT,
  DEFAULT_WALLPAPER_OPACITY,
  WALLPAPER_OPTIONS,
} from '@/lib/constants';
import { clamp, normalizeError } from '@/lib/format';
import { invoke, isTauri } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { WallpaperAsset, WallpaperKind, WallpaperPick } from '@/lib/types';
import { setWallpaper } from '@/stores/prefs';
import { state } from '@/stores/state';

const VIDEO_EXTENSION = /\.mp4$/i;

export const popoverOpen = ref(false);
export const popoverAnchor = ref<HTMLElement | null>(null);
export const builtins = shallowRef<WallpaperAsset[]>([]);
export const builtinsLoaded = ref(false);

export function kindOf(path: string | null | undefined): WallpaperKind {
  return VIDEO_EXTENSION.test(path ?? '') ? 'video' : 'image';
}

export function fileName(path: string | null | undefined): string {
  return String(path ?? '').split(/[\\/]/).pop() || '自定义壁纸';
}

export const wallpaperView = computed(() => {
  const prefs = state.prefs;
  const id = prefs?.wallpaper || DEFAULT_WALLPAPER;
  const customPath = prefs?.customWallpaper ?? null;
  const opacity = clamp(
    Number(prefs?.wallpaperOpacity ?? DEFAULT_WALLPAPER_OPACITY) || 0,
    0,
    0.95,
  );
  const blur = Math.round(
    clamp(Number(prefs?.wallpaperBlur ?? DEFAULT_WALLPAPER_BLUR) || 0, 0, 20),
  );
  const fit = prefs?.wallpaperFit ?? DEFAULT_WALLPAPER_FIT;

  const builtin = builtins.value.find((item) => item.id === id) ?? null;
  const source =
    id === 'custom'
      ? customPath
        ? { path: customPath, kind: kindOf(customPath) }
        : null
      : builtin
        ? { path: builtin.path, kind: builtin.kind }
        : null;

  const panelOpacity = Math.max(0.04, 1 - opacity * 0.92);
  const maskOpacity = Math.max(0, 0.4 - opacity * 0.44);

  return {
    id,
    customPath,
    opacity,
    blur,
    fit,
    source,
    panelOpacity,
    maskOpacity,
    isVideo: source?.kind === 'video',
    hasWallpaper: id !== DEFAULT_WALLPAPER,
    shellClass: [
      `wall-${id}`,
      ...(id !== DEFAULT_WALLPAPER ? ['has-wallpaper'] : []),
      ...(source?.kind === 'video' ? ['has-video'] : []),
    ],
  };
});

export const availableWallpaperOptions = computed(() =>
  WALLPAPER_OPTIONS.filter(
    (option) =>
      !option.id.startsWith('builtin-') ||
      !builtinsLoaded.value ||
      builtins.value.some((item) => item.id === option.id),
  ),
);

export function openWallpaperPopover(anchor: HTMLElement): void {
  popoverAnchor.value = anchor;
  popoverOpen.value = true;
}

export function closeWallpaperPopover(): void {
  popoverOpen.value = false;
}

export function toggleWallpaperPopover(anchor: HTMLElement): void {
  if (popoverOpen.value) closeWallpaperPopover();
  else openWallpaperPopover(anchor);
}

export async function loadBuiltins(): Promise<void> {
  if (!isTauri) {
    builtinsLoaded.value = true;
    return;
  }
  try {
    builtins.value = await invoke<WallpaperAsset[]>(COMMANDS.builtinWallpapers);
  } catch (error) {
    toast(`内置壁纸加载失败：${normalizeError(error)}`, 'error');
  } finally {
    builtinsLoaded.value = true;
  }
}

export async function chooseCustomWallpaper(): Promise<void> {
  if (!isTauri) {
    toast('请在桌面客户端中选择本地壁纸', 'error');
    closeWallpaperPopover();
    return;
  }
  try {
    const picked = await invoke<WallpaperPick | null>(COMMANDS.chooseWallpaperFile);
    if (!picked) return;
    const view = wallpaperView.value;
    setWallpaper('custom', picked.path, {
      opacity: view.opacity,
      blur: view.blur,
      fit: view.fit,
    });
    closeWallpaperPopover();
    toast(`已应用壁纸：${fileName(picked.path)}`, 'success');
  } catch (error) {
    toast(`选择壁纸失败：${normalizeError(error)}`, 'error');
  }
}