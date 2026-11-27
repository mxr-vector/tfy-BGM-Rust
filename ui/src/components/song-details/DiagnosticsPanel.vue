<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { analyseBuffer, computePeaks, type AudioAnalysis } from '@/lib/audio-analysis';
import { loadAudioIndexInfo } from '@/composables/useAudioIndex';
import { COMMANDS, DEFAULT_ACCENT, DEFAULT_EXPORT_SCENE } from '@/lib/constants';
import { formatTime, withAlpha } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import type { AudioIndexInfo, ExportInfo, ExportPreset } from '@/lib/types';
import {
  audioState,
  currentTime,
  getAnalyserHandle,
  getAudioBuffer,
  seekTo,
} from '@/stores/audio-engine';
import { clearLoopPoint, MIN_LOOP_SPAN, saveLoopPoint } from '@/stores/loops';
import { categoryById, state } from '@/stores/state';
import Icon from '@/components/Icon.vue';

const song = computed(() => state.currentSong);
const category = computed(() => (song.value ? categoryById(song.value.category) : null));
const accent = computed(() => category.value?.color ?? DEFAULT_ACCENT);
const loopPoint = computed(() =>
  song.value ? state.loopPoints[song.value.key] ?? null : null,
);

const analysis = ref<AudioAnalysis | null>(null);
const indexedInfo = ref<AudioIndexInfo | null>(null);
const analysing = ref(false);
const momentary = ref<number | null>(null);
const livePeak = ref<number | null>(null);
const exportPresets = ref<ExportPreset[]>([]);
let exportPresetsLoading = false;

// 诊断分析只读源文件；这里仅展示导出归一化目标，便于对照，不影响任何测量值。
const normalizationEnabled = computed(() => state.prefs?.loudnessNormalization ?? true);
const exportTarget = computed(() => {
  if (!normalizationEnabled.value) return null;
  const scene = state.prefs?.exportScene || DEFAULT_EXPORT_SCENE;
  return (
    exportPresets.value.find((preset) => preset.id === scene) ?? exportPresets.value[0] ?? null
  );
});

const loopStart = ref(0);
const loopEnd = ref(0);
const loopEnabled = ref(false);

const waveCanvas = ref<HTMLCanvasElement | null>(null);
const spectrumCanvas = ref<HTMLCanvasElement | null>(null);

let peaks: Float32Array | null = null;
let frame = 0;
let analysisTimer = 0;
let lastMeter = 0;
let observer: ResizeObserver | null = null;
let spectrumBins = new Uint8Array(0);
let loudSamples = new Float32Array(0);
let rawSamples = new Float32Array(0);

const duration = computed(() => audioState.duration || (song.value?.durationMs ?? 0) / 1000);
const verdict = computed(() => {
  const result = analysis.value;
  if (!result) return { label: '待分析', tone: 'idle' };
  if (result.verdict === 'bad') return { label: '建议处理', tone: 'bad' };
  if (result.verdict === 'warning') return { label: '可用，建议留意', tone: 'warn' };
  return { label: '可直接使用', tone: 'good' };
});

function formatValue(value: number | null | undefined, digits = 1, unit = ''): string {
  return value === null || value === undefined || !Number.isFinite(value)
    ? '—'
    : `${value.toFixed(digits)}${unit}`;
}

function fitCanvas(canvas: HTMLCanvasElement) {
  const rect = canvas.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return null;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const width = Math.round(rect.width * dpr);
  const height = Math.round(rect.height * dpr);
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  const context = canvas.getContext('2d');
  if (!context) return null;
  context.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { context, width: rect.width, height: rect.height };
}

