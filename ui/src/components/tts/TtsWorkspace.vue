<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import AppSelect from '@/components/AppSelect.vue';
import Icon from '@/components/Icon.vue';
import { useSoundRecording } from '@/composables/useSoundRecording';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { assetUrl, invoke, invokeBinary, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import { suspendPlayback } from '@/stores/audio-engine';
import { openSettings } from '@/stores/settings';
import {
  cancelTtsInstall, cancelTtsJob, installTtsModel, openTtsRelease, refreshTtsModels,
  ttsDownloadPercent, ttsDownloadProgress, ttsInstallCancelling, ttsInstalling, ttsModels, ttsModelsLoading,
  type TtsJob,
} from '@/stores/tts';

interface Segment { text: string; startMs: number | null; endMs: number | null }
type TextFormat = 'txt' | 'lrc' | 'srt';
type ReferenceMode = 'builtin' | 'custom';

const formatOptions = [
  { value: 'txt', label: '纯文本' },
  { value: 'lrc', label: 'LRC 时间轴' },
  { value: 'srt', label: 'SRT 时间轴' },
];
const referenceModeOptions = [
  { value: 'builtin', label: '内置音色' },
  { value: 'custom', label: '录制或上传' },
];
interface SynthesisProgress { jobId: string; current: number; total: number }

const families = ['Kokoro', 'ZipVoice'] as const;
type Family = typeof families[number];
const family = ref<Family>('Kokoro');
const synthesisProgress = ref<SynthesisProgress | null>(null);
const synthesisCancelling = ref(false);
let synthesisJob: TtsJob | null = null;
let disposed = false;
let textGeneration = 0;
const segments = ref<Segment[]>([]);
const format = ref<TextFormat>('txt');
const title = ref('未载入文本');
const sourceText = ref('');
const voiceSearch = ref('');
const voiceId = ref(3);
const modelId = ref('');
const referenceText = ref('');
const referenceAudio = ref<number[] | null>(null);
const referenceMode = ref<ReferenceMode>('builtin');
const builtinVoice = ref('keai.wav');
const referenceKind = ref('');
const referencePreviewUrl = ref('');
let initialModelDetection = true;
const generating = ref(false);
const recordingClip = ref(false);
const outputUrl = ref('');
const outputName = ref('');
const outputRate = ref(0);
const outputDuration = ref(0);
const outputFamily = ref<Family>('Kokoro');
const durationLabel = computed(() => `${segments.value.length} 段文本`);
const familyModels = computed(() => ttsModels.value.filter(model => model.family === family.value));
const installedModels = computed(() => familyModels.value.filter(model => model.installed));
const isReady = computed(() => installedModels.value.length > 0);
const modelOptions = computed(() => installedModels.value.map(model => ({ value: model.id, label: model.name })));
const voiceOptions = computed(() => voiceNames.map((name, id) => ({ value: String(id), label: `${name} · ${id < 3 ? '英' : '中'}` })));
const visibleVoices = computed(() => voiceOptions.value.filter(voice => voice.value === String(voiceId.value)
  || voice.label.toLowerCase().includes(voiceSearch.value.trim().toLowerCase())));
const referenceReady = computed(() => referenceAudio.value !== null && referenceText.value.trim().length > 0);
const canGenerate = computed(() => installedModels.value.some(model => model.id === modelId.value)
  && segments.value.length > 0 && !generating.value && !captureActive.value
  && (family.value === 'Kokoro' || (referenceReady.value && !recordingClip.value)));
const voiceNames = `af_maple af_sol bf_vale zf_001 zf_002 zf_003 zf_004 zf_005 zf_006 zf_007 zf_008 zf_017 zf_018 zf_019 zf_021 zf_022 zf_023 zf_024 zf_026 zf_027 zf_028 zf_032 zf_036 zf_038 zf_039 zf_040 zf_042 zf_043 zf_044 zf_046 zf_047 zf_048 zf_049 zf_051 zf_059 zf_060 zf_067 zf_070 zf_071 zf_072 zf_073 zf_074 zf_075 zf_076 zf_077 zf_078 zf_079 zf_083 zf_084 zf_085 zf_086 zf_087 zf_088 zf_090 zf_092 zf_093 zf_094 zf_099 zm_009 zm_010 zm_011 zm_012 zm_013 zm_014 zm_015 zm_016 zm_020 zm_025 zm_029 zm_030 zm_031 zm_033 zm_034 zm_035 zm_037 zm_041 zm_045 zm_050 zm_052 zm_053 zm_054 zm_055 zm_056 zm_057 zm_058 zm_061 zm_062 zm_063 zm_064 zm_065 zm_066 zm_068 zm_069 zm_080 zm_081 zm_082 zm_089 zm_091 zm_095 zm_096 zm_097 zm_098 zm_100`.split(' ');
const outputPlayer = ref<HTMLAudioElement | null>(null);
const referencePlayer = ref<HTMLAudioElement | null>(null);
let releasePlayback: (() => void) | undefined;
const recording = useSoundRecording((clip) => { void acceptReference(clip, '麦克风录音'); }, 10_000);
const captureActive = computed(() => recording.requesting.value || recording.recording.value);
const builtinVoices = [
  { file: 'keai.wav', name: '可爱' },
  { file: 'lulu.wav', name: '露露' },
  { file: 'manbo.wav', name: '曼波' },
  { file: 'qingnian.wav', name: '青年' },
  { file: 'roucheng.wav', name: '柔澄' },
  { file: 'weiketuo.wav', name: '维克托' },
];

function syncModels(): void {
  if (initialModelDetection && ttsModels.value.length) {
    const hasKokoro = ttsModels.value.some(model => model.installed && model.family === 'Kokoro');
    const hasZipVoice = ttsModels.value.some(model => model.installed && model.family === 'ZipVoice');
    if (!hasKokoro && hasZipVoice) family.value = 'ZipVoice';
    initialModelDetection = false;
  }
  selectFamilyModel();
}

function selectFamilyModel(): void {
  const candidate = installedModels.value.find(model => model.id === modelId.value)
    ?? installedModels.value[0];
  const nextModelId = candidate?.id ?? '';
  modelId.value = nextModelId;
}

function setFormat(value: string): void {
  if (value === format.value) return;
  format.value = value as TextFormat;
  invalidateText();
}

function setReferenceMode(value: string): void {
  if (value === referenceMode.value) return;
  resetReference();
  referenceMode.value = value as ReferenceMode;
}

function setBuiltinVoice(value: string): void {
  if (value === builtinVoice.value) return;
  resetReference();
  builtinVoice.value = value;
}

function invalidateText(): number {
  segments.value = [];
  return ++textGeneration;
}

async function importText(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = '';
  if (!file) return;
  const extension = file.name.split('.').pop()?.toLowerCase();
  if (extension !== 'txt' && extension !== 'lrc' && extension !== 'srt') { toast('请选择 .txt、.lrc 或 .srt 文件', 'error'); return; }
  const generation = invalidateText();
  try {
    const text = await file.text();
    if (generation !== textGeneration) return;
    sourceText.value = text;
    format.value = extension;
    const parsed = await invoke<Segment[]>(COMMANDS.parseTtsScript, { text, format: extension });
    if (generation !== textGeneration) return;
    title.value = file.name;
    segments.value = parsed;
    toast(`已载入 ${parsed.length} 条合成文本`, 'success');
  } catch (error) {
    if (generation === textGeneration) toast(`解析文本失败：${normalizeError(error)}`, 'error');
  }
}

async function parseText(): Promise<void> {
  const generation = invalidateText();
  const selectedFormat = format.value;
  try {
    const parsed = await invoke<Segment[]>(COMMANDS.parseTtsScript, { text: sourceText.value, format: selectedFormat });
    if (generation !== textGeneration) return;
    segments.value = parsed;
    title.value = selectedFormat === 'txt' ? '粘贴文本' : `粘贴的 ${selectedFormat.toUpperCase()} 字幕`;
  } catch (error) {
    if (generation === textGeneration) toast(`解析文本失败：${normalizeError(error)}`, 'error');
  }
}

const builtinVoiceOptions = builtinVoices.map(voice => ({ value: voice.file, label: voice.name }));

function resetReference(): void {
  referencePlayer.value?.pause();
  if (referencePreviewUrl.value) URL.revokeObjectURL(referencePreviewUrl.value);
  referencePreviewUrl.value = '';
  referenceAudio.value = null;
  referenceText.value = '';
  referenceKind.value = '';
}

function setReferenceAudio(normalized: ArrayBuffer, label: string): void {
  resetReference();
  referenceAudio.value = Array.from(new Uint8Array(normalized));
  referenceKind.value = label;
  referencePreviewUrl.value = URL.createObjectURL(new Blob([normalized], { type: 'audio/wav' }));
}

async function loadBuiltinReference(): Promise<void> {
  if (recordingClip.value) return;
  recordingClip.value = true;
  const voice = builtinVoice.value;
  const model = modelId.value;
  try {
    const normalized = await invoke<ArrayBuffer>(COMMANDS.loadTtsBuiltinReference, { voice });
    if (disposed || family.value !== 'ZipVoice' || modelId.value !== model) return;
    setReferenceAudio(normalized, `models/voices/${voice}`);
    toast('已载入内置参考音频，请填写该录音中说出的对应文本', 'success');
  } catch (error) { toast(`内置参考音频不可用：${normalizeError(error)}`, 'error'); }
  finally { recordingClip.value = false; }
}

async function acceptReference(blob: Blob, label: string): Promise<void> {
  if (blob.size > 25 * 1024 * 1024) { toast('参考音频文件不能超过 25 MB', 'error'); return; }
  if (recordingClip.value) return;
  recordingClip.value = true;
  const model = modelId.value;
  try {
    const bytes = await blob.arrayBuffer();
    if (label === '麦克风录音' && !await checkClipDuration(bytes.slice(0))) return;
    if (disposed) return;
    const normalized = await invokeBinary<ArrayBuffer>(COMMANDS.normalizeTtsReference, bytes);
    if (disposed || family.value !== 'ZipVoice' || modelId.value !== model) return;
    setReferenceAudio(normalized, label);
    toast('参考音频已响度归一化为 16 kHz 单声道', 'success');
  } catch (error) { toast(`参考音频不可用：${normalizeError(error)}`, 'error'); }
  finally { recordingClip.value = false; }
}

async function checkClipDuration(bytes: ArrayBuffer): Promise<boolean> {
  let context: AudioContext | undefined;
  try {
    context = new AudioContext();
    const audio = await context.decodeAudioData(bytes);
    if (audio.duration < 3 || audio.duration > 10) { toast('参考录音必须在 3 到 10 秒之间，请重新录制', 'error'); return false; }
    return true;
  } catch { toast('无法解码参考录音，请重录或选择音频文件', 'error'); return false; }
  finally { await context?.close().catch(() => undefined); }
}

function endCapture(): void {
  releasePlayback?.();
  releasePlayback = undefined;
}

async function startCapture(): Promise<void> {
  if (captureActive.value || recordingClip.value || generating.value) return;
  outputPlayer.value?.pause();
  referencePlayer.value?.pause();
  releasePlayback = suspendPlayback();
  try { await recording.start(); }
  finally { if (!recording.recording.value) endCapture(); }
}

function pauseCompetingAudio(event: Event): void {
  const player = event.currentTarget as HTMLAudioElement;
  if (captureActive.value || (generating.value && player === outputPlayer.value)) player.pause();
  else if (player === outputPlayer.value) referencePlayer.value?.pause();
  else outputPlayer.value?.pause();
}

async function importReference(event: Event): Promise<void> {
  const file = (event.target as HTMLInputElement).files?.[0];
  if (file) await acceptReference(file, file.name);
  (event.target as HTMLInputElement).value = '';
}

async function generate(): Promise<void> {
  if (!canGenerate.value) return;
  const job: TtsJob = { id: crypto.randomUUID(), cancelled: false, finished: false };
  synthesisJob = job;
  generating.value = true;
  synthesisProgress.value = null;
  outputPlayer.value?.pause();
  const selectedFamily = family.value;
  const request = {
    family: selectedFamily,
    modelId: modelId.value,
    voiceId: voiceId.value,
    segments: segments.value,
    referenceAudio: selectedFamily === 'ZipVoice' ? referenceAudio.value : null,
    referenceText: selectedFamily === 'ZipVoice' ? referenceText.value.trim() : null,
  };
  let unlisten: (() => void) | null = null;
  try {
    unlisten = await listen<SynthesisProgress>(EVENTS.ttsSynthesisProgress, (progress) => {
      if (!disposed && !job.cancelled && !job.finished && progress.jobId === job.id) synthesisProgress.value = progress;
    });
    if (disposed || job.cancelled) return;
    const result = await invoke<{ name: string; path: string; sampleRate: number; duration: number }>(COMMANDS.generateTts, { request, jobId: job.id });
    if (disposed || job.cancelled) return;
    outputName.value = result.name;
    outputRate.value = result.sampleRate;
    outputDuration.value = result.duration;
    outputFamily.value = selectedFamily;
    outputUrl.value = assetUrl(result.path);
    toast('语音合成完成，已保留模型原生采样率', 'success');
  } catch (error) {
    if (!disposed && !job.cancelled) toast(`语音合成失败：${normalizeError(error)}`, 'error');
  } finally {
    job.finished = true;
    unlisten?.();
    synthesisJob = null;
    generating.value = false;
    synthesisCancelling.value = false;
  }
}

async function cancelSynthesis(): Promise<void> {
  const job = synthesisJob;
  if (!job || synthesisCancelling.value) return;
  synthesisCancelling.value = true;
  await cancelTtsJob('synthesis', job, disposed);
  if (synthesisJob === job && !job.cancelled) synthesisCancelling.value = false;
}

async function downloadOutput(): Promise<void> {
  if (!outputUrl.value) return;
  try {
    const saved = await invoke<boolean>(COMMANDS.saveTtsAudio, { name: outputName.value });
    if (saved) toast('已导出 WAV 音频', 'success');
  }
  catch (error) { toast(`导出失败：${normalizeError(error)}`, 'error'); }
}

watch([ttsModels, family], syncModels);
watch([family, modelId], resetReference);
watch([modelId, builtinVoice, referenceMode], ([selectedModel, , mode]) => {
  if (family.value === 'ZipVoice' && selectedModel && mode === 'builtin' && !referenceAudio.value) {
    void loadBuiltinReference();
  }
});
watch(captureActive, (active) => { if (!active) endCapture(); }, { flush: 'sync' });
onMounted(() => {
  syncModels();
  void refreshTtsModels();
});
onBeforeUnmount(() => {
  disposed = true;
  ++textGeneration;
  void cancelSynthesis();
  void cancelTtsInstall('workspace');
  recording.stop();
  endCapture();
  outputPlayer.value?.pause();
  resetReference();
});
</script>

<template>
  <section class="tts-page" aria-label="文生语音">
    <header class="tts-heading">
      <div><span class="tts-overline">VOICE / LOCAL INFERENCE</span><h1>让文字，成为声音。</h1><p>稿件朗读与字幕配音，全程在本地完成。</p></div>
      <button type="button" class="tts-action" @click="refreshTtsModels"><Icon name="refresh" />刷新本地模型</button>
    </header>

    <nav class="tts-tabs" aria-label="TTS 模型">
      <button v-for="item in families" :key="item" type="button" :class="{ 'is-active': family === item }" :disabled="captureActive || recordingClip" @click="initialModelDetection = false; family = item">{{ item }}<span>{{ item === 'Kokoro' ? '自然朗读' : '声音克隆' }}</span></button>
    </nav>

    <div class="tts-layout">
      <section class="tts-card tts-script-card">
        <div class="tts-card-heading"><div><span class="tts-overline">MANUSCRIPT</span><h2>合成文本</h2></div><span class="tts-count">{{ durationLabel }}</span></div>
        <div class="tts-import-actions">
          <label class="tts-action tts-file-label"><Icon name="upload" />导入文本<input type="file" accept=".txt,.lrc,.srt,text/plain" @change="importText"></label>
          <AppSelect :model-value="format" :options="formatOptions" label="文本格式" :min-menu-width="132" @update:model-value="setFormat" />
          <button type="button" class="tts-action" @click="parseText">解析文本</button>
        </div>
        <textarea v-model="sourceText" class="tts-textarea" spellcheck="false" placeholder="粘贴口播稿或字幕内容。纯文本会按行拆分；LRC 与 SRT 将按原时间轴合成。" @input="invalidateText"></textarea>
        <div v-if="segments.length" class="tts-segment-preview"><div class="tts-card-heading"><h3>{{ title }}</h3><span>{{ segments.length }} 条</span></div><div v-for="(segment, index) in segments.slice(0, 4)" :key="index" class="tts-segment-row"><time v-if="segment.startMs !== null">{{ (segment.startMs / 1000).toFixed(2) }} s</time><span>{{ segment.text }}</span></div><span v-if="segments.length > 4" class="tts-more">另有 {{ segments.length - 4 }} 条合成文本</span></div>
      </section>

      <div class="tts-side">
        <section class="tts-card tts-voice-card">
          <div class="tts-card-heading"><div><span class="tts-overline">VOICE PALETTE</span><h2>{{ family === 'Kokoro' ? '挑选朗读声音' : '选择克隆音色' }}</h2></div><span class="tts-native">模型原生采样率</span></div>
          <div v-if="isReady" class="tts-field">
            <span>本地模型</span>
            <AppSelect v-model="modelId" :options="modelOptions" :disabled="captureActive || recordingClip" label="选择已安装的 TTS 模型" />
          </div>
          <template v-if="!isReady">
            <div class="tts-model-empty"><Icon name="chip"/><strong>{{ ttsModelsLoading ? '正在扫描本地模型…' : `${family} 模型尚未安装` }}</strong><span>{{ family === 'Kokoro' ? '适合口播与知识讲解：自然语调、多语音、低延迟。' : '适合零样本声音克隆：提供 3–10 秒参考音频及对应文本。' }}</span></div>
            <article v-for="model in familyModels" :key="model.id" class="tts-model-row"><div><strong>{{ model.name }}</strong><span>{{ (model.sizeBytes / 1e9).toFixed(2) }} GB · {{ model.package }}</span></div><button type="button" class="tts-action tts-accent-action" :disabled="ttsInstalling !== null" @click="installTtsModel(model, 'workspace')">{{ ttsDownloadProgress?.id === model.id ? `${ttsDownloadPercent}%` : ttsInstalling === model.id ? '安装中…' : '下载' }}</button></article>
            <button type="button" class="tts-action" @click="openSettings('models')">前往模型管理</button>
            <button type="button" class="tts-source-link" @click="openTtsRelease">查看 sherpa-onnx 模型发布页</button>
          </template>
          <template v-else>
            <template v-if="family === 'Kokoro'">
              <div class="tts-voice-picker">
                <input v-model="voiceSearch" class="tts-search" type="search" placeholder="搜索音色或语言" aria-label="搜索声音" />
                <AppSelect :model-value="String(voiceId)" :options="visibleVoices" label="选择语音" @update:model-value="voiceId = Number($event)" />
              </div>
            </template>
            <p class="tts-voice-note">{{ family === 'Kokoro' ? 'Kokoro 适合口播、知识解说；自然语调、多语音、低延迟，适合长文本朗读。' : 'ZipVoice 适合零样本声音克隆；必须提供短参考音频及对应的参考文本。' }}</p>
            <template v-if="family === 'ZipVoice'">
              <div class="tts-field">
                <span>参考来源</span>
                <AppSelect :model-value="referenceMode" :options="referenceModeOptions" :disabled="recordingClip || captureActive" label="ZipVoice 参考音色来源" @update:model-value="setReferenceMode" />
              </div>
              <template v-if="referenceMode === 'builtin'">
                <div class="tts-reference-actions">
                  <AppSelect :model-value="builtinVoice" :options="builtinVoiceOptions" :disabled="recordingClip" label="选择内置参考音色" @update:model-value="setBuiltinVoice" />
                  <button v-if="!referenceAudio || recordingClip" type="button" class="tts-action" :disabled="recordingClip" @click="loadBuiltinReference">{{ recordingClip ? '正在载入…' : '载入音色' }}</button>
                </div>
              </template>
              <template v-else>
              <div class="tts-reference-actions"><button v-if="recording.recording.value" type="button" class="tts-action" @click="recording.stop">结束录音</button><button v-else type="button" class="tts-action" :disabled="captureActive || recordingClip || generating" @click="startCapture">{{ recording.requesting.value ? '正在请求麦克风…' : '麦克风录制（3–10 秒）' }}</button><label class="tts-action tts-file-label">上传音频<input type="file" accept="audio/*,.wav,.mp3,.m4a,.ogg,.flac" :disabled="captureActive || recordingClip" @change="importReference" /></label></div>
                <p class="tts-reference-status" :title="referenceKind">{{ recordingClip ? '正在处理参考音频…' : referenceAudio ? referenceKind : '请选择 3–10 秒的清晰人声' }}</p>
              </template>
              <audio v-if="referencePreviewUrl" ref="referencePlayer" class="tts-voice-preview" :src="referencePreviewUrl" :controls="!captureActive" preload="none" aria-label="试听参考音频" @play="pauseCompetingAudio" />
              <textarea v-model="referenceText" class="tts-reference-text" rows="2" placeholder="填写参考音频中说出的文字" aria-label="参考音频的对应文本"></textarea>
            </template>
          </template>
          <div v-if="ttsInstalling" class="tts-download-status">
            <p v-if="ttsDownloadProgress">{{ ttsDownloadProgress.file }} · {{ (ttsDownloadProgress.received / 1e6).toFixed(1) }} / {{ (ttsDownloadProgress.total / 1e6).toFixed(1) }} MB · {{ ttsDownloadPercent }}%</p>
            <p v-else>正在准备模型下载…</p>
            <button type="button" class="tts-action" :disabled="ttsInstallCancelling" @click="cancelTtsInstall()">{{ ttsInstallCancelling ? '正在取消…' : '取消下载' }}</button>
          </div>
        </section>
        <section class="tts-card tts-render-card">
          <div class="tts-card-heading"><div><span class="tts-overline">RENDER / WAV</span><h2>生成声音</h2></div><span class="tts-live-dot">LOCAL ONLY</span></div>
          <p>文本在本机进行离线合成。带时间码的字幕会生成原采样率 WAV 音轨。</p>
          <button type="button" class="tts-generate" :disabled="!canGenerate" @click="generate"><Icon :name="generating ? 'refresh' : 'play'"/>{{ generating ? '正在本地合成…' : segments.length ? `合成 ${segments.length} 段语音` : '载入文本以开始' }}</button>
          <p v-if="generating && synthesisProgress" class="tts-download-status">已合成 {{ synthesisProgress.current }} / {{ synthesisProgress.total }} 段</p>
          <button v-if="generating" type="button" class="tts-action" :disabled="synthesisCancelling" @click="cancelSynthesis">{{ synthesisCancelling ? '正在取消…' : '取消合成' }}</button>
          <div v-if="outputUrl" class="tts-output">
            <span class="tts-output-meta">{{ outputFamily }} · {{ outputRate.toLocaleString() }} Hz · {{ outputDuration.toFixed(1) }} 秒 · {{ outputName }}</span>
            <audio ref="outputPlayer" :src="outputUrl" :controls="!generating && !captureActive" preload="metadata" aria-label="试听合成语音" @play="pauseCompetingAudio" />
            <div><button type="button" class="tts-action tts-accent-action" :disabled="generating" @click="downloadOutput"><Icon name="download"/>导出 WAV</button></div>
          </div>
        </section>
      </div>
    </div>
  </section>
</template>

<style scoped>
.tts-page{flex:1;min-height:0;min-width:0;overflow:auto;display:flex;flex-direction:column;gap:10px;padding:clamp(10px,2vh,24px) clamp(12px,3vw,42px);background:radial-gradient(ellipse at 78% 3%,rgba(25,217,139,.075),transparent 33%),rgba(7,17,14,.68);color:var(--text)}
.tts-heading{display:flex;flex:none;justify-content:space-between;align-items:center;gap:10px}
.tts-overline{display:block;color:var(--text-3);font:600 10px/1.6 ui-monospace,Consolas,monospace;letter-spacing:.17em}
.tts-heading h1{font-size:clamp(22px,3vw,30px);font-weight:620;letter-spacing:-.055em}
.tts-heading p{color:var(--text-2);font-size:12px}
.tts-action{display:inline-flex;min-height:36px;align-items:center;justify-content:center;gap:8px;padding:8px 12px;border:1px solid var(--line-strong);border-radius:var(--radius-btn);color:var(--text-2);font-size:12px}
.tts-action:hover:not(:disabled){color:var(--text);background:var(--hover)}
.tts-action :deep([data-icon]){width:15px;height:15px}
.tts-tabs{display:flex;flex:none;width:max-content;max-width:100%;gap:4px;padding:3px;border:1px solid var(--line);border-radius:10px;background:rgba(0,0,0,.16)}
.tts-tabs button{display:flex;align-items:center;gap:8px;padding:6px 11px;border-radius:7px;color:var(--text-2);font-size:13px}
.tts-tabs button span{font-size:11px;opacity:.68}
.tts-tabs button.is-active{background:var(--accent-soft);color:var(--accent-bright)}
.tts-layout{display:grid;flex:1 0 auto;grid-template-columns:minmax(0,1.08fr) minmax(270px,.92fr);gap:10px;align-items:stretch}
.tts-side{display:flex;min-width:0;flex-direction:column;gap:10px}
.tts-card{min-width:0;padding:12px;border:1px solid var(--line);border-radius:12px;background:rgba(10,24,19,.76)}
.tts-card-heading{display:flex;justify-content:space-between;align-items:center;gap:12px}
.tts-card-heading h2{font-size:15px;font-weight:580}
.tts-card-heading h3{font-size:13px}
.tts-count,.tts-card-heading>span:not(.tts-native){color:var(--text-3);font-size:11px}
.tts-import-actions,.tts-reference-actions{display:flex;flex-wrap:wrap;gap:6px;margin-top:6px}
.tts-file-label{position:relative;overflow:hidden;cursor:pointer}
.tts-file-label input{position:absolute;inset:0;width:100%;height:100%;opacity:0;cursor:pointer}
.tts-textarea,.tts-reference-text{display:block;width:100%;margin-top:6px;padding:8px 10px;border:1px solid var(--line);border-radius:8px;background:rgba(1,8,5,.42);color:var(--text);font:13px/1.45 var(--font-sans);resize:none}
.tts-textarea{flex:1;min-height:110px}
.tts-textarea::placeholder,.tts-reference-text::placeholder,.tts-search::placeholder{color:var(--text-3)}
.tts-segment-preview{max-height:117px;overflow:auto;margin-top:8px;padding-top:6px;border-top:1px solid var(--line)}
.tts-segment-row{display:flex;gap:8px;padding:4px 0;border-bottom:1px solid var(--line);color:var(--text-2);font-size:12px}
.tts-segment-row time{flex:none;color:var(--accent-bright);font:11px ui-monospace,Consolas,monospace}
.tts-more,.tts-reference-status{display:block;margin-top:4px;color:var(--text-3);font-size:11px}
.tts-native,.tts-live-dot{color:var(--accent-bright);font:10px ui-monospace,Consolas,monospace}
.tts-download-status{margin-top:10px;color:var(--accent-bright);font-size:11px}
.tts-source-link{display:flex;align-items:center;gap:6px;margin-top:13px;color:var(--text-3);font-size:11px;text-decoration:none}
.tts-model-empty{display:grid;justify-items:center;gap:4px;margin:7px 0;padding:8px;border:1px dashed var(--line-strong);border-radius:8px;text-align:center}
.tts-model-empty :deep([data-icon]){color:var(--accent-bright)}
.tts-model-empty strong{font-size:13px}
.tts-model-empty span{max-width:360px;color:var(--text-3);font-size:11px}
.tts-model-row{display:flex;justify-content:space-between;align-items:center;gap:12px;padding:12px 0;border-top:1px solid var(--line)}
.tts-model-row>div{min-width:0}
.tts-model-row strong,.tts-model-row span{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tts-model-row strong{font-size:12px}
.tts-model-row span{margin-top:4px;color:var(--text-3);font-size:10px}
.tts-accent-action{border-color:rgba(25,217,139,.3);color:var(--accent-bright)}
.tts-source-link:hover{color:var(--accent-bright)}
.tts-voice-note{margin-top:5px;color:var(--text-3);font-size:11px}
.tts-reference-text{min-height:0}
.tts-voice-preview{display:block;width:100%;height:32px;margin-top:6px}
.tts-render-card>p{margin:5px 0 7px;color:var(--text-3);font-size:12px}
.tts-generate{display:flex;width:100%;min-height:46px;align-items:center;justify-content:center;gap:9px;border:1px solid rgba(25,217,139,.35);border-radius:10px;background:linear-gradient(110deg,rgba(25,217,139,.18),rgba(25,217,139,.07));color:var(--accent-bright);font-weight:580}
.tts-generate:hover:not(:disabled){background:rgba(25,217,139,.22)}
.tts-generate:disabled{opacity:.4;cursor:not-allowed}
.tts-output{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:center;gap:5px 8px;margin-top:6px}
.tts-output audio{width:100%;height:34px}
.tts-script-card{display:flex;flex-direction:column}
.tts-segment-row span{min-width:0;overflow-wrap:anywhere}
.tts-field,.tts-voice-picker{display:grid;align-items:center;gap:6px;margin-top:6px}
.tts-field{grid-template-columns:4.5em minmax(0,1fr);color:var(--text-2);font-size:12px}
.tts-voice-picker{grid-template-columns:minmax(0,1fr) minmax(0,1fr)}
.tts-search{width:100%;min-width:0;min-height:32px;padding:0 9px;border:1px solid var(--line);border-radius:var(--radius-btn);background:rgba(255,255,255,.035);color:var(--text);font-size:12px}
.tts-page :deep(.app-select-trigger){min-height:32px;font-size:12px}
.tts-reference-actions .app-select{flex:1 1 116px}
.tts-reference-status,.tts-output-meta{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tts-output-meta{grid-column:1 / -1;font-size:11px;color:var(--text-3)}
@media(max-height:760px){
  .tts-page{gap:8px;padding-block:8px}
  .tts-heading .tts-overline,.tts-heading p,.tts-card-heading .tts-overline,.tts-render-card>p:not(.tts-download-status){display:none}
  .tts-layout,.tts-side{gap:8px}
  .tts-card{padding:9px}
  .tts-model-empty{display:block;text-align:left}
  .tts-model-empty :deep([data-icon]),.tts-model-empty span{display:none}
  .tts-model-row{padding-block:4px}
  .tts-action{min-height:32px;padding-block:4px}
  .tts-source-link{display:inline-flex;margin:0 0 0 8px}
  .tts-generate{min-height:36px;margin-top:6px}
}
@media(max-width:880px){.tts-layout{grid-template-columns:1fr}.tts-textarea{min-height:130px}}
@media(max-width:480px){.tts-heading{flex-wrap:wrap}.tts-output{grid-template-columns:minmax(0,1fr)}}
</style>
