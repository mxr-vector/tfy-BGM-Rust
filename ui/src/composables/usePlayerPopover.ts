import { ref } from 'vue';

/** 播放器内同一时间只保留一个浮层（倍速列表 / 导出设置 / 播放输出）。 */
export type PlayerPopover = 'rate' | 'export' | 'output' | null;

export const playerPopover = ref<PlayerPopover>(null);

export function togglePlayerPopover(kind: Exclude<PlayerPopover, null>): void {
  playerPopover.value = playerPopover.value === kind ? null : kind;
}

export function closePlayerPopover(kind: Exclude<PlayerPopover, null>): void {
  if (playerPopover.value === kind) playerPopover.value = null;
}