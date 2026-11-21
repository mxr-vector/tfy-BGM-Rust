/**
 * 关闭窗口确认：点击标题栏关闭按钮时，让用户选择「关闭程序」或「最小化托盘」，
 * 并可勾选「不再提示」把这次选择记到偏好里（`prefs.closeAction`）。
 *
 * 关闭程序走 Rust 的 `quit_app`，即使存在托盘图标也能立即退出；
 * 最小化托盘与「常规 · 最小化托盘」联动：会一并开启该设置并确保托盘图标存在，
 * 避免窗口被隐藏后无法恢复、或最小化按钮行为与关闭行为不一致。
 */

import { ref } from 'vue';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { getCurrentWindow, invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { CloseAction, Prefs } from '@/lib/types';
import { flushPrefsNow } from './prefs';
import { updatePrefs } from './settings';
import { state } from './state';

export const closePromptOpen = ref(false);

export function openClosePrompt(): void {
  closePromptOpen.value = true;
}

export function closeClosePrompt(): void {
  closePromptOpen.value = false;
}

/** 执行关闭动作；`remember` 为真时把选择写入偏好，下次不再询问。 */
export async function performCloseAction(
  action: Exclude<CloseAction, 'ask'>,
  remember: boolean,
): Promise<void> {
  closeClosePrompt();

  const trayWasEnabled = Boolean(state.prefs?.minimizeToTray);
  const patch: Partial<Prefs> = {};
  if (remember) patch.closeAction = action;
  // 关闭到托盘等价于启用「最小化托盘」：托盘图标与最小化行为保持一致。
  if (action === 'tray') patch.minimizeToTray = true;

  const changed = Object.keys(patch).length > 0;
  if (changed) updatePrefs(patch);

  if (action === 'quit') {
    // 关闭程序会立即退出：即使这次没有新增修改，也要等防抖中的偏好（音量、壁纸等）落盘。
    await flushPrefsNow();
    try {
      await invoke(COMMANDS.quitApp);
    } catch (error) {
      toast(`退出失败：${normalizeError(error)}`, 'error');
      void getCurrentWindow()?.close();
    }
    return;
  }

  if (changed) await flushPrefsNow();
  try {
    await invoke(COMMANDS.hideToTray);
  } catch (error) {
    // 托盘创建失败时不保留刚开启的设置 / 记住的选择，避免界面显示与实际行为不一致。
    if (action === 'tray' && !trayWasEnabled) {
      updatePrefs(
        remember
          ? { minimizeToTray: false, closeAction: 'ask' }
          : { minimizeToTray: false },
      );
    }
    toast(`最小化到托盘失败：${normalizeError(error)}`, 'error');
  }
}
