<script setup lang="ts">
/**
 * 基本系统配置：顶栏齿轮打开的分组。
 *
 * - 常规：字体、开机自动启动、最小化托盘、GPU 加速，以及当前平台特有的集成项；
 * - 播放：启动自动播放、防休眠、淡入淡出与输出设备；
 * - 模型：ASR 模型目录、下载地址、扫描到的模型列表与 sherpa-onnx 加载状态；
 * - 第三方接入：token、开放接口清单与 CORS。
 */

import { computed, nextTick, ref, watch } from 'vue';
import { cachedFonts, fontOptions, loadSystemFonts, type FontOption } from '@/lib/fonts';
import { formatBytes, normalizeError } from '@/lib/format';
import { COMMANDS } from '@/lib/constants';
import { showModal } from '@/lib/modal';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import { wakeLockSupported } from '@/composables/useSystemIntegration';
import type { AsrModel, CorsConfig, CorsMode, CudaMirrorInfo, OutputRoute } from '@/lib/types';
import { apiStatus, refreshApiInfo } from '@/stores/api';
import {
  asrLoadCancelling,
  autoLoading,
  cancelAsrLoad,
  cancelDownload,
  chooseModelsDir,
  deleteAsrModel,
  deletingModelId,
  downloadDefaultModel,
  downloadPercent,
  downloadProgress,
  downloading,
  loadAsrModel,
  loadStatus,
  loadingModelPath,
  modelBusy,
  modelsInfo,
  modelsLoading,
  modelsRootBadge,
  openModelsDir,
  refreshModels,
  resetModelsDir,
  retryModelLoad,
  unloadAsrModel,
} from '@/stores/models';
import {
  applyOutputRoutes,
  describeOutputIssue,
  outputRows,
  outputsState,
  refreshDevices,
  requestDeviceAccess,
  setOutputVolume,
  toggleOutput,
  toggleOutputMute,
} from '@/stores/outputs';
import {
  apiEndpointGroups,
  apiEndpoints,
  closeSettings,
  crossfadeEnabled,
  isLinux,
  isWindows,
  loadApiEndpoints,
  saveIntegrationSettings,
  setAudioEnhancement,
  setAutoplayOnStart,
  setAutostart,
  setCrossfadeEnabled,
  setGpuAcceleration,
  setMinimizeToTray,
  setMpris2,
  setPreventSleep,
  setSpatialAudio,
  setSmtc,
  setUiFont,
  settingsOpen,
  settingsTab,
  type SettingsTab,
} from '@/stores/settings';
import { state } from '@/stores/state';
import {
  cancelTtsInstall, cancelTtsLoad, installTtsModel, manageTtsModel,
  openModelDownloadSource, refreshTtsModels, ttsDownloadPercent, ttsDownloadProgress,
  ttsGpuInstalling, ttsGpuRemoving, ttsInstallCancelling, ttsInstalling, ttsLoadCancelling, ttsLoadProgress, ttsModelActionLabel,
  ttsModelBusy, ttsModels, ttsModelsLoading, type TtsModel,
} from '@/stores/tts';
import FontSelect from './FontSelect.vue';
import Icon from '@/components/Icon.vue';
import SettingSwitch from './SettingSwitch.vue';
import SettingsModelRow from './SettingsModelRow.vue';

const prefs = computed(() => state.prefs);

const TABS: { id: SettingsTab; label: string; hint: string; icon: string }[] = [
  { id: 'general', label: '常规', hint: '字体与系统集成', icon: 'sliders' },
  { id: 'playback', label: '播放', hint: '启动行为与输出', icon: 'wave' },
  { id: 'models', label: '模型', hint: 'ASR 与 TTS 模型', icon: 'chip' },
  { id: 'integrations', label: '第三方接入', hint: 'token / 接口 / CORS', icon: 'external' },
];

const CORS_MODES: { id: CorsMode; label: string; hint: string }[] = [
  { id: 'local', label: '仅本机', hint: '只允许应用自身与 localhost 页面跨域访问' },
  { id: 'any', label: '允许所有来源', hint: '任意网页都能读取曲库并控制播放，请谨慎使用' },
  { id: 'custom', label: '自定义来源', hint: '在本机之外，额外放行下面列出的来源' },
];

const tokenDraft = ref('');
const tokenVisible = ref(false);
const savingToken = ref(false);
const corsMode = ref<CorsMode>('local');
const corsOriginsText = ref('');
const savingCors = ref(false);
const copied = ref(false);

const fontsLoading = ref(false);
const fontList = ref<FontOption[]>(fontOptions());

/**
 * 读取系统字体。首次打开「常规」时扫描一次，之后复用会话缓存；
 * 用户安装新字体后可用下拉框里的「重新扫描」刷新。
 */
async function ensureFonts(force = false): Promise<void> {
  if (!force && cachedFonts().length > 0) {
    fontList.value = fontOptions();
    return;
  }
  fontsLoading.value = true;
  try {
    await loadSystemFonts(force);
  } catch (error) {
    toast(`读取系统字体失败：${normalizeError(error)}`, 'error');
  } finally {
    fontList.value = fontOptions();
    fontsLoading.value = false;
  }
}

const apiStateLabel = computed(() => {
  const status = apiStatus.value;
  if (!status) return '未启用';
  if (!status.enabled) return '已禁用';
  if (!status.running) return '未监听';
  return status.ready ? '运行中' : '启动中';
});

const apiBase = computed(() => `${apiStatus.value?.baseUrl ?? 'http://127.0.0.1:8737'}/v1`);
const issue = computed(() => describeOutputIssue());
const activeOutputs = computed(
  () => (prefs.value?.outputRoutes ?? []).filter((route) => route.enabled).length,
);

const corsHint = computed(
  () => CORS_MODES.find((mode) => mode.id === corsMode.value)?.hint ?? '',
);

