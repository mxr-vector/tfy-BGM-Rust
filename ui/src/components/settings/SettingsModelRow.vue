<script setup lang="ts">
import { formatBytes } from '@/lib/format';

withDefaults(defineProps<{
  name: string;
  sizeBytes: number;
  /** 覆盖体积文案，用于「免下载」这类在线引擎。 */
  sizeLabel?: string;
  path?: string;
  status?: string;
  loaded?: boolean;
  defaultModel?: boolean;
}>(), { loaded: false, defaultModel: false });
</script>

<template>
  <article class="settings-model" :class="{ 'is-loaded': loaded }">
    <div class="settings-model-main">
      <div class="settings-model-title">
        <strong>{{ name }}</strong>
        <span v-if="defaultModel" class="settings-badge">默认</span>
        <span v-if="loaded || status" class="settings-badge" :class="{ 'is-accent': loaded }">
          {{ loaded ? '已加载' : status }}
        </span>
      </div>
      <p v-if="path" class="settings-model-path" :title="path">{{ path }}</p>
      <p class="settings-model-meta">
        {{ sizeLabel ?? formatBytes(sizeBytes) }}<slot name="details" />
      </p>
    </div>
    <div class="settings-model-actions">
      <slot />
    </div>
  </article>
</template>

<style scoped>
.settings-model-actions {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  justify-content: flex-end;
  gap: 6px;
}
</style>
