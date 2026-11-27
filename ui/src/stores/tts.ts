import { computed, shallowRef, ref } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';

export interface TtsModel {
  id: string;
  name: string;
  family: 'Kokoro' | 'ZipVoice' | 'CosyVoice3';
  package: string;
  recommended: boolean;
  sizeBytes: number;
  installed: boolean;
  hasFiles: boolean;
  loaded: boolean;
  device: 'cpu' | 'cuda' | null;
  gpuEnvironmentReady: boolean | null;
  gpuEnvironmentPresent: boolean;
  path: string;
  /** 默认浏览地址；未自定义时「访问下载源」打开它。 */
  defaultDownloadUrl: string;
  /** 可安装的默认基址；编辑下载源时作为占位。 */
  defaultSourceUrl: string;
  /** 用户按模型覆盖的下载源；空表示使用默认。 */
  customDownloadUrl: string;
}
interface TtsDownloadProgress { jobId: string; id: string; file: string; received: number; total: number }
export interface TtsLoadProgress { jobId: string; stage: string }
export interface TtsJob { id: string; cancelled: boolean; finished: boolean }
export type TtsFamily = TtsModel['family'];

export const ttsScriptText = ref('');
export const ttsScriptFormat = ref<'txt' | 'lrc' | 'srt'>('txt');
export const ttsModels = ref<TtsModel[]>([]);
export const ttsModelsLoading = ref(false);
export const ttsPreferredFamily = ref<TtsFamily | null>(null);
export const ttsPreferredModelIds = ref<Partial<Record<TtsFamily, string>>>({});
export const ttsDownloadProgress = ref<TtsDownloadProgress | null>(null);
export const ttsDownloadPercent = computed(() => ttsDownloadProgress.value && ttsDownloadProgress.value.total > 0
  ? Math.round(ttsDownloadProgress.value.received / ttsDownloadProgress.value.total * 100) : 0);
const activeInstall = shallowRef<{ job: TtsJob; modelId: string; owner: 'settings' | 'workspace' } | null>(null);
const activeLoad = shallowRef<{ job: TtsJob; modelId: string } | null>(null);
export const ttsLoadProgress = ref<TtsLoadProgress | null>(null);
export const ttsLoadCancelling = ref(false);
export const ttsInstalling = computed(() => activeInstall.value?.modelId ?? null);
export const ttsInstallCancelling = ref(false);
export const ttsSynthesizing = ref(false);
type TtsModelAction = 'load' | 'release' | 'delete' | 'installGpu' | 'removeGpu';
const ttsModelAction = shallowRef<{ id: string; action: TtsModelAction } | null>(null);
export const ttsGpuInstalling = computed(() => ttsModelAction.value?.action === 'installGpu');
export const ttsGpuRemoving = computed(() => ttsModelAction.value?.action === 'removeGpu');
export const ttsModelBusy = computed(() => ttsSynthesizing.value || Boolean(ttsInstalling.value || ttsModelAction.value));

export function ttsModelActionLabel(id: string): string | undefined {
  const task = ttsModelAction.value;
  if (task?.id === id) return { load: '加载中…', release: '释放中…', delete: '删除中…', installGpu: undefined, removeGpu: undefined }[task.action];
}

