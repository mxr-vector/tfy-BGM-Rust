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
  const storedId = prefs?.wallpaper || DEFAULT_WALLPAPER;
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

  const builtin = builtins.value.find((item) => item.id === storedId) ?? null;
  // 壁纸文件被移出资源目录后，旧 id 回退默认样式，避免停留在无画面的状态。
  const id =
    storedId.startsWith('builtin-') && builtinsLoaded.value && !builtin
      ? DEFAULT_WALLPAPER
      : storedId;
  const asset = id === storedId ? builtin : null;
  const source =
    id === 'custom'
      ? customPath
        ? { path: customPath, kind: kindOf(customPath) }
        : null
      : asset
        ? { path: asset.path, kind: asset.kind }
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
      ...(source ? ['wall-art'] : []),
      ...(id !== DEFAULT_WALLPAPER ? ['has-wallpaper'] : []),
      ...(source?.kind === 'video' ? ['has-video'] : []),
    ],
  };
});

/** 渐变底色始终可用；图片 / 视频壁纸由后端扫描资源目录后动态补进列表。 */
export const availableWallpaperOptions = computed(() => [
  ...WALLPAPER_OPTIONS,
  ...builtins.value.map((item) => ({
    id: item.id,
    label: item.label,
    hint: item.kind === 'video' ? 'MP4 动态' : '静态图片',
    preview: 'auto',
  })),
]);

export function openWallpaperPopover(anchor: HTMLElement): void {
  popoverAnchor.value = anchor;
  popoverOpen.value = true;
  // 每次打开时重新扫描资源目录，新放进 assets/bg 的壁纸无需重启即可出现。
  void loadBuiltins();
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