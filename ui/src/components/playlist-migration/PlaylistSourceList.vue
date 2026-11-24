<script setup lang="ts">
import type { DeepReadonly } from 'vue';
import type { PlaylistSource } from '@/composables/usePlaylistMigration';

defineProps<{ sources: DeepReadonly<PlaylistSource[]> }>();
const emit = defineEmits<{ retry: [id: number] }>();

const STATUS_LABELS = {
  queued: '等待中',
  loading: '读取中',
  success: '读取成功',
  partial: '部分读取',
  error: '读取失败',
} satisfies Record<PlaylistSource['status'], string>;

const PLATFORM_LABELS = {
  netease: '网易云音乐',
  qq: 'QQ 音乐',
  qishui: '汽水音乐',
};
</script>

<template>
  <section class="migration-panel" aria-labelledby="playlist-source-list-title">
    <h2 id="playlist-source-list-title">来源 <span v-if="sources.length" class="migration-hint">{{ sources.length }}</span></h2>
    <p v-if="!sources.length" class="empty migration-hint">尚未解析歌单链接。</p>
    <ol v-else class="source-list" role="list">
      <li v-for="(source, index) in sources" :key="source.id" class="source-row">
        <div class="source-heading">
          <h3>{{ index + 1 }}. {{ source.result?.title || '歌单链接' }}</h3>
          <span class="source-status" :class="`status-${source.status}`" role="status">
            {{ STATUS_LABELS[source.status] }}
          </span>
        </div>
        <p class="source-url migration-hint">{{ source.url }}</p>
        <p v-if="source.result" class="migration-hint">
          {{ PLATFORM_LABELS[source.result.platform] }} · 已读 {{ source.result.tracks.length }} 首<span
            v-if="source.result.expectedTotal !== null"
          > / 共 {{ source.result.expectedTotal }} 首</span>
        </p>
        <p v-if="source.result?.warning != null" class="source-warning">{{ source.result.warning || '未能读取完整歌单' }}</p>
        <p v-if="source.error" class="source-error">{{ source.error }}</p>
        <button
          v-if="source.status === 'partial' || source.status === 'error'"
          class="migration-button retry"
          type="button"
          :aria-label="`重试来源 ${index + 1}`"
          @click="emit('retry', source.id)"
        >重试</button>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.empty { margin-top: 8px; }
.source-list { list-style: none; }
.source-row { display: grid; gap: 4px; padding-top: 10px; margin-top: 10px; border-top: 1px solid var(--line); }
.source-heading { display: flex; align-items: flex-start; justify-content: space-between; gap: 8px; }
h3 { min-width: 0; overflow-wrap: anywhere; font-size: 13px; }
.source-status {
  flex: none;
  padding: 1px 6px;
  border-radius: var(--radius-tag);
  color: var(--text-2);
  background: rgba(255, 255, 255, 0.055);
  font-size: 11px;
}
.status-success { color: var(--accent-bright); background: var(--accent-soft); }
.status-partial, .source-warning { color: #fde68a; }
.status-error, .source-error { color: #ffc4c4; }
.source-url, .source-warning, .source-error { overflow-wrap: anywhere; user-select: text; }
.source-warning, .source-error { font-size: 12px; }
.retry { margin-top: 2px; justify-self: end; }
</style>
