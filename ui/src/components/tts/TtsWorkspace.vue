<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';
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
interface Voice { id: number; name: string; language: string }
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
const format = ref<'txt' | 'lrc' | 'srt'>('txt');
const title = ref('未载入文本');
const sourceText = ref('');
const voiceSearch = ref('');
const voiceId = ref(3);
const modelId = ref('');
const referenceText = ref('');
const referenceAudio = ref<number[] | null>(null);
const referenceMode = ref<'builtin' | 'custom'>('builtin');
const builtinVoice = ref('keai.wav');
const referenceKind = ref('');
const builtinPreviewUrl = ref('');
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
const voices = computed(() => voiceNames.map((name, id): Voice => ({ id, name, language: id < 3 ? '英' : '中' })));
const visibleVoices = computed(() => voices.value.filter(v => `${v.name} ${v.language}`.toLowerCase().includes(voiceSearch.value.toLowerCase())));
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
  if (modelId.value !== nextModelId) {
    modelId.value = nextModelId;
    referenceAudio.value = null;
    referenceText.value = '';
    releaseBuiltinPreview();
  }
  if (family.value === 'Kokoro' && !voices.value.some(voice => voice.id === voiceId.value)) voiceId.value = 3;
  if (family.value !== 'ZipVoice') referenceAudio.value = null;
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

function releaseBuiltinPreview(): void {
  referencePlayer.value?.pause();
  if (builtinPreviewUrl.value) URL.revokeObjectURL(builtinPreviewUrl.value);
  builtinPreviewUrl.value = '';
}

async function loadBuiltinReference(): Promise<void> {
  if (recordingClip.value) return;
  recordingClip.value = true;
  referenceAudio.value = null;
  try {
    const normalized = await invoke<ArrayBuffer>(COMMANDS.loadTtsBuiltinReference, { voice: builtinVoice.value });
    if (disposed) return;
    referenceAudio.value = Array.from(new Uint8Array(normalized));
    referenceKind.value = `models/voices/${builtinVoice.value}`;
    referenceMode.value = 'builtin';
    referenceText.value = '';
    releaseBuiltinPreview();
    builtinPreviewUrl.value = URL.createObjectURL(new Blob([normalized], { type: 'audio/wav' }));
    toast('已载入内置参考音频，请填写该录音中说出的对应文本', 'success');
  } catch (error) { toast(`内置参考音频不可用：${normalizeError(error)}`, 'error'); }
  finally { recordingClip.value = false; }
}

