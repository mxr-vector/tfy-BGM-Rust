import type { ScheduleEntry } from './types';

export interface DraftSong {
  key: string;
  start: string;
  end: string;
}

export interface Draft {
  name: string;
  start: string;
  end: string;
  songs: DraftSong[];
  volumeEnabled: boolean;
  volume: number;
  enabled: boolean;
}

export function pad2(value: number): string {
  return String(value).padStart(2, '0');
}

export function formatMinutes(total: number): string {
  return `${pad2(Math.floor(total / 60))}:${pad2(total % 60)}`;
}

export function minutesOf(time: string): number | null {
  const match = /^([01]\d|2[0-3]):([0-5]\d)$/.exec(time);
  if (!match) return null;
  return Number(match[1]) * 60 + Number(match[2]);
}

/** 下一小时到再下一小时的默认节目范围。 */
export function nextHourRange(): { start: string; end: string } {
  const date = new Date();
  date.setMinutes(0, 0, 0);
  date.setHours(date.getHours() + 1);
  const start = date.getHours() * 60;
  return { start: formatMinutes(start), end: formatMinutes(Math.min(start + 60, 23 * 60 + 59)) };
}

export function emptyDraft(): Draft {
  const range = nextHourRange();
  return {
    name: '',
    start: range.start,
    end: range.end,
    songs: [],
    volumeEnabled: false,
    volume: 0.5,
    enabled: true,
  };
}

export function draftFromEntry(entry: ScheduleEntry): Draft {
  return {
    name: entry.name,
    start: entry.start,
    end: entry.end,
    songs: entry.songs.map((song) => ({ ...song })),
    volumeEnabled: entry.volume !== null,
    volume: entry.volume ?? 0.5,
    enabled: entry.enabled,
  };
}