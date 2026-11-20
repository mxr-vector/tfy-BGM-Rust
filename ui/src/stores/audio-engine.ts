/**
 * 音频播放引擎：用 Web Audio 解码整首音频，按 AudioBuffer 播放。
 *
 * 相比 `<audio>` 元素，这里有三点不可替代：
 * 1. 采样级精确的循环区间（`loopStart` / `loopEnd`），A/B 循环听不出接缝；
 * 2. 实时 AnalyserNode 供频谱、LUFS 仪表使用；
 * 3. 解码后的 AudioBuffer 可直接做整曲波形、削波与相位分析。
 *
 * 业务状态（当前曲目、队列、错误提示）仍由播放器层维护，本模块只负责发声与时钟。
 */

import { reactive } from 'vue';
import { playerConfig } from '@/lib/config';
import { clamp } from '@/lib/format';
import { assetUrl } from '@/lib/tauri';

export const audioState = reactive({
  duration: 0,
  currentTime: 0,
  bufferedPercent: 0,
  /** 已完成解码的曲目路径；加载中不更新，供 UI 判断 buffer 是否属于当前曲目。 */
  loadedPath: '',
});

export interface LoopRegion {
  start: number;
  end: number;
}

export interface AnalyserHandle {
  context: AudioContext;
  /** 原始信号，用于波形与频谱。 */
  analyser: AnalyserNode;
  /** 经过 K-weighting 后的信号，用于 LUFS 瞬时值。 */
  loudness: AnalyserNode;
}

export type EngineEvent = 'play' | 'pause' | 'ended';

export type LoadOutcome =
  | { status: 'loaded' }
  | { status: 'superseded' }
  | { status: 'failed'; error: unknown };

export interface LoadOptions {
  autoplay?: boolean;
  restoreTime?: number;
  loop?: LoopRegion | null;
  /** 自动播放被 WebView 拦截时回调，供播放器把 UI 状态改回暂停。 */
  onAutoplayBlocked?: () => void;
}

const listeners = new Set<(event: EngineEvent) => void>();

let context: AudioContext | null = null;
let master: GainNode | null = null;
let analyser: AnalyserNode | null = null;
let loudness: AnalyserNode | null = null;
let kShelf: BiquadFilterNode | null = null;
let source: AudioBufferSourceNode | null = null;
let buffer: AudioBuffer | null = null;

let loadedPath = '';
let loadToken = 0;
let sourceToken = 0;
let offset = 0;
let startedAt = 0;
let playing = false;
let loading = false;
let rate = 1;
let volume = 0.85;
let muted = false;
let loopRegion: LoopRegion | null = null;
let autoplayBlocked: (() => void) | null = null;
let clockFrame = 0;

/** 订阅播放状态；返回取消订阅函数。 */
export function subscribeEngine(listener: (event: EngineEvent) => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function emit(event: EngineEvent): void {
  for (const listener of listeners) listener(event);
}

function ensureContext(): AudioContext | null {
  if (context) return context;
  const Constructor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!Constructor) return null;

  context = new Constructor();
  master = context.createGain();
  master.gain.value = muted ? 0 : volume;
  master.connect(context.destination);

  analyser = context.createAnalyser();
  analyser.fftSize = 2048;
  analyser.smoothingTimeConstant = 0.72;
  analyser.connect(master);

  // BS.1770 K-weighting：高架 + 高通，结果只进 loudness analyser。
  kShelf = context.createBiquadFilter();
  kShelf.type = 'highshelf';
  kShelf.frequency.value = 1681.974;
  kShelf.Q.value = 0.707175;
  kShelf.gain.value = 3.9998;

  const highpass = context.createBiquadFilter();
  highpass.type = 'highpass';
  highpass.frequency.value = 38.135;
  highpass.Q.value = 0.500327;

  loudness = context.createAnalyser();
  loudness.fftSize = 2048;
  loudness.smoothingTimeConstant = 0;

  const sink = context.createGain();
  sink.gain.value = 0;
  kShelf.connect(highpass);
  highpass.connect(loudness);
  loudness.connect(sink);
  sink.connect(context.destination);

  return context;
}

export function getAnalyserHandle(): AnalyserHandle | null {
  const ctx = ensureContext();
  if (!ctx || !analyser || !loudness) return null;
  return { context: ctx, analyser, loudness };
}

export function getAudioBuffer(): AudioBuffer | null {
  return buffer;
}

