/**
 * 节目单调度器：每秒对照系统时间，在歌曲设定的开始时间应用对应节目。
 *
 * WebView 在窗口失焦/最小化时会把定时器节流到一分钟一次，因此不能只判断
 * 「当前这一秒」：每次都会回看最近 2 分钟，取最晚的一条未触发歌曲；
 * 休眠恢复（间隔超过 90 秒）则直接对齐到「当前节目里最晚一首已开始的歌」。
 */

import { computed, ref } from 'vue';
import { toast } from '@/lib/toast';
import type { ScheduleEntry, ScheduleSong } from '@/lib/types';
import { setEndedHandler } from '@/stores/player';
import { applyScheduleEntry } from '@/stores/schedule';
import { state } from '@/stores/state';

export const schedulerNow = ref(new Date());
export const activeScheduleId = ref<string | null>(null);

const triggered = new Set<string>();
let lastTick = Date.now();
let timer = 0;
const CATCHUP_WINDOW_MS = 2 * 60_000;

interface Candidate {
  entry: ScheduleEntry;
  song: ScheduleSong | null;
  at: number;
  key: string;
}

function minuteOf(time: string): number {
  const [hour, minute] = time.split(':').map(Number);
  return (hour ?? 0) * 60 + (minute ?? 0);
}

function startOfDay(date: Date, time: string): Date {
  const [hour, minute] = time.split(':').map(Number);
  const result = new Date(date);
  result.setHours(hour ?? 0, minute ?? 0, 0, 0);
  return result;
}

function dayKey(date: Date): string {
  return `${date.getFullYear()}-${date.getMonth() + 1}-${date.getDate()}`;
}

function trigger(entry: ScheduleEntry, song: ScheduleSong | null, key: string): void {
  // 只有节目发生切换时才提示，节目内切歌保持安静。
  const switched = activeScheduleId.value !== entry.id;
  if (applyScheduleEntry(entry, { songKey: song?.key ?? null, announce: switched, restart: false })) {
    activeScheduleId.value = entry.id;
  }
  triggered.add(key);
}

function tick(): void {
  const now = new Date();
  schedulerNow.value = now;
  const gap = now.getTime() - lastTick;
  lastTick = now.getTime();
  const day = dayKey(now);
  const cutoff = now.getTime() - CATCHUP_WINDOW_MS;

  const candidates: Candidate[] = [];
  for (const entry of state.schedule) {
    if (!entry.enabled) continue;
    if (entry.songs.length === 0) {
      const at = startOfDay(now, entry.start).getTime();
      const key = `${entry.id}@${day}@${entry.start}@`;
      if (at <= now.getTime() && at >= cutoff && !triggered.has(key)) {
        candidates.push({ entry, song: null, at, key });
      }
      continue;
    }
    for (const song of entry.songs) {
      const at = startOfDay(now, song.start).getTime();
      if (at > now.getTime() || at < cutoff) continue;
      const key = `${entry.id}@${day}@${song.start}@${song.key}`;
      if (!triggered.has(key)) candidates.push({ entry, song, at, key });
    }
  }

  if (candidates.length > 0) {
    candidates.sort((left, right) => right.at - left.at);
    const latest = candidates[0]!;
    // 同一窗口内的多条歌曲视为已过期，只应用最晚的一条。
    for (const item of candidates) triggered.add(item.key);
    trigger(latest.entry, latest.song, latest.key);
  } else if (gap > 90_000 || activeScheduleId.value === null) {
    // 启动、睡眠恢复或新节目生效：直接对齐到今天当前节目里最晚一首已开始的歌。
    const minutes = now.getHours() * 60 + now.getMinutes();
    const current = currentProgram.value;
    if (current) {
      const due = latestSongBefore(current, minutes);
      if (due) {
        const key = `${current.id}@${day}@${due.start}@${due.key}`;
        if (!triggered.has(key)) trigger(current, due, key);
      } else if (current.songs.length === 0) {
        const key = `${current.id}@${day}@${current.start}@`;
        if (!triggered.has(key)) trigger(current, null, key);
      }
    }
  }

  if (!currentProgram.value) activeScheduleId.value = null;
  if (triggered.size > 4000) triggered.clear();
}

