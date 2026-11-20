import { ref, watch } from 'vue';
import { COMMANDS, PLAY_MODE_CYCLE, PLAY_MODES } from '@/lib/constants';
import { playerConfig } from '@/lib/config';
import { clamp, normalizeError } from '@/lib/format';
import { showModal } from '@/lib/modal';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { ExportResult, PlayMode, Song } from '@/lib/types';
import {
  applyAudioSettings,
  audioElement,
  currentTime,
  hasSourceUrl,
  isSourceLoaded,
  loadSource,
  pausePlayback,
  rewindToStart,
  seekTo as seekAudio,
  startPlayback,
  unloadSource,
} from './audio-engine';
import { persistPrefs, toggleFavorite } from './prefs';
import {
  detachQueueItem,
  nextSequentialIndex,
  prevSequentialIndex,
  queueIndexByKey,
  randomOtherIndex,
  resetQueue,
  syncQueueForPlayback,
} from './queue';
import { songByKey, state } from './state';

export const drawerOpen = ref(false);
export const exporting = ref(false);
export const playbackError = ref('');

const audio = audioElement();
let failureStreak = 0;

function applyPrefsToAudio(): void {
  const prefs = state.prefs;
  if (!prefs) return;
  applyAudioSettings(prefs.volume, prefs.muted, prefs.playbackRate);
}

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
      if (audio.hasAttribute('src') || hasSourceUrl()) stopPlayback();
      return;
    }
    if (isSourceLoaded(song.path)) return;
    const time = currentTime();
    const wasPlaying = previous ? !audio.paused : false;
    void loadSongSource(song, { autoplay: wasPlaying, restoreTime: previous ? time : 0 });
  },
);

export function playSong(
  song: Song,
  newQueue: Song[] | null = null,
  forcePlay = false,
  autoplay = true,
): void {
  if (!song) return;

  syncQueueForPlayback(song, newQueue);

  if (state.currentKey === song.key && state.currentSong) {
    if (!hasSourceUrl()) {
      void loadSongSource(song, { autoplay });
    } else if (forcePlay) {
      rewindToStart();
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
  void loadSongSource(song, { autoplay });
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

async function loadSongSource(
  song: Song,
  { autoplay = true, restoreTime = 0 }: { autoplay?: boolean; restoreTime?: number } = {},
): Promise<void> {
  applyPrefsToAudio();
  const outcome = await loadSource(song.path, {
    autoplay,
    restoreTime,
    onAutoplayBlocked: () => {
      state.playing = false;
    },
  });
  if (outcome.status === 'failed') {
    // 失败时元素还挂着上一首的源，先卸载，避免 UI 暂停但旧音频仍在播放。
    unloadSource();
    failCurrent(`无法读取音频（${normalizeError(outcome.error)}）`);
  }
}

export function togglePlay(): void {
  if (!state.currentSong) return;
  if (audio.paused) startPlayback();
  else pausePlayback();
}

export function seekBy(deltaSeconds: number): void {
  seekTo(currentTime() + deltaSeconds);
}

export function seekTo(seconds: number): void {
  seekAudio(seconds);
}

export function nextTrack(forcePlay = true): void {
  if (state.queue.length === 0) return;
  if (state.queue.length === 1) {
    rewindToStart();
    if (forcePlay) startPlayback();
    return;
  }

  const nextIndex =
    state.prefs?.playMode === PLAY_MODES.shuffle
      ? randomOtherIndex()
      : nextSequentialIndex();
  if (nextIndex < 0) return;
  state.queueIndex = nextIndex;
  playSong(state.queue[nextIndex]!, null, forcePlay, forcePlay);
}

export function prevTrack(forcePlay = true): void {
  if (state.queue.length === 0) return;
  if (currentTime() > playerConfig().restartThresholdSeconds) {
    seekTo(0);
    if (!state.playing && forcePlay) startPlayback();
    return;
  }
  if (state.queue.length === 1) {
    seekTo(0);
    if (!state.playing && forcePlay) startPlayback();
    return;
  }

  const prevIndex = prevSequentialIndex();
  if (prevIndex < 0) return;
  state.queueIndex = prevIndex;
  playSong(state.queue[prevIndex]!, null, forcePlay, forcePlay);
}

function handleEnded(): void {
  if (state.prefs?.playMode === PLAY_MODES.repeatOne) {
    rewindToStart();
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
    // 用 toast 而不是 playbackError：自动跳曲会立刻进入下一首的 playSong 并清空 playbackError。
    toast(`《${song.title}》${reason}，已自动跳到下一首`, 'error');
    nextTrack(true);
    return;
  }
  playbackError.value = `《${song.title}》${reason}：文件可能已损坏或被移动`;
}

function stopPlayback(): void {
  unloadSource();
  state.currentSong = null;
  state.currentKey = null;
  state.playing = false;
  playbackError.value = '';
}

export async function removeFromQueue(index: number): Promise<void> {
  if (index < 0 || index >= state.queue.length) return;
  const song = state.queue[index]!;
  const confirmed = await showModal({
    title: '移出播放队列',
    description:
      state.queueIndex === index
        ? `确定将正在播放的《${song.title}》移出队列吗？${
            state.queue.length > 1 ? '移除后会切到下一首，' : '移除后会停止播放，'
          }本地文件不会被删除。`
        : `确定将《${song.title}》移出播放队列吗？本地文件不会被删除。`,
    confirmText: '移出队列',
  });
  if (!confirmed) return;

  // 确认期间播放可能自动切歌，按 key 重新定位，避免误删别的曲目。
  const liveIndex = queueIndexByKey(song.key);
  if (liveIndex < 0) return;
  const removed = detachQueueItem(liveIndex);
  if (!removed) return;

  if (state.queue.length === 0) {
    stopPlayback();
  } else if (removed.wasCurrent) {
    // 移除当前曲目时沿用移除前的播放状态：暂停中就不要自动开播。
    playSong(state.queue[state.queueIndex]!, null, removed.wasPlaying, removed.wasPlaying);
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

  resetQueue();
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

/** 从配置的倍速档位中选择；非法档位直接忽略。 */
export function setPlaybackRate(rate: number): void {
  if (!state.prefs) return;
  if (!playerConfig().rates.some((value) => Math.abs(value - rate) < 0.001)) return;
  if (Math.abs(state.prefs.playbackRate - rate) < 0.001) return;
  state.prefs.playbackRate = rate;
  applyPrefsToAudio();
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
  if (state.prefs?.loudnessNormalization) {
    toast('正在导出并做响度归一化，请稍候…', 'info');
  }
  exporting.value = true;
  try {
    const result = await invoke<ExportResult | null>(COMMANDS.exportTrack, { key });
    if (result) {
      const detail = result.warning
        ? `（${result.warning}）`
        : result.normalized
          ? '（已响度归一化）'
          : '';
      toast(`已导出到 ${result.path}${detail}`, result.warning ? 'info' : 'success');
    }
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