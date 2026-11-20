import { reactive } from 'vue';

export interface ContextMenuEntry {
  separator?: boolean;
  icon?: string;
  label?: string;
  danger?: boolean;
  action?: () => void;
}

export interface MenuPoint {
  clientX: number;
  clientY: number;
  preventDefault?: () => void;
}

export const contextMenuState = reactive({
  open: false,
  x: 0,
  y: 0,
  items: [] as ContextMenuEntry[],
});

export function openContextMenu(point: MenuPoint, items: ContextMenuEntry[]): void {
  point.preventDefault?.();
  contextMenuState.items = items;
  contextMenuState.x = point.clientX;
  contextMenuState.y = point.clientY;
  contextMenuState.open = true;
}

export function closeContextMenu(): void {
  if (!contextMenuState.open) return;
  contextMenuState.open = false;
  contextMenuState.items = [];
}

export function separator(): ContextMenuEntry {
  return { separator: true };
}