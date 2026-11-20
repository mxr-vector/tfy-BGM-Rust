/** 播放队列的状态操作：只调整曲目顺序与指针，不触发加载或播放。 */

import type { Song } from '@/lib/types';
import { state } from './state';

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

/** 随机抽取一个与当前不同的索引（队列至少两首时才可能换歌）。 */
export function randomOtherIndex(): number {
  let index = state.queueIndex;
  while (state.queue.length > 1 && index === state.queueIndex) {
    index = Math.floor(Math.random() * state.queue.length);
  }
  return index;
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