function drawWave(): void {
  const canvas = waveCanvas.value;
  if (!canvas) return;
  const fit = fitCanvas(canvas);
  if (!fit) return;
  const { context, width, height } = fit;
  context.clearRect(0, 0, width, height);

  const buffer = getAudioBuffer();
  let waveform: ArrayLike<number> | null = peaks;
  if (!waveform && buffer) {
    peaks = computePeaks(buffer, Math.max(240, Math.floor(width)));
    waveform = peaks;
  }
  // 音频尚未解码时直接复用索引波形，避免再次读取整首源文件。
  if (!waveform) waveform = indexedInfo.value?.waveform ?? null;

  context.strokeStyle = 'rgba(255, 255, 255, 0.08)';
  context.lineWidth = 1;
  context.beginPath();
  context.moveTo(0, height / 2);
  context.lineTo(width, height / 2);
  context.stroke();

  if (waveform && waveform.length > 0) {
    const middle = height / 2;
    const step = width / waveform.length;
    const gradient = context.createLinearGradient(0, 0, 0, height);
    gradient.addColorStop(0, withAlpha(accent.value, 0.85));
    gradient.addColorStop(0.5, withAlpha(accent.value, 0.55));
    gradient.addColorStop(1, withAlpha(accent.value, 0.85));
    context.fillStyle = gradient;
    for (let index = 0; index < waveform.length; index += 1) {
      const amplitude = Math.min(1, waveform[index] ?? 0) * middle * 0.92;
      context.fillRect(index * step, middle - amplitude, Math.max(1, step * 0.72), amplitude * 2);
    }
  }

  const total = audioState.duration || duration.value;
  if (total > 0) {
    if (loopEnd.value - loopStart.value > 0.05) {
      const startX = (Math.max(0, loopStart.value) / total) * width;
      const endX = (Math.min(total, loopEnd.value) / total) * width;
      context.fillStyle = loopEnabled.value
        ? 'rgba(111, 243, 188, 0.16)'
        : 'rgba(255, 255, 255, 0.06)';
      context.fillRect(startX, 0, Math.max(1, endX - startX), height);
      context.strokeStyle = loopEnabled.value ? '#6ff3bc' : 'rgba(255, 255, 255, 0.35)';
      context.lineWidth = 1.5;
      context.beginPath();
      context.moveTo(startX, 0);
      context.lineTo(startX, height);
      context.moveTo(endX, 0);
      context.lineTo(endX, height);
      context.stroke();
    }

    const playX = (Math.min(total, audioState.currentTime) / total) * width;
    context.strokeStyle = '#ffffff';
    context.lineWidth = 1.5;
    context.beginPath();
    context.moveTo(playX, 0);
    context.lineTo(playX, height);
    context.stroke();
    context.fillStyle = '#ffffff';
    context.beginPath();
    context.arc(playX, 4, 3, 0, Math.PI * 2);
    context.fill();
  }
}

function drawSpectrum(): void {
  const canvas = spectrumCanvas.value;
  if (!canvas) return;
  const fit = fitCanvas(canvas);
  if (!fit) return;
  const { context, width, height } = fit;
  context.clearRect(0, 0, width, height);

  const handle = getAnalyserHandle();
  if (!handle) return;
  if (spectrumBins.length !== handle.analyser.frequencyBinCount) {
    spectrumBins = new Uint8Array(handle.analyser.frequencyBinCount);
  }
  handle.analyser.getByteFrequencyData(spectrumBins);

  const bars = 72;
  const gap = 2;
  const barWidth = Math.max(1, (width - gap * (bars - 1)) / bars);
  const gradient = context.createLinearGradient(0, height, 0, 0);
  gradient.addColorStop(0, withAlpha(accent.value, 0.35));
  gradient.addColorStop(0.65, withAlpha(accent.value, 0.9));
  gradient.addColorStop(1, '#eafff5');
  context.fillStyle = gradient;

  for (let index = 0; index < bars; index += 1) {
    const position = Math.floor((index / bars) ** 1.7 * spectrumBins.length);
    const value = (spectrumBins[position] ?? 0) / 255;
    const barHeight = Math.max(1.5, value * height);
    context.fillRect(index * (barWidth + gap), height - barHeight, barWidth, barHeight);
  }
}

