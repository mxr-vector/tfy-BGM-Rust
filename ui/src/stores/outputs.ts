/**
 * 多输出设备路由：把引擎的同一路混音复制到多个物理输出设备。
 *
 * Web Audio 的 `AudioContext.destination` 只能指向一个设备，因此这里为每个额外设备
 * 建立独立的 AudioContext，通过 `setSinkId()` 指定设备，再用 `MediaStream` 把主混音
 * 桥接过去。好处是所有设备共享主引擎的渲染时钟，长时间播放不会互相漂移；
 * 每路都有自己的 GainNode，界面与开放接口可以独立调音量、独立静音。
 *
 * 能力探测：`setSinkId` 与 `MediaStreamAudioDestinationNode` 只在 Chromium 系
 * WebView（Windows WebView2）可用，其它平台会自动降级为「仅系统默认输出」。
 */

import { computed, reactive, watch } from 'vue';
import { clamp } from '@/lib/format';
import { toast } from '@/lib/toast';
import type { OutputDevice, OutputRoute, Prefs } from '@/lib/types';
import {
  getAudioContext,
  getMixStream,
  setDefaultOutput,
  subscribeEngine,
} from './audio-engine';
import { persistPrefs } from './prefs';
import { state } from './state';

interface SinkHandle {
  context: AudioContext;
  gain: GainNode;
  source: MediaStreamAudioSourceNode;
}

type SinkCapableContext = AudioContext & {
  setSinkId?: (sinkId: string) => Promise<void>;
};

/** 与 Rust `prefs::MAX_OUTPUT_ROUTES` 保持一致；运行时新增路由也要受它约束。 */
export const MAX_OUTPUT_ROUTES = 8;

export const outputsState = reactive({
  /** 当前环境是否支持把音频送到指定设备。 */
  supported: true,
  /** `enumerateDevices()` 返回的设备标签是否可用（未授权时为空）。 */
  labelsAvailable: false,
  devices: [] as OutputDevice[],
  /** 已经建立的实际输出路数（不含系统默认）。 */
  routed: 0,
  /** 设备枚举 / 授权失败的原因。 */
  deviceError: '',
  /** 建立或切换输出路由失败的原因。 */
  routeError: '',
  busy: false,
});
const sinks = new Map<string, SinkHandle>();
let applyQueue: Promise<void> = Promise.resolve();
let installed = false;

/** 输出设备行的展示模型：设备信息 + 对应路由（未配置时为 null）。 */
export interface OutputRow extends OutputDevice {
  route: OutputRoute | null;
  /** 配置里存在但系统当前没有枚举到（例如耳机已拔出）。 */
  missing: boolean;
}

export const outputRows = computed<OutputRow[]>(() => {
  const routes = state.prefs?.outputRoutes ?? [];
  const rows: OutputRow[] = [];
  const seen = new Set<string>();

  for (const device of outputsState.devices) {
    const route = routes.find((item) => item.deviceId === device.deviceId) ?? null;
    rows.push({ ...device, route, missing: false });
    seen.add(device.deviceId);
  }
  for (const route of routes) {
    if (seen.has(route.deviceId)) continue;
    rows.push({
      deviceId: route.deviceId,
      label: route.label,
      kind: 'audiooutput',
      route,
      missing: true,
    });
  }

  // 系统默认输出固定排在第一行，方便一键回到默认路由。
  rows.sort((a, b) => {
    if (a.deviceId === 'default') return -1;
    if (b.deviceId === 'default') return 1;
    return a.label.localeCompare(b.label, 'zh-Hans-CN');
  });
  return rows;
});

function defaultRoute(): OutputRoute {
  return {
    deviceId: 'default',
    label: '系统默认输出',
    volume: 1,
    muted: false,
    enabled: true,
  };
}

function routesOf(prefs: Prefs | null): OutputRoute[] {
  const routes = prefs?.outputRoutes ?? [];
  return routes.length > 0 ? routes : [defaultRoute()];
}

