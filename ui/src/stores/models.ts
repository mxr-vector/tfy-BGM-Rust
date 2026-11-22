/**
 * 「基本系统配置 → 模型」：模型目录、下载地址、ASR 模型列表与 sherpa-onnx 加载状态。
 *
 * 后端在每次 `models_info` 时重新扫描模型目录，因此进入分组即可拿到最新列表；
 * 下载进度由 Rust 通过 `model-download-progress` 事件推送。
 */

import { computed, ref } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { AsrModel, ModelDownloadProgress, ModelsInfo } from '@/lib/types';
import { flushPrefsNow } from './prefs';
import { state } from './state';

export const modelsInfo = ref<ModelsInfo | null>(null);
export const modelsLoading = ref(false);
export const downloading = ref(false);
export const loadTarget = ref<string | null>(null);
export const downloadProgress = ref<ModelDownloadProgress | null>(null);
export const transcribing = ref(false);
/** 最近一次 WAV 识别结果；`null` 表示尚未识别。 */
export const transcript = ref<string | null>(null);

/** 已下载 / 总字节数；未知总量时返回 0。 */
export const downloadPercent = computed(() => {
  const progress = downloadProgress.value;
  if (!progress || progress.overallBytes <= 0) return 0;
  return Math.min(100, Math.round((progress.overallReceived / progress.overallBytes) * 100));
});

/** 自定义下载地址（输入框草稿），保存后由后端归一化。 */
export const downloadUrlDraft = ref('');

let listening = false;

/** 下载是长任务，进度事件只需订阅一次。 */
async function ensureProgressListener(): Promise<void> {
  if (listening) return;
  listening = true;
  await listen<ModelDownloadProgress>(EVENTS.modelDownload, (progress) => {
    downloadProgress.value = progress;
  });
}

function apply(info: ModelsInfo | null | undefined): void {
  if (!info) return;
  modelsInfo.value = info;
  downloadUrlDraft.value = info.downloadUrl;
}

/** 读取模型分组快照：同时触发一次后端扫描。 */
export async function refreshModels(): Promise<void> {
  if (modelsLoading.value) return;
  modelsLoading.value = true;
  void ensureProgressListener();
  try {
    apply(await invoke<ModelsInfo>(COMMANDS.modelsInfo));
  } catch (error) {
    toast(`读取模型列表失败：${normalizeError(error)}`, 'error');
  } finally {
    modelsLoading.value = false;
  }
}

/** 选择自定义模型目录；取消时保持当前目录。 */
export async function chooseModelsDir(): Promise<void> {
  try {
    const info = await invoke<ModelsInfo | null>(COMMANDS.chooseModelsDir);
    if (!info) return;
    apply(info);
    toast('模型目录已更新', 'success');
  } catch (error) {
    toast(`切换模型目录失败：${normalizeError(error)}`, 'error');
  }
}

/** 恢复默认模型目录（系统「下载」目录下的 bgm-models）。 */
export async function resetModelsDir(): Promise<void> {
  try {
    apply(await invoke<ModelsInfo>(COMMANDS.resetModelsDir));
    toast('已恢复默认模型目录', 'success');
  } catch (error) {
    toast(`恢复默认模型目录失败：${normalizeError(error)}`, 'error');
  }
}

export async function openModelsDir(): Promise<void> {
  try {
    await invoke(COMMANDS.openModelsDir);
  } catch (error) {
    toast(`打开模型目录失败：${normalizeError(error)}`, 'error');
  }
}

/** 保存自定义下载地址；留空表示使用内置的 0.6B-int8 地址。 */
export async function saveDownloadUrl(url: string): Promise<void> {
  const trimmed = url.trim();
  if (trimmed && !/^https?:\/\//i.test(trimmed)) {
    toast('下载地址需以 http:// 或 https:// 开头', 'error');
    return;
  }
  const prefs = state.prefs;
  if (!prefs) return;
  // 直接改内存并立即落盘，等后端归一化结果回来后以 models_info 为准。
  state.prefs = { ...prefs, asrDownloadUrl: trimmed };
  await flushPrefsNow();
  await refreshModels();
  toast(trimmed ? '下载地址已保存' : '已恢复默认下载地址', 'success');
}

let cancelRequested = false;

/** 下载默认 ASR 模型（后端固定为 0.6B-int8 清单）。 */
export async function downloadDefaultModel(): Promise<void> {
  if (downloading.value) return;
  void ensureProgressListener();
  downloading.value = true;
  cancelRequested = false;
  downloadProgress.value = {
    file: '',
    index: 0,
    totalFiles: 0,
    received: 0,
    fileBytes: 0,
    overallReceived: 0,
    overallBytes: modelsInfo.value?.defaultModel.expectedBytes ?? 0,
    done: false,
    error: null,
  };
  try {
    apply(await invoke<ModelsInfo>(COMMANDS.downloadAsrModel));
    toast('默认 ASR 模型已下载完成', 'success');
  } catch (error) {
    if (cancelRequested) {
      toast('已取消下载模型', 'info');
    } else {
      toast(`下载模型失败：${normalizeError(error)}`, 'error');
    }
  } finally {
    downloading.value = false;
    cancelRequested = false;
    await refreshModels();
  }
}

/** 请求取消正在进行的下载；后端会在下一个数据块前中断并清理临时文件。 */
export async function cancelDownload(): Promise<void> {
  cancelRequested = true;
  try {
    await invoke<boolean>(COMMANDS.cancelAsrDownload);
  } catch {
    // 任务可能刚好结束，忽略即可。
  }
}

/** 用 sherpa-onnx 加载模型；加载 1GB 左右的模型需要数秒。 */
export async function loadAsrModel(model: AsrModel): Promise<void> {
  if (loadTarget.value) return;
  loadTarget.value = model.path;
  try {
    apply(await invoke<ModelsInfo>(COMMANDS.loadAsrModel, { path: model.path }));
    toast(`已加载 ${model.name}（sherpa-onnx）`, 'success');
  } catch (error) {
    toast(`加载 ASR 模型失败：${normalizeError(error)}`, 'error');
  } finally {
    loadTarget.value = null;
  }
}

/** 释放已加载模型，归还内存。 */
export async function unloadAsrModel(): Promise<void> {
  try {
    apply(await invoke<ModelsInfo>(COMMANDS.unloadAsrModel));
    toast('已释放 ASR 模型', 'success');
  } catch (error) {
    toast(`释放模型失败：${normalizeError(error)}`, 'error');
  }
}

/** 选择 WAV 文件并用已加载模型识别；取消选择时什么也不做。 */
export async function transcribeWav(): Promise<void> {
  if (transcribing.value) return;
  transcribing.value = true;
  try {
    const text = await invoke<string | null>(COMMANDS.transcribeWav);
    if (text === null) return;
    transcript.value = text;
    toast(text ? '识别完成' : '识别完成（未识别到文本）', text ? 'success' : 'info');
  } catch (error) {
    toast(`识别失败：${normalizeError(error)}`, 'error');
  } finally {
    transcribing.value = false;
  }
}

export function clearTranscript(): void {
  transcript.value = null;
}

/** 当前是否处于加载中；用于按钮禁用与文案。 */
export const loadingModelPath = computed(() => loadTarget.value);

/** 模型目录是否为默认位置时展示的说明。 */
export const modelsRootBadge = computed(() =>
  modelsInfo.value?.isDefaultRoot ? '默认：下载目录 / bgm-models' : '自定义目录',
);
