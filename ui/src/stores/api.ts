/**
 * 开放接口桥接：把 Rust 转发过来的 HTTP 命令交给界面执行，并回传当前播放状态。
 *
 * HTTP 服务在 Rust 侧（`src-tauri/src/api.rs`），播放引擎在 WebView 里，
 * 因此命令走 `api-command` 事件、结果走 `api_reply` 命令，双方用自增 id 对应。
 * 状态每秒推送一次，并在播放 / 切歌 / 音量 / 路由变化时立即推送。
 */

import { ref, watch } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { clamp, normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import type { ApiStatus, OutputDevice, Song } from '@/lib/types';
import { audioState } from './audio-engine';
import {
  applyOutputRoutes,
  describeOutputIssue,
  MAX_OUTPUT_ROUTES,
  outputsState,
  refreshDevices,
  requestDeviceAccess,
  setCrossfadeSeconds,
  setOutputRoute,
} from './outputs';
import {
  nextTrack,
  playSong,
  prevTrack,
  seekTo,
  setVolume,
  togglePlay,
} from './player';
import { state } from './state';
import { currentProgram } from '@/composables/useScheduler';

interface ApiCommand {
  id: number;
  action: string;
  params?: Record<string, unknown>;
}

/** 命令执行失败：`status` 会作为 HTTP 状态码回给外部调用方。 */
class ApiCommandError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = 'ApiCommandError';
  }
}

export const apiStatus = ref<ApiStatus | null>(null);
let installed = false;
let publishTimer = 0;

/** 查询接口监听状态；打开「播放输出」面板或启动时调用。 */
export async function refreshApiInfo(): Promise<ApiStatus | null> {
  try {
    apiStatus.value = await invoke<ApiStatus>(COMMANDS.apiInfo);
  } catch {
    apiStatus.value = null;
  }
  return apiStatus.value;
}

export async function installApiBridge(): Promise<void> {
  if (installed) return;
  installed = true;

  await listen<ApiCommand>(EVENTS.apiCommand, (command) => {
    void handleCommand(command);
  });
  await refreshApiInfo();

  // 状态变化立即推送；播放中的进度由 1 秒定时器补齐。
  watch(
    () => [
      state.playing,
      state.currentKey,
      state.prefs?.volume,
      state.prefs?.muted,
      state.prefs?.crossfadeSeconds,
      (state.prefs?.outputRoutes ?? [])
        .map((route) => `${route.deviceId}:${route.enabled}:${route.volume}:${route.muted}`)
        .join('|'),
    ],
    () => publishState(),
    { immediate: true },
  );

  publishTimer = window.setInterval(publishState, 1_000);
  window.addEventListener('beforeunload', () => window.clearInterval(publishTimer));
  publishState();
}

async function handleCommand(command: ApiCommand): Promise<void> {
  try {
    const data = await runAction(command.action, command.params ?? {});
    await invoke(COMMANDS.apiReply, { id: command.id, ok: true, data }).catch(() => undefined);
  } catch (error) {
    await invoke(COMMANDS.apiReply, {
      id: command.id,
      ok: false,
      error: normalizeError(error),
      // 未预期的异常按 500 返回，显式抛出的 ApiCommandError 携带语义状态。
      code: error instanceof ApiCommandError ? error.status : 500,
    }).catch(() => undefined);
  }
  publishState();
}

