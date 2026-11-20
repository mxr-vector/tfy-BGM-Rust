/**
 * 音频播放引擎：持有单个 `HTMLAudioElement`，负责读取播放时钟、加载 Blob 源
 * 并及时释放过期请求。业务状态（当前曲目、播放队列、错误提示）由播放器层维护。
 */

import { reactive } from 'vue';
import { playerConfig } from '@/lib/config';
import { clamp } from '@/lib/format';
import { assetUrl } from '@/lib/tauri';

export const audioState = reactive({
  duration: 0,
  currentTime: 0,
  bufferedPercent: 0,
});

export type LoadOutcome =
  | { status: 'loaded' }
  | { status: 'superseded' }
  | { status: 'failed'; error: unknown };

export interface LoadOptions {
  autoplay?: boolean;
  restoreTime?: number;
  /** 自动播放被 WebView 拦截时回调，供播放器把 UI 状态改回暂停。 */
  onAutoplayBlocked?: () => void;
}

const audio = new Audio();
audio.preload = 'metadata';

let sourceUrl = '';
let loadedPath = '';
let sourceToken = 0;

export function audioElement(): HTMLAudioElement {
  return audio;
}

export function isSourceLoaded(path: string): boolean {
  return loadedPath === path;
}

export function hasSourceUrl(): boolean {
  return Boolean(sourceUrl);
}

export function applyAudioSettings(volume: number, muted: boolean, rate: number): void {
  audio.volume = muted ? 0 : volume;
  audio.playbackRate = rate;
}

export function startPlayback(): void {
  void audio.play().catch(() => {});
}

export function pausePlayback(): void {
  audio.pause();
}

export function currentTime(): number {
  return Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
}

export function seekTo(seconds: number): void {
  const duration = audio.duration;
  if (!Number.isFinite(duration) || duration <= 0) return;
  audio.currentTime = clamp(seconds, 0, duration);
  syncClock();
}

export function rewindToStart(): void {
  audio.currentTime = 0;
  syncClock();
}

export function syncClock(): void {
  audioState.duration = Number.isFinite(audio.duration) ? audio.duration : 0;
  audioState.currentTime = Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
  updateBuffered();
}

function updateBuffered(): void {
  const duration = audio.duration;
  if (!Number.isFinite(duration) || duration <= 0 || audio.buffered.length === 0) {
    audioState.bufferedPercent = 0;
    return;
  }
  const end = audio.buffered.end(audio.buffered.length - 1);
  audioState.bufferedPercent = clamp((end / duration) * 100, 0, 100);
}

audio.addEventListener('loadstart', syncClock);
audio.addEventListener('loadedmetadata', syncClock);
audio.addEventListener('durationchange', syncClock);
audio.addEventListener('canplay', syncClock);
audio.addEventListener('progress', updateBuffered);
audio.addEventListener('timeupdate', syncClock);

/**
 * 加载曲目源。WebKitGTK 无法直接播放 `asset://`（会报 MEDIA_ERR_SRC_NOT_SUPPORTED），
 * 先经 Asset Protocol 读取字节，再交给 Blob URL 播放。
 */
export async function loadSource(
  path: string,
  { autoplay = true, restoreTime = 0, onAutoplayBlocked }: LoadOptions = {},
): Promise<LoadOutcome> {
  const token = (sourceToken += 1);
  loadedPath = path;

  let blob: Blob;
  try {
    blob = await fetchTrack(path);
  } catch (error) {
    if (token !== sourceToken) return { status: 'superseded' };
    loadedPath = '';
    return { status: 'failed', error };
  }
  if (token !== sourceToken) return { status: 'superseded' };

  const url = URL.createObjectURL(blob);
  revokeSourceUrl();
  sourceUrl = url;
  loadedPath = path;

  if (restoreTime > 0) {
    audio.addEventListener(
      'loadedmetadata',
      () => {
        // 加载失败或被后继请求取代时不能再用旧的 restoreTime 调整新曲目的进度。
        if (token !== sourceToken) return;
        if (Number.isFinite(audio.duration) && audio.duration > 0) {
          audio.currentTime = Math.min(restoreTime, audio.duration);
        }
      },
      { once: true },
    );
  }
  audio.src = url;
  if (autoplay) {
    audio.play().catch(() => {
      if (token === sourceToken) onAutoplayBlocked?.();
    });
  }
  return { status: 'loaded' };
}

async function fetchTrack(path: string): Promise<Blob> {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), playerConfig().loadTimeoutMs);
  try {
    const response = await fetch(assetUrl(path), { signal: controller.signal });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const blob = await response.blob();
    if (blob.size === 0) throw new Error('音频文件为空');
    return blob.type === 'audio/mpeg' ? blob : new Blob([blob], { type: 'audio/mpeg' });
  } finally {
    window.clearTimeout(timer);
  }
}

/** 停止播放并释放当前源；下一次 `loadSource` 会从干净状态开始。 */
export function unloadSource(): void {
  sourceToken += 1;
  audio.pause();
  audio.removeAttribute('src');
  audio.load();
  revokeSourceUrl();
  loadedPath = '';
  audioState.currentTime = 0;
  audioState.duration = 0;
  audioState.bufferedPercent = 0;
}

function revokeSourceUrl(): void {
  if (!sourceUrl) return;
  URL.revokeObjectURL(sourceUrl);
  sourceUrl = '';
}