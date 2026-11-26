<script setup lang="ts">
import { computed } from 'vue';
import { formatBytes } from '@/lib/format';
import type { AudioIndexProgress, AudioIndexStatus } from '@/lib/types';
import Icon from '@/components/Icon.vue';

const props = defineProps<{
  status: AudioIndexStatus | null;
  busy: boolean;
  progress: AudioIndexProgress | null;
  percent: number;
}>();
const emit = defineEmits<{ build: []; disable: [] }>();

const stateLabel = computed(() => {
  if (props.busy) return '正在建立 / 更新索引';
  if (!props.status) return '正在读取索引状态';
  return props.status.enabled ? '索引已启用，自动同步' : '当前目录尚未建立索引';
});
</script>

<template>
  <section class="index-status" :class="{ 'is-ready': status?.enabled }" aria-label="音频索引状态">
    <div class="index-status-icon"><Icon name="wave" /></div>
    <div class="index-status-copy">
      <strong>{{ stateLabel }}</strong>
      <span v-if="status?.enabled">
        {{ status.readyTracks }} 首可检索 · {{ status.failedTracks }} 首分析失败 · 音频共
        {{ formatBytes(status.sizeBytes) }}
      </span>
      <span v-else>索引包含 BPM、调性、响度、波形与音频指纹，仅保存在本机。</span>
      <div v-if="busy && progress" class="index-progress">
        <div class="index-progress-track"><i :style="{ width: `${percent}%` }"></i></div>
        <span>{{ percent }}% · {{ progress.title }}</span>
      </div>
    </div>
    <div class="index-actions">
      <button type="button" :disabled="busy" @click="emit('build')">
        <Icon name="refresh" /> {{ status?.enabled ? '更新索引' : '建立音频索引' }}
      </button>
      <button v-if="status?.enabled" type="button" :disabled="busy" @click="emit('disable')">关闭并删除索引</button>
    </div>
  </section>
</template>

<style scoped>
.index-status {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 14px 16px;
  border: 1px solid rgba(255, 255, 255, 0.09);
  border-radius: 14px;
  background: rgba(255, 255, 255, 0.035);
}
.index-status.is-ready { border-color: rgba(25, 217, 139, 0.25); }
.index-status-icon {
  display: grid;
  place-items: center;
  width: 38px;
  height: 38px;
  flex: none;
  border-radius: 12px;
  color: #6ff3bc;
  background: rgba(25, 217, 139, 0.1);
}
.index-status-copy { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 4px; }
.index-status-copy strong { color: var(--text); font-size: 13px; }
.index-status-copy > span { overflow: hidden; color: var(--text-3); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.index-actions { display: flex; flex-wrap: wrap; gap: 8px; }
.index-status button, .empty-action {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 7px;
  min-height: 34px;
  padding: 0 13px;
  border-radius: 9px;
  color: #baf7dc;
  background: rgba(25, 217, 139, 0.12);
}
.index-status button:disabled { opacity: 0.55; cursor: wait; }
.index-progress { display: flex; align-items: center; gap: 9px; color: var(--text-3); font-size: 10px; }
.index-progress-track { width: min(220px, 35vw); height: 4px; overflow: hidden; border-radius: 99px; background: rgba(255, 255, 255, 0.09); }
.index-progress-track i { display: block; height: 100%; border-radius: inherit; background: #19d98b; transition: width 120ms linear; }
@media (max-width: 720px) {
  .index-status { flex-wrap: wrap; }
  .index-status button { width: 100%; }
}
</style>
