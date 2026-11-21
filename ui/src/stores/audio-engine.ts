/**
 * 音频播放引擎：用 Web Audio 解码整首音频，按 AudioBuffer 播放。
 *
 * 相比 `<audio>` 元素，这里有三点不可替代：
 * 1. 采样级精确的循环区间（`loopStart` / `loopEnd`），A/B 循环听不出接缝；
 * 2. 实时 AnalyserNode 供频谱、LUFS 仪表使用；
 * 3. 解码后的 AudioBuffer 可直接做整曲波形、削波与相位分析。
 *
 * 切歌时引擎会同时保留新旧两个声部：旧声部按设定时长淡出，新声部淡入，
 * 因此「下一首」不会出现硬切或静音缝；曲目自然播完切到下一首时，新声部
 * 也按同一时长淡入。播放暂停使用更短的淡出，避免爆音。多输出设备通过
 * `MediaStreamAudioDestinationNode` 把同一路混音桥接到各设备的独立 AudioContext；
 * 每个设备有自己的增益，互不影响。
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
  /** 原始信号（单声道下混），用于波形与频谱。 */
  analyser: AnalyserNode;
  /** 各声道 K-weighting 后的信号，用于按 BS.1770 求和计算 LUFS。 */
  loudness: AnalyserNode[];
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
  /** 与上一首交叉淡入淡出的秒数；`0` 表示硬切。 */
  crossfadeSeconds?: number;
  /** 本曲启动时的淡入秒数（自然播完切下一首时补齐淡入）；`0` 用默认短淡入。 */
  fadeInSeconds?: number;
  /** 自动播放被 WebView 拦截时回调，供播放器把 UI 状态改回暂停。 */
  onAutoplayBlocked?: () => void;
}

interface Voice {
  node: AudioBufferSourceNode;
  gain: GainNode;
  token: number;
}

const listeners = new Set<(event: EngineEvent) => void>();

/** 播放 / 暂停时的短淡入淡出：足够盖住爆音，又不会让按键感觉迟滞。 */
const TRANSPORT_FADE_SECONDS = 0.08;

let context: AudioContext | null = null;
let master: GainNode | null = null;
/** 系统默认输出这一路的独立增益（多设备路由中的 `default`）。 */
let defaultSink: GainNode | null = null;
let defaultSinkVolume = 1;
let defaultSinkMuted = false;
/** 供其它输出设备桥接的混音流；没有多设备需求时不会产生额外输出。 */
let mixDest: MediaStreamAudioDestinationNode | null = null;
let analyser: AnalyserNode | null = null;
let loudness: AnalyserNode[] | null = null;
let kShelf: BiquadFilterNode | null = null;

let source: AudioBufferSourceNode | null = null;
let sourceGain: GainNode | null = null;
let outgoing: Voice | null = null;
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

  // 默认输出与外部设备各有一路增益：主音量是全局的，设备音量只作用于本路。
  defaultSink = context.createGain();
  defaultSink.gain.value = defaultSinkMuted ? 0 : defaultSinkVolume;
  master.connect(defaultSink);
  defaultSink.connect(context.destination);

  // 多设备路由：把混音复制成 MediaStream，由各设备自己的 AudioContext 播放，
  // 这样所有设备共享同一条渲染时钟，不会因多路独立播放而逐渐跑偏。
  if (typeof context.createMediaStreamDestination === 'function') {
    mixDest = context.createMediaStreamDestination();
    master.connect(mixDest);
  }

  analyser = context.createAnalyser();
  analyser.fftSize = 2048;
  analyser.smoothingTimeConstant = 0.72;
  analyser.minDecibels = -90;
  analyser.maxDecibels = -10;
  analyser.connect(master);

  // BS.1770 K-weighting：高架 + 高通。响度按声道能量求和，所以分左右两路采集。
  kShelf = context.createBiquadFilter();
  kShelf.type = 'highshelf';
  kShelf.frequency.value = 1681.974;
  kShelf.Q.value = 0.707175;
  kShelf.gain.value = 3.9998;

  const highpass = context.createBiquadFilter();
  highpass.type = 'highpass';
  highpass.frequency.value = 38.135;
  highpass.Q.value = 0.500327;

  const splitter = context.createChannelSplitter(2);
  loudness = [context.createAnalyser(), context.createAnalyser()];

  const sink = context.createGain();
  sink.gain.value = 0;
  kShelf.connect(highpass);
  highpass.connect(splitter);
  for (const [channel, node] of loudness.entries()) {
    // 400ms 瞬时窗口需要足够的 K 加权样本：32768 点在 44.1k 下约 743ms。
    node.fftSize = 32768;
    node.smoothingTimeConstant = 0;
    splitter.connect(node, channel);
    node.connect(sink);
  }
  sink.connect(context.destination);

  return context;
}