export function hasSourceUrl(): boolean {
  return buffer !== null;
}

/** 是否有曲目正在取数/解码；期间 buffer 为空但加载完成后会自动播放。 */
export function isLoading(): boolean {
  return loading;
}

export function isSourceLoaded(path: string): boolean {
  return loadedPath === path;
}

export function applyAudioSettings(nextVolume: number, nextMuted: boolean, nextRate: number): void {
  volume = nextVolume;
  muted = nextMuted;
  if (context && master) {
    // 用短时间常数过渡，避免拖动音量滑杆时产生拉链噪声。
    master.gain.setTargetAtTime(muted ? 0 : volume, context.currentTime, 0.012);
  }
  if (Math.abs(nextRate - rate) > 1e-6) {
    if (playing && source && context) {
      // 倍速变化会改变位置推进速度，先按旧倍速结算当前进度再改速率。
      const position = currentTime();
      offset = position;
      startedAt = context.currentTime;
    }
    rate = nextRate;
    if (source) source.playbackRate.value = rate;
  }
}

function activeRegion(): LoopRegion | null {
  if (!loopRegion || !buffer) return null;
  const start = clamp(loopRegion.start, 0, buffer.duration);
  const end = clamp(loopRegion.end, 0, buffer.duration);
  return end - start > 0.05 ? { start, end } : null;
}

function sameRegion(a: LoopRegion | null, b: LoopRegion | null): boolean {
  if (!a || !b) return a === b;
  return Math.abs(a.start - b.start) < 1e-3 && Math.abs(a.end - b.end) < 1e-3;
}

export function setLoopRegion(region: LoopRegion | null): void {
  if (sameRegion(loopRegion, region)) return;
  if (!buffer) {
    loopRegion = region;
    return;
  }
  if (playing) {
    // 先按旧区间结算位置，再切换区间；落到新区间外时回到起点。
    let position = currentTime();
    if (region && (position < region.start || position >= region.end)) {
      position = region.start;
    }
    loopRegion = region;
    spawnSource(position);
  } else {
    loopRegion = region;
    offset = clamp(offset, 0, buffer.duration);
    syncClock();
  }
}

export function startPlayback(): void {
  if (!buffer || playing || loading) return;
  const ctx = ensureContext();
  if (!ctx) return;

  const start = () => {
    if (!buffer) return;
    const region = activeRegion();
    if (region && (offset < region.start || offset >= region.end - 0.01)) {
      offset = region.start;
    }
    spawnSource(offset);
    playing = true;
    emit('play');
    startClock();
  };

  if (ctx.state === 'running') {
    start();
    return;
  }
  void ctx
    .resume()
    .then(() => {
      if (ctx.state === 'running') start();
      else {
        autoplayBlocked?.();
        emit('pause');
      }
    })
    .catch(() => {
      autoplayBlocked?.();
      emit('pause');
    });
}

export function pausePlayback(): void {
  if (!playing) return;
  offset = currentTime();
  playing = false;
  stopSource();
  stopClock();
  syncClock();
  emit('pause');
}

export function currentTime(): number {
  if (!playing || !context || !buffer) {
    return clamp(offset, 0, buffer?.duration ?? audioState.duration);
  }
  const elapsed = (context.currentTime - startedAt) * rate;
  let position = offset + elapsed;
  const region = activeRegion();
  if (region) {
    const span = region.end - region.start;
    position = region.start + (((position - region.start) % span) + span) % span;
  } else {
    position = Math.min(position, buffer.duration);
  }
  return Number.isFinite(position) ? position : 0;
}

export function seekTo(seconds: number): void {
  if (!buffer) return;
  const target = clamp(seconds, 0, buffer.duration);
  if (playing) {
    spawnSource(target);
  } else {
    offset = target;
  }
  syncClock();
}

export function rewindToStart(): void {
  const region = activeRegion();
  seekTo(region?.start ?? 0);
}

export function syncClock(): void {
  audioState.duration = buffer?.duration ?? 0;
  audioState.currentTime = playing ? currentTime() : clamp(offset, 0, audioState.duration);
  audioState.bufferedPercent = buffer ? 100 : 0;
}