/** 读取系统输出设备；标签需要媒体权限，未授权时用序号占位。 */
export async function refreshDevices(): Promise<void> {
  const media = navigator.mediaDevices;
  if (!media?.enumerateDevices) {
    outputsState.supported = false;
    outputsState.devices = [
      { deviceId: 'default', label: '系统默认输出', kind: 'audiooutput' },
    ];
    return;
  }

  try {
    const all = await media.enumerateDevices();
    const outputs = all.filter((device) => device.kind === 'audiooutput');
    outputsState.labelsAvailable = outputs.some((device) => device.label.trim().length > 0);

    const devices: OutputDevice[] = [
      { deviceId: 'default', label: '系统默认输出', kind: 'audiooutput' },
    ];
    const seen = new Set(['default']);
    outputs.forEach((device, index) => {
      const id = device.deviceId || 'default';
      if (seen.has(id)) return;
      seen.add(id);
      devices.push({
        deviceId: id,
        label: device.label.trim() || `输出设备 ${index + 1}`,
        kind: 'audiooutput',
      });
    });
    outputsState.devices = devices;
    outputsState.deviceError = '';
  } catch (error) {
    // 枚举失败不代表不支持多输出，保留 supported，只记录原因。
    outputsState.deviceError = `无法枚举输出设备：${error instanceof Error ? error.message : String(error)}`;
  }
}

/**
 * 申请一次音频设备访问，用完后立刻停止音轨。
 *
 * Chromium 在未获得媒体权限时只会返回一个空的默认设备，`setSinkId()` 也就无法
 * 指定到具体硬件。这里不录音，只为解锁 `enumerateDevices()` 的完整列表；
 * 权限会按应用持久化，用户只需授权一次。
 */
export async function requestDeviceAccess(): Promise<boolean> {
  const media = navigator.mediaDevices;
  if (!media?.getUserMedia) {
    outputsState.supported = false;
    return false;
  }
  outputsState.busy = true;
  try {
    const stream = await media.getUserMedia({ audio: true });
    for (const track of stream.getTracks()) track.stop();
    await refreshDevices();
    return outputsState.devices.length > 1 || outputsState.labelsAvailable;
  } catch (error) {
    outputsState.deviceError = `无法读取输出设备列表：${
      error instanceof Error ? error.message : String(error)
    }（需要在系统弹窗中允许音频设备访问）`;
    return false;
  } finally {
    outputsState.busy = false;
  }
}

/** 应用当前偏好：默认输出增益 + 每个启用设备的独立 AudioContext。 */
export function applyOutputRoutes(): Promise<void> {
  // 串行执行，避免快速开关设备时重复创建 AudioContext。
  applyQueue = applyQueue.then(runApply).catch((error) => {
    outputsState.routeError = error instanceof Error ? error.message : String(error);
  });
  return applyQueue;
}

async function runApply(): Promise<void> {
  const prefs = state.prefs;
  if (!prefs) return;

  const routes = routesOf(prefs);
  const fallback = defaultRoute();
  const systemRoute = routes.find((route) => route.deviceId === 'default') ?? fallback;
  setDefaultOutput(
    systemRoute.enabled ? systemRoute.volume : 0,
    systemRoute.muted || !systemRoute.enabled,
  );

  const errors: string[] = [];
  for (const route of routes) {
    if (route.deviceId === 'default') continue;
    if (!route.enabled) {
      closeSink(route.deviceId);
      continue;
    }
    try {
      await ensureSink(route);
    } catch (error) {
      errors.push(`《${route.label}》${error instanceof Error ? error.message : String(error)}`);
      closeSink(route.deviceId);
    }
  }

  // 关闭已经从路由里移除的设备。
  for (const id of [...sinks.keys()]) {
    if (!routes.some((route) => route.deviceId === id && route.enabled)) closeSink(id);
  }

  outputsState.routed = sinks.size;
  outputsState.routeError = errors.join('；');
  // 只有明确的能力缺失才把 supported 降级；该标记不会被一次成功的 apply 重新置真。
  if (errors.some((message) => message.includes('不支持'))) {
    outputsState.supported = false;
  }
}

async function ensureSink(route: OutputRoute): Promise<void> {
  let sink = sinks.get(route.deviceId);
  if (!sink) {
    const context = new AudioContext();
    const setSinkId = (context as SinkCapableContext).setSinkId;
    if (typeof setSinkId !== 'function') {
      await context.close().catch(() => undefined);
      throw new Error('当前运行环境不支持指定音频输出设备');
    }
    try {
      await setSinkId.call(context, route.deviceId);
    } catch (error) {
      await context.close().catch(() => undefined);
      throw new Error(`无法切换到该设备：${error instanceof Error ? error.message : String(error)}`);
    }

    const stream = getMixStream();
    if (!stream) {
      await context.close().catch(() => undefined);
      throw new Error('当前运行环境无法建立混音桥接');
    }

    const source = context.createMediaStreamSource(stream);
    const gain = context.createGain();
    gain.gain.value = route.muted ? 0 : route.volume;
    source.connect(gain);
    gain.connect(context.destination);
    sink = { context, gain, source };
    sinks.set(route.deviceId, sink);
  }

  await sink.context.resume().catch(() => undefined);
  sink.gain.gain.setTargetAtTime(
    route.muted ? 0 : route.volume,
    sink.context.currentTime,
    0.02,
  );
}

