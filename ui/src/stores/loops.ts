import { COMMANDS } from '@/lib/constants';
import { formatTime, normalizeError } from '@/lib/format';
import { showModal } from '@/lib/modal';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { LoopBook, LoopPoint, Song } from '@/lib/types';
import { audioState } from './audio-engine';
import { refreshLoopRegion } from './player';
import { state } from './state';

export function loopPointFor(key: string): LoopPoint | null {
  return state.loopPoints[key] ?? null;
}

/** 与后端 `loops::MIN_LOOP_SPAN` 保持一致，避免前端放行后被后端拒绝。 */
export const MIN_LOOP_SPAN = 0.2;

function knownDuration(song: Song): number {
  if (
    state.currentSong?.key === song.key &&
    audioState.loadedPath === song.path &&
    audioState.duration > 0
  ) {
    return audioState.duration;
  }
  return song.durationMs ? song.durationMs / 1000 : 0;
}

/** 保存 A/B 点；起点/终点单位为秒，终点必须晚于起点。 */
export async function saveLoopPoint(
  song: Song,
  start: number,
  end: number,
  enabled: boolean,
): Promise<boolean> {
  if (!Number.isFinite(start) || !Number.isFinite(end) || end - start < MIN_LOOP_SPAN) {
    toast(`循环终点至少要比起点晚 ${MIN_LOOP_SPAN} 秒`, 'error');
    return false;
  }
  const duration = knownDuration(song);
  if (duration > 0 && end > duration + 0.5) {
    toast(`循环终点超出曲目时长 ${formatTime(duration)}`, 'error');
    return false;
  }

  try {
    const book = await invoke<LoopBook>(COMMANDS.saveLoopPoint, {
      key: song.key,
      start: Math.max(0, start),
      end,
      enabled,
    });
    state.loopPoints = { ...book.points };
    refreshLoopRegion();
    toast(
      enabled
        ? `《${song.title}》已启用循环 ${formatTime(start)}–${formatTime(end)}`
        : `《${song.title}》循环已关闭`,
      'success',
    );
    return true;
  } catch (error) {
    toast(`保存循环点失败：${normalizeError(error)}`, 'error');
    return false;
  }
}

export async function clearLoopPoint(song: Song): Promise<void> {
  try {
    const book = await invoke<LoopBook>(COMMANDS.removeLoopPoint, { key: song.key });
    state.loopPoints = { ...book.points };
    refreshLoopRegion();
    toast(`已清除《${song.title}》的循环点`, 'success');
  } catch (error) {
    toast(`清除循环点失败：${normalizeError(error)}`, 'error');
  }
}

/** 曲目菜单入口：用弹窗编辑起止秒数。 */
export async function openLoopEditor(song: Song): Promise<void> {
  const existing = loopPointFor(song.key);
  const duration = knownDuration(song);
  const values = await showModal({
    title: `循环点 · ${song.title}`,
    description:
      '设置 A/B 循环区间（单位：秒）。启用后播放器会在区间内无缝循环；也可以在播放器诊断面板里用当前进度标记起点/终点。',
    fields: [
      {
        name: 'start',
        label: '循环起点（秒）',
        type: 'text',
        value: existing ? existing.start.toFixed(2) : '0',
      },
      {
        name: 'end',
        label: '循环终点（秒）',
        type: 'text',
        value: existing ? existing.end.toFixed(2) : duration > 0 ? duration.toFixed(2) : '',
        placeholder: duration > 0 ? `曲目时长 ${formatTime(duration)}` : '例如 32.5',
      },
      {
        name: 'enabled',
        label: '循环状态',
        type: 'select',
        value: existing?.enabled === false ? 'off' : 'on',
        options: [
          { value: 'on', label: '启用循环' },
          { value: 'off', label: '关闭循环（保留标记）' },
        ],
      },
    ],
    confirmText: '保存',
    validate: (input) => {
      const start = Number(input.start);
      const end = Number(input.end);
      if (!Number.isFinite(start) || !Number.isFinite(end)) return '请输入有效的秒数';
      if (start < 0) return '起点不能为负数';
      if (end - start < MIN_LOOP_SPAN) return `终点至少要比起点晚 ${MIN_LOOP_SPAN} 秒`;
      if (duration > 0 && end > duration + 0.5) {
        return `终点超出曲目时长 ${formatTime(duration)}`;
      }
      return null;
    },
  });
  if (!values) return;
  await saveLoopPoint(song, Number(values.start), Number(values.end), values.enabled === 'on');
}