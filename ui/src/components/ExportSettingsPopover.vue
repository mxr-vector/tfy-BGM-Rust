<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import {
  closePlayerPopover,
  playerPopover,
  togglePlayerPopover,
} from '@/composables/usePlayerPopover';
import { COMMANDS } from '@/lib/constants';
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
const scene = computed(() => state.prefs?.exportScene ?? 'video-editing');
const presets = computed(() => info.value?.presets ?? []);
const activePreset = computed(
  () => presets.value.find((preset) => preset.id === scene.value) ?? presets.value[0] ?? null,
);
const triggerTitle = computed(() => {
  const mode = enabled.value ? '响度归一化' : '仅复制原文件';
  return activePreset.value
    ? `导出设置：${mode} · ${activePreset.value.label}`
    : `导出设置：${mode}`;
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
  setExportSettings(!enabled.value, scene.value);
}

function chooseScene(id: string): void {
  if (id === scene.value) return;
  setExportSettings(enabled.value, id);
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
          <span class="export-toggle-hint">
            {{ enabled ? '已开启 · 导出时用 ffmpeg 统一电平' : '已关闭 · 仅复制原文件' }}
          </span>
        </span>
        <span class="switch" :class="{ 'is-on': enabled }" aria-hidden="true">
          <span class="switch-thumb"></span>
        </span>
      </button>

      <div class="export-section-label">场景预设</div>
      <div v-if="loading && presets.length === 0" class="export-loading">
        正在检测 ffmpeg…
      </div>
      <div v-else class="export-scenes" role="listbox" aria-label="场景预设">
        <button
          v-for="preset in presets"
          :key="preset.id"
          type="button"
          class="export-scene"
          :class="{ 'is-active': preset.id === scene }"
          role="option"
          :aria-selected="preset.id === scene"
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

      <p v-if="activePreset" class="export-scene-desc">{{ activePreset.description }}</p>

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