async function acceptReference(blob: Blob, label: string): Promise<void> {
  if (blob.size > 25 * 1024 * 1024) { toast('参考音频文件不能超过 25 MB', 'error'); return; }
  recordingClip.value = true;
  try {
    const bytes = await blob.arrayBuffer();
    if (label === '麦克风录音' && !await checkClipDuration(bytes.slice(0))) return;
    if (disposed) return;
    const normalized = await invokeBinary<ArrayBuffer>(COMMANDS.normalizeTtsReference, bytes);
    if (disposed) return;
    referenceAudio.value = Array.from(new Uint8Array(normalized));
    referenceKind.value = label;
    referenceMode.value = 'custom';
    referenceText.value = '';
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
watch([modelId, builtinVoice, referenceMode], ([selectedModel, , mode]) => {
  if (family.value === 'ZipVoice' && selectedModel && mode === 'builtin' && !referenceAudio.value && !recordingClip.value) {
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
  releaseBuiltinPreview();
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
        <div class="tts-import-actions"><label class="tts-action tts-file-label"><Icon name="upload" />导入 .txt / .lrc / .srt<input type="file" accept=".txt,.lrc,.srt,text/plain" @change="importText"></label><select v-model="format" aria-label="文本格式" @change="invalidateText"><option value="txt">纯文本</option><option value="lrc">LRC 时间轴</option><option value="srt">SRT 时间轴</option></select><button type="button" class="tts-action" @click="parseText">解析文本</button></div>
        <textarea v-model="sourceText" class="tts-textarea" spellcheck="false" placeholder="粘贴口播稿或字幕内容。纯文本会按行拆分；LRC 与 SRT 将按原时间轴合成。" @input="invalidateText"></textarea>
        <div v-if="segments.length" class="tts-segment-preview"><div class="tts-card-heading"><h3>{{ title }}</h3><span>{{ segments.length }} 条</span></div><div v-for="(segment, index) in segments.slice(0, 4)" :key="index" class="tts-segment-row"><time v-if="segment.startMs !== null">{{ (segment.startMs / 1000).toFixed(2) }} s</time><span>{{ segment.text }}</span></div><span v-if="segments.length > 4" class="tts-more">另有 {{ segments.length - 4 }} 条合成文本</span></div>
      </section>

      <div class="tts-side">
        <section class="tts-card tts-voice-card">
          <div class="tts-card-heading"><div><span class="tts-overline">VOICE PALETTE</span><h2>{{ family === 'Kokoro' ? '挑选朗读声音' : '选择克隆音色' }}</h2></div><span class="tts-native">模型原生采样率</span></div>
          <p v-if="isReady" class="tts-installed-model">本地模型 <select v-model="modelId" :disabled="captureActive || recordingClip" aria-label="选择已安装的 TTS 模型"><option v-for="model in installedModels" :key="model.id" :value="model.id">{{ model.name }}</option></select></p>
          <template v-if="!isReady">
            <div class="tts-model-empty"><Icon name="chip"/><strong>{{ ttsModelsLoading ? '正在扫描本地模型…' : `${family} 模型尚未安装` }}</strong><span>{{ family === 'Kokoro' ? '适合口播与知识讲解：自然语调、多语音、低延迟。' : '适合零样本声音克隆：提供 3–10 秒参考音频及对应文本。' }}</span></div>
            <article v-for="model in familyModels" :key="model.id" class="tts-model-row"><div><strong>{{ model.name }}</strong><span>{{ (model.sizeBytes / 1e9).toFixed(2) }} GB · {{ model.package }}</span></div><button type="button" class="tts-action tts-accent-action" :disabled="ttsInstalling !== null" @click="installTtsModel(model, 'workspace')">{{ ttsDownloadProgress?.id === model.id ? `${ttsDownloadPercent}%` : ttsInstalling === model.id ? '安装中…' : '下载' }}</button></article>
            <button type="button" class="tts-action" @click="openSettings('models')">前往模型管理</button>
            <button type="button" class="tts-source-link" @click="openTtsRelease">查看 sherpa-onnx 模型发布页</button>
          </template>
          <template v-else>
            <template v-if="family === 'Kokoro'">
              <input v-model="voiceSearch" class="tts-search" type="search" placeholder="搜索音色名或语言，例如 zf / 中" aria-label="搜索声音" />
              <select v-model.number="voiceId" class="tts-voice-select" aria-label="选择语音"><option v-for="voice in visibleVoices" :key="voice.id" :value="voice.id">{{ voice.name }}　·　{{ voice.language }}</option></select>
            </template>
            <p class="tts-voice-note">{{ family === 'Kokoro' ? 'Kokoro 适合口播、知识解说；自然语调、多语音、低延迟，适合长文本朗读。' : 'ZipVoice 适合零样本声音克隆；必须提供短参考音频及对应的参考文本。' }}</p>
            <template v-if="family === 'ZipVoice'">
              <label class="tts-reference-mode">参考音色来源<select v-model="referenceMode" :disabled="recordingClip || captureActive" aria-label="ZipVoice 参考音色来源" @change="referenceAudio = null; referenceText = ''; releaseBuiltinPreview()"><option value="builtin">内置音色（models/voices）</option><option value="custom">录制或上传</option></select></label>
              <template v-if="referenceMode === 'builtin'">
                <div class="tts-reference-actions"><select v-model="builtinVoice" :disabled="recordingClip" class="tts-voice-select" aria-label="选择内置参考音色" @change="releaseBuiltinPreview(); referenceAudio = null; referenceText = ''"><option v-for="voice in builtinVoices" :key="voice.file" :value="voice.file">{{ voice.name }}</option></select><audio v-if="builtinPreviewUrl" ref="referencePlayer" class="tts-voice-preview" :src="builtinPreviewUrl" controls preload="none" aria-label="试听内置参考音色" @play="pauseCompetingAudio" /><button type="button" class="tts-action" :disabled="recordingClip" @click="loadBuiltinReference">{{ recordingClip ? '正在处理参考音频…' : referenceAudio ? '重新载入内置音色' : '载入内置音色' }}</button></div>
                <textarea v-model="referenceText" class="tts-reference-text" placeholder="填写所选内置录音中说出的对应文本" aria-label="内置参考音频的对应文本"></textarea>
              </template>
              <template v-else>
              <div class="tts-reference-actions"><button v-if="recording.recording.value" type="button" class="tts-action" @click="recording.stop">结束录音</button><button v-else type="button" class="tts-action" :disabled="captureActive || recordingClip || generating" @click="startCapture">{{ recording.requesting.value ? '正在请求麦克风…' : '麦克风录制（3–10 秒）' }}</button><label class="tts-action tts-file-label">上传音频<input type="file" accept="audio/*,.wav,.mp3,.m4a,.ogg,.flac" :disabled="captureActive || recordingClip" @change="importReference" /></label></div>
              <p class="tts-reference-status">{{ recordingClip ? '正在归一化参考录音…' : referenceAudio ? `${referenceKind} · 已校验 3–10 秒 · 16 kHz 单声道` : '提供清晰人声；文件与录音均需为 3–10 秒' }}</p>
              <textarea v-model="referenceText" class="tts-reference-text" placeholder="准确填写参考音频中说出的文字" aria-label="参考音频的对应文本"></textarea>
              </template>
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
.tts-page{width:100%;height:100%;overflow:auto;padding:42px clamp(24px,5vw,72px) 136px;background:radial-gradient(ellipse at 78% 3%,rgba(25,217,139,.075),transparent 33%),rgba(7,17,14,.68);color:var(--text)}.tts-heading{display:flex;justify-content:space-between;align-items:end;margin-bottom:30px}.tts-overline{display:block;color:var(--text-3);font:600 10px/1.6 ui-monospace,Consolas,monospace;letter-spacing:.17em}.tts-heading h1{margin-top:8px;font-size:clamp(28px,4vw,40px);font-weight:620;letter-spacing:-.065em}.tts-heading p{margin-top:8px;color:var(--text-2)}.tts-action{display:inline-flex;min-height:36px;align-items:center;justify-content:center;gap:8px;padding:8px 12px;border:1px solid var(--line-strong);border-radius:var(--radius-btn);color:var(--text-2);font-size:12px}.tts-action:hover:not(:disabled){color:var(--text);background:var(--hover)}.tts-action :deep([data-icon]){width:15px;height:15px}.tts-tabs{display:flex;width:max-content;max-width:100%;gap:6px;padding:5px;border:1px solid var(--line);border-radius:12px;background:rgba(0,0,0,.16)}.tts-tabs button{display:flex;align-items:center;gap:12px;padding:10px 17px;border-radius:8px;color:var(--text-2);font-size:13px}.tts-tabs button span{font-size:11px;opacity:.68}.tts-tabs button.is-active{background:var(--accent-soft);color:var(--accent-bright)}.tts-layout{display:grid;grid-template-columns:minmax(0,1.08fr) minmax(320px,.92fr);gap:16px;margin-top:18px;align-items:start}.tts-side{display:grid;gap:16px}.tts-card{min-width:0;padding:20px;border:1px solid var(--line);border-radius:14px;background:rgba(10,24,19,.76);box-shadow:0 12px 36px rgba(0,0,0,.1)}.tts-card-heading{display:flex;justify-content:space-between;align-items:center;gap:12px}.tts-card-heading h2{margin-top:3px;font-size:16px;font-weight:580}.tts-card-heading h3{font-size:13px}.tts-count,.tts-card-heading>span:not(.tts-native){color:var(--text-3);font-size:11px}.tts-import-actions,.tts-reference-actions{display:flex;flex-wrap:wrap;gap:8px;margin-top:17px}.tts-import-actions select,.tts-search,.tts-voice-select{min-height:36px;padding:7px 10px;border:1px solid var(--line-strong);border-radius:var(--radius-btn);background:#0b1813;color:var(--text-2);font:inherit;font-size:12px}.tts-file-label{position:relative;overflow:hidden;cursor:pointer}.tts-file-label input{position:absolute;inset:0;width:100%;height:100%;opacity:0;cursor:pointer}.tts-textarea,.tts-reference-text{display:block;width:100%;margin-top:13px;padding:13px;border:1px solid var(--line);border-radius:10px;background:rgba(1,8,5,.42);color:var(--text);font:13px/1.85 var(--font-sans);resize:vertical}.tts-textarea{min-height:290px}.tts-textarea::placeholder,.tts-reference-text::placeholder,.tts-search::placeholder{color:var(--text-3)}.tts-segment-preview{margin-top:20px;padding-top:16px;border-top:1px solid var(--line)}.tts-segment-row{display:flex;gap:13px;padding:11px 0;border-bottom:1px solid var(--line);color:var(--text-2);font-size:12px}.tts-segment-row time{flex:none;color:var(--accent-bright);font:11px ui-monospace,Consolas,monospace}.tts-more,.tts-reference-status{display:block;margin-top:11px;color:var(--text-3);font-size:11px}.tts-native,.tts-live-dot{color:var(--accent-bright);font:10px ui-monospace,Consolas,monospace}.tts-download-status{margin-top:10px;color:var(--accent-bright);font-size:11px}.tts-source-link{border:0;background:none;cursor:pointer}.tts-model-empty{display:grid;justify-items:center;gap:8px;margin:22px 0 15px;padding:23px 16px;border:1px dashed rgba(255,255,255,.12);border-radius:10px;text-align:center}.tts-model-empty :deep([data-icon]){color:var(--accent-bright)}.tts-model-empty strong{font-size:13px}.tts-model-empty span{max-width:360px;color:var(--text-3);font-size:11px}.tts-model-row{display:flex;justify-content:space-between;align-items:center;gap:12px;padding:12px 0;border-top:1px solid var(--line)}.tts-model-row>div{min-width:0}.tts-model-row strong,.tts-model-row span{display:block;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.tts-model-row strong{font-size:12px}.tts-model-row span{margin-top:4px;color:var(--text-3);font-size:10px}.tts-accent-action{border-color:rgba(25,217,139,.3);color:var(--accent-bright)}.tts-source-link{display:flex;align-items:center;gap:6px;margin-top:13px;color:var(--text-3);font-size:11px;text-decoration:none}.tts-source-link:hover{color:var(--accent-bright)}.tts-search,.tts-voice-select{width:100%;margin-top:15px}.tts-voice-select{margin-top:8px}.tts-voice-note{margin-top:11px;color:var(--text-3);font-size:11px;line-height:1.7}.tts-reference-text{min-height:72px;resize:vertical}.tts-voice-preview{width:min(100%,320px);height:34px;accent-color:var(--accent)}.tts-render-card>p{margin:11px 0 15px;color:var(--text-3);font-size:12px;line-height:1.7}.tts-generate{display:flex;width:100%;min-height:46px;align-items:center;justify-content:center;gap:9px;border:1px solid rgba(25,217,139,.35);border-radius:10px;background:linear-gradient(110deg,rgba(25,217,139,.18),rgba(25,217,139,.07));color:var(--accent-bright);font-weight:580}.tts-generate:hover:not(:disabled){background:rgba(25,217,139,.22)}.tts-generate:disabled{opacity:.4;cursor:not-allowed}.tts-output{margin-top:16px}.tts-output audio{width:100%;height:38px}.tts-output>div{display:flex;gap:8px;margin-top:10px}@media(max-width:940px){.tts-layout{grid-template-columns:1fr}.tts-page{padding-inline:24px}}@media(max-width:580px){.tts-heading{align-items:start;gap:12px;flex-direction:column}.tts-page{padding:26px 15px 130px}.tts-import-actions select{flex:1}.tts-card{padding:16px}}
</style>
