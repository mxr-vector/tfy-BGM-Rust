import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { Schedule, ScheduleEntry, Song } from '@/lib/types';
import { playSong, setVolume } from '@/stores/player';
import { syncQueueForPlayback } from './queue';
import { songByKey, state } from './state';

/** 保存整张节目单；后端会按开始时间排序、裁剪歌曲时间并校验范围重叠。 */
export async function persistSchedule(entries: ScheduleEntry[]): Promise<boolean> {
  try {
    const saved = await invoke<Schedule>(COMMANDS.saveSchedule, {
      schedule: { version: 2, entries },
    });
    state.schedule = saved.entries;
    toast('节目单已保存', 'success');
    return true;
  } catch (error) {
    toast(`保存节目单失败：${normalizeError(error)}`, 'error');
    return false;
  }
}

/** 按节目内的顺序解析出仍在曲库中的歌曲；已移除的曲目会被跳过。 */
export function resolveScheduleSongs(entry: ScheduleEntry): Song[] {
  const seen = new Set<string>();
  const songs: Song[] = [];
  for (const item of entry.songs) {
    if (seen.has(item.key)) continue;
    const song = songByKey(item.key);
    if (!song) continue;
    seen.add(song.key);
    songs.push(song);
  }
  return songs;
}

export interface ApplyOptions {
  announce?: boolean;
  /** 从指定歌曲开始播放；缺失时顺延到后面的曲目。 */
  songKey?: string | null;
  /** 目标歌曲正在播放时是否从头重播。 */
  restart?: boolean;
}

/**
 * 应用节目：把节目歌曲设为播放队列并开始播放，同时应用该节目的音量。
 * 不改变当前页面（用户可能正在编辑节目单）。
 */
export function applyScheduleEntry(
  entry: ScheduleEntry,
  { announce = true, songKey = null, restart = true }: ApplyOptions = {},
): boolean {
  // 先调音量再开播，避免新节目以旧音量突然响起。
  if (entry.volume !== null) setVolume(entry.volume);

  const queue = resolveScheduleSongs(entry);
  if (queue.length === 0) {
    if (announce) toast(`节目「${entry.name}」没有可播放的曲目`, 'error');
    return false;
  }

  let target: Song | undefined;
  if (songKey) {
    const order = entry.songs.map((item) => item.key);
    const start = order.indexOf(songKey);
    for (let index = Math.max(start, 0); index < order.length && !target; index += 1) {
      target = queue.find((song) => song.key === order[index]);
    }
  }
  target ??= queue[0]!;

  if (state.currentKey === target.key && !restart) {
    syncQueueForPlayback(target, queue);
  } else {
    playSong(target, queue, true);
  }

  if (announce) {
    toast(
      `已切换到节目「${entry.name}」${entry.volume !== null ? ` · 音量 ${Math.round(entry.volume * 100)}%` : ''}`,
      'success',
    );
  }
  return true;
}