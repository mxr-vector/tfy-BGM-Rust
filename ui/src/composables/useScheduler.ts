/**
 * 节目单调度器：每 15 秒对照系统时间，在分钟边界应用节目。
 *
 * WebView 在窗口失焦/最小化时会把定时器节流到一分钟一次，因此不能只判断
 * 「当前这一分钟」：每次都会回看最近 2 分钟，取最晚的一条未触发节目；
 * 休眠恢复（间隔超过 90 秒）则直接对齐到「当前生效节目」。
 */

import { computed, ref } from 'vue';
import type { ScheduleEntry } from '@/lib/types';
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
  at: number;
  key: string;
}

function isoDay(date: Date): number {
  const day = date.getDay();
  return day === 0 ? 7 : day;
}

function minuteOf(start: string): number {
  const [hour, minute] = start.split(':').map(Number);
  return (hour ?? 0) * 60 + (minute ?? 0);
}

function startOfDay(date: Date, start: string): Date {
  const [hour, minute] = start.split(':').map(Number);
  const result = new Date(date);
  result.setHours(hour ?? 0, minute ?? 0, 0, 0);
  return result;
}

function trigger(entry: ScheduleEntry, day: number): void {
  if (applyScheduleEntry(entry)) activeScheduleId.value = entry.id;
  triggered.add(`${entry.id}@${day}@${entry.start}`);
}

function tick(): void {
  const now = new Date();
  schedulerNow.value = now;
  const gap = now.getTime() - lastTick;
  lastTick = now.getTime();
  const day = isoDay(now);
  const cutoff = now.getTime() - CATCHUP_WINDOW_MS;

  const candidates: Candidate[] = [];
  for (const entry of state.schedule) {
    if (!entry.enabled || !entry.days.includes(day)) continue;
    const at = startOfDay(now, entry.start).getTime();
    if (at > now.getTime() || at < cutoff) continue;
    const key = `${entry.id}@${day}@${entry.start}`;
    if (!triggered.has(key)) candidates.push({ entry, at, key });
  }

  if (candidates.length > 0) {
    candidates.sort((left, right) => right.at - left.at);
    const latest = candidates[0]!;
    // 同一窗口内的多条节目视为已过期，只应用最晚的一条。
    for (const item of candidates) triggered.add(item.key);
    trigger(latest.entry, day);
  } else if (gap > 90_000) {
    // 睡眠恢复：直接对齐到今天最晚一条已开始的节目。
    const minutes = now.getHours() * 60 + now.getMinutes();
    let current: ScheduleEntry | null = null;
    let currentStart = -1;
    for (const entry of state.schedule) {
      if (!entry.enabled || !entry.days.includes(day)) continue;
      const start = minuteOf(entry.start);
      if (start <= minutes && start > currentStart) {
        current = entry;
        currentStart = start;
      }
    }
    if (current) {
      const key = `${current.id}@${day}@${current.start}`;
      if (!triggered.has(key)) trigger(current, day);
    }
  }

  if (triggered.size > 800) triggered.clear();
}

/** 当前时刻生效的节目（按今天已开始的最晚一条计算）。 */
export const currentProgram = computed<ScheduleEntry | null>(() => {
  const date = schedulerNow.value;
  const day = isoDay(date);
  const minutes = date.getHours() * 60 + date.getMinutes();
  let best: ScheduleEntry | null = null;
  let bestStart = -1;
  for (const entry of state.schedule) {
    if (!entry.enabled || !entry.days.includes(day)) continue;
    const start = minuteOf(entry.start);
    if (start <= minutes && start > bestStart) {
      best = entry;
      bestStart = start;
    }
  }
  return best;
});

/** 今天接下来的一条节目；没有则为 `null`。 */
export const nextProgram = computed<ScheduleEntry | null>(() => {
  const date = schedulerNow.value;
  const day = isoDay(date);
  const minutes = date.getHours() * 60 + date.getMinutes();
  let next: ScheduleEntry | null = null;
  let nextStart = Number.POSITIVE_INFINITY;
  for (const entry of state.schedule) {
    if (!entry.enabled || !entry.days.includes(day)) continue;
    const start = minuteOf(entry.start);
    if (start > minutes && start < nextStart) {
      next = entry;
      nextStart = start;
    }
  }
  return next;
});

export function installScheduler(): () => void {
  tick();
  timer = window.setInterval(tick, 15_000);
  return () => {
    window.clearInterval(timer);
    timer = 0;
  };
}