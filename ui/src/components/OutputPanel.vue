<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';
import {
  closePlayerPopover,
  playerPopover,
  togglePlayerPopover,
} from '@/composables/usePlayerPopover';
import { clamp } from '@/lib/format';
import { toast } from '@/lib/toast';
import { apiStatus, refreshApiInfo } from '@/stores/api';
import type { OutputRow } from '@/stores/outputs';
import {
  applyOutputRoutes,
  describeOutputIssue,
  outputRows,
  outputsState,
  refreshDevices,
  requestDeviceAccess,
  setCrossfadeSeconds,
  setOutputVolume,
  toggleOutput,
  toggleOutputMute,
} from '@/stores/outputs';
import { state } from '@/stores/state';
import AppDropdown from './AppDropdown.vue';
import Icon from './Icon.vue';

const open = computed(() => playerPopover.value === 'output');
const rows = computed(() => outputRows.value);
const crossfade = computed(() => state.prefs?.crossfadeSeconds ?? 0);
const trigger = ref<HTMLButtonElement | null>(null);

const CROSSFADE_OPTIONS = [0, 0.5, 1, 2, 3, 5] as const;

const activeCount = computed(
  () => (state.prefs?.outputRoutes ?? []).filter((route) => route.enabled).length,
);
const issue = computed(() => describeOutputIssue());
const apiState = computed(() => {
  const status = apiStatus.value;
  if (!status) return '未启用';
  if (!status.enabled) return '已禁用';
  if (!status.running) return '未监听';
  return status.ready ? '运行中' : '启动中';
});
const crossfadeLabel = computed(() => {
  const value = crossfade.value;
  if (value <= 0) return '已关闭';
  return `${Number(value.toFixed(2))} 秒`;
});

function toggleOpen(): void {
  togglePlayerPopover('output');
  if (!open.value) return;
  void refreshDevices().then(() => applyOutputRoutes());
  void refreshApiInfo();
  void nextTick(() => {
    document
      .getElementById('output-popover')
      ?.querySelector<HTMLElement>('.output-row')
      ?.focus();
  });
}

function onVolumeInput(deviceId: string, event: Event): void {
  const value = Number((event.target as HTMLInputElement).value);
  setOutputVolume(deviceId, clamp(value, 0, 1), false);
}

/** 未授权时只能看到默认设备；授权后立即重扫并恢复已保存的路由。 */
async function unlockDevices(): Promise<void> {
  const ok = await requestDeviceAccess();
  if (!ok) {
    toast(describeOutputIssue() || '未能读取输出设备列表', 'error');
    return;
  }
  await applyOutputRoutes();
  toast(`已发现 ${outputsState.devices.length} 个输出设备`, 'success');
}

function onCrossfade(seconds: number): void {
  setCrossfadeSeconds(seconds);
}

function rowSubtitle(row: OutputRow): string {
  if (row.missing) {
    return outputsState.labelsAvailable
      ? '未连接 · 插回设备后自动恢复'
      : '待授权 · 读取设备列表后管理';
  }
  if (!row.route?.enabled) return row.deviceId === 'default' ? '未启用' : '点击开启这一路输出';
  if (row.route.muted) return '已静音 · 独立音量';
  return row.route.deviceId === 'default' ? '跟随系统默认设备' : '独立音量';
}

function percentOf(row: OutputRow): number {
  return Math.round((row.route?.volume ?? 1) * 100);
}

async function copyApiBase(): Promise<void> {
  const url = `${apiStatus.value?.baseUrl ?? 'http://127.0.0.1:8737'}/v1`;
  try {
    await navigator.clipboard.writeText(url);
    toast('已复制接口地址', 'success');
  } catch {
    toast('复制失败，请手动选择地址', 'error');
  }
}

</script>