export async function manageTtsModel(model: TtsModel, action: TtsModelAction): Promise<void> {
  if (ttsModelBusy.value) return;
  ttsModelAction.value = { id: model.id, action };
  const verb = { load: model.loaded ? '重新加载' : '加载', release: '释放', delete: '删除', installGpu: '安装 GPU 环境', removeGpu: '删除 GPU 环境' }[action];
  let job: TtsJob | null = null;
  let unlisten: (() => void) | null = null;
  try {
    if (action === 'load' || action === 'installGpu') {
      job = { id: crypto.randomUUID(), cancelled: false, finished: false };
      activeLoad.value = { job, modelId: model.id };
      ttsLoadProgress.value = { jobId: job.id, stage: action === 'installGpu' ? '正在准备 GPU 环境安装…' : '正在准备模型加载…' };
      unlisten = await listen<TtsLoadProgress>(EVENTS.ttsLoadProgress, (progress) => {
        if (job && !job.cancelled && !job.finished && progress.jobId === job.id) ttsLoadProgress.value = progress;
      });
      if (job.cancelled) return;
      if (action === 'installGpu') await invoke(COMMANDS.installTtsGpu, { jobId: job.id });
      else await invoke(COMMANDS.loadTtsModel, { id: model.id, jobId: job.id });
    } else if (action === 'removeGpu') {
      await invoke(COMMANDS.removeTtsGpu);
    } else {
      await invoke(COMMANDS.removeTtsModel, { id: model.id, deleteFiles: action === 'delete' });
    }
    if (!job?.cancelled) {
      const message = action === 'installGpu' ? 'GPU 环境已就绪，重新加载 CosyVoice3 即可尝试用 GPU 推理'
        : action === 'removeGpu' ? '已删除托管 GPU 环境' : `${verb}成功：${model.name}`;
      toast(message, 'success');
    }
  } catch (error) {
    if (!job?.cancelled) toast(`${verb}${action === 'installGpu' || action === 'removeGpu' ? '' : ' TTS 模型'}失败：${normalizeError(error)}`, 'error');
  } finally {
    if (job) job.finished = true;
    unlisten?.();
    await refreshTtsModels();
    if (activeLoad.value?.job === job) activeLoad.value = null;
    ttsLoadProgress.value = null;
    ttsLoadCancelling.value = false;
    ttsModelAction.value = null;
  }
}
let refreshGeneration = 0;

export async function cancelTtsJob(kind: 'install' | 'load' | 'synthesis', job: TtsJob, quiet = false): Promise<void> {
  if (job.cancelled || job.finished) return;
  job.cancelled = true;
  try {
    await invoke(COMMANDS.cancelTtsJob, { kind, jobId: job.id });
  } catch (error) {
    job.cancelled = false;
    if (!quiet && !job.finished) toast(`无法取消语音任务：${normalizeError(error)}`, 'error');
  }
}

export async function openModelDownloadSource(tts: boolean, modelId?: string): Promise<void> {
  try { await invoke(COMMANDS.openModelDownloadSource, { tts, id: modelId ?? null }); }
  catch (error) { toast(`无法打开模型下载源：${normalizeError(error)}`, 'error'); }
}

export async function refreshTtsModels(): Promise<void> {
  const generation = ++refreshGeneration;
  ttsModelsLoading.value = true;
  try {
    const models = await invoke<TtsModel[]>(COMMANDS.ttsModels);
    if (generation === refreshGeneration) {
      ttsModels.value = models.sort((left, right) => Number(right.recommended) - Number(left.recommended));
    }
  } catch (error) {
    if (generation === refreshGeneration) toast(`读取 TTS 模型失败：${normalizeError(error)}`, 'error');
  } finally {
    if (generation === refreshGeneration) ttsModelsLoading.value = false;
  }
}

export async function cancelTtsLoad(): Promise<void> {
  const task = activeLoad.value;
  if (!task || task.job.finished || ttsLoadCancelling.value) return;
  ttsLoadCancelling.value = true;
  await cancelTtsJob('load', task.job);
  if (activeLoad.value === task && !task.job.cancelled) ttsLoadCancelling.value = false;
}

export async function cancelTtsInstall(owner?: 'settings' | 'workspace'): Promise<void> {
  const task = activeInstall.value;
  if (!task || (owner && owner !== task.owner) || task.job.finished || ttsInstallCancelling.value) return;
  ttsInstallCancelling.value = true;
  await cancelTtsJob('install', task.job, owner !== undefined);
  if (activeInstall.value === task && !task.job.cancelled) ttsInstallCancelling.value = false;
}

export async function installTtsModel(model: TtsModel, owner: 'settings' | 'workspace'): Promise<void> {
  if (ttsModelBusy.value || model.installed) return;
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
