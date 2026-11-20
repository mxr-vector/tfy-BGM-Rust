/** 播放队列的状态操作：只调整曲目顺序与指针，不触发加载或播放。 */

import type { Song } from '@/lib/types';
import { state } from './state';

const HISTORY_LIMIT = 48;
const HISTORY_WINDOW_MINUTES = 45;

interface PlayRecord {
  key: string;
  at: number;
}

const history: PlayRecord[] = [];

/** 记录一次实际开始播放，供「近期播放降权」使用。 */
export function recordPlay(key: string): void {
  history.push({ key, at: Date.now() });
  if (history.length > HISTORY_LIMIT) history.shift();
}

/** 开始播放某曲时同步队列：显式列表优先，否则复用已有队列或追加到末尾。 */
export function syncQueueForPlayback(song: Song, newQueue: Song[] | null): void {
  if (newQueue) {
    state.queue = [...newQueue];
    const index = state.queue.findIndex((queued) => queued.key === song.key);
    state.queueIndex = index >= 0 ? index : 0;
    return;
  }

  const index = queueIndexByKey(song.key);
  if (index >= 0) {
    state.queueIndex = index;
  } else {
    state.queue.push(song);
    state.queueIndex = state.queue.length - 1;
  }
}

export function queueIndexByKey(key: string): number {
  return state.queue.findIndex((song) => song.key === key);
}

export function nextSequentialIndex(): number {
  if (state.queue.length === 0) return -1;
  return (state.queueIndex + 1) % state.queue.length;
}

export function prevSequentialIndex(): number {
  const length = state.queue.length;
  if (length === 0) return -1;
  return (state.queueIndex - 1 + length) % length;
}

/**
 * 智能随机：不是纯随机，而是按 BGM 场景的可听性加权。
 * - 同艺人 / 同专辑不连续（有替代曲目时直接排除）；
 * - 收藏、同分类、同流派加权；
 * - 45 分钟内播过的曲目显著降权，越近降得越狠；
 * - 长曲之后避免立刻接短曲，短曲之后也避免再堆短曲。
 */
export function smartNextIndex(queue: Song[], currentIndex: number): number {
  if (queue.length <= 1) return -1;
  const current = queue[currentIndex] ?? null;
  const choice = smartPick(queue, current, new Set([currentIndex]));
  if (!choice) return -1;
  return queue.indexOf(choice);
}

/** 随机试听入口：在没有「当前曲目」时挑一个加权起点。 */
export function smartPickSong(list: Song[], current: Song | null): Song | null {
  return smartPick(list, current, new Set());
}

function smartPick(
  candidates: Song[],
  current: Song | null,
  excluded: Set<number>,
): Song | null {
  if (candidates.length === 0) return null;

  const recent = recentPenalty();
  const currentArtist = current?.artist?.toLowerCase();
  const currentAlbum = current?.album?.toLowerCase();

  const scored = candidates.map((song, index) => {
    if (excluded.has(index)) return { song, weight: 0 };
    let weight = 1;

    if (state.favorites.has(song.key)) weight *= 1.9;
    if (current?.category === song.category) weight *= 1.18;
    else weight *= 0.95;
    if (song.genre && current?.genre && song.genre === current.genre) weight *= 1.08;

    const penalty = recent.get(song.key) ?? 0;
    weight *= Math.max(0.05, 1 - penalty);

    if (current?.durationMs && song.durationMs) {
      if (current.durationMs >= 180_000 && song.durationMs < 90_000) weight *= 0.22;
      if (current.durationMs < 60_000 && song.durationMs < 60_000) weight *= 0.4;
      if (current.durationMs >= 300_000 && song.durationMs >= 300_000) weight *= 0.75;
    }

    const sameArtist =
      currentArtist && song.artist && song.artist.toLowerCase() === currentArtist;
    const sameAlbum =
      currentAlbum && song.album && song.album.toLowerCase() === currentAlbum;
    if (sameAlbum) weight *= 0.03;
    else if (sameArtist) weight *= 0.05;

    return { song, weight };
  });

  const hasAlternative = scored.some(
    (entry) =>
      entry.weight > 0 &&
      entry.song.artist?.toLowerCase() !== currentArtist &&
      entry.song.album?.toLowerCase() !== currentAlbum,
  );
  if (hasAlternative) {
    for (const entry of scored) {
      const sameArtist =
        currentArtist && entry.song.artist?.toLowerCase() === currentArtist;
      const sameAlbum =
        currentAlbum && entry.song.album?.toLowerCase() === currentAlbum;
      if (sameArtist || sameAlbum) entry.weight = 0;
    }
  }

  const total = scored.reduce((sum, entry) => sum + entry.weight, 0);
  if (total <= 0) return null;
  let roll = Math.random() * total;
  for (const entry of scored) {
    roll -= entry.weight;
    if (roll <= 0 && entry.weight > 0) return entry.song;
  }
  return scored.find((entry) => entry.weight > 0)?.song ?? null;
}

/** 近期播放惩罚：0 ~ 1，1 表示刚刚播过。 */
function recentPenalty(): Map<string, number> {
  const now = Date.now();
  const penalties = new Map<string, number>();
  history.forEach((record, index) => {
    const ageMinutes = (now - record.at) / 60_000;
    if (ageMinutes > HISTORY_WINDOW_MINUTES) return;
    const recency = 1 - ageMinutes / HISTORY_WINDOW_MINUTES;
    const position = (index + 1) / history.length;
    const value = recency * (0.45 + 0.55 * position);
    penalties.set(record.key, Math.max(penalties.get(record.key) ?? 0, value));
  });
  return penalties;
}

export interface DetachedQueueItem {
  song: Song;
  wasCurrent: boolean;
  wasPlaying: boolean;
}

/** 移除指定位置并修正指针，返回被移除曲目与移除前的播放状态。 */
export function detachQueueItem(index: number): DetachedQueueItem | null {
  if (index < 0 || index >= state.queue.length) return null;

  const song = state.queue[index]!;
  const wasCurrent = state.queueIndex === index;
  const wasPlaying = state.playing;
  state.queue.splice(index, 1);

  if (state.queue.length === 0) {
    state.queueIndex = -1;
  } else if (wasCurrent) {
    state.queueIndex = index < state.queue.length ? index : 0;
  } else if (state.queueIndex > index) {
    state.queueIndex -= 1;
  }

  return { song, wasCurrent, wasPlaying };
}

export function resetQueue(): void {
  state.queue = [];
  state.queueIndex = -1;
}