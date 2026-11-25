import { computed, shallowRef, ref } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';

export interface TtsModel {
  id: string;
  name: string;
  family: 'Kokoro' | 'ZipVoice';
  package: string;
  sizeBytes: number;
  installed: boolean;
  path: string;
}
interface TtsDownloadProgress { jobId: string; id: string; file: string; received: number; total: number }
export interface TtsJob { id: string; cancelled: boolean; finished: boolean }

export const ttsModels = ref<TtsModel[]>([]);
export const ttsModelsLoading = ref(false);
export const ttsDownloadProgress = ref<TtsDownloadProgress | null>(null);
export const ttsDownloadPercent = computed(() => ttsDownloadProgress.value && ttsDownloadProgress.value.total > 0
  ? Math.round(ttsDownloadProgress.value.received / ttsDownloadProgress.value.total * 100) : 0);
const activeInstall = shallowRef<{ job: TtsJob; modelId: string; owner: 'settings' | 'workspace' } | null>(null);
export const ttsInstalling = computed(() => activeInstall.value?.modelId ?? null);
export const ttsInstallCancelling = ref(false);
let refreshGeneration = 0;

export async function cancelTtsJob(kind: 'install' | 'synthesis', job: TtsJob, quiet = false): Promise<void> {
  if (job.cancelled || job.finished) return;
  job.cancelled = true;
  try {
    await invoke(COMMANDS.cancelTtsJob, { kind, jobId: job.id });
  } catch (error) {
    job.cancelled = false;
    if (!quiet && !job.finished) toast(`无法取消语音任务：${normalizeError(error)}`, 'error');
  }
}

export async function openTtsRelease(): Promise<void> {
  try { await invoke(COMMANDS.openTtsRelease); }
  catch (error) { toast(`无法打开 sherpa-onnx 模型发布页：${normalizeError(error)}`, 'error'); }
}

export async function refreshTtsModels(): Promise<void> {
  const generation = ++refreshGeneration;
  ttsModelsLoading.value = true;
  try {
    const models = await invoke<TtsModel[]>(COMMANDS.ttsModels);
    if (generation === refreshGeneration) ttsModels.value = models;
  } catch (error) {
    if (generation === refreshGeneration) toast(`读取 TTS 模型失败：${normalizeError(error)}`, 'error');
  } finally {
    if (generation === refreshGeneration) ttsModelsLoading.value = false;
  }
}

export async function cancelTtsInstall(owner?: 'settings' | 'workspace'): Promise<void> {
  const task = activeInstall.value;
  if (!task || (owner && owner !== task.owner) || task.job.finished || ttsInstallCancelling.value) return;
  ttsInstallCancelling.value = true;
  await cancelTtsJob('install', task.job, owner !== undefined);
  if (activeInstall.value === task && !task.job.cancelled) ttsInstallCancelling.value = false;
}

export async function installTtsModel(model: TtsModel, owner: 'settings' | 'workspace'): Promise<void> {
  if (activeInstall.value || model.installed) return;
  const job: TtsJob = { id: crypto.randomUUID(), cancelled: false, finished: false };
  activeInstall.value = { job, modelId: model.id, owner };
  ttsDownloadProgress.value = null;
  let unlisten: (() => void) | null = null;
  try {
    unlisten = await listen<TtsDownloadProgress>(EVENTS.ttsDownloadProgress, (progress) => {
      if (!job.cancelled && !job.finished && progress.jobId === job.id) ttsDownloadProgress.value = progress;
    });
    if (job.cancelled) return;
    await invoke(COMMANDS.installTtsModel, { id: model.id, jobId: job.id });
    if (!job.cancelled) toast(`已安装 ${model.name}`, 'success');
  } catch (error) {
    if (!job.cancelled) toast(`安装 TTS 模型失败：${normalizeError(error)}`, 'error');
  } finally {
    job.finished = true;
    unlisten?.();
    await refreshTtsModels();
    activeInstall.value = null;
    ttsDownloadProgress.value = null;
    ttsInstallCancelling.value = false;
  }
}
