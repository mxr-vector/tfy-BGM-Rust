<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { loadAudioIndexInfo } from '@/composables/useAudioIndex';
import { formatTime } from '@/lib/format';
import { toast } from '@/lib/toast';
import type { AudioIndexInfo, Song } from '@/lib/types';
import { audioState } from '@/stores/audio-engine';
import { loopPointFor, openLoopEditor } from '@/stores/loops';
import {
  addSubtitleRecord,
  editing,
  exportSubtitleForTrack,
  exportingFormat,
  generating,
  loadSavedSubtitle,
  previewFormat,
  savedSubtitles,
  selectedKey,
  selectSong,
  setPreviewFormat,
  startEditing,
} from '@/stores/subtitles';
import { setView } from '@/stores/state';
import { songDetailsTab, type SongDetailsTab } from '@/stores/song-details';
import DiagnosticsPanel from './DiagnosticsPanel.vue';
import Icon from '@/components/Icon.vue';
import SongDetailsLyrics from './SongDetailsLyrics.vue';
import SongDetailsOverview from './SongDetailsOverview.vue';

const props = defineProps<{ song: Song }>();
const emit = defineEmits<{ close: [] }>();

const diagnosticsActive = ref(false);
const indexedInfo = ref<AudioIndexInfo | null>(null);
const duration = computed(() => audioState.duration || (props.song.durationMs ?? 0) / 1000);
const subtitle = computed(() => savedSubtitles.value[props.song.key] ?? null);
const loopPoint = computed(() => loopPointFor(props.song.key));
const decoded = computed(() => audioState.loadedPath === props.song.path);
const currentTab = computed(() => songDetailsTab.value);
const subtitleStatus = computed(() => {
  if (!subtitle.value) return '尚未生成字幕，生成后可在此查看歌词';
  return `已生成《${props.song.title}》字幕，共 ${subtitle.value.segments.length} 段`;
});

watch(
  () => props.song.key,
  (key) => {
    diagnosticsActive.value = false;
    indexedInfo.value = null;
    void loadSavedSubtitle(key);
    void loadAudioIndexInfo(props.song).then((info) => {
      if (props.song.key === key) indexedInfo.value = info;
    });
  },
  { immediate: true },
);

function selectTab(tab: SongDetailsTab): void {
  songDetailsTab.value = tab;
  diagnosticsActive.value = false;
}

function toggleDiagnostics(): void {
  diagnosticsActive.value = !diagnosticsActive.value;
  if (diagnosticsActive.value) songDetailsTab.value = 'details';
}

function openSubtitleGenerator(): void {
  if (generating.value || editing.value) {
    toast('请先完成当前字幕生成或保存编辑', 'info');
    return;
  }
  addSubtitleRecord(props.song);
  emit('close');
}

function openSubtitleEditor(): void {
  if (!subtitle.value) return;
  if (generating.value || editing.value) {
    toast('请先完成当前字幕生成或保存编辑', 'info');
    return;
  }
  selectSong(props.song.key);
  // selectSong 在生成 / 编辑态会拒绝切换；用选中 key 防止误编辑其它曲目。
  if (selectedKey.value !== props.song.key) return;
  startEditing();
  setView('subtitles');
  emit('close');
}

function chooseFormat(format: 'srt' | 'lrc'): void {
  setPreviewFormat(format);
}

async function exportCurrentSubtitle(): Promise<void> {
  if (!subtitle.value) return;
  await exportSubtitleForTrack(props.song.key, previewFormat.value);
}
</script>

