/**
 * 系统集成的运行时效果：字体、启动自动播放、防休眠、媒体控件与总线处理。
 *
 * 这些都是「基本系统配置」里开关的实际效果，统一放在这里，避免散落在组件中：
 * - 字体：写到根元素 `--font-sans`，立即作用于全应用；
 * - 启动自动播放：首屏曲库就绪后从第一首开始；
 * - 防休眠：播放期间持有 Screen Wake Lock，暂停或隐藏时释放；
 * - 媒体控件：Windows 用 SMTC、Linux 用 MPRIS2，均通过 Media Session API 暴露；
 *   两者都要求页面存在「正在播放的媒体元素」，因此额外维持一个静音循环音频；
 * - 总线处理：Windows 的空间音效与音频增强，其它平台保持直通。
 */

import { watch } from 'vue';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { setOutputProcessing, subscribeEngine } from '@/stores/audio-engine';
import { nextTrack, playSong, prevTrack, togglePlay } from '@/stores/player';
import { applyUiFont } from '@/stores/settings';
import { state } from '@/stores/state';

interface WakeLockSentinelLike extends EventTarget {
  release(): Promise<void>;
}

type WakeLockApi = { request(type: 'screen'): Promise<WakeLockSentinelLike> };

const MEDIA_ACTIONS: MediaSessionAction[] = [
  'play',
  'pause',
  'stop',
  'previoustrack',
  'nexttrack',
];

let installed = false;
let wakeLock: WakeLockSentinelLike | null = null;
let wakeLockPending = false;
/** 静音循环音频：让 WebView 被识别为「正在播放媒体」，OS 媒体控件才会出现。 */
let silentMedia: HTMLAudioElement | null = null;
let silentMediaUrl: string | null = null;

function wakeLockApi(): WakeLockApi | null {
  const api = (navigator as Navigator & { wakeLock?: WakeLockApi }).wakeLock;
  return api ?? null;
}

/** Screen Wake Lock 只有 Chromium 系 WebView 提供；WebKit（macOS / Linux）没有该 API。 */
export const wakeLockSupported = typeof navigator !== 'undefined' && 'wakeLock' in navigator;

/** 播放中且开启「不自动休眠」时持有锁；其余情况立即释放。 */
async function syncWakeLock(): Promise<void> {
  if (wakeLockPending) return;
  const wanted = Boolean(state.playing && state.prefs?.preventSleep);
  if (wanted && !wakeLock) {
    const api = wakeLockApi();
    if (!api) return;
    wakeLockPending = true;
    try {
      const sentinel = await api.request('screen');
      wakeLock = sentinel;
      sentinel.addEventListener('release', () => {
        if (wakeLock === sentinel) wakeLock = null;
      });
    } catch {
      // 系统拒绝或页面不可见时保持无锁，不影响播放。
    } finally {
      wakeLockPending = false;
    }
    return;
  }
  if (!wanted && wakeLock) {
    const sentinel = wakeLock;
    wakeLock = null;
    await sentinel.release().catch(() => undefined);
  }
}

/**
 * 生成静音 WAV（8kHz / 8bit 单声道），避免额外携带二进制资源。
 *
 * 时长必须 ≥ 5 秒且不能设置 `muted` / 零音量：Chromium 会忽略过短或静音的媒体，
 * 不会把它暴露给系统媒体控件（SMTC / MPRIS2）。
 */
function silentMediaSource(): string {
  if (silentMediaUrl) return silentMediaUrl;
  const sampleRate = 8_000;
  const samples = sampleRate * 10;
  const buffer = new ArrayBuffer(44 + samples);
  const view = new DataView(buffer);
  const ascii = (offset: number, text: string): void => {
    for (let index = 0; index < text.length; index += 1) {
      view.setUint8(offset + index, text.charCodeAt(index));
    }
  };
  ascii(0, 'RIFF');
  view.setUint32(4, 36 + samples, true);
  ascii(8, 'WAVE');
  ascii(12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate, true);
  view.setUint16(32, 1, true);
  view.setUint16(34, 8, true);
  ascii(36, 'data');
  view.setUint32(40, samples, true);
  for (let index = 0; index < samples; index += 1) view.setUint8(44 + index, 128);
  silentMediaUrl = URL.createObjectURL(new Blob([buffer], { type: 'audio/wav' }));
  return silentMediaUrl;
}

function ensureSilentMedia(): HTMLAudioElement {
  if (silentMedia) return silentMedia;
  const element = new Audio(silentMediaSource());
  element.loop = true;
  element.preload = 'auto';
  element.setAttribute('aria-hidden', 'true');
  silentMedia = element;
  return element;
}

