<script setup lang="ts">
import { usePlaylistMigration } from '@/composables/usePlaylistMigration';
import PlaylistSourceForm from './PlaylistSourceForm.vue';
import PlaylistSourceList from './PlaylistSourceList.vue';
import PlaylistTextOutput from './PlaylistTextOutput.vue';

const { draft, sources, format, busy, coverage, output, saving, parseSources, retrySource, copyText, saveText } =
  usePlaylistMigration();
</script>

<template>
  <section class="migration-page" aria-labelledby="playlist-migration-title">
    <h1 id="playlist-migration-title">歌单迁移</h1>
    <div class="migration-grid">
      <div class="migration-sources">
        <PlaylistSourceForm v-model="draft" :busy="busy" @parse="parseSources" />
        <PlaylistSourceList :sources="sources" @retry="retrySource" />
      </div>
      <PlaylistTextOutput
        v-model:format="format"
        :stats="output.stats"
        :text="output.text"
        :coverage="coverage"
        :saving="saving"
        @copy="copyText"
        @save="saveText"
      />
    </div>
  </section>
</template>

<style scoped>
.migration-page {
  container-type: inline-size;
  min-height: 0;
  flex: 1;
  overflow: auto;
  padding: 2px 2px 4px;
}
h1 { margin: 3px 0 14px; font-size: 25px; letter-spacing: -0.03em; }
.migration-grid { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.15fr); gap: 14px; align-items: start; }
.migration-sources { display: grid; min-width: 0; gap: 12px; }
.migration-page :deep(.migration-panel) {
  min-width: 0;
  padding: 14px;
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-card);
  background: rgba(var(--panel-rgb), var(--panel-opacity));
}
.migration-page :deep(.migration-panel h2) { font-size: 15px; font-weight: 650; }
.migration-page :deep(.migration-hint) { color: var(--text-2); font-size: 12px; }
.migration-page :deep(.migration-field) {
  display: block;
  width: 100%;
  padding: 10px 12px;
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-btn);
  background: var(--bg-base);
  color: var(--text);
  font-size: 13px;
  line-height: 1.6;
  resize: vertical;
}
.migration-page :deep(.migration-field:focus-visible) { outline: 2px solid var(--accent-bright); outline-offset: 1px; }
.migration-page :deep(.migration-button) {
  min-height: 34px;
  padding: 5px 12px;
  border: 1px solid var(--line-strong);
  border-radius: var(--radius-btn);
  background: rgba(255, 255, 255, 0.055);
  color: var(--text);
  font-weight: 600;
}
.migration-page :deep(.migration-button:not(:disabled):hover) { background: var(--hover); }
.migration-page :deep(.migration-button.is-primary) { background: var(--accent); color: var(--accent-ink); }
.migration-page :deep(.migration-button.is-primary:not(:disabled):hover) { background: var(--accent-bright); }
@container (max-width: 640px) {
  .migration-grid { grid-template-columns: minmax(0, 1fr); }
}
</style>