<template>
  <div class="output-wrap">
    <button
      ref="trigger"
      type="button"
      class="action-btn"
      id="btn-output"
      :class="{ 'is-open': open }"
      :title="`播放输出：${activeCount} 路 · 淡入淡出 ${crossfadeLabel}`"
      aria-label="播放输出设置"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :aria-controls="open ? 'output-popover' : undefined"
      @click.stop="toggleOpen"
    >
      <Icon name="headphones" />
      <span v-if="activeCount > 1" class="output-badge" aria-hidden="true">{{ activeCount }}</span>
    </button>

    <AppDropdown
      :open="open"
      :anchor="trigger"
      side="top"
      align="end"
      :width="328"
      :max-height="520"
      panel-class="output-popover"
      panel-id="output-popover"
      role="dialog"
      label="播放输出设置"
      @close="closePlayerPopover('output')"
    >
      <div class="output-pop-head">
        <div class="output-pop-title">
          <strong>播放输出</strong>
          <span>多设备独立音量 · 切歌淡入淡出</span>
        </div>
        <button
          type="button"
          class="output-refresh"
          title="重新扫描输出设备"
          @click="refreshDevices().then(() => applyOutputRoutes())"
        >
          <Icon name="refresh" />
        </button>
      </div>

      <div class="output-section">
        <div class="output-section-title">
          <span>切歌淡入淡出</span>
          <span class="output-section-value">{{ crossfadeLabel }}</span>
        </div>
        <div class="output-fades" role="listbox" aria-label="淡入淡出时长">
          <button
            v-for="option in CROSSFADE_OPTIONS"
            :key="option"
            type="button"
            class="output-fade"
            :class="{ 'is-active': Math.abs(crossfade - option) < 0.01 }"
            role="option"
            :aria-selected="Math.abs(crossfade - option) < 0.01"
            @click="onCrossfade(option)"
          >
            {{ option === 0 ? '关' : `${option}s` }}
          </button>
        </div>
        <p class="output-hint">
          切歌时旧曲在设定时长内淡出、新曲同时淡入；0 秒为硬切。
        </p>
      </div>

      <div class="output-section">
        <div class="output-section-title">
          <span>输出设备</span>
          <button
            v-if="!outputsState.labelsAvailable"
            type="button"
            class="output-unlock"
            :disabled="outputsState.busy"
            title="Chromium 需要一次音频设备授权才会返回完整设备列表，过程不会录音"
            @click="unlockDevices"
          >
            {{ outputsState.busy ? '读取中…' : '读取设备列表' }}
          </button>
          <span v-else class="output-section-value">{{ rows.length }} 个设备</span>
        </div>

        <p v-if="!outputsState.labelsAvailable" class="output-hint">
          仅显示默认设备；授权一次音频设备访问（不会录音）后即可独立路由到耳机 / 虚拟声卡。
        </p>

        <div class="output-list">
          <div
            v-for="row in rows"
            :key="row.deviceId"
            class="output-row"
            :class="{
              'is-off': !row.route?.enabled,
              'is-missing': row.missing,
            }"
            tabindex="0"
          >
            <button
              type="button"
              class="output-switch"
              :class="{ 'is-on': row.route?.enabled }"
              role="switch"
              :aria-checked="Boolean(row.route?.enabled)"
              :title="row.route?.enabled ? '关闭这一路输出' : '开启这一路输出'"
              @click="toggleOutput(row.deviceId)"
            >
              <span class="output-switch-thumb"></span>
            </button>

            <div class="output-meta">
              <span class="output-name" :title="row.deviceId">{{ row.label }}</span>
              <span class="output-sub">{{ rowSubtitle(row) }}</span>
            </div>

            <div class="output-gain">
              <input
                type="range"
                class="output-range"
                min="0"
                max="1"
                step="0.01"
                :value="row.route?.volume ?? 1"
                :disabled="!row.route?.enabled"
                :aria-label="`${row.label} 音量`"
                :style="{ '--level': `${percentOf(row)}%` }"
                @input="onVolumeInput(row.deviceId, $event)"
              />
              <span class="output-percent">{{ percentOf(row) }}%</span>
              <button
                type="button"
                class="output-mute"
                :class="{ 'is-muted': row.route?.muted }"
                :disabled="!row.route?.enabled"
                :title="row.route?.muted ? '取消这一路静音' : '只静音这一路'"
                @click="toggleOutputMute(row.deviceId)"
              >
                <Icon :name="row.route?.muted ? 'volume-mute' : 'volume-low'" />
              </button>
            </div>
          </div>
        </div>
        <p class="output-hint">
          主音量是全局音量，每路音量叠加在它之上；静音只影响对应设备。
        </p>
      </div>

      <div class="output-section">
        <div class="output-section-title">
          <span>开放接口</span>
          <span class="output-section-value">{{ apiState }}</span>
        </div>
        <div class="output-api">
          <code>{{ apiStatus?.baseUrl ?? 'http://127.0.0.1:8737' }}/v1</code>
          <button type="button" @click="copyApiBase">复制</button>
        </div>
        <p class="output-hint">
          状态 /v1/state · 播放 /v1/play · 事件 /v1/events{{
            apiStatus?.token ? ' · 已启用 token' : ' · 仅监听本机'
          }}
        </p>
        <p v-if="apiStatus?.error" class="output-hint is-warn">{{ apiStatus.error }}</p>
      </div>

      <div class="output-foot" :class="{ 'is-warn': Boolean(issue) }">
        <Icon :name="issue ? 'alert' : 'check'" />
        <span>
          {{
            issue ||
            (activeCount > 1
              ? `已启用 ${activeCount} 路输出，各自独立音量`
              : '当前仅系统默认输出，开启其它设备即可同时播放')
          }}
        </span>
      </div>
    </AppDropdown>
  </div>
</template>
