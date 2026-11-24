<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';
import {
  closePlayerPopover,
  playerPopover,
  togglePlayerPopover,
} from '@/composables/usePlayerPopover';
import { COMMANDS, DEFAULT_EXPORT_SCENE } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type { ExportInfo } from '@/lib/types';
import { setExportSettings } from '@/stores/prefs';
import { state } from '@/stores/state';
import AppDropdown from './AppDropdown.vue';
import Icon from './Icon.vue';

const open = computed(() => playerPopover.value === 'export');
const loading = ref(false);
const info = ref<ExportInfo | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);

const enabled = computed(() => state.prefs?.loudnessNormalization ?? true);
// 历史偏好可能存空串；空串等价于默认目标，统一映射到默认场景。
const scene = computed(() => state.prefs?.exportScene || DEFAULT_EXPORT_SCENE);
const presets = computed(() => info.value?.presets ?? []);
const selectedPreset = computed(
  () => presets.value.find((preset) => preset.id === scene.value) ?? presets.value[0] ?? null,
);
const triggerTitle = computed(() => {
  if (!enabled.value) return '导出设置：仅复制原文件';
  return selectedPreset.value
    ? `导出设置：响度归一化 · ${selectedPreset.value.label}（${selectedPreset.value.targetLufs} LUFS）`
    : '导出设置：响度归一化';
});
const toggleHint = computed(() => {
  if (!enabled.value) return '已关闭 · 仅复制原文件';
  return selectedPreset.value
    ? `已开启 · 目标 ${selectedPreset.value.targetLufs} LUFS · ${selectedPreset.value.label}`
    : '已开启 · 人耳响度归一化';
});

async function loadInfo(): Promise<void> {
  if (loading.value) return;
  loading.value = true;
  try {
    info.value = await invoke<ExportInfo>(COMMANDS.exportInfo);
  } catch (error) {
    toast(`读取导出设置失败：${normalizeError(error)}`, 'error');
  } finally {
    loading.value = false;
  }
}

function toggleOpen(): void {
  togglePlayerPopover('export');
  if (!open.value) return;
  void loadInfo();
  void nextTick(() => {
    document
      .getElementById('export-popover')
      ?.querySelector<HTMLElement>('.export-toggle')
      ?.focus();
  });
}

function toggleNormalization(): void {
  if (!state.prefs) return;
  // 场景预设决定归一化目标；关闭归一化时保留所选目标，方便下次恢复。
  setExportSettings(!enabled.value, scene.value);
}

function chooseScene(id: string): void {
  if (!state.prefs) return;
  // 点选场景即表示按该目标归一化；重复点击当前项不改变任何设置。
  if (enabled.value && id === scene.value) return;
  setExportSettings(true, id);
}

</script>

<template>
  <div class="export-wrap">
    <button
      ref="trigger"
      type="button"
      class="action-btn"
      id="btn-export-settings"
      :class="{ 'is-open': open }"
      :title="triggerTitle"
      aria-label="导出设置"
      aria-haspopup="dialog"
      :aria-expanded="open"
      :aria-controls="open ? 'export-popover' : undefined"
      @click.stop="toggleOpen"
    >
      <Icon name="sliders" />
    </button>

    <AppDropdown
      :open="open"
      :anchor="trigger"
      side="top"
      align="end"
      :width="300"
      :max-height="440"
      panel-class="export-popover"
      panel-id="export-popover"
      role="dialog"
      label="导出设置"
      @close="closePlayerPopover('export')"
    >
      <div class="export-pop-head">
        <div class="export-pop-title">
          <strong>导出设置</strong>
          <span>单首与批量导出通用</span>
        </div>
        <Icon name="sliders" class="export-pop-icon" />
      </div>

      <button
        type="button"
        class="export-toggle"
        role="switch"
        :aria-checked="enabled"
        :disabled="!state.prefs"
        @click="toggleNormalization"
      >
        <span class="export-toggle-text">
          <span class="export-toggle-title">响度归一化</span>
          <span class="export-toggle-hint">{{ toggleHint }}</span>
        </span>
        <span class="switch" :class="{ 'is-on': enabled }" aria-hidden="true">
          <span class="switch-thumb"></span>
        </span>
      </button>

      <div class="export-section-label">
        归一化目标
        <span class="export-section-hint">选择导出用途，决定目标响度</span>
      </div>
      <div v-if="loading && presets.length === 0" class="export-loading">
        正在检测 ffmpeg…
      </div>
      <div v-else class="export-scenes" role="listbox" aria-label="归一化目标">
        <button
          v-for="preset in presets"
          :key="preset.id"
          type="button"
          class="export-scene"
          :class="{ 'is-active': enabled && preset.id === scene }"
          role="option"
          :aria-selected="enabled && preset.id === scene"
          :title="preset.description"
          @click="chooseScene(preset.id)"
        >
          <span class="export-scene-label">{{ preset.label }}</span>
          <span
            v-if="preset.id === DEFAULT_EXPORT_SCENE"
            class="export-scene-default"
          >默认</span>
          <span class="export-scene-lufs">{{ preset.targetLufs }} LUFS</span>
          <Icon name="check" class="export-scene-check" />
        </button>
        <button
          v-if="presets.length === 0"
          type="button"
          class="export-retry"
          @click="loadInfo"
        >
          场景预设加载失败，点击重试
        </button>
      </div>

      <p v-if="!enabled" class="export-scene-desc is-muted">
        当前仅复制原文件；开启后按所选目标归一化。
      </p>
      <p v-else-if="selectedPreset" class="export-scene-desc">
        {{ selectedPreset.description }}
      </p>

      <div
        class="export-pop-foot"
        :class="{ 'is-warn': info !== null && !info.ffmpeg }"
      >
        <Icon :name="info?.ffmpeg ? 'check' : 'alert'" />
        <span>
          {{
            info?.ffmpeg
              ? `ffmpeg 已就绪：${info.ffmpeg}`
              : loading
                ? '正在检测 ffmpeg…'
                : '未检测到 ffmpeg，导出将回退为复制原文件'
          }}
        </span>
      </div>
    </AppDropdown>
  </div>
</template>