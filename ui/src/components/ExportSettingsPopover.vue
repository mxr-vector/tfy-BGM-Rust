<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
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
import Icon from './Icon.vue';

const open = computed(() => playerPopover.value === 'export');
const loading = ref(false);
const info = ref<ExportInfo | null>(null);
const wrapper = ref<HTMLElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);

const enabled = computed(() => state.prefs?.loudnessNormalization ?? true);
const scene = computed(() => state.prefs?.exportScene ?? '');
const presets = computed(() => info.value?.presets ?? []);
const basePreset = computed(
  () =>
    presets.value.find((preset) => preset.id === DEFAULT_EXPORT_SCENE) ??
    presets.value[0] ??
    null,
);
const activePreset = computed(
  () => presets.value.find((preset) => preset.id === scene.value) ?? null,
);
const effectivePreset = computed(() => activePreset.value ?? basePreset.value);
const triggerTitle = computed(() => {
  if (!enabled.value) return '导出设置：仅复制原文件';
  if (activePreset.value) {
    return `导出设置：响度归一化 · ${activePreset.value.label}（${activePreset.value.targetLufs} LUFS）`;
  }
  return effectivePreset.value
    ? `导出设置：响度归一化 · 默认 ${effectivePreset.value.targetLufs} LUFS`
    : '导出设置：响度归一化';
});
const toggleHint = computed(() => {
  if (!enabled.value) return '已关闭 · 仅复制原文件';
  if (activePreset.value) {
    return `已开启 · 目标 ${activePreset.value.targetLufs} LUFS · ${activePreset.value.label}`;
  }
  return effectivePreset.value
    ? `已开启 · 默认人耳响度 ${effectivePreset.value.targetLufs} LUFS`
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

function close(focusTrigger = false): void {
  if (!open.value) return;
  closePlayerPopover('export');
  if (focusTrigger) trigger.value?.focus();
}

function toggleOpen(): void {
  togglePlayerPopover('export');
  if (!open.value) return;
  void loadInfo();
  void nextTick(() => {
    panel.value?.querySelector<HTMLElement>('.export-toggle')?.focus();
  });
}

function toggleNormalization(): void {
  if (!state.prefs) return;
  // 场景预设是归一化之上的修饰；关闭归一化时保留已选场景，方便下次恢复。
  setExportSettings(!enabled.value, scene.value);
}

function chooseScene(id: string): void {
  if (!state.prefs) return;
  if (id === scene.value && enabled.value) {
    // 再次点击当前场景：取消套用，回到默认人耳响度归一化。
    setExportSettings(true, '');
    return;
  }
  // 点选场景即表示要按该目标归一化，避免“选中了但没生效”的歧义。
  setExportSettings(true, id);
}

function onDocumentClick(event: MouseEvent): void {
  if (!open.value) return;
  const target = event.target as Node | null;
  if (target && wrapper.value?.contains(target)) return;
  close();
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape' || !open.value) return;
  event.preventDefault();
  event.stopPropagation();
  close(true);
}

onMounted(() => {
  document.addEventListener('click', onDocumentClick);
  document.addEventListener('keydown', onKeydown);
});

onBeforeUnmount(() => {
  document.removeEventListener('click', onDocumentClick);
  document.removeEventListener('keydown', onKeydown);
});
</script>

<template>
  <div ref="wrapper" class="export-wrap">
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
      @click.stop="toggleOpen"
    >
      <Icon name="sliders" />
    </button>

    <div
      v-if="open"
      ref="panel"
      class="export-popover"
      id="export-popover"
      role="dialog"
      aria-label="导出设置"
      @click.stop
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
        场景预设
        <span class="export-section-hint">可选 · 在默认归一化上调整</span>
      </div>
      <div v-if="loading && presets.length === 0" class="export-loading">
        正在检测 ffmpeg…
      </div>
      <div v-else class="export-scenes" role="listbox" aria-label="场景预设">
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
        当前仅复制原文件；开启后默认按人耳响度目标归一化。
      </p>
      <p v-else-if="!activePreset" class="export-scene-desc">
        未套用场景预设，按默认人耳响度目标
        {{ effectivePreset?.targetLufs ?? 14 }} LUFS 导出；点选上方场景可调整。
      </p>
      <p v-else class="export-scene-desc">{{ activePreset.description }}</p>

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
    </div>
  </div>
</template>