export async function loadSource(
  path: string,
  { autoplay = true, restoreTime = 0, loop = null, onAutoplayBlocked }: LoadOptions = {},
): Promise<LoadOutcome> {
  const token = (loadToken += 1);
  autoplayBlocked = onAutoplayBlocked ?? null;
  loadedPath = path;

  const ctx = ensureContext();
  if (!ctx) {
    loadedPath = '';
    audioState.loadedPath = '';
    return { status: 'failed', error: new Error('当前环境不支持 Web Audio') };
  }

  // 加载期间禁止 startPlayback：否则会放出尚未被新曲替换的旧 buffer。
  const wasPlaying = playing;
  playing = false;
  loading = true;
  stopSource();
  stopClock();
  // 立即释放旧曲 PCM，避免解码新曲时新旧两份同时驻留。
  buffer = null;
  audioState.duration = 0;
  audioState.currentTime = 0;
  audioState.bufferedPercent = 0;
  if (wasPlaying) emit('pause');

  let blob: Blob;
  try {
    blob = await fetchTrack(path);
  } catch (error) {
    if (token !== loadToken) return { status: 'superseded' };
    loading = false;
    loadedPath = '';
    audioState.loadedPath = '';
    return { status: 'failed', error };
  }
  if (token !== loadToken) return { status: 'superseded' };

  let decoded: AudioBuffer;
  try {
    const bytes = await blob.arrayBuffer();
    decoded = await ctx.decodeAudioData(bytes);
  } catch (error) {
    if (token !== loadToken) return { status: 'superseded' };
    loading = false;
    loadedPath = '';
    audioState.loadedPath = '';
    return { status: 'failed', error };
  }
  if (token !== loadToken) return { status: 'superseded' };

  loading = false;
  buffer = decoded;
  loadedPath = path;
  audioState.loadedPath = path;
  loopRegion = loop;
  offset = clamp(restoreTime, 0, decoded.duration);
  audioState.duration = decoded.duration;
  audioState.bufferedPercent = 100;

  if (autoplay) {
    startPlayback();
  } else {
    syncClock();
  }
  return { status: 'loaded' };
}

/** 停止播放并释放当前曲目；下一次 `loadSource` 会从干净状态开始。 */
export function unloadSource(): void {
  loadToken += 1;
  playing = false;
  loading = false;
  stopSource();
  stopClock();
  buffer = null;
  loadedPath = '';
  loopRegion = null;
  offset = 0;
  audioState.currentTime = 0;
  audioState.duration = 0;
  audioState.bufferedPercent = 0;
  audioState.loadedPath = '';
}

function spawnSource(position: number): void {
  const ctx = context;
  if (!ctx || !buffer || !analyser || !kShelf) return;

  stopSource();
  const node = ctx.createBufferSource();
  node.buffer = buffer;
  node.playbackRate.value = rate;

  const region = activeRegion();
  let startPosition = clamp(position, 0, Math.max(0, buffer.duration - 0.02));
  if (region) {
    startPosition = clamp(startPosition, region.start, Math.max(region.start, region.end - 0.01));
    node.loop = true;
    node.loopStart = region.start;
    node.loopEnd = region.end;
  }

  node.connect(analyser);
  node.connect(kShelf);

  offset = startPosition;
  startedAt = ctx.currentTime;
  const token = (sourceToken += 1);
  node.onended = () => {
    if (token !== sourceToken || !playing) return;
    playing = false;
    offset = buffer?.duration ?? 0;
    stopClock();
    syncClock();
    emit('ended');
  };

  node.start(0, startPosition);
  source = node;
}

function stopSource(): void {
  sourceToken += 1;
  const node = source;
  source = null;
  if (!node) return;
  node.onended = null;
  try {
    node.stop();
  } catch {
    // 已经自然结束时 stop 会抛错，忽略即可。
  }
  node.disconnect();
}

function startClock(): void {
  if (clockFrame) return;
  const tick = () => {
    audioState.currentTime = currentTime();
    audioState.duration = buffer?.duration ?? 0;
    clockFrame = requestAnimationFrame(tick);
  };
  clockFrame = requestAnimationFrame(tick);
}

function stopClock(): void {
  if (!clockFrame) return;
  cancelAnimationFrame(clockFrame);
  clockFrame = 0;
}

async function fetchTrack(path: string): Promise<Blob> {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), playerConfig().loadTimeoutMs);
  try {
    const response = await fetch(assetUrl(path), { signal: controller.signal });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const blob = await response.blob();
    if (blob.size === 0) throw new Error('音频文件为空');
    return blob;
  } finally {
    window.clearTimeout(timer);
  }
}