<template>
  <div class="details-content">
    <div class="details-content-head">
      <nav class="details-tabs" role="tablist" aria-label="歌曲内容">
        <button
          type="button"
          role="tab"
          :aria-selected="currentTab === 'lyrics' && !diagnosticsActive"
          :class="{ 'is-active': currentTab === 'lyrics' && !diagnosticsActive }"
          @click="selectTab('lyrics')"
        >
          歌词
        </button>
        <button
          type="button"
          role="tab"
          :aria-selected="currentTab === 'details' && !diagnosticsActive"
          :class="{ 'is-active': currentTab === 'details' && !diagnosticsActive }"
          @click="selectTab('details')"
        >
          详情
        </button>
      </nav>

      <div class="details-subtitle-formats" aria-label="字幕格式">
        <button
          type="button"
          :class="{ 'is-active': previewFormat === 'srt' }"
          :disabled="!subtitle"
          @click="chooseFormat('srt')"
        >
          SRT
        </button>
        <button
          type="button"
          :class="{ 'is-active': previewFormat === 'lrc' }"
          :disabled="!subtitle"
          @click="chooseFormat('lrc')"
        >
          LRC
        </button>
      </div>
    </div>

    <div class="details-toolbar">
      <button
        type="button"
        :class="{ 'is-active': diagnosticsActive }"
        :aria-pressed="diagnosticsActive"
        @click="toggleDiagnostics"
      >
        <Icon name="wave" /> 音频诊断
      </button>
      <button type="button" @click="openSubtitleGenerator">
        <Icon name="captions" /> 生成字幕
      </button>
      <button
        type="button"
        :class="{ 'is-active': loopPoint?.enabled }"
        @click="openLoopEditor(song)"
      >
        <Icon name="repeat-1" /> A/B 循环
      </button>
      <span class="details-toolbar-spacer"></span>
      <button type="button" :disabled="!subtitle" @click="openSubtitleEditor">
        <Icon name="edit" /> 编辑
      </button>
      <button
        type="button"
        :disabled="!subtitle || Boolean(exportingFormat)"
        @click="exportCurrentSubtitle"
      >
        <Icon name="download" /> 导出
      </button>
    </div>

    <div class="details-content-body">
      <section v-if="diagnosticsActive" class="details-diagnostics" aria-label="音频诊断">
        <div class="details-diagnostics-head">
          <button type="button" @click="diagnosticsActive = false">
            <Icon name="chevron-down" class="is-back" /> 返回{{ currentTab === 'lyrics' ? '歌词' : '详情' }}
          </button>
          <span>{{ decoded ? '源文件实测分析' : '等待当前音频解码' }}</span>
        </div>
        <DiagnosticsPanel />
      </section>
      <section v-else-if="currentTab === 'lyrics'" class="details-tab-panel" role="tabpanel">
        <SongDetailsLyrics :song-key="song.key" :subtitle="subtitle" />
      </section>
      <section v-else class="details-tab-panel" role="tabpanel">
        <SongDetailsOverview
          :song="song"
          :duration="duration"
          :subtitle="subtitle"
          :loop-point="loopPoint"
          :decoded="decoded"
          :indexed-info="indexedInfo"
        />
      </section>
    </div>

    <div class="details-content-status">
      <span class="status-dot" :class="{ 'is-ready': subtitle }"></span>
      <span>{{ subtitleStatus }}</span>
      <span v-if="loopPoint" class="details-loop-summary">
        {{ loopPoint.enabled ? '循环已启用' : '循环已关闭' }}
        {{ formatTime(loopPoint.start) }}–{{ formatTime(loopPoint.end) }}
      </span>
    </div>
  </div>
</template>

<style scoped>
.details-content {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
}

.details-content-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 13px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.1);
}

.details-tabs {
  display: flex;
  align-items: center;
  gap: 25px;
}

.details-tabs button {
  position: relative;
  padding: 0 2px 12px;
  color: rgba(255, 255, 255, 0.42);
  font-size: 13px;
  font-weight: 600;
}

.details-tabs button:hover {
  color: rgba(255, 255, 255, 0.72);
}

.details-tabs button.is-active {
  color: #f3fff8;
}

