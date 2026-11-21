<script setup lang="ts">
/**
 * 基本系统配置：顶栏齿轮打开的三个分组。
 *
 * - 常规：字体、开机自动启动、最小化托盘、GPU 加速，以及当前平台特有的集成项；
 * - 播放：启动自动播放、防休眠、淡入淡出与输出设备；
 * - 第三方接入：token、开放接口清单与 CORS。
 */

import { computed, nextTick, ref, watch } from 'vue';
import { toast } from '@/lib/toast';
import { wakeLockSupported } from '@/composables/useSystemIntegration';
import type { CorsConfig, CorsMode, OutputRoute } from '@/lib/types';
import { apiStatus, refreshApiInfo } from '@/stores/api';
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
import FontSelect from './FontSelect.vue';
import Icon from './Icon.vue';
import SettingSwitch from './SettingSwitch.vue';

const prefs = computed(() => state.prefs);

const TABS: { id: SettingsTab; label: string; hint: string; icon: string }[] = [
  { id: 'general', label: '常规', hint: '字体与系统集成', icon: 'sliders' },
  { id: 'playback', label: '播放', hint: '启动行为与输出', icon: 'wave' },
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

function selectTab(tab: SettingsTab): void {
  settingsTab.value = tab;
  if (tab === 'playback') void refreshOutputs();
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
  if (settingsTab.value === 'playback') void refreshOutputs();
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
            <p class="settings-sub">字体与系统集成 · 播放行为 · 第三方接入</p>
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
                  <span>界面文字使用的字体；系统未安装时自动回退</span>
                </div>
                <div class="settings-font">
                  <FontSelect
                    :model-value="prefs?.uiFont ?? 'system'"
                    @update:model-value="setUiFont"
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
                  <span class="settings-muted">共 {{ apiEndpoints.length }} 个</span>
                </div>
                <div class="settings-endpoints">
                  <article
                    v-for="endpoint in apiEndpoints"
                    :key="`${endpoint.method} ${endpoint.path}`"
                    class="settings-endpoint"
                  >
                    <div class="settings-endpoint-head">
                      <span class="settings-method" :class="`is-${endpoint.method.toLowerCase()}`">
                        {{ endpoint.method }}
                      </span>
                      <code class="settings-endpoint-path">{{ endpoint.path }}</code>
                      <span class="settings-endpoint-summary">{{ endpoint.summary }}</span>
                    </div>
                    <p class="settings-endpoint-desc">{{ endpoint.description }}</p>
                  </article>
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
