/**
 * 「基本系统配置」界面状态与偏好写入。
 *
 * 偏好本身仍保存在 `prefs.json`（由 Rust 归一化并落盘），这里只负责：
 * - 弹窗的打开 / 关闭与当前分组；
 * - 把界面的离散操作合并进 `state.prefs` 并立即持久化；
 * - 需要同步到操作系统的项（开机自启动、最小化托盘）在写盘后调用 Rust 命令；
 * - 第三方接入的 token / CORS 保存到 `config.json` 并立即作用于接口服务。
 */

import { computed, ref } from 'vue';
import { COMMANDS, DEFAULT_CROSSFADE_SECONDS } from '@/lib/constants';
import { fontStack } from '@/lib/fonts';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { ApiEndpoint, ApiStatus, CorsConfig, Prefs } from '@/lib/types';
import { apiStatus } from './api';
import { persistPrefs } from './prefs';
import { state } from './state';

export type SettingsTab = 'general' | 'playback' | 'models' | 'integrations';

export const settingsOpen = ref(false);
export const settingsTab = ref<SettingsTab>('general');

/** 接口清单由 Rust 提供，读取一次后缓存。 */
export const apiEndpoints = ref<ApiEndpoint[]>([]);
let endpointsLoading = false;

export const isWindows = computed(() => state.platform === 'windows');
export const isLinux = computed(() => state.platform === 'linux');

export function openSettings(tab: SettingsTab = 'general'): void {
  settingsTab.value = tab;
  settingsOpen.value = true;
}

export function closeSettings(): void {
  settingsOpen.value = false;
}

/** 合并偏好并立即落盘；开关类操作不等待防抖，避免关窗时丢失。 */
export function updatePrefs(patch: Partial<Prefs>): void {
  const prefs = state.prefs;
  if (!prefs) return;
  state.prefs = { ...prefs, ...patch };
  persistPrefs(true);
}

// ---------------- 常规 ----------------

/** 把字体选择写到根元素；`system` 使用应用内置字体栈，其余按系统字体族名应用。 */
export function applyUiFont(fontId: string | undefined = state.prefs?.uiFont): void {
  document.documentElement.style.setProperty('--font-sans', fontStack(fontId));
}

export function setUiFont(fontId: string): void {
  if (!state.prefs || state.prefs.uiFont === fontId) return;
  state.prefs = { ...state.prefs, uiFont: fontId };
  persistPrefs(true);
  applyUiFont(fontId);
}

async function applyDesktopSettings(next: {
  autostart: boolean;
  minimizeToTray: boolean;
}): Promise<boolean> {
  try {
    await invoke(COMMANDS.applyDesktopSettings, next);
    return true;
  } catch (error) {
    toast(`系统集成设置失败：${normalizeError(error)}`, 'error');
    return false;
  }
}

export async function setAutostart(enabled: boolean): Promise<void> {
  const prefs = state.prefs;
  if (!prefs || prefs.autostart === enabled) return;
  const previous = prefs.autostart;
  state.prefs = { ...prefs, autostart: enabled };
  persistPrefs(true);

  const ok = await applyDesktopSettings({
    autostart: enabled,
    minimizeToTray: prefs.minimizeToTray,
  });
  if (!ok) {
    if (state.prefs) state.prefs = { ...state.prefs, autostart: previous };
    persistPrefs(true);
    return;
  }
  toast(enabled ? '已开启开机自动启动' : '已关闭开机自动启动', 'success');
}

export async function setMinimizeToTray(enabled: boolean): Promise<void> {
  const prefs = state.prefs;
  if (!prefs || prefs.minimizeToTray === enabled) return;
  const previous = prefs.minimizeToTray;
  state.prefs = { ...prefs, minimizeToTray: enabled };
  persistPrefs(true);

  const ok = await applyDesktopSettings({
    autostart: prefs.autostart,
    minimizeToTray: enabled,
  });
  if (!ok) {
    if (state.prefs) state.prefs = { ...state.prefs, minimizeToTray: previous };
    persistPrefs(true);
    return;
  }

  // 关闭托盘后「关闭时最小化托盘」已无可用入口，恢复为每次询问。
  if (!enabled && state.prefs?.closeAction === 'tray') {
    updatePrefs({ closeAction: 'ask' });
    toast('已关闭最小化托盘，关闭窗口时将重新询问', 'success');
    return;
  }
  toast(enabled ? '已开启最小化托盘' : '已关闭最小化托盘', 'success');
}

export function setGpuAcceleration(enabled: boolean): void {
  if (!state.prefs || state.prefs.gpuAcceleration === enabled) return;
  updatePrefs({ gpuAcceleration: enabled });
  toast(
    enabled ? '已开启 GPU 加速，重启应用后生效' : '已关闭 GPU 加速，重启应用后生效',
    'info',
  );
}

/** Windows：SMTC / 空间音效 / 音频增强，仅在对应平台读写。 */
export function setSmtc(enabled: boolean): void {
  updatePrefs({ smtc: enabled });
}

export function setSpatialAudio(enabled: boolean): void {
  updatePrefs({ spatialAudio: enabled });
}

export function setAudioEnhancement(enabled: boolean): void {
  updatePrefs({ audioEnhancement: enabled });
}

/** Linux：MPRIS2 只在 Wayland 会话下提供。 */
export function setMpris2(enabled: boolean): void {
  updatePrefs({ mpris2: enabled });
}

// ---------------- 播放 ----------------

export const crossfadeEnabled = computed(() => (state.prefs?.crossfadeSeconds ?? 0) > 0.001);

/** 「开启音乐淡入淡出」：关闭写 0（硬切），开启恢复默认时长。 */
export function setCrossfadeEnabled(enabled: boolean): void {
  const prefs = state.prefs;
  if (!prefs) return;
  const seconds = enabled
    ? prefs.crossfadeSeconds > 0
      ? prefs.crossfadeSeconds
      : DEFAULT_CROSSFADE_SECONDS
    : 0;
  updatePrefs({ crossfadeSeconds: seconds });
}

export function setAutoplayOnStart(enabled: boolean): void {
  updatePrefs({ autoplayOnStart: enabled });
}

export function setPreventSleep(enabled: boolean): void {
  updatePrefs({ preventSleep: enabled });
}

// ---------------- 第三方接入 ----------------

export async function loadApiEndpoints(): Promise<void> {
  if (apiEndpoints.value.length > 0 || endpointsLoading) return;
  endpointsLoading = true;
  try {
    apiEndpoints.value = await invoke<ApiEndpoint[]>(COMMANDS.apiEndpoints);
  } catch (error) {
    toast(`读取开放接口清单失败：${normalizeError(error)}`, 'error');
  } finally {
    endpointsLoading = false;
  }
}

/** 保存 token 与 CORS 配置；成功后接口立即按新配置鉴权与放行来源。 */
export async function saveIntegrationSettings(
  token: string,
  cors: CorsConfig,
): Promise<boolean> {
  try {
    const status = await invoke<ApiStatus>(COMMANDS.updateApiConfig, { token, cors });
    apiStatus.value = status;
    toast('第三方接入设置已保存', 'success');
    return true;
  } catch (error) {
    toast(`保存第三方接入设置失败：${normalizeError(error)}`, 'error');
    return false;
  }
}
