import {
  COMMANDS,
  PLAY_MODE_CYCLE,
  PLAY_MODES,
} from '@/lib/constants';
import { playerConfig } from '@/lib/config';
import { clamp, normalizeError } from '@/lib/format';
import { showModal } from '@/lib/modal';
import { assetUrl, invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { ExportResult, PlayMode, Song } from '@/lib/types';
import { persistPrefs, toggleFavorite } from './prefs';
import { songByKey, state } from './state';
import { reactive, ref, watch } from 'vue';

const audio = new Audio();
audio.preload = 'metadata';

export const audioState = reactive({
  duration: 0,
  currentTime: 0,
  bufferedPercent: 0,
});

export const drawerOpen = ref(false);
export const exporting = ref(false);
export const playbackError = ref('');

let sourceToken = 0;
let sourceUrl = '';
let loadedPath = '';
let failureStreak = 0;

export function audioElement(): HTMLAudioElement {
  return audio;
}

function applyPrefsToAudio(): void {
  const prefs = state.prefs;
  if (!prefs) return;
  audio.volume = prefs.muted ? 0 : prefs.volume;
  audio.playbackRate = prefs.playbackRate;
}

function startPlayback(): void {
  void audio.play().catch(() => {});
}

function syncClock(): void {
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
audio.addEventListener('play', () => {
  state.playing = true;
  playbackError.value = '';
});
audio.addEventListener('playing', () => {
  failureStreak = 0;
});
audio.addEventListener('pause', () => {
  state.playing = false;
});
audio.addEventListener('ended', handleEnded);
audio.addEventListener('error', () => failCurrent('音频加载失败'));

watch(
  () => [state.prefs?.volume, state.prefs?.muted, state.prefs?.playbackRate] as const,
  applyPrefsToAudio,
  { immediate: true },
);

// 曲库刷新或写操作会替换 currentSong 对象：路径变化时按原进度重新加载，曲目消失时停止播放。
watch(
  () => state.currentSong,
  (song, previous) => {
    if (!song) {
      if (audio.hasAttribute('src') || sourceUrl) stopPlayback();
      return;
    }
    if (song.path === loadedPath) return;
    const time = Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
    const wasPlaying = previous ? !audio.paused : false;
    void loadSongSource(song, { autoplay: wasPlaying, restoreTime: previous ? time : 0 });
  },
);

export function playSong(song: Song, newQueue: Song[] | null = null, forcePlay = false): void {
  if (!song) return;

  if (newQueue) {
    state.queue = [...newQueue];
    const index = state.queue.findIndex((queued) => queued.key === song.key);
    state.queueIndex = index >= 0 ? index : 0;
  } else if (!state.queue.some((queued) => queued.key === song.key)) {
    state.queue.push(song);
    state.queueIndex = state.queue.length - 1;
  } else {
    state.queueIndex = state.queue.findIndex((queued) => queued.key === song.key);
  }

  if (state.currentKey === song.key && state.currentSong) {
    if (!sourceUrl) {
      void loadSongSource(song, { autoplay: true });
    } else if (forcePlay) {
      audio.currentTime = 0;
      syncClock();
      startPlayback();
    } else {
      togglePlay();
    }
    return;
  }

  state.currentKey = song.key;
  state.currentSong = song;
  state.playing = false;
  playbackError.value = '';
  void loadSongSource(song, { autoplay: true });
}

export function playSongWithMode(song: Song, list: Song[] | null, mode: PlayMode): void {
  if (!song) return;
  setPlayMode(mode);
  playSong(song, Array.isArray(list) ? list : null, true);
}

export function setPlayMode(mode: PlayMode): void {
  if (!PLAY_MODE_CYCLE.includes(mode) || !state.prefs || state.prefs.playMode === mode) return;
  state.prefs.playMode = mode;
  persistPrefs(true);
}

/**
 * WebKitGTK 无法直接播放 `asset://`（会报 MEDIA_ERR_SRC_NOT_SUPPORTED），
 * 先经 Asset Protocol 读取字节，再交给 Blob URL 播放。
 */
async function loadSongSource(
  song: Song,
  { autoplay = true, restoreTime = 0 }: { autoplay?: boolean; restoreTime?: number } = {},
): Promise<void> {
  const token = (sourceToken += 1);
  loadedPath = song.path;
  try {
    const blob = await fetchTrack(song.path);
    if (token !== sourceToken) return;

    const url = URL.createObjectURL(blob);
    if (sourceUrl) URL.revokeObjectURL(sourceUrl);
    sourceUrl = url;

    applyPrefsToAudio();
    if (restoreTime > 0) {
      audio.addEventListener(
        'loadedmetadata',
        () => {
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
        if (token !== sourceToken) return;
        state.playing = false;
      });
    }
  } catch (error) {
    if (token !== sourceToken) return;
    loadedPath = '';
    failCurrent(`无法读取音频（${normalizeError(error)}）`);
  }
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

export function togglePlay(): void {
  if (!state.currentSong) return;
  if (audio.paused) startPlayback();
  else audio.pause();
}

export function seekBy(deltaSeconds: number): void {
  seekTo(audio.currentTime + deltaSeconds);
}

export function seekTo(seconds: number): void {
  const duration = audio.duration;
  if (!Number.isFinite(duration) || duration <= 0) return;
  audio.currentTime = clamp(seconds, 0, duration);
  syncClock();
}

export function nextTrack(forcePlay = true): void {
  if (state.queue.length === 0) return;
  if (state.queue.length === 1) {
    audio.currentTime = 0;
    if (forcePlay) startPlayback();
    syncClock();
    return;
  }

  if (state.prefs?.playMode === PLAY_MODES.shuffle) {
    let nextIndex = state.queueIndex;
    while (nextIndex === state.queueIndex) {
      nextIndex = Math.floor(Math.random() * state.queue.length);
    }
    state.queueIndex = nextIndex;
    playSong(state.queue[nextIndex]!, null, forcePlay);
    return;
  }

  state.queueIndex = (state.queueIndex + 1) % state.queue.length;
  playSong(state.queue[state.queueIndex]!, null, forcePlay);
}

export function prevTrack(forcePlay = true): void {
  if (state.queue.length === 0) return;
  if (audio.currentTime > playerConfig().restartThresholdSeconds) {
    seekTo(0);
    if (!state.playing && forcePlay) startPlayback();
    return;
  }
  if (state.queue.length === 1) {
    seekTo(0);
    if (!state.playing && forcePlay) startPlayback();
    return;
  }

  const prevIndex = (state.queueIndex - 1 + state.queue.length) % state.queue.length;
  state.queueIndex = prevIndex;
  playSong(state.queue[prevIndex]!, null, forcePlay);
}

function handleEnded(): void {
  if (state.prefs?.playMode === PLAY_MODES.repeatOne) {
    audio.currentTime = 0;
    syncClock();
    startPlayback();
    return;
  }
  nextTrack(true);
}

/** 连续失败不超过上限时自动跳下一首，否则停下来提示。 */
function failCurrent(reason: string): void {
  const song = state.currentSong;
  if (!song) return;
  state.playing = false;
  failureStreak += 1;

  if (state.queue.length > 1 && failureStreak <= playerConfig().maxAutoSkips) {
    playbackError.value = `《${song.title}》${reason}，已自动跳到下一首`;
    nextTrack(true);
    return;
  }
  playbackError.value = `《${song.title}》${reason}：文件可能已损坏或被移动`;
}

function stopPlayback(): void {
  sourceToken += 1;
  audio.pause();
  audio.removeAttribute('src');
  audio.load();
  releaseSource();
  state.currentSong = null;
  state.currentKey = null;
  state.playing = false;
  playbackError.value = '';
  audioState.currentTime = 0;
  audioState.duration = 0;
  audioState.bufferedPercent = 0;
}

function releaseSource(): void {
  if (sourceUrl) {
    URL.revokeObjectURL(sourceUrl);
    sourceUrl = '';
  }
  loadedPath = '';
}

export async function removeFromQueue(index: number): Promise<void> {
  if (index < 0 || index >= state.queue.length) return;
  const song = state.queue[index]!;
  const key = song.key;
  const wasCurrent = state.queueIndex === index;
  const confirmed = await showModal({
    title: '移出播放队列',
    description: wasCurrent
      ? `确定将正在播放的《${song.title}》移出队列吗？${
          state.queue.length > 1 ? '移除后会切到下一首，' : '移除后会停止播放，'
        }本地文件不会被删除。`
      : `确定将《${song.title}》移出播放队列吗？本地文件不会被删除。`,
    confirmText: '移出队列',
  });
  if (!confirmed) return;

  // 确认期间播放可能自动切歌，按 key 重新定位，避免误删别的曲目。
  const liveIndex = state.queue.findIndex((item) => item.key === key);
  if (liveIndex < 0) return;
  const liveCurrent = state.queueIndex === liveIndex;
  const wasPlaying = state.playing;
  state.queue.splice(liveIndex, 1);

  if (state.queue.length === 0) {
    stopPlayback();
  } else if (liveCurrent) {
    state.queueIndex = liveIndex < state.queue.length ? liveIndex : 0;
    playSong(state.queue[state.queueIndex]!, null, wasPlaying);
  } else if (state.queueIndex > liveIndex) {
    state.queueIndex -= 1;
  }
}

export async function clearQueue(): Promise<void> {
  if (state.queue.length === 0) return;
  const confirmed = await showModal({
    title: '清空播放队列',
    description: `确定清空播放队列中的 ${state.queue.length} 首曲目吗？当前播放会停止，本地文件不会被删除。`,
    confirmText: '清空队列',
    danger: true,
  });
  if (!confirmed) return;

  state.queue = [];
  state.queueIndex = -1;
  stopPlayback();
}

export function setVolume(value: number): void {
  if (!state.prefs) return;
  const volume = clamp(Number.isFinite(value) ? value : 0.85, 0, 1);
  state.prefs.volume = volume;
  if (volume > 0) state.prefs.muted = false;
  applyPrefsToAudio();
  persistPrefs();
}

export function adjustVolume(delta: number): void {
  if (!state.prefs) return;
  setVolume(Math.round((state.prefs.volume + delta) * 100) / 100);
}

export function toggleMute(): void {
  if (!state.prefs) return;
  state.prefs.muted = !state.prefs.muted;
  if (!state.prefs.muted && state.prefs.volume === 0) {
    state.prefs.volume = 0.5;
  }
  applyPrefsToAudio();
  persistPrefs(true);
}

export function cycleRate(): void {
  if (!state.prefs) return;
  const rates = playerConfig().rates;
  const currentIndex = rates.findIndex(
    (rate) => Math.abs(rate - state.prefs!.playbackRate) < 0.001,
  );
  const next = rates[(currentIndex + 1) % rates.length] ?? 1;
  state.prefs.playbackRate = next;
  audio.playbackRate = next;
  persistPrefs(true);
}

export function cyclePlayMode(): void {
  if (!state.prefs) return;
  const currentIndex = PLAY_MODE_CYCLE.indexOf(state.prefs.playMode);
  setPlayMode(PLAY_MODE_CYCLE[(currentIndex + 1) % PLAY_MODE_CYCLE.length]!);
}

export function toggleCurrentFavorite(): void {
  if (!state.currentKey) return;
  toggleFavorite(state.currentKey);
}

export async function exportSong(key: string | null): Promise<void> {
  if (!key || exporting.value) return;
  const song = songByKey(key);
  if (!song) {
    toast('曲目不存在或已从曲库移除', 'error');
    return;
  }
  exporting.value = true;
  try {
    const result = await invoke<ExportResult | null>(COMMANDS.exportTrack, { key });
    if (result) toast(`已导出到 ${result.path}`, 'success');
  } catch (error) {
    toast(`导出失败：${normalizeError(error)}`, 'error');
  } finally {
    exporting.value = false;
  }
}

export function toggleDrawer(): void {
  drawerOpen.value = !drawerOpen.value;
}

export function closeDrawer(): void {
  drawerOpen.value = false;
}

export function openDrawer(): void {
  drawerOpen.value = true;
}

export function modeMeta(mode: PlayMode): { icon: string; title: string } {
  switch (mode) {
    case PLAY_MODES.repeatOne:
      return { icon: 'repeat-1', title: '单曲循环（点击切换）' };
    case PLAY_MODES.shuffle:
      return { icon: 'shuffle', title: '随机播放（点击切换）' };
    default:
      return { icon: 'repeat', title: '顺序播放（点击切换）' };
  }
}

export function formatRate(rate: number): string {
  if (rate === 1) return '1.0x';
  return `${Number(rate.toFixed(2))}x`;
}