function updateMeters(): void {
  const handle = getAnalyserHandle();
  if (!handle) {
    momentary.value = null;
    livePeak.value = null;
    return;
  }
  const frameSize = handle.loudness[0]?.fftSize ?? 0;
  if (frameSize === 0) {
    momentary.value = null;
    livePeak.value = null;
    return;
  }
  if (loudSamples.length !== frameSize) loudSamples = new Float32Array(frameSize);
  // BS.1770 瞬时窗口为 400ms，且各声道能量相加而不是取平均。
  const windowSamples = Math.min(frameSize, Math.round(handle.context.sampleRate * 0.4));
  const offset = frameSize - windowSamples;
  let energy = 0;
  for (const node of handle.loudness) {
    node.getFloatTimeDomainData(loudSamples);
    for (let index = offset; index < frameSize; index += 1) {
      const sample = loudSamples[index]!;
      energy += sample * sample;
    }
  }
  const mean = energy / windowSamples;
  momentary.value = mean > 0 ? -0.691 + 10 * Math.log10(mean) : null;

  const rawData =
    rawSamples.length === handle.analyser.fftSize
      ? rawSamples
      : (rawSamples = new Float32Array(handle.analyser.fftSize));
  handle.analyser.getFloatTimeDomainData(rawData);
  let peak = 0;
  for (const sample of rawData) peak = Math.max(peak, Math.abs(sample));
  livePeak.value = peak > 0 ? 20 * Math.log10(peak) : null;
}

function loop(): void {
  const tick = (time: number) => {
    drawWave();
    drawSpectrum();
    if (time - lastMeter > 100) {
      lastMeter = time;
      updateMeters();
    }
    frame = requestAnimationFrame(tick);
  };
  frame = requestAnimationFrame(tick);
}

function stopLoop(): void {
  if (frame) cancelAnimationFrame(frame);
  frame = 0;
}

function runAnalysis(): void {
  window.clearTimeout(analysisTimer);
  const current = song.value;
  const buffer = getAudioBuffer();
  // buffer 必须确实是当前曲目解码完成的产物；切歌加载期间它可能还是上一首。
  if (!buffer || !current || audioState.loadedPath !== current.path) {
    analysis.value = null;
    peaks = null;
    analysing.value = false;
    return;
  }
  analysing.value = true;
  // 让面板先完成一帧绘制，再做可能耗时数百毫秒的整曲分析。
  analysisTimer = window.setTimeout(() => {
    if (getAudioBuffer() !== buffer) {
      analysing.value = false;
      return;
    }
    peaks = computePeaks(buffer, 1200);
    analysis.value = analyseBuffer(buffer);
    analysing.value = false;
  }, 30);
}

async function loadExportPresets(): Promise<void> {
  if (exportPresets.value.length > 0 || exportPresetsLoading) return;
  exportPresetsLoading = true;
  try {
    const info = await invoke<ExportInfo>(COMMANDS.exportInfo);
    exportPresets.value = info.presets;
  } catch {
    // 对照信息属于附加内容，读取失败时保持静默。
  } finally {
    exportPresetsLoading = false;
  }
}

function syncLoopDraft(): void {
  const point = loopPoint.value;
  loopStart.value = point?.start ?? 0;
  loopEnd.value = point?.end ?? duration.value;
  loopEnabled.value = point?.enabled ?? false;
}

function markStart(): void {
  loopStart.value = Math.min(
    currentTime(),
    Math.max(0, loopEnd.value - MIN_LOOP_SPAN),
  );
}

function markEnd(): void {
  loopEnd.value = Math.max(currentTime(), loopStart.value + MIN_LOOP_SPAN);
}

function markFullTrack(): void {
  loopStart.value = 0;
  loopEnd.value = duration.value;
  loopEnabled.value = true;
}

async function applyLoop(): Promise<void> {
  if (!song.value) return;
  await saveLoopPoint(song.value, loopStart.value, loopEnd.value, loopEnabled.value);
}

async function removeLoop(): Promise<void> {
  if (!song.value) return;
  await clearLoopPoint(song.value);
  loopStart.value = 0;
  loopEnd.value = duration.value;
  loopEnabled.value = false;
}

function seekFromWave(event: PointerEvent): void {
  const canvas = waveCanvas.value;
  const total = audioState.duration || duration.value;
  if (!canvas || total <= 0) return;
  const rect = canvas.getBoundingClientRect();
  const ratio = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
  seekTo(ratio * total);
}

// 面板只在「歌曲详情 · 诊断」标签激活时挂载，挂载即视为可见：
// 这里一次性建立可视化循环与整曲分析，卸载时统一收尾。
onMounted(async () => {
  syncLoopDraft();
  await nextTick();

  void loadExportPresets();

  if ('ResizeObserver' in window) {
    observer ??= new ResizeObserver(() => {
      drawWave();
      drawSpectrum();
    });
    if (waveCanvas.value) observer.observe(waveCanvas.value);
    if (spectrumCanvas.value) observer.observe(spectrumCanvas.value);
  }
  loop();
  runAnalysis();
});

