<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue';
import AppSelect from '@/components/AppSelect.vue';
import Icon from '@/components/Icon.vue';
import TtsScriptEditor from './components/TtsScriptEditor.vue';
import { useSoundRecording } from '@/composables/useSoundRecording';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { formatBytes, formatTime, normalizeError } from '@/lib/format';
import { assetUrl, invoke, invokeBinary, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import { suspendPlayback } from '@/stores/audio-engine';
import { openSettings } from '@/stores/settings';
import {
  cancelTtsInstall, cancelTtsJob, installTtsModel, openModelDownloadSource, refreshTtsModels,
  ttsDownloadPercent, ttsDownloadProgress, ttsInstallCancelling, ttsInstalling, ttsModelBusy,
  ttsModels, ttsModelsLoading, ttsPreferredFamily, ttsPreferredModelIds, ttsSynthesizing,
  type TtsFamily, type TtsJob,
} from '@/stores/tts';

interface BuiltinReference { audio: number[]; text: string | null }
type ReferenceMode = 'builtin' | 'custom';

const referenceModeOptions = [
  { value: 'builtin', label: '内置音色' },
  { value: 'custom', label: '麦克风录制' },
];
const RECORDING_PROMPT = '心之所向，素履所往，生如逆旅，一苇以航。';
interface SynthesisProgress { jobId: string; current: number; total: number }

const families = ['CosyVoice3', 'Kokoro', 'ZipVoice'] as const;
const familyDescriptions: Record<TtsFamily, string> = {
  CosyVoice3: '零样本克隆',
  Kokoro: '自然朗读',
  ZipVoice: '声音克隆',
};
// Wait for a fresh catalog after entering the workspace, even if Settings populated the cache.
const cachedCatalog = ttsModels.value;
const catalog = computed(() => ttsModels.value === cachedCatalog ? null : ttsModels.value);
const defaultModel = computed(() => {
  const models = catalog.value ?? [];
  return models.find(model => model.installed && model.recommended)
    ?? models.find(model => model.installed && model.family === 'Kokoro')
    ?? models.find(model => model.installed && model.family === 'ZipVoice')
    ?? models.find(model => model.installed)
    ?? models[0];
});
const family = computed(() => ttsPreferredFamily.value ?? defaultModel.value?.family ?? 'CosyVoice3');
const synthesisProgress = ref<SynthesisProgress | null>(null);
const synthesisCancelling = ref(false);
let synthesisJob: TtsJob | null = null;
let disposed = false;
const scriptEditor = useTemplateRef('scriptEditor');
const voiceSearch = ref('');
const voiceId = ref(3);
const referenceText = ref('');
const referenceAudio = ref<number[] | null>(null);
const referenceMode = ref<ReferenceMode>('builtin');
const builtinVoice = ref('');
const builtinVoices = ref<string[]>([]);
const referenceKind = ref('');
const referencePreviewUrl = ref('');
const generating = ref(false);
const recordingClip = ref(false);
const outputUrl = ref('');
const outputName = ref('');
const outputRate = ref(0);
const outputDuration = ref(0);
const outputFamily = ref<TtsFamily>('CosyVoice3');
const familyModels = computed(() => catalog.value?.filter(model => model.family === family.value) ?? []);
const installedModels = computed(() => familyModels.value.filter(model => model.installed));
const selectedModel = computed(() => {
  const preferredId = ttsPreferredModelIds.value[family.value];
  return installedModels.value.find(model => model.id === preferredId) ?? installedModels.value[0];
});
const modelId = computed(() => selectedModel.value?.id ?? '');
const isReady = computed(() => Boolean(selectedModel.value));
const modelOptions = computed(() => installedModels.value
  .map(model => ({ value: model.id, label: model.recommended ? `${model.name} · 推荐` : model.name })));
const modelEmptyTitle = computed(() => ttsModelsLoading.value
  ? '正在扫描本地模型…'
  : catalog.value === null ? '无法读取模型列表，请刷新后重试' : `${family.value} 模型尚未安装`);
const modelHelp: Record<TtsFamily, string> = {
  CosyVoice3: '首次加载会自动准备本地运行环境。',
  Kokoro: '适合口播与知识讲解，支持多音色低延迟朗读。',
  ZipVoice: '支持以 5–15 秒参考音频及对应文本克隆声音。',
};
const voiceOptions = computed(() => voiceNames.map((name, id) => ({ value: String(id), label: `${name} · ${id < 3 ? '英' : '中'}` })));
const visibleVoices = computed(() => voiceOptions.value.filter(voice => voice.value === String(voiceId.value)
  || voice.label.toLowerCase().includes(voiceSearch.value.trim().toLowerCase())));
const parsingForGeneration = ref(false);
const generationBlocker = computed(() => {
  if (!isReady.value) return '请先安装并选择本地模型';
  if (ttsModelBusy.value) return '模型正在忙，请稍后再试';
  if (captureActive.value || recordingClip.value) return '参考音频正在录制或处理';
  if (!scriptEditor.value?.hasText) return '请在左侧输入或导入合成文本';
  if (family.value === 'ZipVoice' || family.value === 'CosyVoice3') {
    if (!referenceAudio.value) return '请先载入或录制参考音色';
    if (!referenceText.value.trim()) return '请录制参考音色';
  }
  return '';
});
const canGenerate = computed(() => !generationBlocker.value && !parsingForGeneration.value && !generating.value);
const voiceNames = `af_maple af_sol bf_vale zf_001 zf_002 zf_003 zf_004 zf_005 zf_006 zf_007 zf_008 zf_017 zf_018 zf_019 zf_021 zf_022 zf_023 zf_024 zf_026 zf_027 zf_028 zf_032 zf_036 zf_038 zf_039 zf_040 zf_042 zf_043 zf_044 zf_046 zf_047 zf_048 zf_049 zf_051 zf_059 zf_060 zf_067 zf_070 zf_071 zf_072 zf_073 zf_074 zf_075 zf_076 zf_077 zf_078 zf_079 zf_083 zf_084 zf_085 zf_086 zf_087 zf_088 zf_090 zf_092 zf_093 zf_094 zf_099 zm_009 zm_010 zm_011 zm_012 zm_013 zm_014 zm_015 zm_016 zm_020 zm_025 zm_029 zm_030 zm_031 zm_033 zm_034 zm_035 zm_037 zm_041 zm_045 zm_050 zm_052 zm_053 zm_054 zm_055 zm_056 zm_057 zm_058 zm_061 zm_062 zm_063 zm_064 zm_065 zm_066 zm_068 zm_069 zm_080 zm_081 zm_082 zm_089 zm_091 zm_095 zm_096 zm_097 zm_098 zm_100`.split(' ');
const outputPlayer = ref<HTMLAudioElement | null>(null);
const referencePlayer = ref<HTMLAudioElement | null>(null);
let releasePlayback: (() => void) | undefined;
const recording = useSoundRecording((clip) => { void acceptReference(clip, '麦克风录音'); }, 15_000, 5_000);
const captureActive = computed(() => recording.requesting.value || recording.recording.value);
async function refreshBuiltinVoices(): Promise<void> {
  try {
    const voices = await invoke<string[]>(COMMANDS.listTtsBuiltinVoices);
    if (disposed) return;
    builtinVoices.value = voices;
    if (!voices.includes(builtinVoice.value)) setBuiltinVoice(voices[0] ?? '');
  } catch (error) {
    if (!disposed) toast(`无法读取内置参考音色：${normalizeError(error)}`, 'error');
  }
}

function selectModel(value: string): void {
  if (!installedModels.value.some(model => model.id === value)) return;
  ttsPreferredFamily.value = family.value;
  ttsPreferredModelIds.value[family.value] = value;
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

const builtinVoiceOptions = computed(() => builtinVoices.value.map(voice => ({ value: voice, label: voice.slice(0, -4) })));

function resetReference(): void {
  referencePlayer.value?.pause();
  if (referencePreviewUrl.value) URL.revokeObjectURL(referencePreviewUrl.value);
  referencePreviewUrl.value = '';
  referenceAudio.value = null;
  referenceText.value = '';
  referenceKind.value = '';
}

function setReferenceAudio(audio: Uint8Array<ArrayBuffer>, label: string, transcript: string | null = null): void {
  resetReference();
  referenceAudio.value = Array.from(audio);
  referenceText.value = transcript ?? '';
  referenceKind.value = label;
  referencePreviewUrl.value = URL.createObjectURL(new Blob([audio], { type: 'audio/wav' }));
}

async function loadBuiltinReference(): Promise<void> {
  const voice = builtinVoice.value;
  if (!voice || recordingClip.value) return;
  recordingClip.value = true;
  const model = modelId.value;
  try {
    const reference = await invoke<BuiltinReference>(COMMANDS.loadTtsBuiltinReference, { voice });
    if (disposed || family.value === 'Kokoro' || modelId.value !== model
      || referenceMode.value !== 'builtin' || builtinVoice.value !== voice) return;
    setReferenceAudio(Uint8Array.from(reference.audio), voice, reference.text);
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
    if (!await checkClipDuration(bytes.slice(0))) return;
    if (disposed) return;
    const normalized = await invokeBinary<ArrayBuffer>(COMMANDS.normalizeTtsReference, bytes);
    if (disposed || family.value === 'Kokoro' || modelId.value !== model || referenceMode.value !== 'custom') return;
    setReferenceAudio(new Uint8Array(normalized), label, RECORDING_PROMPT);
    toast('参考音频已响度归一化为 16 kHz 单声道', 'success');
  } catch (error) { toast(`参考音频不可用：${normalizeError(error)}`, 'error'); }
  finally { recordingClip.value = false; }
}

async function checkClipDuration(bytes: ArrayBuffer): Promise<boolean> {
  let context: AudioContext | undefined;
  try {
    context = new AudioContext();
    const audio = await context.decodeAudioData(bytes);
    if (audio.duration < 5 || audio.duration > 15) { toast('参考录音必须在 5 到 15 秒之间，请重新录制', 'error'); return false; }
    return true;
  } catch { toast('无法解码参考录音，请检查麦克风后重录', 'error'); return false; }
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

async function generate(): Promise<void> {
  const editor = scriptEditor.value;
  if (!canGenerate.value || !editor) return;
  if (!editor.segments.length) {
    const selectedFamily = family.value;
    const selectedModel = modelId.value;
    const selectedReference = referenceAudio.value;
    parsingForGeneration.value = true;
    try {
      const parsed = await editor.parseText();
      if (!parsed?.length || !editor.segments.length || disposed || generationBlocker.value
        || family.value !== selectedFamily || modelId.value !== selectedModel
        || referenceAudio.value !== selectedReference) return;
    } finally { parsingForGeneration.value = false; }
  }
  const job: TtsJob = { id: crypto.randomUUID(), cancelled: false, finished: false };
  synthesisJob = job;
  generating.value = true;
  ttsSynthesizing.value = true;
  synthesisProgress.value = null;
  outputPlayer.value?.pause();
  const selectedFamily = family.value;
  const request = {
    family: selectedFamily,
    modelId: modelId.value,
    voiceId: voiceId.value,
    segments: editor.segments,
    referenceAudio: selectedFamily !== 'Kokoro' ? referenceAudio.value : null,
    referenceText: selectedFamily !== 'Kokoro' ? referenceText.value.trim() : null,
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
    toast('语音已优化响度，并保留模型原生采样率', 'success');
  } catch (error) {
    if (!disposed && !job.cancelled) toast(`语音合成失败：${normalizeError(error)}`, 'error');
  } finally {
    job.finished = true;
    unlisten?.();
    await refreshTtsModels();
    ttsSynthesizing.value = false;
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

watch([family, modelId], resetReference);
watch([modelId, builtinVoice, referenceMode], ([selectedModel, , mode]) => {
  if ((family.value === 'ZipVoice' || family.value === 'CosyVoice3') && selectedModel && mode === 'builtin' && !referenceAudio.value) {
    void loadBuiltinReference();
  }
});
watch(captureActive, (active) => { if (!active) endCapture(); }, { flush: 'sync' });
onMounted(() => {
  void refreshTtsModels();
  void refreshBuiltinVoices();
});
onBeforeUnmount(() => {
  disposed = true;
  void cancelSynthesis();
  // 模型安装属于全局后台任务，离开页面只停止录音，不中断下载。
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
      <button
        v-for="item in families"
        :key="item"
        type="button"
        :class="{ 'is-active': family === item }"
        :aria-pressed="family === item"
        :disabled="captureActive || recordingClip"
        @click="ttsPreferredFamily = item"
      >
        {{ item }}<span>{{ familyDescriptions[item] }}</span>
      </button>
    </nav>

    <div class="tts-layout">
      <TtsScriptEditor ref="scriptEditor" />

      <div class="tts-side">
        <section class="tts-card tts-voice-card">
          <div class="tts-card-heading"><div><span class="tts-overline">VOICE PALETTE</span><h2>{{ family === 'Kokoro' ? '挑选朗读声音' : family === 'CosyVoice3' ? 'CosyVoice3 零样本克隆' : '选择克隆音色' }}</h2></div><span class="tts-native">模型原生采样率</span></div>
          <div v-if="isReady" class="tts-field">
            <span>本地模型</span>
            <AppSelect :model-value="modelId" :options="modelOptions" :disabled="captureActive || recordingClip" label="选择已安装的 TTS 模型" @update:model-value="selectModel" />
          </div>
          <template v-if="!isReady">
            <div class="tts-model-empty">
              <Icon name="chip" />
              <strong>{{ modelEmptyTitle }}</strong>
              <span>{{ modelHelp[family] }}</span>
            </div>
            <article v-for="model in familyModels" :key="model.id" class="tts-model-row">
              <div>
                <strong :title="model.name">{{ model.name }}</strong>
                <span>{{ formatBytes(model.sizeBytes) }} · {{ model.package }}{{ model.recommended ? ' · 推荐' : '' }}</span>
              </div>
              <div class="tts-model-actions">
                <button type="button" class="tts-action" :title="`访问 ${model.name} 下载源`" @click="openModelDownloadSource(true, model.id)">源</button>
                <button type="button" class="tts-action tts-accent-action" :disabled="ttsModelBusy || ttsModelsLoading" @click="installTtsModel(model, 'workspace')">
                  {{ ttsDownloadProgress?.id === model.id ? `${ttsDownloadPercent}%` : ttsInstalling === model.id ? '准备下载…' : '下载' }}
                </button>
              </div>
            </article>
            <button type="button" class="tts-action" @click="openSettings('models')">前往模型管理</button>
          </template>
          <template v-else>
            <template v-if="family === 'Kokoro'">
              <div class="tts-voice-picker">
                <input v-model="voiceSearch" class="tts-search" type="search" placeholder="搜索音色或语言" aria-label="搜索声音" />
                <AppSelect :model-value="String(voiceId)" :options="visibleVoices" label="选择语音" @update:model-value="voiceId = Number($event)" />
              </div>
            </template>
            <p class="tts-voice-note">{{ family === 'Kokoro' ? 'Kokoro 适合口播、知识解说；自然语调、多语音、低延迟，适合长文本朗读。' : '内置音色附带逐字稿；麦克风录音请朗读下方参考句。' }}</p>
            <template v-if="family === 'ZipVoice' || family === 'CosyVoice3'">
              <div class="tts-field">
                <span>参考来源</span>
                <AppSelect :model-value="referenceMode" :options="referenceModeOptions" :disabled="recordingClip || captureActive" label="参考音色来源" @update:model-value="setReferenceMode" />
              </div>
              <template v-if="referenceMode === 'builtin'">
                <div v-if="builtinVoiceOptions.length" class="tts-reference-actions">
                  <AppSelect :model-value="builtinVoice" :options="builtinVoiceOptions" :disabled="recordingClip" label="选择内置参考音色" @update:model-value="setBuiltinVoice" />
                  <button v-if="!referenceAudio || recordingClip" type="button" class="tts-action" :disabled="recordingClip" @click="loadBuiltinReference">{{ recordingClip ? '正在载入…' : '载入音色' }}</button>
                </div>
                <p v-else class="tts-reference-status">暂无内置参考音色，可切换到麦克风录制。</p>
              </template>
              <template v-else>
                <blockquote class="tts-recording-prompt">{{ RECORDING_PROMPT }}</blockquote>
                <div class="tts-reference-actions">
                  <button type="button" class="tts-action" :disabled="recording.requesting.value || (recording.recording.value && !recording.canStop.value) || recordingClip || generating" @click="recording.recording.value ? recording.stop() : startCapture()">
                    <Icon :name="recording.recording.value ? 'check' : 'wave'" />
                    {{ recording.requesting.value ? '正在请求麦克风…' : recording.recording.value ? `结束录音 · ${formatTime(recording.elapsedMs.value / 1000)}` : recordingClip ? '正在处理参考音频…' : referenceAudio ? '重新录制音色' : '开始录制音色（5–15 秒）' }}
                  </button>
                </div>
                <div v-if="recording.recording.value" class="tts-recording-status" role="status" aria-live="polite">
                  <span class="tts-recording-dot" aria-hidden="true"></span>
                  <div class="tts-recording-copy">
                    <strong>正在录制，请清晰朗读上方参考句</strong>
                    <span>{{ formatTime(recording.elapsedMs.value / 1000) }} / 00:15 · {{ recording.canStop.value ? '可结束录音' : '至少录制 5 秒后可结束' }}</span>
                  <span v-if="recording.elapsedMs.value > 1200 && recording.inputLevel.value < 0.006" class="tts-recording-warning">未检测到明显输入，请检查麦克风</span>
                  <span v-else-if="recording.inputLevel.value > 0.88" class="tts-recording-warning">输入过强，可能产生失真</span>
                  </div>
                  <div class="tts-input-meter" role="meter" aria-label="麦克风输入电平" :aria-valuenow="Math.round(recording.inputLevel.value * 100)" aria-valuemin="0" aria-valuemax="100">
                    <span :style="{ transform: `scaleX(${recording.inputLevel.value})` }"></span>
                  </div>
                </div>
                <p v-else class="tts-reference-status" :title="referenceKind">{{ recordingClip ? '正在处理参考音频…' : referenceAudio ? `${referenceKind} · 已录制` : '请安静环境录制清晰人声，录制满 5 秒后可结束，15 秒自动停止。' }}</p>
              </template>
              <audio v-if="referencePreviewUrl" ref="referencePlayer" class="tts-voice-preview" :src="referencePreviewUrl" :controls="!captureActive" preload="none" aria-label="试听参考音频" @play="pauseCompetingAudio" />
              <p v-if="referenceAudio && referenceMode === 'builtin'" class="tts-reference-status">已载入配套逐字稿</p>
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
          <p>在本机离线合成语音，可试听并导出 WAV。</p>
          <button type="button" class="tts-generate" :disabled="!canGenerate" @click="generate"><Icon :name="generating || parsingForGeneration ? 'refresh' : 'play'"/>{{ generating ? '正在本地合成…' : parsingForGeneration ? '正在解析文本…' : '生成声音' }}</button>
          <p v-if="!generating && !parsingForGeneration && generationBlocker" class="tts-reference-status" aria-live="polite">{{ generationBlocker }}</p>
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
.tts-page :deep(.tts-overline){display:block;color:var(--text-3);font:600 10px/1.6 ui-monospace,Consolas,monospace;letter-spacing:.17em}
.tts-heading h1{font-size:clamp(22px,3vw,30px);font-weight:620;letter-spacing:-.055em}
.tts-heading p{color:var(--text-2);font-size:12px}
.tts-page :deep(.tts-action){display:inline-flex;min-height:36px;align-items:center;justify-content:center;gap:8px;padding:8px 12px;border:1px solid var(--line-strong);border-radius:var(--radius-btn);color:var(--text-2);font-size:12px}
.tts-page :deep(.tts-action:hover:not(:disabled)){color:var(--text);background:var(--hover)}
.tts-page :deep(.tts-action [data-icon]){width:15px;height:15px}
.tts-tabs{display:flex;flex:none;width:max-content;max-width:100%;gap:4px;padding:3px;border:1px solid var(--line);border-radius:10px;background:rgba(0,0,0,.16)}
.tts-tabs button{display:flex;align-items:center;gap:8px;padding:6px 11px;border-radius:7px;color:var(--text-2);font-size:13px}
.tts-tabs button span{font-size:11px;opacity:.68}
.tts-tabs button.is-active{background:var(--accent-soft);color:var(--accent-bright)}
.tts-layout{display:grid;flex:1 0 auto;grid-template-columns:minmax(0,1.08fr) minmax(270px,.92fr);gap:10px;align-items:stretch}
.tts-side{display:flex;min-width:0;flex-direction:column;gap:10px}
.tts-card{min-width:0;padding:12px;border:1px solid var(--line);border-radius:12px;background:rgba(10,24,19,.76)}
.tts-page :deep(.tts-card-heading){display:flex;justify-content:space-between;align-items:center;gap:12px}
.tts-page :deep(.tts-card-heading h2){font-size:15px;font-weight:580}
.tts-page :deep(.tts-card-heading>span:not(.tts-native)){color:var(--text-3);font-size:11px}
.tts-reference-actions{display:flex;flex-wrap:wrap;gap:6px;margin-top:6px}
.tts-recording-prompt{margin:8px 0 4px;padding:10px 12px;border-left:3px solid var(--accent-bright);background:rgba(25,217,139,.07);color:var(--text);font-size:13px;line-height:1.7}
.tts-recording-status{display:grid;grid-template-columns:10px minmax(0,1fr) 88px;align-items:center;gap:10px;margin-top:8px;padding:10px 12px;border:1px solid rgba(248,113,113,.28);border-radius:8px;background:rgba(239,68,68,.1)}
.tts-recording-dot{width:9px;height:9px;border-radius:50%;background:#f87171;box-shadow:0 0 0 4px rgba(248,113,113,.12);animation:tts-recording-pulse 1.2s ease-in-out infinite}
.tts-recording-copy{display:flex;min-width:0;flex-direction:column;gap:3px}
.tts-recording-copy strong{color:#fecaca;font-size:11px}
.tts-recording-copy span{color:var(--text-3);font-size:10px}
.tts-recording-copy .tts-recording-warning{color:#fca5a5}
.tts-input-meter{height:7px;overflow:hidden;border-radius:4px;background:rgba(255,255,255,.12)}
.tts-input-meter span{display:block;width:100%;height:100%;transform:scaleX(0);transform-origin:left center;border-radius:inherit;background:linear-gradient(90deg,#28cf91 0%,#d7d852 75%,#ef6868 100%);transition:transform .08s linear}
@keyframes tts-recording-pulse{50%{opacity:.45;box-shadow:0 0 0 7px rgba(248,113,113,.04)}}
.tts-search::placeholder{color:var(--text-3)}
.tts-reference-status{display:block;margin-top:4px;color:var(--text-3);font-size:11px}
.tts-native,.tts-live-dot{color:var(--accent-bright);font:10px ui-monospace,Consolas,monospace}
.tts-download-status{margin-top:10px;color:var(--accent-bright);font-size:11px}
.tts-model-actions{display:flex;flex:0 0 auto;align-items:center;gap:8px;min-width:0}
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
.tts-voice-note{margin-top:5px;color:var(--text-3);font-size:11px}
.tts-voice-preview{display:block;width:100%;height:32px;margin-top:6px}
.tts-render-card>p{margin:5px 0 7px;color:var(--text-3);font-size:12px}
.tts-generate{display:flex;width:100%;min-height:46px;align-items:center;justify-content:center;gap:9px;border:1px solid rgba(25,217,139,.35);border-radius:10px;background:linear-gradient(110deg,rgba(25,217,139,.18),rgba(25,217,139,.07));color:var(--accent-bright);font-weight:580}
.tts-generate:hover:not(:disabled){background:rgba(25,217,139,.22)}
.tts-generate:disabled{opacity:.4;cursor:not-allowed}
.tts-output{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:center;gap:5px 8px;margin-top:6px}
.tts-output audio{width:100%;height:34px}
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
  .tts-page :deep(.tts-overline),.tts-heading p,.tts-render-card>p:not(.tts-download-status){display:none}
  .tts-layout,.tts-side{gap:8px}
  .tts-card{padding:9px}
  .tts-model-empty{display:block;text-align:left}
  .tts-model-empty :deep([data-icon]),.tts-model-empty span{display:none}
  .tts-model-row{padding-block:4px}
  .tts-page :deep(.tts-action){min-height:32px;padding-block:4px}
  .tts-generate{min-height:36px;margin-top:6px}
}
@media(max-width:880px){.tts-layout{grid-template-columns:1fr}}
@media(max-width:480px){.tts-heading{flex-wrap:wrap}.tts-output{grid-template-columns:minmax(0,1fr)}}
</style>