// ---------------- 模型 ----------------

const models = computed(() => modelsInfo.value?.models ?? []);
const defaultModel = computed(() => modelsInfo.value?.defaultModel ?? null);
const loadedModel = computed(() => modelsInfo.value?.loaded ?? null);
const savingDownloadSource = ref(false);

type DownloadSourceTarget =
  | { kind: 'asr' }
  | { kind: 'tts'; model: TtsModel };

function ttsSourceDescription(model: TtsModel): string {
  return model.family === 'CosyVoice3'
    ? '填写 ModelScope 模型仓库首页或 /files 地址，仓库需保持 CosyVoice3 ONNX 的目录结构。留空恢复默认源。'
    : `默认地址指向发布页；自定义时请填写可直接拼接文件名的 HTTP(S) 镜像目录，例如 ${model.defaultSourceUrl}。ZipVoice 还需镜像 vocos_24khz.onnx、lexicon-en-us.txt 等依赖。留空恢复默认源。`;
}

async function editDownloadSource(target: DownloadSourceTarget): Promise<void> {
  const tts = target.kind === 'tts';
  const model = target.kind === 'tts' ? target.model : null;
  const label = tts ? `「${model!.name}」` : 'ASR 模型';
  const result = await showModal({
    title: `修改${label}下载源`,
    description: tts
      ? ttsSourceDescription(model!)
      : '填写 ModelScope 仓库地址或 0.6B-int8 文件所在目录的直链。留空恢复默认源。',
    fields: [{
      name: 'url',
      label: '下载源地址',
      type: 'textarea',
      rows: 3,
      // 默认地址直接填进输入框而不是灰字占位，长 URL 才能完整看到；
      // 保存默认值等同于恢复默认，不会写成覆盖项。
      value: tts
        ? (model!.customDownloadUrl || model!.defaultDownloadUrl)
        : modelsInfo.value?.downloadUrl,
      placeholder: tts ? model!.defaultSourceUrl : modelsInfo.value?.defaultDownloadUrl,
    }],
    confirmText: '保存',
    validate: ({ url }) => {
      const source = url.trim();
      if (!source) return null;
      if (!/^https?:\/\//i.test(source) || !URL.canParse(source) || /[\s\x00-\x1f\x7f]/.test(source)
        || Array.from(source).length > 1024) return '请输入有效的 HTTP 或 HTTPS 地址';
      if (tts && /[?#]/.test(source)) return 'TTS 下载源不能包含查询参数或片段';
      return null;
    },
  });
  if (!result) return;
  const cleared = !result.url.trim();
  savingDownloadSource.value = true;
  try {
    const url = await invoke<string>(COMMANDS.saveModelDownloadSource, {
      tts,
      id: model?.id ?? null,
      url: result.url,
    });
    if (tts) {
      await refreshTtsModels();
    } else {
      if (state.prefs) state.prefs.asrDownloadUrl = url;
      await refreshModels();
    }
    // TTS 后端把默认值归一成空串，因此以返回值为准判断是否恢复默认。
    const reset = tts ? url === '' : cleared;
    toast(reset ? `已恢复${label}默认下载源` : `${label}下载源已保存`, 'success');
  } catch (error) {
    toast(`保存${label}下载源失败：${normalizeError(error)}`, 'error');
  } finally {
    savingDownloadSource.value = false;
  }
}

const gpuEnvironmentModel = computed(() => ttsModels.value.find(model => model.family === 'CosyVoice3'));

const cudaMirrorInfo = ref<CudaMirrorInfo | null>(null);
const savingCudaMirror = ref(false);

async function refreshCudaMirrors(): Promise<void> {
  try {
    cudaMirrorInfo.value = await invoke<CudaMirrorInfo>(COMMANDS.cudaMirrors);
  } catch (error) {
    toast(`读取 CUDA 镜像源失败：${normalizeError(error)}`, 'error');
  }
}

function validateMirrorUrl(value: string, label: string): string | null {
  const source = value.trim();
  if (!source) return `${label}不能为空`;
  if (
    !/^https?:\/\//i.test(source) || !URL.canParse(source)
    || /[\s\x00-\x1f\x7f]/.test(source) || Array.from(source).length > 1024
  ) {
    return `${label}不是有效的 HTTP 或 HTTPS 地址`;
  }
  const url = new URL(source);
  if (url.search || url.hash) return `${label}不能包含查询参数或片段`;
  return null;
}

async function editCudaMirrors(): Promise<void> {
  const info = cudaMirrorInfo.value;
  if (!info) return;
  const result = await showModal({
    title: '修改 CUDA 镜像源',
    description: `安装 GPU 推理环境下载依赖时使用。默认 PyPI 镜像 ${info.defaultPypi}，PyTorch 镜像 ${info.defaultPytorch}。`,
    fields: [
      { name: 'pypi', label: 'PyPI 镜像源', value: info.pypi, placeholder: info.defaultPypi },
      { name: 'pytorch', label: 'PyTorch 镜像源', value: info.pytorch, placeholder: info.defaultPytorch },
    ],
    confirmText: '保存',
    validate: ({ pypi, pytorch }) =>
      validateMirrorUrl(pypi, 'PyPI 镜像源') ?? validateMirrorUrl(pytorch, 'PyTorch 镜像源'),
  });
  if (!result) return;
  savingCudaMirror.value = true;
  try {
    cudaMirrorInfo.value = await invoke<CudaMirrorInfo>(COMMANDS.saveCudaMirrors, {
      pypi: result.pypi.trim(),
      pytorch: result.pytorch.trim(),
    });
    toast('CUDA 镜像源已保存，下次安装 GPU 环境时生效', 'success');
  } catch (error) {
    toast(`保存 CUDA 镜像源失败：${normalizeError(error)}`, 'error');
  } finally {
    savingCudaMirror.value = false;
  }
}

async function confirmInstallGpu(model: TtsModel): Promise<void> {
  const confirmed = await showModal({
    title: '安装 GPU 环境',
    description: '需要 NVIDIA 驱动和网络连接。首次下载约 2 GB，运行库安装到本地缓存，不会更改系统驱动或 CPU 环境。',
    confirmText: '安装',
  });
  if (confirmed !== null) await manageTtsModel(model, 'installGpu');
}

async function confirmRemoveGpu(model: TtsModel): Promise<void> {
  const confirmed = await showModal({
    title: '删除 GPU 环境',
    description: '只删除应用托管的 GPU 环境和专用下载缓存。如果模型正在使用该环境，会先释放模型。模型文件、CPU 环境和 NVIDIA 驱动不受影响。',
    confirmText: '删除',
    danger: true,
  });
  if (confirmed !== null) await manageTtsModel(model, 'removeGpu');
}

async function confirmDeleteTtsModel(model: TtsModel): Promise<void> {
  const confirmed = await showModal({
    title: '删除 TTS 模型',
    description: `确定永久删除「${model.name}」的模型文件吗？已加载的模型会在删除前自动释放。`,
    confirmText: '删除',
    danger: true,
  });
  if (confirmed !== null) await manageTtsModel(model, 'delete');
}
const modelDownloading = computed(() => downloading.value || Boolean(modelsInfo.value?.downloading));

const progressText = computed(() => {
  const progress = downloadProgress.value;
  if (!progress) return '准备下载…';
  const file = progress.file || '准备中';
  return `${file} · ${formatBytes(progress.overallReceived)} / ${formatBytes(progress.overallBytes)}`;
});

/** 删除模型前确认一次；确认后后端会自动卸载正在使用的目标模型。 */
async function confirmDeleteModel(model: AsrModel): Promise<void> {
  const confirmed = await showModal({
    title: '删除 ASR 模型',
    description: `确定删除「${model.name}」（${formatBytes(model.sizeBytes)}）吗？模型文件将永久删除，无法恢复。若正在使用，会自动释放并卸载。`,
    confirmText: '删除',
    danger: true,
  });
  if (confirmed === null) return;
  const reconfirmed = await showModal({
    title: '再次确认删除',
    description: `即将永久删除「${model.name}」及其全部附属文件，此操作无法撤销。请确认不是误操作。`,
    confirmText: '确认删除',
    cancelText: '返回',
    danger: true,
  });
  if (reconfirmed === null) return;
  await deleteAsrModel(model);
}

function selectTab(tab: SettingsTab): void {
  settingsTab.value = tab;
  if (tab === 'general') void ensureFonts();
  if (tab === 'playback') void refreshOutputs();
  if (tab === 'models') {
    void refreshModels();
    void refreshTtsModels();
    void refreshCudaMirrors();
  }
  if (tab === 'integrations') void syncIntegrationForm();
}

// ---------------- 第三方接入 ----------------

async function syncIntegrationForm(): Promise<void> {
  const status = await refreshApiInfo();
  tokenDraft.value = status?.token ?? '';
  corsMode.value = status?.cors.mode ?? 'local';
  corsOriginsText.value = (status?.cors.origins ?? []).join('\n');
  void loadApiEndpoints();
}

function savedCors(): CorsConfig {
  return apiStatus.value?.cors ?? { mode: 'local', origins: [] };
}

function draftCors(): CorsConfig {
  const origins = corsOriginsText.value
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
  return {
    mode: corsMode.value,
    // 非自定义模式保留已保存的来源，切换回来时不用重新输入。
    origins: corsMode.value === 'custom' ? origins : savedCors().origins,
  };
}

async function saveToken(): Promise<void> {
  if (savingToken.value) return;
  savingToken.value = true;
  const ok = await saveIntegrationSettings(tokenDraft.value, savedCors());
  if (ok) {
    tokenDraft.value = apiStatus.value?.token ?? tokenDraft.value;
    tokenVisible.value = false;
  }
  savingToken.value = false;
}

async function saveCors(): Promise<void> {
  if (savingCors.value) return;
  savingCors.value = true;
  const draft = draftCors();
  // `*` 保存后由服务端转成「允许所有来源」，不计入来源条数。
  const submitted = draft.origins.filter((origin) => origin !== '*').length;
  const ok = await saveIntegrationSettings(apiStatus.value?.token ?? tokenDraft.value, draft);
  if (ok) {
    corsMode.value = apiStatus.value?.cors.mode ?? corsMode.value;
    corsOriginsText.value = (apiStatus.value?.cors.origins ?? []).join('\n');
    const saved = apiStatus.value?.cors.origins.length ?? 0;
    if (apiStatus.value?.cors.mode === 'custom' && saved < submitted) {
      toast(`有 ${submitted - saved} 条来源无效、重复或超出上限（最多 16 条），未保存`, 'error');
    }
  }
  savingCors.value = false;
}

async function copyApiBase(): Promise<void> {
  try {
    await navigator.clipboard.writeText(apiBase.value);
    copied.value = true;
    window.setTimeout(() => {
      copied.value = false;
    }, 1500);
    toast('已复制接口地址', 'success');
  } catch {
    toast('复制失败，请手动选择地址', 'error');
  }
}

// ---------------- 输出设备 ----------------

function percentOf(route: OutputRoute | null): number {
  return Math.round((route?.volume ?? 1) * 100);
}

function onVolumeInput(deviceId: string, event: Event): void {
  const value = Number((event.target as HTMLInputElement).value);
  setOutputVolume(deviceId, Number.isFinite(value) ? value : 0, false);
}

async function refreshOutputs(): Promise<void> {
  await refreshDevices();
  await applyOutputRoutes();
}

async function unlockDevices(): Promise<void> {
  const ok = await requestDeviceAccess();
  if (!ok) {
    toast(describeOutputIssue() || '未能读取输出设备列表', 'error');
    return;
  }
  toast(`已发现 ${outputsState.devices.length} 个输出设备`, 'success');
}

// ---------------- 生命周期 ----------------

const panel = ref<HTMLElement | null>(null);

/** 打开后焦点落在面板上，Esc 由面板统一处理；内部下拉框会先行拦截。 */
function onPanelKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape') return;
  event.preventDefault();
  closeSettings();
}

watch(settingsOpen, async (open) => {
  if (!open) return;
  if (settingsTab.value === 'general') void ensureFonts();
  if (settingsTab.value === 'playback') void refreshOutputs();
  if (settingsTab.value === 'models') {
    void refreshModels();
    void refreshTtsModels();
    void refreshCudaMirrors();
  } else if (ttsModels.value.length === 0) void refreshTtsModels();
  if (settingsTab.value === 'integrations') void syncIntegrationForm();
  await nextTick();
  panel.value?.focus();
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="settingsOpen"
      class="settings-backdrop"
      @mousedown.self="closeSettings"
    >
      <section
        ref="panel"
        class="settings-panel"
        role="dialog"
        aria-modal="true"
        aria-label="基本系统配置"
        tabindex="-1"
        @keydown.stop="onPanelKeydown"
      >
        <header class="settings-header">
          <div class="settings-heading">
            <span class="settings-kicker"><Icon name="settings" /> SYSTEM SETTINGS</span>
            <h2 class="settings-title">基本系统配置</h2>
            <p class="settings-sub">字体与系统集成 · 播放行为 · ASR 模型 · 第三方接入</p>
          </div>
          <button
            type="button"
            class="settings-close"
            title="关闭设置"
            aria-label="关闭设置"
            @click="closeSettings"
          >
            <Icon name="x" />
          </button>
        </header>

        <div class="settings-body">
          <nav class="settings-tabs" role="tablist" aria-label="配置分组">
            <button
              v-for="tab in TABS"
              :key="tab.id"
              type="button"
              class="settings-tab"
              :class="{ 'is-active': settingsTab === tab.id }"
              role="tab"
              :aria-selected="settingsTab === tab.id"
              @click="selectTab(tab.id)"
            >
              <Icon :name="tab.icon" />
              <span class="settings-tab-text">
                <strong>{{ tab.label }}</strong>
                <small>{{ tab.hint }}</small>
              </span>
            </button>
          </nav>

          <div class="settings-content">
            <!-- ============ 常规 ============ -->
            <section
              v-if="settingsTab === 'general'"
              class="settings-group"
              role="tabpanel"
              aria-label="常规"
            >
              <div class="settings-group-head">
                <h3>常规</h3>
                <p>界面字体与系统级集成，开关保存后立即生效。</p>
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>字体选择</strong>
                  <span>界面文字使用的字体，列表直接读取系统已安装的字体</span>
                </div>
                <div class="settings-font">
                  <FontSelect
                    :model-value="prefs?.uiFont ?? 'system'"
                    :options="fontList"
                    :loading="fontsLoading"
                    @update:model-value="setUiFont"
                    @refresh="ensureFonts(true)"
                  />
                </div>
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>开机自动启动</strong>
                  <span>登录系统后自动打开听风吟</span>
                </div>
                <SettingSwitch
                  label="开机自动启动"
                  :model-value="Boolean(prefs?.autostart)"
                  @update:model-value="setAutostart"
                />
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>最小化托盘</strong>
                  <span>最小化窗口时隐藏到系统托盘；关闭时选择最小化托盘也会开启此项</span>
                </div>
                <SettingSwitch
                  label="最小化托盘"
                  :model-value="Boolean(prefs?.minimizeToTray)"
                  @update:model-value="setMinimizeToTray"
                />
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>开启 GPU 加速</strong>
                  <span>使用显卡渲染界面；切换后需重启应用</span>
                </div>
                <SettingSwitch
                  label="开启 GPU 加速"
                  :model-value="Boolean(prefs?.gpuAcceleration)"
                  @update:model-value="setGpuAcceleration"
                />
              </div>

              <div v-if="isWindows" class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>Windows 集成</h4>
                  <span>仅 Windows 提供</span>
                </div>

                <div class="settings-row">
                  <div class="settings-row-text">
                    <strong>SMTC</strong>
                    <span>在系统媒体控制中心显示曲目并接受播放控制</span>
                  </div>
                  <SettingSwitch
                    label="SMTC"
                    :model-value="Boolean(prefs?.smtc)"
                    @update:model-value="setSmtc"
                  />
                </div>

                <div class="settings-row">
                  <div class="settings-row-text">
                    <strong>系统空间音效</strong>
                    <span>为耳机提供前方声场的空间感</span>
                  </div>
                  <SettingSwitch
                    label="系统空间音效"
                    :model-value="Boolean(prefs?.spatialAudio)"
                    @update:model-value="setSpatialAudio"
                  />
                </div>

                <div class="settings-row">
                  <div class="settings-row-text">
                    <strong>系统音频增强</strong>
                    <span>对总线做温和的动态与响度增强</span>
                  </div>
                  <SettingSwitch
                    label="系统音频增强"
                    :model-value="Boolean(prefs?.audioEnhancement)"
                    @update:model-value="setAudioEnhancement"
                  />
                </div>
              </div>

              <div v-if="isLinux" class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>Linux 集成</h4>
                  <span>仅 Linux 提供</span>
                </div>

                <div class="settings-row">
                  <div class="settings-row-text">
                    <strong>MPRIS2</strong>
                    <span>把播放状态接入桌面媒体控件；仅 Wayland 会话支持</span>
                  </div>
                  <SettingSwitch
                    label="MPRIS2"
                    :model-value="Boolean(prefs?.mpris2)"
                    @update:model-value="setMpris2"
                  />
                </div>
              </div>
            </section>

            <!-- ============ 播放 ============ -->
            <section
              v-else-if="settingsTab === 'playback'"
              class="settings-group"
              role="tabpanel"
              aria-label="播放"
            >
              <div class="settings-group-head">
                <h3>播放</h3>
                <p>启动与播放过程中的行为，以及多输出设备。</p>
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>程序启动时自动播放</strong>
                  <span>应用启动后从曲库第一首开始播放</span>
                </div>
                <SettingSwitch
                  label="程序启动时自动播放"
                  :model-value="Boolean(prefs?.autoplayOnStart)"
                  @update:model-value="setAutoplayOnStart"
                />
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>音乐播放时不自动休眠或熄屏</strong>
                  <span v-if="wakeLockSupported">播放期间向系统申请保持唤醒，暂停后自动释放</span>
                  <span v-else>当前平台的 WebView 不支持屏幕唤醒锁，此开关仅在 Windows 生效</span>
                </div>
                <SettingSwitch
                  label="音乐播放时不自动休眠或熄屏"
                  :model-value="Boolean(prefs?.preventSleep)"
                  :disabled="!wakeLockSupported"
                  @update:model-value="setPreventSleep"
                />
              </div>

              <div class="settings-row">
                <div class="settings-row-text">
                  <strong>开启音乐淡入淡出</strong>
                  <span>
                    切歌时新旧曲交叉淡化；当前
                    {{ crossfadeEnabled ? `${prefs?.crossfadeSeconds ?? 0} 秒` : '硬切' }}
                  </span>
                </div>
                <SettingSwitch
                  label="开启音乐淡入淡出"
                  :model-value="crossfadeEnabled"
                  @update:model-value="setCrossfadeEnabled"
                />
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>输出设备列表</h4>
                  <div class="settings-subgroup-actions">
                    <button
                      v-if="!outputsState.labelsAvailable"
                      type="button"
                      class="settings-link"
                      :disabled="outputsState.busy"
                      @click="unlockDevices"
                    >
                      {{ outputsState.busy ? '读取中…' : '读取设备列表' }}
                    </button>
                    <span v-else class="settings-muted">{{ outputRows.length }} 个设备</span>
                    <button
                      type="button"
                      class="settings-icon-btn"
                      title="重新扫描输出设备"
                      aria-label="重新扫描输出设备"
                      @click="refreshOutputs"
                    >
                      <Icon name="refresh" />
                    </button>
                  </div>
                </div>

                <p v-if="!outputsState.labelsAvailable" class="settings-hint">
                  未授权时只显示默认设备；授权一次音频设备访问（不会录音）后可独立路由到耳机 / 虚拟声卡。
                </p>

                <div class="settings-outputs">
                  <div
                    v-for="row in outputRows"
                    :key="row.deviceId"
                    class="settings-output-row"
                    :class="{ 'is-off': !row.route?.enabled, 'is-missing': row.missing }"
                  >
                    <SettingSwitch
                      :label="`${row.label} 输出`"
                      :model-value="Boolean(row.route?.enabled)"
                      @update:model-value="toggleOutput(row.deviceId)"
                    />
                    <div class="settings-output-meta">
                      <span class="settings-output-name" :title="row.deviceId">{{ row.label }}</span>
                      <span class="settings-output-sub">
                        {{ row.missing ? '未连接' : row.route?.muted ? '已静音' : '独立音量' }}
                      </span>
                    </div>
                    <div class="settings-output-gain">
                      <input
                        type="range"
                        class="output-range"
                        min="0"
                        max="1"
                        step="0.01"
                        :value="row.route?.volume ?? 1"
                        :disabled="!row.route?.enabled"
                        :aria-label="`${row.label} 音量`"
                        :style="{ '--level': `${percentOf(row.route)}%` }"
                        @input="onVolumeInput(row.deviceId, $event)"
                      />
                      <span class="settings-output-percent">{{ percentOf(row.route) }}%</span>
                      <button
                        type="button"
                        class="settings-icon-btn"
                        :class="{ 'is-warn': row.route?.muted }"
                        :disabled="!row.route?.enabled"
                        :title="row.route?.muted ? '取消这一路静音' : '只静音这一路'"
                        @click="toggleOutputMute(row.deviceId)"
                      >
                        <Icon :name="row.route?.muted ? 'volume-mute' : 'volume-low'" />
                      </button>
                    </div>
                  </div>
                </div>

                <p class="settings-hint" :class="{ 'is-warn': Boolean(issue) }">
                  {{
                    issue ||
                    `已启用 ${activeOutputs} 路输出；主音量是全局音量，每路音量叠加在它之上。`
                  }}
                </p>
              </div>
            </section>

            <!-- ============ 模型 ============ -->
            <section
              v-else-if="settingsTab === 'models'"
              class="settings-group"
              role="tabpanel"
              aria-label="模型"
            >
              <div class="settings-group-head">
                <h3>模型</h3>
                <p>管理本地语音识别与语音合成模型。</p>
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>GPU 推理环境</h4>
                </div>
                <p v-if="cudaMirrorInfo" class="settings-hint settings-mirror-hint">
                  依赖镜像：PyPI <code>{{ cudaMirrorInfo.pypi }}</code> · PyTorch <code>{{ cudaMirrorInfo.pytorch }}</code>
                  <button type="button" class="settings-link" :disabled="savingCudaMirror" @click="editCudaMirrors">修改镜像源</button>
                </p>
                <template v-if="gpuEnvironmentModel">
                  <div v-if="ttsGpuInstalling && ttsLoadProgress" class="settings-download">
                    <div class="settings-download-bar" role="progressbar" aria-label="GPU 环境安装进度">
                      <span class="settings-download-fill is-indeterminate"></span>
                    </div>
                    <p class="settings-hint" role="status">{{ ttsLoadProgress.stage }}</p>
                    <button type="button" class="settings-btn" :disabled="ttsLoadCancelling" @click="cancelTtsLoad">{{ ttsLoadCancelling ? '正在取消…' : '取消安装' }}</button>
                  </div>
                  <p v-else-if="gpuEnvironmentModel.gpuEnvironmentReady === null" class="settings-hint">一键安装需要 Windows x64 和 NVIDIA 显卡。</p>
                  <template v-else-if="gpuEnvironmentModel.gpuEnvironmentReady">
                    <p class="settings-hint" role="status">GPU 环境已就绪，模型加载时会在显存充足的情况下尝试使用 GPU。</p>
                  </template>
                  <template v-else>
                    <p class="settings-hint">{{ gpuEnvironmentModel.installed ? '需要 NVIDIA 驱动。环境单独安装，不影响 CPU 推理。' : '请先安装语音模型，用于验证 GPU 环境。' }}</p>
                    <button type="button" class="settings-btn" :disabled="ttsModelBusy || !gpuEnvironmentModel.installed" @click="confirmInstallGpu(gpuEnvironmentModel)">安装 GPU 环境</button>
                  </template>
                  <button v-if="gpuEnvironmentModel.gpuEnvironmentPresent && !ttsGpuInstalling" type="button" class="settings-btn is-danger" :disabled="ttsModelBusy" @click="confirmRemoveGpu(gpuEnvironmentModel)">{{ ttsGpuRemoving ? '正在删除 GPU 环境…' : '删除 GPU 环境' }}</button>
                </template>
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>模型目录</h4>
                  <span class="settings-muted">{{ modelsRootBadge }}</span>
                </div>
                <div class="settings-token-row">
                  <input
                    class="settings-input"
                    :value="modelsInfo?.root ?? '读取中…'"
                    readonly
                    spellcheck="false"
                    aria-label="模型目录"
                  />
                  <button type="button" class="settings-btn" @click="chooseModelsDir">
                    更改…
                  </button>
                  <button
                    type="button"
                    class="settings-btn"
                    :disabled="modelsInfo?.isDefaultRoot"
                    @click="resetModelsDir"
                  >
                    恢复默认
                  </button>
                  <button
                    type="button"
                    class="settings-icon-btn"
                    title="打开模型目录"
                    aria-label="打开模型目录"
                    @click="openModelsDir"
                  >
                    <Icon name="folder" />
                  </button>
                  <button
                    type="button"
                    class="settings-icon-btn"
                    title="重新扫描模型目录"
                    aria-label="重新扫描模型目录"
                    :disabled="modelsLoading"
                    @click="refreshModels"
                  >
                    <Icon name="refresh" />
                  </button>
                </div>
                <p class="settings-hint">
                  默认位置为系统「下载」目录下的 <code>bgm-models</code>（Windows / Linux 均取系统下载目录）；
                  进入本分组时，会自动扫描该目录下所有 <code>.onnx</code> 模型文件。
                </p>
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>ASR 模型列表</h4>
                </div>

                <div v-if="loadStatus.phase === 'failed'" class="settings-subgroup-actions" role="status">
                  <p class="settings-hint is-warn">模型加载失败{{ loadStatus.error ? `：${loadStatus.error}` : '' }}</p>
                  <button type="button" class="settings-btn" :disabled="modelBusy" @click="retryModelLoad">
                    重试加载
                  </button>
                </div>
                <div v-else-if="loadStatus.phase === 'loading'" class="settings-subgroup-actions">
                  <p class="settings-hint" role="status">{{ autoLoading ? '下载完成，正在自动加载模型…' : '正在加载 ASR 模型…' }}</p>
                  <button type="button" class="settings-btn" :disabled="asrLoadCancelling" @click="cancelAsrLoad">
                    {{ asrLoadCancelling ? '正在取消…' : '取消加载' }}
                  </button>
                </div>

                <div v-if="defaultModel && !defaultModel.installed" class="settings-model-callout">
                  <div class="settings-model-callout-text">
                    <strong>默认模型未安装</strong>
                    <span>{{ defaultModel.name }}</span>
                    <small>
                      {{ formatBytes(defaultModel.expectedBytes) }}
                      <template v-if="defaultModel.sizeBytes > 0">
                        · 缺少 {{ defaultModel.missing.join('、') }}
                      </template>
                      <template v-else> · 尚未下载</template>
                    </small>
                  </div>
                  <div class="settings-subgroup-actions">
                    <button type="button" class="settings-link" :disabled="savingDownloadSource" @click="editDownloadSource({ kind: 'asr' })">修改下载源</button>
                    <button type="button" class="settings-link" :disabled="savingDownloadSource" @click="openModelDownloadSource(false)">访问下载源</button>
                    <button
                      type="button"
                      class="settings-btn is-primary"
                      :disabled="modelDownloading || savingDownloadSource"
                      @click="downloadDefaultModel"
                    >
                      {{ autoLoading ? '自动加载中…' : modelDownloading ? '下载中…' : '下载' }}
                    </button>
                  </div>
                </div>

                <!-- 下载完成后命令仍在自动加载模型：此时隐藏下载进度与「取消下载」，
                 避免用户点击一个不会生效的取消按钮。 -->
                <div v-if="modelDownloading && !autoLoading" class="settings-download">
                  <div class="settings-download-bar">
                    <span class="settings-download-fill" :style="{ width: `${downloadPercent}%` }"></span>
                  </div>
                  <div class="settings-download-meta">
                    <span>{{ progressText }}</span>
                    <span class="settings-download-tail">
                      <span>{{ downloadPercent }}%</span>
                      <button type="button" class="settings-link" @click="cancelDownload">
                        取消下载
                      </button>
                    </span>
                  </div>
                </div>

                <div class="settings-models">
                  <SettingsModelRow
                    v-for="model in models"
                    :key="model.id"
                    :name="model.name"
                    :path="model.path"
                    :size-bytes="model.sizeBytes"
                    :default-model="model.isDefault"
                    :loaded="loadedModel?.path === model.path"
                    :status="model.loadable ? '已安装' : model.missing.length ? '文件不完整' : '不可加载'"
                  >
                    <template #details>
                      · {{ model.files.length }} 个 .onnx
                      <template v-if="loadedModel?.path === model.path">· {{ loadedModel.engine }} · {{ loadedModel.threads }} 线程</template>
                      <template v-if="model.missing.length">· 缺少 {{ model.missing.join('、') }}</template>
                    </template>
                      <template v-if="model.isDefault && defaultModel?.installed">
                        <button type="button" class="settings-link" :disabled="savingDownloadSource" @click="editDownloadSource({ kind: 'asr' })">修改下载源</button>
                        <button type="button" class="settings-link" :disabled="savingDownloadSource" @click="openModelDownloadSource(false)">访问下载源</button>
                      </template>
                      <button
                        type="button"
                        class="settings-btn"
                        :disabled="!model.loadable || modelBusy"
                        @click="loadAsrModel(model)"
                      >
                        {{
                          loadingModelPath === model.path
                            ? '加载中…'
                            : loadedModel?.path === model.path
                              ? '重新加载'
                              : model.loadable
                                ? '加载'
                                : '不可加载'
                        }}
                      </button>
                      <button v-if="loadedModel?.path === model.path" type="button" class="settings-btn" :disabled="modelBusy" @click="unloadAsrModel">释放模型</button>
                      <button
                        type="button"
                        class="settings-btn is-danger"
                        :disabled="modelBusy || modelDownloading"
                        :title="
                          modelBusy || modelDownloading
                            ? '模型任务进行中，暂不能删除'
                            : loadedModel?.path === model.path
                              ? '删除模型并自动卸载'
                              : '删除模型文件'
                        "
                        @click="confirmDeleteModel(model)"
                      >
                        {{ deletingModelId === model.id ? '删除中…' : '删除' }}
                      </button>
                  </SettingsModelRow>
                  <p v-if="modelsLoading && models.length === 0" class="settings-hint">
                    正在扫描模型目录…
                  </p>
                  <p v-else-if="models.length === 0" class="settings-hint">
                    未在模型目录下找到 .onnx 模型{{ defaultModel?.installed ? '' : '，可先下载默认模型' }}。
                  </p>
                </div>

                <p v-if="modelsInfo?.warnings.length" class="settings-hint is-warn">
                  {{ modelsInfo.warnings.join('；') }}
                </p>
              </div>

              <div class="settings-subgroup">
                  <div class="settings-subgroup-head">
                    <h4>TTS 语音模型</h4>
                  </div>
                  <p class="settings-hint">CosyVoice3 使用 5–15 秒参考音频和对应文本进行零样本克隆，首次加载自动准备运行环境。Kokoro 适合多音色朗读，ZipVoice 支持中英文声音克隆。</p>
                  <div class="settings-models">
                    <SettingsModelRow
                      v-for="model in ttsModels"
                      :key="model.id"
                      :name="model.name"
                      :path="model.hasFiles ? model.path : undefined"
                      :size-bytes="model.sizeBytes"
                      :loaded="model.loaded"
                      :status="model.installed ? '已安装' : model.hasFiles ? '文件不完整' : undefined"
                    >
                        <template #details>
                          <template v-if="model.recommended"> · 推荐</template>
                          <template v-if="!model.hasFiles"> · {{ model.package }}</template>
                        </template>
                        <span v-if="ttsModelActionLabel(model.id)" class="settings-muted" role="status">{{ ttsModelActionLabel(model.id) }}</span>
                        <button type="button" class="settings-link" :disabled="savingDownloadSource" @click="editDownloadSource({ kind: 'tts', model })">修改下载源</button>
                        <button type="button" class="settings-link" :disabled="savingDownloadSource" @click="openModelDownloadSource(true, model.id)">访问下载源</button>
                        <button v-if="model.installed" type="button" class="settings-btn" :disabled="ttsModelBusy" @click="manageTtsModel(model, 'load')">
                          {{ model.loaded ? '重新加载' : '加载' }}
                        </button>
                        <button v-else type="button" class="settings-btn" :disabled="ttsModelBusy || savingDownloadSource" @click="installTtsModel(model, 'settings')">
                          {{ ttsInstalling === model.id ? '正在下载…' : '下载' }}
                        </button>
                        <button v-if="model.loaded" type="button" class="settings-btn" :disabled="ttsModelBusy" @click="manageTtsModel(model, 'release')">释放模型</button>
                        <button v-if="model.hasFiles" type="button" class="settings-btn is-danger" :disabled="ttsModelBusy" @click="confirmDeleteTtsModel(model)">删除</button>
                    </SettingsModelRow>
                  </div>
                  <p v-if="ttsModelsLoading && !ttsModels.length" class="settings-hint">正在扫描 TTS 模型…</p>
                  <div v-if="ttsInstalling" class="settings-download">
                    <div class="settings-download-bar"><span class="settings-download-fill" :style="{ width: `${ttsDownloadPercent}%` }"></span></div>
                    <p class="settings-hint">{{ ttsDownloadProgress ? `${ttsDownloadProgress.file} · ${formatBytes(ttsDownloadProgress.received)} / ${formatBytes(ttsDownloadProgress.total)} · ${ttsDownloadPercent}%` : '正在准备安全校验的模型下载…' }}</p>
                    <button type="button" class="settings-btn" :disabled="ttsInstallCancelling" @click="cancelTtsInstall()">{{ ttsInstallCancelling ? '正在取消…' : '取消下载' }}</button>
                  </div>
                  <div v-else-if="ttsLoadProgress && !ttsGpuInstalling" class="settings-download" role="status" aria-live="polite">
                    <p class="settings-hint">{{ ttsLoadProgress.stage }}</p>
                    <button type="button" class="settings-btn" :disabled="ttsLoadCancelling" @click="cancelTtsLoad">{{ ttsLoadCancelling ? '正在取消…' : '取消加载' }}</button>
                  </div>
                  <p v-else-if="!ttsModels.some(model => model.installed)" class="settings-hint">模型安装后才会出现在“文生语音”可选项中。</p>
              </div>
            </section>

            <!-- ============ 第三方接入 ============ -->
            <section v-else class="settings-group" role="tabpanel" aria-label="第三方接入">
              <div class="settings-group-head">
                <h3>第三方接入</h3>
                <p>本机 HTTP 接口的鉴权、接口说明与浏览器跨域策略。</p>
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>Token 配置</h4>
                  <span class="settings-muted">{{ apiStateLabel }} · {{ apiBase }}</span>
                </div>
                <p class="settings-hint">
                  token 为空时不做鉴权；设置后所有请求需携带
                  <code>Authorization: Bearer &lt;token&gt;</code>，保存后立即生效。
                </p>
                <div class="settings-token-row">
                  <input
                    v-model="tokenDraft"
                    class="settings-input"
                    :type="tokenVisible ? 'text' : 'password'"
                    autocomplete="off"
                    spellcheck="false"
                    placeholder="留空表示不启用鉴权"
                    aria-label="API token"
                  />
                  <button
                    type="button"
                    class="settings-btn"
                    :title="tokenVisible ? '隐藏 token' : '显示 token'"
                    @click="tokenVisible = !tokenVisible"
                  >
                    {{ tokenVisible ? '隐藏' : '显示' }}
                  </button>
                  <button
                    type="button"
                    class="settings-btn is-primary"
                    :disabled="savingToken"
                    @click="saveToken"
                  >
                    {{ savingToken ? '保存中…' : '保存 token' }}
                  </button>
                </div>
                <div class="settings-inline">
                  <button type="button" class="settings-link" @click="copyApiBase">
                    {{ copied ? '已复制' : '复制接口地址' }}
                  </button>
                  <span v-if="apiStatus?.error" class="settings-hint is-warn">{{ apiStatus.error }}</span>
                </div>
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>当前开放接口</h4>
                  <span class="settings-muted">
                    共 {{ apiEndpoints.length }} 个 · {{ apiEndpointGroups.length }} 类
                  </span>
                </div>
                <div class="settings-endpoints">
                  <section
                    v-for="group in apiEndpointGroups"
                    :key="group.id"
                    class="settings-endpoint-group"
                  >
                    <div class="settings-endpoint-group-head">
                      <h5>{{ group.label }}</h5>
                      <span class="settings-endpoint-group-count">{{ group.endpoints.length }}</span>
                      <p>{{ group.description }}</p>
                    </div>
                    <article
                      v-for="endpoint in group.endpoints"
                      :key="`${endpoint.method} ${endpoint.path}`"
                      class="settings-endpoint"
                    >
                      <div class="settings-endpoint-head">
                        <span
                          class="settings-method"
                          :class="`is-${endpoint.method.toLowerCase()}`"
                        >
                          {{ endpoint.method }}
                        </span>
                        <code class="settings-endpoint-path">{{ endpoint.path }}</code>
                        <span class="settings-endpoint-summary">{{ endpoint.summary }}</span>
                      </div>
                      <p class="settings-endpoint-desc">{{ endpoint.description }}</p>
                    </article>
                  </section>
                  <p v-if="apiEndpoints.length === 0" class="settings-hint">正在读取接口清单…</p>
                </div>
              </div>

              <div class="settings-subgroup">
                <div class="settings-subgroup-head">
                  <h4>CORS 配置</h4>
                  <span class="settings-muted">应用自身与 localhost 始终放行</span>
                </div>
                <div class="settings-segments" role="radiogroup" aria-label="跨域策略">
                  <button
                    v-for="mode in CORS_MODES"
                    :key="mode.id"
                    type="button"
                    class="settings-segment"
                    :class="{ 'is-active': corsMode === mode.id }"
                    role="radio"
                    :aria-checked="corsMode === mode.id"
                    @click="corsMode = mode.id"
                  >
                    {{ mode.label }}
                  </button>
                </div>
                <p class="settings-hint">{{ corsHint }}</p>
                <textarea
                  v-if="corsMode === 'custom'"
                  v-model="corsOriginsText"
                  class="settings-textarea"
                  rows="4"
                  spellcheck="false"
                  placeholder="每行一个来源，例如：&#10;https://example.com&#10;http://localhost:3000"
                  aria-label="自定义跨域来源"
                ></textarea>
                <p v-if="corsMode === 'custom'" class="settings-hint">
                  来源需带 http:// 或 https:// 前缀，最多 16 条；无效或重复的条目会被忽略，
                  <code>*</code> 等价于允许所有来源。
                </p>
                <div class="settings-inline">
                  <button
                    type="button"
                    class="settings-btn is-primary"
                    :disabled="savingCors"
                    @click="saveCors"
                  >
                    {{ savingCors ? '保存中…' : '保存 CORS 配置' }}
                  </button>
                </div>
              </div>
            </section>
          </div>
        </div>
      </section>
    </div>
  </Teleport>
</template>
