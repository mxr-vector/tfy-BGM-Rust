<script setup lang="ts">
import { formatBytes, formatTime } from '@/lib/format';
import type { DuplicateGroup, DuplicateReport } from '@/lib/types';
import Icon from '../Icon.vue';

defineProps<{
  report: DuplicateReport | null;
  selected: ReadonlySet<string>;
  loading: boolean;
}>();
const emit = defineEmits<{ toggle: [path: string] }>();

function duration(ms: number | null): string {
  return ms ? formatTime(ms / 1000) : '—';
}
function groupLabel(group: DuplicateGroup): string {
  return group.matchKind === 'exact' ? '指纹完全一致' : '高度相似';
}
</script>

<template>
  <section class="duplicate-list" aria-label="重复文件列表">
    <div v-if="loading" class="empty-state"><span class="loading-ring"></span>正在比对本地指纹…</div>
    <div v-else-if="report && report.groups.length === 0" class="empty-state">
      <Icon name="check-circle" />
      <strong>没有发现重复音频</strong>
      <span>已检查 {{ report.indexedTracks }} 首可检索曲目。</span>
    </div>
    <div v-else-if="!report" class="empty-state">
      <Icon name="disc" />
      <strong>等待扫描重复文件</strong>
      <span>不会默认选中任何文件。</span>
    </div>
    <article v-for="group in report?.groups ?? []" :key="group.id" class="duplicate-group">
      <header>
        <div><span class="match-badge">{{ groupLabel(group) }}</span><strong>{{ group.tracks.length }} 个文件</strong></div>
        <span>匹配度 {{ Math.round(group.score * 100) }}%</span>
      </header>
      <label v-for="(track, index) in group.tracks" :key="track.id" class="duplicate-track" :class="{ 'is-selected': selected.has(track.path) }">
        <input type="checkbox" :checked="selected.has(track.path)" @change="emit('toggle', track.path)" />
        <span class="track-copy">
          <strong :title="track.fileName">{{ track.fileName }}</strong>
          <span :title="track.path">{{ track.path }}</span>
        </span>
        <span v-if="index === 0" class="keep-hint">建议保留</span>
        <span class="track-meta">
          <span>{{ duration(track.durationMs) }} · {{ formatBytes(track.sizeBytes) }}</span>
          <span>{{ track.bpm ? `${track.bpm.toFixed(1)} BPM` : 'BPM —' }} · {{ track.musicalKey ?? '调性 —' }}</span>
        </span>
      </label>
    </article>
  </section>
</template>

<style scoped>
.duplicate-list { display: flex; flex: 1; min-height: 0; flex-direction: column; gap: 12px; overflow-y: auto; }
.duplicate-group { overflow: hidden; flex: none; border: 1px solid rgba(255, 255, 255, 0.085); border-radius: 14px; background: rgba(255, 255, 255, 0.028); }
.duplicate-group > header { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 11px 14px; color: var(--text-3); font-size: 10.5px; background: rgba(255, 255, 255, 0.025); }
.duplicate-group > header > div { display: flex; align-items: center; gap: 9px; }
.duplicate-group > header strong { color: var(--text-2); font-size: 11px; }
.match-badge { padding: 3px 7px; border-radius: 99px; color: #9df7cf; font-size: 9px; background: rgba(25, 217, 139, 0.1); }
.duplicate-track { display: flex; align-items: center; gap: 12px; padding: 12px 14px; border-top: 1px solid rgba(255, 255, 255, 0.055); cursor: pointer; transition: background 120ms ease; }
.duplicate-track:hover, .duplicate-track.is-selected { background: rgba(25, 217, 139, 0.065); }
.duplicate-track input { width: 16px; height: 16px; flex: none; accent-color: #19d98b; }
.track-copy { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 4px; }
.track-copy strong { overflow: hidden; color: var(--text); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.track-copy span { overflow: hidden; color: var(--text-3); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
.track-meta { display: flex; flex: none; flex-direction: column; gap: 4px; color: var(--text-3); font-size: 10px; text-align: right; }
.keep-hint { flex: none; padding: 3px 7px; border: 1px solid rgba(25, 217, 139, 0.2); border-radius: 99px; color: #9df7cf; font-size: 9px; }
.empty-state { display: flex; flex: 1; min-height: 180px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: var(--text-3); font-size: 12px; }
.empty-state strong { color: var(--text-2); font-size: 14px; }
.empty-state [data-icon] { width: 30px; height: 30px; color: #6ff3bc; }
.loading-ring { width: 24px; height: 24px; border: 2px solid rgba(255, 255, 255, 0.12); border-top-color: #6ff3bc; border-radius: 50%; animation: spin 800ms linear infinite; }
@keyframes spin { to { transform: rotate(360deg); } }
@media (max-width: 720px) { .track-meta { display: none; } }
</style>