function latestSongBefore(entry: ScheduleEntry, minutes: number): ScheduleSong | null {
  let best: ScheduleSong | null = null;
  let bestStart = -1;
  for (const song of entry.songs) {
    const start = minuteOf(song.start);
    if (start <= minutes && start > bestStart) {
      best = song;
      bestStart = start;
    }
  }
  return best;
}

/** 当前时刻生效的节目（时间范围左闭右开）。 */
export const currentProgram = computed<ScheduleEntry | null>(() => {
  const date = schedulerNow.value;
  const minutes = date.getHours() * 60 + date.getMinutes();
  for (const entry of state.schedule) {
    if (!entry.enabled) continue;
    if (minuteOf(entry.start) <= minutes && minutes < minuteOf(entry.end)) return entry;
  }
  return null;
});

/** 当前节目里最晚一首已到播放时间的歌曲。 */
export const currentProgramSong = computed<ScheduleSong | null>(() => {
  const entry = currentProgram.value;
  if (!entry) return null;
  const date = schedulerNow.value;
  return latestSongBefore(entry, date.getHours() * 60 + date.getMinutes());
});

/** 处于节目单时段时拦截手动点播并提示；返回 true 表示已拦截。 */
export function guardLibraryPlayback(): boolean {
  const program = currentProgram.value;
  if (!program) return false;
  toast(
    `当前处于节目单「${program.name}」播放时段（${program.start}–${program.end}），暂不能手动点播`,
    'info',
  );
  return true;
}

/**
 * 立即按当前时间对齐节目，供启用/保存节目后调用，无需等待下一秒的 tick。
 * 时段内还没有歌曲到点时从节目第一首开始，避免启用后仍在播放音乐库曲目。
 */
export function syncToCurrentProgram(): boolean {
  schedulerNow.value = new Date();
  const program = currentProgram.value;
  if (!program) return false;
  const date = schedulerNow.value;
  const due = latestSongBefore(program, date.getHours() * 60 + date.getMinutes());
  const target = due ?? program.songs[0] ?? null;
  if (!target) return false;

  const switched = activeScheduleId.value !== program.id;
  if (!applyScheduleEntry(program, { songKey: target.key, announce: switched, restart: false })) {
    return false;
  }
  activeScheduleId.value = program.id;
  return true;
}

/** 下一档节目；今天已无节目时指向明天的第一档。 */
export const nextProgram = computed<{ entry: ScheduleEntry; tomorrow: boolean } | null>(() => {
  const date = schedulerNow.value;
  const minutes = date.getHours() * 60 + date.getMinutes();
  let next: ScheduleEntry | null = null;
  let nextStart = Number.POSITIVE_INFINITY;
  let first: ScheduleEntry | null = null;
  let firstStart = Number.POSITIVE_INFINITY;

  for (const entry of state.schedule) {
    if (!entry.enabled) continue;
    const start = minuteOf(entry.start);
    if (start < firstStart) {
      first = entry;
      firstStart = start;
    }
    if (start > minutes && start < nextStart) {
      next = entry;
      nextStart = start;
    }
  }

  if (next) return { entry: next, tomorrow: false };
  return first ? { entry: first, tomorrow: true } : null;
});

export function installScheduler(): () => void {
  setEndedHandler(handleScheduleEnded);
  tick();
  timer = window.setInterval(tick, 1_000);
  return () => {
    window.clearInterval(timer);
    timer = 0;
    setEndedHandler(null);
  };
}

/**
 * 节目生效时段内，歌曲播完后只循环「当前到点」的歌曲，等下一首的起始时间再切换；
 * 不再顺着整张节目单队列自动往后跳，避免播到其它时段的歌。
 */
function handleScheduleEnded(): boolean {
  const program = currentProgram.value;
  if (!program) return false;
  const due = currentProgramSong.value;
  if (!due) return false;
  return applyScheduleEntry(program, { songKey: due.key, announce: false, restart: true });
}