.details-tabs button.is-active::after {
  position: absolute;
  right: 0;
  bottom: -1px;
  left: 0;
  height: 2px;
  border-radius: 2px;
  background: var(--details-accent, #19d98b);
  content: '';
}

.details-subtitle-formats {
  display: flex;
  gap: 4px;
  padding-bottom: 9px;
}

.details-subtitle-formats button {
  min-width: 35px;
  height: 23px;
  padding: 0 7px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 6px;
  color: var(--text-3);
  font-size: 9.5px;
  font-weight: 650;
}

.details-subtitle-formats button.is-active {
  border-color: rgba(255, 255, 255, 0.22);
  color: var(--text);
  background: rgba(255, 255, 255, 0.08);
}

.details-subtitle-formats button:disabled {
  cursor: default;
  opacity: 0.35;
}

.details-toolbar {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 7px;
  margin-bottom: 14px;
}

.details-toolbar button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 30px;
  padding: 0 11px;
  border: 1px solid rgba(255, 255, 255, 0.11);
  border-radius: 9px;
  color: rgba(255, 255, 255, 0.63);
  background: rgba(255, 255, 255, 0.035);
  font-size: 11px;
  font-weight: 550;
  transition:
    border-color 160ms ease,
    color 160ms ease,
    background 160ms ease;
}

.details-toolbar button [data-icon] {
  width: 13px;
  height: 13px;
}

.details-toolbar button:hover:not(:disabled) {
  border-color: rgba(255, 255, 255, 0.22);
  color: #fff;
  background: rgba(255, 255, 255, 0.07);
}

.details-toolbar button.is-active {
  border-color: color-mix(in srgb, var(--details-accent, #19d98b) 40%, transparent);
  color: #c9ffe9;
  background: color-mix(in srgb, var(--details-accent, #19d98b) 12%, transparent);
}

.details-toolbar button:disabled {
  cursor: default;
  opacity: 0.32;
}

.details-toolbar-spacer {
  flex: 1;
}

.details-content-body {
  flex: 1;
  min-height: 0;
}

.details-tab-panel,
.details-diagnostics {
  height: 100%;
  min-height: 0;
}

.details-diagnostics {
  display: flex;
  flex-direction: column;
}

.details-diagnostics-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
  color: var(--text-3);
  font-size: 10.5px;
}

.details-diagnostics-head button {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  color: var(--text-2);
  font-size: 11px;
}

.details-diagnostics-head button:hover {
  color: #fff;
}

.details-diagnostics-head [data-icon] {
  width: 13px;
  height: 13px;
  transform: rotate(90deg);
}

.details-content-status {
  display: flex;
  align-items: center;
  gap: 7px;
  min-height: 23px;
  padding-top: 11px;
  color: var(--text-3);
  font-size: 10.5px;
  overflow-wrap: anywhere;
}

.status-dot {
  width: 5px;
  height: 5px;
  flex: none;
  border-radius: 50%;
  background: rgba(255, 255, 255, 0.22);
}

.status-dot.is-ready {
  background: var(--details-accent, #19d98b);
  box-shadow: 0 0 7px color-mix(in srgb, var(--details-accent, #19d98b) 60%, transparent);
}

.details-loop-summary {
  margin-left: auto;
  color: rgba(255, 255, 255, 0.42);
  font-variant-numeric: tabular-nums;
}

.details-content :deep(.diag-embedded) {
  display: flex;
  flex: 1;
  flex-direction: column;
  height: auto;
  min-height: 0;
}

.details-content :deep(.diag-panel--embedded) {
  width: 100%;
  height: 100%;
  min-height: 0;
  overflow: hidden;
  border: 0;
  border-radius: 0;
  background: transparent;
  box-shadow: none;
}

.details-content :deep(.diag-panel--embedded .diag-header) {
  flex: none;
  padding: 0 0 12px;
  background: transparent;
}

.details-content :deep(.diag-panel--embedded .diag-title) {
  font-size: 15px;
}

.details-content :deep(.diag-panel--embedded .diag-body) {
  padding: 12px 0 0;
  scrollbar-color: rgba(255, 255, 255, 0.14) transparent;
  scrollbar-width: thin;
}

@media (max-width: 760px) {
  .details-toolbar {
    gap: 6px;
  }

  .details-toolbar button {
    padding-inline: 9px;
  }

  .details-loop-summary {
    display: none;
  }
}
</style>