function closeSink(deviceId: string): void {
  const sink = sinks.get(deviceId);
  if (!sink) return;
  sinks.delete(deviceId);
  try {
    sink.source.disconnect();
    sink.gain.disconnect();
  } catch {
    // 已断开的节点重复 disconnect 会抛错，忽略即可。
  }
  void sink.context.close().catch(() => undefined);
}

/** 浏览器要求用户手势后才能启动音频，播放入口统一恢复所有输出上下文。 */
export async function resumeOutputs(): Promise<void> {
  const context = getAudioContext();
  await context?.resume().catch(() => undefined);
  for (const sink of sinks.values()) {
    await sink.context.resume().catch(() => undefined);
  }
}

function findRoute(deviceId: string): OutputRoute | undefined {
  return state.prefs?.outputRoutes?.find((route) => route.deviceId === deviceId);
}

/** 更新一路输出（可顺带创建路由）；`persist` 为 false 时只改内存。
 *
 * 返回 `false` 表示已达到路由上限、没有写入任何改动。
 */
export function setOutputRoute(
  deviceId: string,
  patch: Partial<Omit<OutputRoute, 'deviceId'>>,
  immediate = true,
): boolean {
  const prefs = state.prefs;
  if (!prefs) return false;

  const routes = [...(prefs.outputRoutes ?? [])];
  let route = routes.find((item) => item.deviceId === deviceId);
  if (!route) {
    if (routes.length >= MAX_OUTPUT_ROUTES) return false;
    route = {
      deviceId,
      label: deviceLabel(deviceId),
      volume: 1,
      muted: false,
      enabled: true,
    };
    routes.push(route);
  }
  Object.assign(route, patch);
  if (route.volume !== undefined) route.volume = clamp(Number(route.volume) || 0, 0, 1);

  state.prefs = { ...prefs, outputRoutes: routes };
  persistPrefs(immediate);
  void applyOutputRoutes();
  return true;
}

/** 开关一路输出；首次开启时按设备当前标签建立路由。 */
export function toggleOutput(deviceId: string): void {
  const route = findRoute(deviceId);
  setOutputRoute(deviceId, {
    enabled: !(route?.enabled ?? false),
    label: route?.label ?? deviceLabel(deviceId),
  });
}

/** 拖动滑杆时用 `immediate=false`，等 280ms 防抖后再落盘。 */
export function setOutputVolume(deviceId: string, volume: number, immediate = true): void {
  setOutputRoute(deviceId, { volume: clamp(Number(volume) || 0, 0, 1) }, immediate);
}

export function toggleOutputMute(deviceId: string): void {
  const route = findRoute(deviceId);
  setOutputRoute(deviceId, { muted: !(route?.muted ?? false), enabled: true });
}

/** 切歌交叉淡入淡出时长（秒）；0 为硬切。 */
export function setCrossfadeSeconds(seconds: number): void {
  const prefs = state.prefs;
  if (!prefs) return;
  const next = clamp(Number(seconds) || 0, 0, 12);
  if (Math.abs(prefs.crossfadeSeconds - next) < 0.01) return;
  state.prefs = { ...prefs, crossfadeSeconds: next };
  persistPrefs();
}

function deviceLabel(deviceId: string): string {
  return (
    outputsState.devices.find((device) => device.deviceId === deviceId)?.label ??
    (deviceId === 'default' ? '系统默认输出' : '输出设备')
  );
}

/** 启动时安装：监听偏好与设备插拔，并恢复上次启用的输出路由。 */
export function installOutputs(): void {
  if (installed) return;
  installed = true;

  watch(
    () => state.prefs?.outputRoutes,
    () => {
      void applyOutputRoutes();
    },
    { deep: true, immediate: true },
  );

  subscribeEngine((event) => {
    if (event === 'play') void resumeOutputs();
  });

  if (navigator.mediaDevices?.addEventListener) {
    navigator.mediaDevices.addEventListener('devicechange', () => {
      void refreshDevices();
      void applyOutputRoutes();
    });
  }

  void refreshDevices().then(() => applyOutputRoutes());
}

/** 输出路由异常时给出一次可读提示（由界面在打开面板时调用）。 */
export function describeOutputIssue(): string {
  if (!outputsState.supported) {
    return '当前运行环境不支持多输出设备，仅能使用系统默认输出';
  }
  return outputsState.routeError || outputsState.deviceError;
}

export function notifyOutputIssue(): void {
  const message = describeOutputIssue();
  if (message) toast(message, 'error');
}
