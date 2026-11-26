import { computed, shallowRef, ref } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { showModal } from '@/lib/modal';
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
  path: string;
}
interface TtsDownloadProgress { jobId: string; id: string; file: string; received: number; total: number }
export interface TtsJob { id: string; cancelled: boolean; finished: boolean }
export type TtsFamily = TtsModel['family'];

export const DEFAULT_TTS_DOWNLOAD_SOURCE = 'https://www.modelscope.cn/models/Lourdle/Fun-CosyVoice3-0.5B-2512_ONNX/files';
const COSYVOICE_CHECKOUT = [
  'git clone https://github.com/Lourdle/CosyVoice.git',
  'cd CosyVoice',
  'git checkout 24c9212eb54e832af4d9b03bcd9157d88acbf6db',
  'git submodule update --init --recursive',
].join(' && ');
const COSYVOICE_CPU_PYTORCH = 'python -m pip install torch==2.3.1 torchaudio==2.3.1 --index-url https://download.pytorch.org/whl/cpu';
const COSYVOICE_CPU_DEPS = 'python -m pip install setuptools==80.9.0 numpy==1.26.4 onnxruntime==1.23.2 transformers==4.51.3 modelscope==1.20.0 HyperPyYAML==1.2.3 openai-whisper==20250625 soundfile==0.12.1 librosa==0.10.2 wetext==0.0.4 conformer==0.3.2 diffusers==0.29.0 x-transformers==2.11.24 hydra-core==1.3.2 lightning==2.2.4 rich==13.7.1 gdown==5.1.0 matplotlib==3.7.5 wget==3.2 pyarrow==18.1.0 pyworld==0.3.5 inflect pydantic';

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
export const ttsInstalling = computed(() => activeInstall.value?.modelId ?? null);
export const ttsInstallCancelling = ref(false);
export const ttsSynthesizing = ref(false);
const ttsModelAction = shallowRef<{ id: string; action: 'load' | 'release' | 'delete' } | null>(null);
export const ttsModelBusy = computed(() => ttsSynthesizing.value || Boolean(ttsInstalling.value || ttsModelAction.value));

export function ttsModelActionLabel(id: string): string | undefined {
  const task = ttsModelAction.value;
  if (task?.id === id) return { load: '加载中…', release: '释放中…', delete: '删除中…' }[task.action];
}

export async function showCosyVoiceRuntimeSetup(): Promise<void> {
  const modelPath = ttsModels.value.find(model => model.family === 'CosyVoice3')?.path;
  const result = await showModal({
    title: '配置 CosyVoice3 运行环境',
    description: '模型文件由应用下载。加载或合成前，请在 Python 3.11 虚拟环境中执行下方安装命令。把 runtime.json 示例保存到 CosyVoice3 模型文件夹，并填入仓库和虚拟环境解释器的绝对路径（Windows 路径可用 /）。也可用 COSYVOICE_HOME 指定仓库。',
    fields: [
      { name: 'checkout', label: '用 Git Bash 或终端检出仓库（含 Matcha-TTS 子模块）', value: COSYVOICE_CHECKOUT, readonly: true },
      { name: 'pytorch', label: '安装 CPU 版 PyTorch', value: COSYVOICE_CPU_PYTORCH, readonly: true },
      { name: 'dependencies', label: '安装 ONNX Runtime 及推理依赖', value: COSYVOICE_CPU_DEPS, readonly: true },
      { name: 'config', label: 'runtime.json 示例', value: '{"cosyvoice_home":"<仓库绝对路径>","python":"<解释器绝对路径>"}', readonly: true },
      ...(modelPath ? [{ name: 'path', label: 'CosyVoice3 模型文件夹', value: modelPath, readonly: true }] : []),
    ],
    cancelText: '关闭',
    confirmText: '打开模型总目录',
  });
  if (result === null) return;
  try {
    await invoke(COMMANDS.openModelsDir);
  } catch (error) {
    toast(`无法打开模型目录：${normalizeError(error)}`, 'error');
  }
}

export async function manageTtsModel(model: TtsModel, action: 'load' | 'release' | 'delete'): Promise<void> {
  if (ttsModelBusy.value) return;
  ttsModelAction.value = { id: model.id, action };
  const verb = { load: model.loaded ? '重新加载' : '加载', release: '释放', delete: '删除' }[action];
  try {
    if (action === 'load') {
      await invoke(COMMANDS.loadTtsModel, { id: model.id });
    } else {
      await invoke(COMMANDS.removeTtsModel, { id: model.id, deleteFiles: action === 'delete' });
    }
    toast(`${verb}成功：${model.name}`, 'success');
  } catch (error) {
    toast(`${verb} TTS 模型失败：${normalizeError(error)}`, 'error');
  } finally {
    await refreshTtsModels();
    ttsModelAction.value = null;
  }
}
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

export async function openModelDownloadSource(tts: boolean): Promise<void> {
  try { await invoke(COMMANDS.openModelDownloadSource, { tts }); }
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