watch(
  () => state.currentSong?.key,
  () => {
    analysis.value = null;
    peaks = null;
    indexedInfo.value = null;
    const current = state.currentSong;
    if (current) {
      void loadAudioIndexInfo(current).then((info) => {
        if (state.currentSong?.key === current.key) indexedInfo.value = info;
      });
    }
    syncLoopDraft();
    runAnalysis();
  },
  { immediate: true },
);

// 解码完成（而不是时长变化）才意味着可以分析；两首时长相同也要重新分析。
watch(
  () => audioState.loadedPath,
  () => {
    analysis.value = null;
    peaks = null;
    syncLoopDraft();
    runAnalysis();
  },
);

watch(
  () => state.loopPoints,
  () => {
    syncLoopDraft();
  },
  { deep: true },
);

onBeforeUnmount(() => {
  stopLoop();
  window.clearTimeout(analysisTimer);
  observer?.disconnect();
  observer = null;
});
</script>

<template>
  <div class="diag-embedded">
    <section class="diag-panel diag-panel--embedded" role="dialog" aria-label="音频诊断">
      <header class="diag-header">
        <h2 class="diag-title">音频诊断</h2>
        <span class="diag-verdict" :class="`is-${verdict.tone}`">{{ verdict.label }}</span>
      </header>

      <div class="diag-body">
        <div class="diag-visuals">
          <div class="diag-canvas-block">
            <div class="diag-block-head">
              <span>整曲波形</span>
              <span class="diag-block-hint">
                点击波形跳转 · 播放 {{ formatTime(audioState.currentTime) }} /
                {{ formatTime(duration) }}
              </span>
            </div>
            <canvas
              ref="waveCanvas"
              class="diag-canvas diag-wave"
              @pointerdown="seekFromWave"
            ></canvas>
          </div>

          <div class="diag-canvas-block">
            <div class="diag-block-head">
              <span>实时频谱</span>
              <span class="diag-block-hint">AnalyserNode · 72 bands · -90~-10 dBFS 映射</span>
            </div>
            <canvas ref="spectrumCanvas" class="diag-canvas diag-spectrum"></canvas>
          </div>

          <div class="diag-meters">
            <div class="diag-meter">
              <span class="meter-label">瞬时 LUFS</span>
              <span class="meter-value" :class="{ 'is-hot': (momentary ?? -99) > -9 }">
                {{ formatValue(momentary, 1) }}
              </span>
              <span class="meter-hint">K-weighting · 400ms</span>
            </div>
            <div class="diag-meter">
              <span class="meter-label">实时峰值</span>
              <span class="meter-value" :class="{ 'is-hot': (livePeak ?? -99) > -1 }">
                {{ formatValue(livePeak, 1) }}<small> dBFS</small>
              </span>
              <span class="meter-hint">单声道下混 · 采样峰值</span>
            </div>
            <div class="diag-meter">
              <span class="meter-label">最大跳变</span>
              <span class="meter-value">{{ formatValue(analysis?.maxDelta, 3) }}</span>
              <span class="meter-hint">整曲最大相邻采样差</span>
            </div>
          </div>
        </div>

        <aside class="diag-side">
          <div v-if="indexedInfo" class="diag-indexed">
            <strong>音频索引</strong>
            <span>{{ indexedInfo.bpm?.toFixed(1) ?? '—' }} BPM · {{ indexedInfo.musicalKey ?? '—' }}</span>
            <span>真峰值 {{ formatValue(indexedInfo.truePeakDb, 1) }} dBTP</span>
          </div>
          <div v-if="analysing" class="diag-loading">
            <span class="diag-spinner"></span> 正在分析整曲…
          </div>
          <div v-else-if="!analysis" class="diag-empty">
            播放这首曲目后即可分析。音频需要先完成解码。
          </div>
          <template v-else>
            <p class="diag-source-note">
              <span>以下为源文件实测值（归一化前）</span>
              <span v-if="!normalizationEnabled">· 导出仅复制原文件</span>
              <span v-else-if="exportTarget">
                · 导出目标 {{ exportTarget.targetLufs }} LUFS · {{ exportTarget.label }}
              </span>
            </p>
            <div class="diag-stats">
              <div class="diag-stat">
                <span>综合响度</span>
                <strong>{{ formatValue(analysis.integratedLufs ?? indexedInfo?.integratedLufs, 1) }}</strong>
                <small>LUFS · {{ analysis?.integratedLufs != null ? '浏览器实测' : '音频索引' }}</small>
              </div>
              <div class="diag-stat">
                <span>采样峰值</span>
                <strong>{{ formatValue(analysis.peakDb ?? indexedInfo?.peakDb, 1) }}</strong>
                <small>dBFS · 非真峰值</small>
              </div>
              <div class="diag-stat">
                <span>RMS</span>
                <strong>{{ formatValue(analysis.rmsDb, 1) }}</strong>
                <small>dB</small>
              </div>
              <div class="diag-stat">
                <span>相位相关</span>
                <strong>{{ formatValue(analysis.minCorrelation, 2) }}</strong>
                <small>最低 / 均值 {{ formatValue(analysis.averageCorrelation, 2) }}</small>
              </div>
              <div class="diag-stat">
                <span>削波采样</span>
                <strong :class="{ 'is-bad': analysis.clippedSamples > 0 }">
                  {{ analysis.clippedSamples }}
                </strong>
                <small>{{ analysis.clipRuns }} 段连续</small>
              </div>
              <div class="diag-stat">
                <span>瞬态突变</span>
                <strong :class="{ 'is-warn': analysis.pops > 40 }">{{ analysis.pops }}</strong>
                <small>阈值 0.65</small>
              </div>
            </div>

            <div class="diag-issues">
              <h3>可用性结论</h3>
              <p v-if="analysis.issues.length === 0" class="diag-ok">
                未发现明显问题，可以直接用于剪辑。
              </p>
              <ul v-else>
                <li v-for="issue in analysis.issues" :key="issue">
                  <Icon name="alert" /> {{ issue }}
                </li>
              </ul>
              <p v-if="analysis.silence.length > 0" class="diag-silence">
                静音段：
                <span
                  v-for="segment in analysis.silence.slice(0, 3)"
                  :key="segment.start"
                >
                  {{ formatTime(segment.start) }}–{{ formatTime(segment.end) }}
                </span>
                <span v-if="analysis.silence.length > 3"> 等 {{ analysis.silence.length }} 段</span>
              </p>
            </div>
          </template>

          <div class="diag-loop">
            <div class="diag-loop-head">
              <h3>循环区间</h3>
              <span v-if="loopPoint" class="diag-loop-state">
                {{ loopPoint.enabled ? '已启用' : '已关闭' }}
              </span>
            </div>
            <div class="diag-loop-values">
              <label>
                <span>起点</span>
                <input v-model.number="loopStart" type="number" min="0" step="0.01" />
              </label>
              <label>
                <span>终点</span>
                <input v-model.number="loopEnd" type="number" min="0" step="0.01" />
              </label>
            </div>
            <div class="diag-loop-actions">
              <button type="button" @click="markStart">起点 = 当前</button>
              <button type="button" @click="markEnd">终点 = 当前</button>
              <button type="button" @click="markFullTrack">整曲循环</button>
            </div>
            <label class="diag-loop-toggle">
              <input v-model="loopEnabled" type="checkbox" /> 启用无缝循环
            </label>
            <div class="diag-loop-actions">
              <button type="button" class="is-primary" :disabled="!song" @click="applyLoop">
                保存循环点
              </button>
              <button
                v-if="loopPoint"
                type="button"
                class="is-danger"
                @click="removeLoop"
              >
                清除
              </button>
            </div>
          </div>
        </aside>
      </div>
    </section>
  </div>
</template>

<style scoped>
.diag-indexed {
  display: flex;
  flex-wrap: wrap;
  gap: 5px 10px;
  margin-bottom: 10px;
  padding: 9px 10px;
  border: 1px solid rgba(25, 217, 139, 0.14);
  border-radius: 9px;
  color: var(--text-3);
  font-size: 10px;
  background: rgba(25, 217, 139, 0.055);
}
.diag-indexed strong { color: #9df7cf; font-weight: 600; }
</style>