export function getAudioContext(): AudioContext | null {
  return ensureContext();
}

/** 混音流：供多输出设备桥接使用；不支持 MediaStream 的环境返回 `null`。 */
export function getMixStream(): MediaStream | null {
  const ctx = ensureContext();
  if (!ctx || !mixDest) return null;
  return mixDest.stream;
}

/** 系统默认输出这一路的独立音量（0~1）。 */
export function setDefaultOutput(volumeValue: number, isMuted: boolean): void {
  defaultSinkVolume = clamp(Number.isFinite(volumeValue) ? volumeValue : 1, 0, 1);
  defaultSinkMuted = isMuted;
  if (context && defaultSink) {
    defaultSink.gain.setTargetAtTime(
      defaultSinkMuted ? 0 : defaultSinkVolume,
      context.currentTime,
      0.012,
    );
  }
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
    if (outgoing) outgoing.node.playbackRate.value = rate;
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

export function startPlayback(fadeIn: number = TRANSPORT_FADE_SECONDS): void {
  if (!buffer || playing || loading) return;
  const ctx = ensureContext();
  if (!ctx) return;

  const start = () => {
    if (!buffer) return;
    const region = activeRegion();
    if (region && (offset < region.start || offset >= region.end - 0.01)) {
      offset = region.start;
    }
    spawnSource(offset, fadeIn);
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

  const voice = takeVoice();
  if (context && voice) {
    const now = context.currentTime;
    const gain = voice.gain.gain;
    gain.cancelScheduledValues(now);
    gain.setValueAtTime(Math.max(gain.value, 0.0001), now);
    gain.linearRampToValueAtTime(0, now + TRANSPORT_FADE_SECONDS);
    window.setTimeout(() => stopVoice(voice), TRANSPORT_FADE_SECONDS * 1000 + 80);
  }

  // 交叉淡出中的旧声部也要一起收掉，否则按下暂停后它还会继续响到淡出结束。
  if (outgoing && context) {
    const fading = outgoing;
    outgoing = null;
    const now = context.currentTime;
    const gain = fading.gain.gain;
    gain.cancelScheduledValues(now);
    gain.setValueAtTime(Math.max(gain.value, 0.0001), now);
    gain.linearRampToValueAtTime(0, now + TRANSPORT_FADE_SECONDS);
    window.setTimeout(() => stopVoice(fading), TRANSPORT_FADE_SECONDS * 1000 + 80);
  }
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
  {
    autoplay = true,
    restoreTime = 0,
    loop = null,
    crossfadeSeconds = 0,
    fadeInSeconds = 0,
    onAutoplayBlocked,
  }: LoadOptions = {},
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

  // 正在播放时切换曲目：保留旧声部继续发声，等新曲解码完成再做交叉淡出。
  const crossfade = clamp(crossfadeSeconds, 0, 12);
  const keepPlaying = autoplay && playing && source !== null && buffer !== null && crossfade > 0.05;
  const wasPlaying = playing;

  loading = true;
  if (keepPlaying) {
    // 新曲信息已经显示在界面上，进度先归零，避免沿用上一首的时长。
    audioState.duration = 0;
    audioState.currentTime = 0;
    audioState.bufferedPercent = 0;
  } else {
    // 加载期间禁止 startPlayback：否则会放出尚未被新曲替换的旧 buffer。
    playing = false;
    stopSource();
    stopClock();
    buffer = null;
    audioState.duration = 0;
    audioState.currentTime = 0;
    audioState.bufferedPercent = 0;
    if (wasPlaying) emit('pause');
  }

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

  if (keepPlaying && playing) {
    crossfadeTo(decoded, offset, crossfade);
    startClock();
    // 交叉淡入期间播放从未中断，重新广播一次 play，让界面状态与引擎保持一致。
    emit('play');
    return { status: 'loaded' };
  }

  // 未走交叉淡出的路径（关闭淡入淡出 / 原本暂停 / 上一首自然播完）在解码完成后
  // 按调用方的 autoplay 意图播放；交叉淡出路径若中途被暂停则保持暂停。
  if (autoplay && !keepPlaying) {
    startPlayback(fadeInSeconds > 0.05 ? clamp(fadeInSeconds, 0, 12) : TRANSPORT_FADE_SECONDS);
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

/**
 * 生成一个新声部并接到分析节点；`fadeIn` 大于 0 时从静音线性推到满音量。
 * 返回的 token 与 `sourceToken` 对齐，用于丢弃被后续操作取代的 ended 回调。
 */
function spawnSource(position: number, fadeIn = 0): void {
  const ctx = context;
  if (!ctx || !buffer || !analyser || !kShelf) return;

  stopSource();
  const voice = buildVoice(buffer, position, fadeIn);
  if (!voice) return;
  source = voice.node;
  sourceGain = voice.gain;
  sourceToken = voice.token;
}

/** 交叉淡出：旧声部在 `seconds` 内收到 0，新声部同时淡入。 */
function crossfadeTo(nextBuffer: AudioBuffer, position: number, seconds: number): void {
  const ctx = context;
  if (!ctx || !analyser || !kShelf) return;

  // 连续快速切歌时，上一轮正在淡出的声部直接停掉，避免叠出三层声音。
  if (outgoing) {
    stopVoice(outgoing);
    outgoing = null;
  }

  const previous = takeVoice();
  const voice = buildVoice(nextBuffer, position, seconds);
  if (!voice) return;
  source = voice.node;
  sourceGain = voice.gain;
  sourceToken = voice.token;
  buffer = nextBuffer;
  startedAt = ctx.currentTime;
  offset = position;

  if (previous) {
    const now = ctx.currentTime;
    const gain = previous.gain.gain;
    gain.cancelScheduledValues(now);
    gain.setValueAtTime(Math.max(gain.value, 0.0001), now);
    gain.linearRampToValueAtTime(0, now + seconds);
    const fading = previous;
    outgoing = fading;
    window.setTimeout(() => {
      stopVoice(fading);
      if (outgoing === fading) outgoing = null;
    }, seconds * 1000 + 120);
  }
}

function buildVoice(
  targetBuffer: AudioBuffer,
  position: number,
  fadeIn: number,
): Voice | null {
  const ctx = context;
  if (!ctx || !analyser || !kShelf) return null;

  const node = ctx.createBufferSource();
  node.buffer = targetBuffer;
  node.playbackRate.value = rate;

  const region = activeRegionFor(targetBuffer);
  let startPosition = clamp(position, 0, Math.max(0, targetBuffer.duration - 0.02));
  if (region) {
    startPosition = clamp(startPosition, region.start, Math.max(region.start, region.end - 0.01));
    node.loop = true;
    node.loopStart = region.start;
    node.loopEnd = region.end;
  }

  const gain = ctx.createGain();
  const now = ctx.currentTime;
  if (fadeIn > 0.001) {
    gain.gain.setValueAtTime(0, now);
    gain.gain.linearRampToValueAtTime(1, now + fadeIn);
  } else {
    gain.gain.setValueAtTime(1, now);
  }

  node.connect(gain);
  gain.connect(analyser);
  gain.connect(kShelf);

  offset = startPosition;
  startedAt = now;
  const token = (sourceToken += 1);
  node.onended = () => {
    if (token !== sourceToken || !playing) return;
    playing = false;
    offset = targetBuffer.duration;
    stopClock();
    syncClock();
    emit('ended');
  };
  node.start(0, startPosition);
  return { node, gain, token };
}

function activeRegionFor(target: AudioBuffer): LoopRegion | null {
  if (!loopRegion) return null;
  const start = clamp(loopRegion.start, 0, target.duration);
  const end = clamp(loopRegion.end, 0, target.duration);
  return end - start > 0.05 ? { start, end } : null;
}

/** 取出当前声部，同时作废它的 ended 回调。 */
function takeVoice(): Voice | null {
  const node = source;
  const gain = sourceGain;
  source = null;
  sourceGain = null;
  sourceToken += 1;
  if (!node || !gain) return null;
  node.onended = null;
  return { node, gain, token: sourceToken };
}

function stopVoice(voice: Voice): void {
  voice.node.onended = null;
  try {
    voice.node.stop();
  } catch {
    // 已经自然结束时 stop 会抛错，忽略即可。
  }
  voice.node.disconnect();
  voice.gain.disconnect();
}

function stopSource(): void {
  sourceToken += 1;
  const current = source && sourceGain ? { node: source, gain: sourceGain, token: sourceToken } : null;
  source = null;
  sourceGain = null;
  if (current) stopVoice(current);

  if (outgoing) {
    stopVoice(outgoing);
    outgoing = null;
  }
}

function startClock(): void {
  if (clockFrame) return;
  const tick = () => {
    if (!loading) audioState.currentTime = currentTime();
    // 加载新曲期间 buffer 仍是上一首，时长要跟随加载状态归零，避免界面显示旧时长。
    audioState.duration = loading ? 0 : (buffer?.duration ?? 0);
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