/** 播放时维持静音音频；暂停或关闭媒体控件时立即暂停，避免后台无端阻止休眠。 */
function syncSilentMedia(enabled: boolean): void {
  if (!enabled) {
    if (silentMedia && !silentMedia.paused) silentMedia.pause();
    return;
  }
  const element = ensureSilentMedia();
  if (element.paused) {
    void element.play().catch(() => {
      // 自动播放被拦截时静默失败；下次播放动作会重试。
    });
  }
}

function mediaSessionEnabled(): boolean {
  return (
    (state.platform === 'windows' && Boolean(state.prefs?.smtc)) ||
    (state.platform === 'linux' && Boolean(state.prefs?.mpris2))
  );
}

/** 媒体控件：Windows 的 SMTC 与 Linux 的 MPRIS2 都通过 Media Session 暴露。 */
function syncMediaSession(): void {
  if (!('mediaSession' in navigator)) return;
  const session = navigator.mediaSession;
  const enabled = mediaSessionEnabled();
  // 只在真正播放时维持媒体元素，暂停时释放系统唤醒，避免后台无端阻止休眠。
  syncSilentMedia(enabled && state.playing);

  if (!enabled) {
    session.metadata = null;
    session.playbackState = 'none';
    for (const action of MEDIA_ACTIONS) {
      try {
        session.setActionHandler(action, null);
      } catch {
        // 个别动作在当前 WebView 不受支持，忽略即可。
      }
    }
    return;
  }

  const song = state.currentSong;
  session.metadata =
    typeof MediaMetadata === 'undefined'
      ? null
      : new MediaMetadata({
          title: song?.title ?? '听风吟 · BGM',
          artist: song?.artist ?? 'BGM 素材库',
          album: song?.album ?? '听风吟',
        });
  session.playbackState = state.playing ? 'playing' : 'paused';

  const handlers: [MediaSessionAction, MediaSessionActionHandler][] = [
    ['play', () => { if (!state.playing) togglePlay(); }],
    ['pause', () => { if (state.playing) togglePlay(); }],
    ['stop', () => { if (state.playing) togglePlay(); }],
    ['previoustrack', () => prevTrack(true)],
    ['nexttrack', () => nextTrack(true)],
  ];
  for (const [action, handler] of handlers) {
    try {
      session.setActionHandler(action, handler);
    } catch {
      // 同上。
    }
  }
}

/** Windows 才应用空间音效与音频增强；其它平台保持直通。 */
function syncOutputProcessing(): void {
  const windows = state.platform === 'windows';
  setOutputProcessing({
    enhancement: windows && Boolean(state.prefs?.audioEnhancement),
    spatial: windows && Boolean(state.prefs?.spatialAudio),
  });
}

function onVisibilityChange(): void {
  // 页面隐藏时系统会自动释放 Wake Lock，回到前台后按当前状态重新申请。
  if (document.visibilityState === 'visible') void syncWakeLock();
}

export function installSystemIntegration(): void {
  if (installed) return;
  installed = true;

  watch(() => state.prefs?.uiFont, (font) => applyUiFont(font), { immediate: true });

  watch(
    () => [state.playing, state.prefs?.preventSleep] as const,
    () => void syncWakeLock(),
  );

  watch(
    () => [state.platform, state.prefs?.smtc, state.prefs?.mpris2] as const,
    () => syncMediaSession(),
    { immediate: true },
  );

  watch(
    () =>
      [
        state.currentSong?.key,
        state.currentSong?.title,
        state.currentSong?.artist,
        state.playing,
      ] as const,
    () => syncMediaSession(),
  );

  watch(
    () => [state.platform, state.prefs?.spatialAudio, state.prefs?.audioEnhancement] as const,
    () => syncOutputProcessing(),
    { immediate: true },
  );

  document.addEventListener('visibilitychange', onVisibilityChange);

  // 播放开始时重试静音音频：WebView 的自动播放限制通常在用户手势后解除。
  subscribeEngine((event) => {
    if (event === 'play') syncSilentMedia(mediaSessionEnabled() && state.playing);
  });
}

/** 曲库就绪后调用：按偏好从第一首开始播放。 */
export function autoplayOnStartIfEnabled(): void {
  if (!state.prefs?.autoplayOnStart) return;
  if (state.playing || state.currentSong) return;
  const songs = state.library.songs;
  if (songs.length === 0) return;
  if (guardLibraryPlayback()) return;
  playSong(songs[0]!, songs, true);
}
