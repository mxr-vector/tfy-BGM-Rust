import { ALL_CATEGORY, COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { Schedule, ScheduleEntry } from '@/lib/types';
import { smartPickSong } from './queue';
import { playSong, setVolume } from './player';
import { state } from './state';

/** 保存整张节目单；后端会按开始时间排序并校验分类是否存在。 */
export async function persistSchedule(entries: ScheduleEntry[]): Promise<boolean> {
  try {
    const saved = await invoke<Schedule>(COMMANDS.saveSchedule, {
      schedule: { version: 1, entries },
    });
    state.schedule = saved.entries;
    toast('节目单已保存', 'success');
    return true;
  } catch (error) {
    toast(`保存节目单失败：${normalizeError(error)}`, 'error');
    return false;
  }
}

export interface ApplyOptions {
  announce?: boolean;
}

/**
 * 到点应用节目：切到目标分类的完整歌单并开始播放，同时应用该时段的音量。
 * 不改变当前页面（用户可能正在编辑节目单）。
 */
export function applyScheduleEntry(entry: ScheduleEntry, { announce = true }: ApplyOptions = {}): boolean {
  const category = entry.category;
  const songs = category
    ? state.library.songs.filter((song) => song.category === category)
    : state.library.songs;
  if (songs.length === 0) {
    toast(
      category
        ? `节目「${entry.name}」的分类还没有曲目`
        : `节目「${entry.name}」没有可播放的曲目`,
      'error',
    );
    return false;
  }

  state.category = category ?? ALL_CATEGORY;
  state.favoritesOnly = false;

  // 先调音量再开播，避免新节目以旧音量突然响起。
  if (entry.volume !== null) setVolume(entry.volume);
  const first = smartPickSong(songs, state.currentSong) ?? songs[0]!;
  playSong(first, songs, true);

  if (announce) {
    toast(
      `已切换到节目「${entry.name}」${entry.volume !== null ? ` · 音量 ${Math.round(entry.volume * 100)}%` : ''}`,
      'success',
    );
  }
  return true;
}