async function runAction(
  action: string,
  params: Record<string, unknown>,
): Promise<unknown> {
  switch (action) {
    case 'state':
      return snapshot();

    case 'play': {
      const key = String(params.key ?? '');
      const song = state.library.songs.find((item) => item.key === key);
      if (!song) throw new ApiCommandError(`曲目不存在：${key}`, 404);
      if (currentProgram.value) {
        throw new ApiCommandError(
          `节目单「${currentProgram.value.name}」正在接管播放（${currentProgram.value.start}–${currentProgram.value.end}）`,
          409,
        );
      }
      const queue = queueFromKeys(params.queue) ?? state.library.songs;
      playSong(song, queue, true);
      return snapshot();
    }

    case 'pause':
      if (state.playing) togglePlay();
      return snapshot();

    case 'resume':
      if (!state.playing) togglePlay();
      return snapshot();

    case 'toggle':
      togglePlay();
      return snapshot();

    case 'next':
      nextTrack(true);
      return snapshot();

    case 'previous':
      prevTrack(true);
      return snapshot();

    case 'seek': {
      const seconds = Number(params.seconds);
      if (!Number.isFinite(seconds)) throw new ApiCommandError('seconds 必须是数字', 400);
      seekTo(seconds);
      return snapshot();
    }

    case 'volume': {
      const volume = Number(params.volume);
      if (!Number.isFinite(volume)) throw new ApiCommandError('volume 必须是 0~1 的数字', 400);
      setVolume(clamp(volume, 0, 1));
      return snapshot();
    }

    case 'crossfade': {
      const seconds = Number(params.seconds);
      if (!Number.isFinite(seconds)) throw new ApiCommandError('seconds 必须是数字', 400);
      setCrossfadeSeconds(seconds);
      return snapshot();
    }

    case 'outputs.list':
      return outputSnapshot();

    case 'outputs.refresh':
      await refreshDevices();
      await applyOutputRoutes();
      return outputSnapshot();

    case 'outputs.unlock': {
      const unlocked = await requestDeviceAccess();
      if (!unlocked) {
        throw new ApiCommandError(describeOutputIssue() || '未能读取输出设备列表', 503);
      }
      await applyOutputRoutes();
      return outputSnapshot();
    }

    case 'outputs.set': {
      const deviceId = String(params.deviceId ?? '');
      if (!deviceId) throw new ApiCommandError('缺少 deviceId', 400);
      const patch: { enabled?: boolean; volume?: number; muted?: boolean } = {};
      if (typeof params.enabled === 'boolean') patch.enabled = params.enabled;
      if (typeof params.muted === 'boolean') patch.muted = params.muted;
      if (params.volume !== undefined) {
        const volume = Number(params.volume);
        if (!Number.isFinite(volume)) throw new ApiCommandError('volume 必须是 0~1 的数字', 400);
        patch.volume = clamp(volume, 0, 1);
      }
      if (Object.keys(patch).length === 0) throw new ApiCommandError('没有可更新的字段', 400);
      if (!setOutputRoute(deviceId, patch)) {
        throw new ApiCommandError(`输出设备已达上限（${MAX_OUTPUT_ROUTES} 路）`, 409);
      }
      return outputSnapshot();
    }

    default:
      throw new ApiCommandError(`未知命令：${action}`, 404);
  }
}

function queueFromKeys(value: unknown): Song[] | null {
  if (!Array.isArray(value)) return null;
  const songs = value
    .map((key) => state.library.songs.find((song) => song.key === String(key)))
    .filter((song): song is Song => Boolean(song));
  return songs.length > 0 ? songs : null;
}

function formatOf(fileName: string): string {
  const dot = fileName.lastIndexOf('.');
  // 与 /v1/library 的 format 保持一致：统一小写扩展名。
  return dot >= 0 ? fileName.slice(dot + 1).toLowerCase() : '';
}

/** 播放状态快照；与 README「开放接口」的字段一一对应。 */
export function snapshot(): Record<string, unknown> | null {
  const prefs = state.prefs;
  if (!prefs) return null;
  const song = state.currentSong;
  return {
    ready: true,
    playing: state.playing,
    position: Math.round(audioState.currentTime * 1000) / 1000,
    duration: Math.round(audioState.duration * 1000) / 1000,
    volume: prefs.volume,
    muted: prefs.muted,
    rate: prefs.playbackRate,
    playMode: prefs.playMode,
    crossfadeSeconds: prefs.crossfadeSeconds,
    track: song
      ? {
          key: song.key,
          title: song.title,
          category: song.category,
          artist: song.artist ?? null,
          album: song.album ?? null,
          format: formatOf(song.fileName),
          durationMs: song.durationMs ?? null,
          fileName: song.fileName,
          path: song.path,
        }
      : null,
    queue: { length: state.queue.length, index: state.queueIndex },
    outputs: (prefs.outputRoutes ?? []).map((route) => ({
      deviceId: route.deviceId,
      label: route.label,
      volume: route.volume,
      muted: route.muted,
      enabled: route.enabled,
    })),
    outputDevices: outputsState.devices.map((device: OutputDevice) => ({
      deviceId: device.deviceId,
      label: device.label,
    })),
    library: {
      root: state.library.root,
      count: state.library.songs.length,
    },
  };
}

function outputSnapshot(): Record<string, unknown> {
  return {
    supported: outputsState.supported,
    labelsAvailable: outputsState.labelsAvailable,
    routed: outputsState.routed,
    error: describeOutputIssue() || null,
    devices: outputsState.devices,
    routes: (state.prefs?.outputRoutes ?? []).map((route) => ({ ...route })),
  };
}

function publishState(): void {
  if (!state.prefs) return;
  const data = snapshot();
  if (!data) return;
  void invoke(COMMANDS.apiPublishState, { snapshot: data }).catch(() => undefined);
}
