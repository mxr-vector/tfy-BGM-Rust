/**
 * 主窗口侧的「桌面歌词」开关与状态同步。
 *
 * 开关可用性以 `modelsInfo.loaded` 为准（ASR 模型加载成功后才允许开启）；
 * 开启后主窗口把「当前曲目 / 播放状态 / 进度 / 歌词行」推给 Rust，由 Rust 广播到
 * 独立的置顶歌词窗口。歌词行取自「字幕生成」持久化的字幕文件，按曲目 key 缓存。
 */

import { computed, ref, watch } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type {
  LyricsBootstrap,
  LyricsSnapshot,
  StoredSubtitle,
  SubtitleSegment,
} from '@/lib/types';
import { audioState } from './audio-engine';
import { modelsInfo } from './models';
import { state } from './state';

export const desktopLyricsEnabled = ref(false);
export const desktopLyricsBusy = ref(false);

/** 模型加载成功后开关才可用。 */
export const desktopLyricsAvailable = computed(() =>
  Boolean(modelsInfo.value?.loaded),
);

export const desktopLyricsHint = computed(() =>
  desktopLyricsAvailable.value
    ? desktopLyricsEnabled.value
      ? '关闭桌面歌词'
      : '开启桌面歌词（显示当前曲目字幕）'
    : '需先完成 ASR 模型加载，才能开启桌面歌词',
);

interface ControlPayload {
  enabled: boolean;
}

const linesByKey = new Map<string, SubtitleSegment[]>();
let installed = false;
let syncTimer = 0;
/** 上次广播的进度基准，用于把「跳转」和「正常播放推进」区分开。 */
let publishedPosition = 0;
let publishedAt = performance.now();
let publishedRate = 1;
let publishedPlaying = false;

/** 字幕重新生成后失效缓存，下一次同步会重新读取。 */
export function invalidateDesktopLyrics(key: string): void {
  linesByKey.delete(key);
}

async function fetchLines(key: string): Promise<SubtitleSegment[]> {
  const cached = linesByKey.get(key);
  if (cached) return cached;
  try {
    const stored = await invoke<StoredSubtitle | null>(COMMANDS.subtitleForTrack, { key });
    const lines = stored?.segments ?? [];
    linesByKey.set(key, lines);
    return lines;
  } catch {
    return [];
  }
}

async function publish(): Promise<void> {
  if (!desktopLyricsEnabled.value) return;
  const key = state.currentKey;
  // 切歌或缓存失效时重新读取该曲目的字幕；读取期间可能已切到别的曲目，
  // 若仍是当前曲目才继续，避免旧请求的歌词覆盖新曲目的快照。
  if (key && !linesByKey.has(key)) {
    await fetchLines(key);
    if (key !== state.currentKey) return;
  }
  const lines = key ? linesByKey.get(key) ?? [] : [];
  const snapshot: LyricsSnapshot = {
    trackKey: key,
    title: state.currentSong?.title ?? '',
    playing: state.playing,
    position: audioState.currentTime,
    rate: state.prefs?.playbackRate ?? 1,
    lines: lines.map((line) => ({
      start: line.start,
      end: line.end,
      text: line.text,
    })),
    hasLyrics: lines.length > 0,
  };
  publishedPosition = snapshot.position;
  publishedAt = performance.now();
  publishedRate = snapshot.rate;
  publishedPlaying = snapshot.playing;
  try {
    await invoke(COMMANDS.syncDesktopLyrics, { snapshot });
  } catch {
    // 歌词窗口可能正在关闭；同步失败不影响播放，下一秒会重试。
  }
}

/** 开启 / 关闭桌面歌词；未加载模型时禁用并提示。 */
export function toggleDesktopLyrics(): Promise<void> {
  return setEnabled(!desktopLyricsEnabled.value, false);
}

/**
 * 执行开关切换；`silent` 用于启动恢复，避免每次启动都弹「已开启」提示。
 * 开关本身由后端写入偏好，模型下次可用时随启动恢复。
 */
async function setEnabled(target: boolean, silent: boolean): Promise<void> {
  if (desktopLyricsBusy.value) return;
  if (target && !desktopLyricsAvailable.value) {
    if (!silent) toast('需先完成 ASR 模型加载，才能开启桌面歌词', 'error');
    return;
  }
  desktopLyricsBusy.value = true;
  try {
    const control = await invoke<ControlPayload>(COMMANDS.setDesktopLyricsEnabled, {
      enabled: target,
    });
    desktopLyricsEnabled.value = control.enabled;
    if (state.prefs) state.prefs = { ...state.prefs, desktopLyricsEnabled: control.enabled };
    if (control.enabled) {
      await publish();
      if (!silent) toast('桌面歌词已开启', 'success');
    } else if (!silent) {
      toast('桌面歌词已关闭', 'info');
    }
  } catch (cause) {
    toast(`桌面歌词切换失败：${normalizeError(cause)}`, 'error');
  } finally {
    desktopLyricsBusy.value = false;
  }
}

/** 应用启动时安装：恢复后端开关、监听窗口关闭事件、推送播放状态。 */
export async function installDesktopLyrics(): Promise<void> {
  if (installed) return;
  installed = true;

  try {
    const boot = await invoke<LyricsBootstrap>(COMMANDS.desktopLyricsSnapshot);
    desktopLyricsEnabled.value = boot.enabled;
  } catch {
    // 后端状态不可用时不阻塞启动。
  }

  await listen<ControlPayload>(EVENTS.desktopLyricsControl, ({ enabled }) => {
    desktopLyricsEnabled.value = enabled;
    // 后端可能因用户关闭窗口而广播 false；同步本地偏好，
    // 否则之后模型「释放→重新加载」时 watcher 会把刚关掉的窗口又开回来。
    if (state.prefs) state.prefs = { ...state.prefs, desktopLyricsEnabled: enabled };
  });

  // 切歌 / 播放暂停 / 倍速变化立即同步；播放进度由 1 秒定时器校准，歌词窗口本地平滑推进。
  watch(
    () => [state.playing, state.currentKey, state.prefs?.playbackRate],
    () => {
      void publish();
    },
    { immediate: true },
  );
  // 拖动进度条、跳转循环点等会让 currentTime 突然偏离本地推进值；
  // 检测到明显跳变时立即补发快照，避免桌面歌词的已唱词滞后到下一秒。
  watch(
    () => audioState.currentTime,
    (value) => {
      if (!desktopLyricsEnabled.value) return;
      const expected = publishedPlaying
        ? publishedPosition + ((performance.now() - publishedAt) / 1000) * publishedRate
        : publishedPosition;
      if (Math.abs(value - expected) > 0.25) void publish();
    },
    { flush: 'post' },
  );
  syncTimer = window.setInterval(() => {
    void publish();
  }, 1_000);
  window.addEventListener('beforeunload', () => window.clearInterval(syncTimer));

  // 模型可用后恢复上次开启的桌面歌词（启动自动加载完成时同理会触发）；
  // 模型被卸载后开关自动关闭，避免出现「模型已卸载但开关仍显示开启」。
  watch(
    [desktopLyricsAvailable, () => state.prefs?.desktopLyricsEnabled],
    ([available]) => {
      if (!available && desktopLyricsEnabled.value) {
        void setEnabled(false, false);
        return;
      }
      if (
        available &&
        !desktopLyricsEnabled.value &&
        !desktopLyricsBusy.value &&
        state.prefs?.desktopLyricsEnabled
      ) {
        void setEnabled(true, true);
      }
    },
    { immediate: true },
  );
}
