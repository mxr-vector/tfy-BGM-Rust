import { ref, watch } from 'vue';
import { COMMANDS, PLAY_MODE_CYCLE, PLAY_MODES } from '@/lib/constants';
import { playerConfig } from '@/lib/config';
import { clamp, formatTime, normalizeError } from '@/lib/format';
import { showModal } from '@/lib/modal';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { ExportResult, LoopPoint, PlayMode, Song } from '@/lib/types';
import {
  applyAudioSettings,
  audioState,
  currentTime,
  hasSourceUrl,
  isSourceLoaded,
  isLoading,
  loadSource,
  pausePlayback,
  rewindToStart,
  seekTo as seekAudio,
  setLoopRegion,
  startPlayback,
  subscribeEngine,
  unloadSource,
  type LoopRegion,
} from './audio-engine';
import { persistPrefs, toggleFavorite } from './prefs';
import {
  detachQueueItem,
  nextSequentialIndex,
  prevSequentialIndex,
  queueIndexByKey,
  recordPlay,
  resetQueue,
  smartNextIndex,
  syncQueueForPlayback,
} from './queue';
import { songByKey, state } from './state';

export const drawerOpen = ref(false);
export const exporting = ref(false);
export const playbackError = ref('');

let failureStreak = 0;

type EndedHandler = () => boolean;
let endedHandler: EndedHandler | null = null;

/**
 * 节目单在生效时段内接管「播完」行为：返回 true 表示已处理，不再自动跳下一首。
 * 用于把播放限制在每首歌自己的时间槽内，而不是顺着整张节目单队列往后跑。
 */
export function setEndedHandler(handler: EndedHandler | null): void {
  endedHandler = handler;
}

/** 整曲 AudioBuffer 解码的安全上限（30 分钟约 690MB），超出直接跳过。 */
const MAX_DECODE_SECONDS = 30 * 60;

function applyPrefsToAudio(): void {
  const prefs = state.prefs;
  if (!prefs) return;
  applyAudioSettings(prefs.volume, prefs.muted, prefs.playbackRate);
}

subscribeEngine((event) => {
  if (event === 'play') {
    state.playing = true;
    playbackError.value = '';
    failureStreak = 0;
  } else if (event === 'pause') {
    state.playing = false;
  } else if (event === 'ended') {
    state.playing = false;
    handleEnded();
  }
});

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
      // 加载中也必须停止：unloadSource 会作废未完成的请求，避免曲目已被移出曲库后仍自动播放。
      if (hasSourceUrl() || isLoading()) stopPlayback();
      return;
    }
    if (isSourceLoaded(song.path)) {
      refreshLoopRegion();
      return;
    }
    const time = currentTime();
    const wasPlaying = previous ? state.playing : false;
    // 同一曲目换了路径（重命名 / 外部移动）不是切歌：硬切重载，避免同内容叠加淡入淡出。
    void loadSongSource(song, {
      autoplay: wasPlaying,
      restoreTime: previous ? time : 0,
      crossfade: 0,
    });
  },
);

// 循环点被编辑 / 外部迁移时立即作用到当前曲目。
watch(
  () => state.loopPoints,
  () => refreshLoopRegion(),
  { deep: true },
);

/** 当前曲目启用的循环区间；终点会被实际时长裁剪，非法区间自动忽略。 */
function loopRegionFor(song: Song | null): LoopRegion | null {
  if (!song) return null;
  const point: LoopPoint | undefined = state.loopPoints[song.key];
  if (!point?.enabled) return null;
  // 只有当前 buffer 确实属于这首歌时，audioState.duration 才可信；
  // 切歌过程中它还停留在上一首的时长，会错误裁剪终点。
  const loadedHere =
    state.currentSong?.key === song.key &&
    audioState.loadedPath === song.path &&
    audioState.duration > 0;
  const knownDuration = loadedHere
    ? audioState.duration
    : song.durationMs
      ? song.durationMs / 1000
      : 0;
  const start = Math.max(0, point.start);
  let end = point.end;
  if (knownDuration > 0) end = Math.min(end, knownDuration);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end - start < 0.1) return null;
  return { start, end };
}

export function refreshLoopRegion(): void {
  setLoopRegion(loopRegionFor(state.currentSong));
}

/** 当前设置的切歌淡入淡出时长（秒）；0 表示硬切。 */
function crossfadeSeconds(): number {
  return state.prefs?.crossfadeSeconds ?? 0;
}

export function playSong(
  song: Song,
  newQueue: Song[] | null = null,
  forcePlay = false,
  autoplay = true,
  fadeInSeconds = 0,
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
  // 交叉淡入淡出时旧曲仍在发声：播放状态保持不变，避免按钮与界面闪一下暂停。
  playbackError.value = '';
  recordPlay(song.key);
  void loadSongSource(song, { autoplay, fadeInSeconds });
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
  {
    autoplay = true,
    restoreTime = 0,
    crossfade = crossfadeSeconds(),
    fadeInSeconds = 0,
  }: {
    autoplay?: boolean;
    restoreTime?: number;
    crossfade?: number;
    fadeInSeconds?: number;
  } = {},
): Promise<void> {
  // 整曲解码为 float32 PCM：48kHz 立体声约 23MB/分钟，过长曲目会撑爆 WebView 内存。
  const seconds = (song.durationMs ?? 0) / 1000;
  if (seconds > MAX_DECODE_SECONDS) {
    unloadSource();
    failCurrent(`时长 ${formatTime(seconds)} 超过 ${MAX_DECODE_SECONDS / 60} 分钟，整曲解码内存占用过高`);
    return;
  }
  applyPrefsToAudio();
  const outcome = await loadSource(song.path, {
    autoplay,
    restoreTime,
    loop: loopRegionFor(song),
    crossfadeSeconds: crossfade,
    fadeInSeconds,
    onAutoplayBlocked: () => {
      state.playing = false;
    },
  });
  if (outcome.status === 'failed') {
    // 失败时先卸载，避免 UI 暂停但旧音频仍在播放。
    unloadSource();
    failCurrent(`无法读取音频（${normalizeError(outcome.error)}）`);
  }
}

export function togglePlay(): void {
  if (!state.currentSong) return;
  if (state.playing) pausePlayback();
  else startPlayback();
}

export function seekBy(deltaSeconds: number): void {
  seekTo(currentTime() + deltaSeconds);
}

export function seekTo(seconds: number): void {
  seekAudio(seconds);
}

export function nextTrack(forcePlay = true, fadeInSeconds = 0): void {
  if (state.queue.length === 0) return;
  if (state.queue.length === 1) {
    rewindToStart();
    if (forcePlay) startPlayback(fadeInSeconds || undefined);
    return;
  }

  const nextIndex =
    state.prefs?.playMode === PLAY_MODES.shuffle
      ? smartNextIndex(state.queue, state.queueIndex)
      : nextSequentialIndex();
  if (nextIndex < 0) return;
  state.queueIndex = nextIndex;
  playSong(state.queue[nextIndex]!, null, forcePlay, forcePlay, fadeInSeconds);
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
  if (endedHandler?.()) return;
  // 自然播完后没有旧声部可交叉，用相同时长给新曲补一个淡入，避免列表播放时每次都是硬切。
  const fade = crossfadeSeconds();
  if (state.prefs?.playMode === PLAY_MODES.repeatOne) {
    rewindToStart();
    startPlayback(fade || undefined);
    return;
  }
  nextTrack(true, fade);
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