<script setup lang="ts">
import { useId } from 'vue';
import type { PlaylistCoverage, PlaylistFormat, PlaylistStats } from '../composables/usePlaylistMigration';

defineProps<{
  format: PlaylistFormat;
  stats: PlaylistStats;
  text: string;
  coverage: PlaylistCoverage;
  saving: boolean;
}>();
const emit = defineEmits<{
  'update:format': [format: PlaylistFormat];
  copy: [];
  save: [];
}>();

const previewId = useId();
const FORMATS: { value: PlaylistFormat; label: string }[] = [
  { value: 'title-artist', label: '歌名 - 歌手' },
  { value: 'artist-title', label: '歌手 - 歌名' },
  { value: 'title-only', label: '仅歌名' },
];
</script>

<template>
  <section class="migration-panel text-output" aria-labelledby="playlist-output-title">
    <h2 id="playlist-output-title"><label :for="previewId">文本预览</label></h2>
    <fieldset>
      <legend class="migration-hint">文本格式</legend>
      <div class="format-options">
        <label v-for="option in FORMATS" :key="option.value" class="format-option">
          <input
            :name="`${previewId}-format`"
            :checked="format === option.value"
            type="radio"
            @change="emit('update:format', option.value)"
          />
          {{ option.label }}
        </label>
      </div>
    </fieldset>
    <p class="output-stats migration-hint" role="status">
      <span>已读 <strong>{{ stats.total }}</strong> 首</span>
      <span>去重后 <strong>{{ stats.unique }}</strong> 首</span>
      <span>移除 <strong>{{ stats.removed }}</strong> 首</span>
    </p>
    <p :id="`${previewId}-coverage`" class="migration-hint coverage" role="status">
      <template v-if="coverage === 'loading'">部分来源仍在读取，预览仅包含当前已读曲目。</template>
      <template v-else-if="coverage === 'incomplete'">部分来源未完整读取，预览仅包含已读曲目。</template>
    </p>
    <textarea
      :id="previewId"
      :aria-describedby="coverage !== 'complete' ? `${previewId}-coverage` : undefined"
      :value="text"
      class="migration-field preview"
      rows="10"
      placeholder="暂无可导出的曲目"
      readonly
    ></textarea>
    <div class="output-actions">
      <button class="migration-button is-primary" type="button" :disabled="!text" @click="emit('copy')">复制文本</button>
      <button class="migration-button" type="button" :disabled="!text || saving" @click="emit('save')">
        {{ saving ? '正在保存…' : '另存为 TXT' }}
      </button>
    </div>
    <p class="migration-hint">可将文本粘贴到 TunemyMusic、Spotlistr 等第三方迁移服务。</p>
  </section>
</template>

<style scoped>
.text-output { display: grid; gap: 10px; }
fieldset { min-width: 0; border: 0; }
.format-options, .output-actions, .output-stats { display: flex; flex-wrap: wrap; gap: 8px; }
.format-options { margin-top: 5px; }
.format-option {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 5px 8px;
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-tag);
  cursor: pointer;
  font-size: 12px;
}
.format-option:has(input:checked) { border-color: var(--accent); background: var(--accent-soft); }
.format-option input { accent-color: var(--accent); }
.output-stats { column-gap: 14px; }
.output-stats strong { color: var(--text); font-variant-numeric: tabular-nums; }
.coverage { color: #fde68a; }
.coverage:empty { margin-top: -10px; }
.preview { min-height: 130px; font-family: ui-monospace, Consolas, monospace; }
</style>
