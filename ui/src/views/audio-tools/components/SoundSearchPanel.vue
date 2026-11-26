<script setup lang="ts">
import { computed, onScopeDispose, ref, watch } from 'vue';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { audioIndexBusy, audioIndexStatus, searchLibraryBySound, soundSearchLoading } from '@/composables/useAudioIndex';
import { useSoundRecording } from '@/composables/useSoundRecording';
import { formatBytes, formatTime } from '@/lib/format';
import type { SoundSearchResult } from '@/lib/types';
import { suspendPlayback } from '@/stores/audio-engine';
import { playSong } from '@/stores/player';
import { state } from '@/stores/state';
import Icon from '@/components/Icon.vue';

const result = ref<SoundSearchResult | null>(null);
const limit = ref(10);
const { requesting, recording, elapsedMs, maxDurationMs, minDurationMs, canStop, start, stop } =
  useSoundRecording((clip) => void search(clip), 30_000, 5_000);
const canSearch = computed(() =>
  audioIndexStatus.value?.enabled && audioIndexStatus.value.readyTracks > 0 &&
  !audioIndexBusy.value && !soundSearchLoading.value,
);
const captureActive = computed(() => requesting.value || recording.value);
let releasePlayback: (() => void) | undefined;

watch(captureActive, (active) => {
  if (active) releasePlayback ??= suspendPlayback();
  else {
    releasePlayback?.();
    releasePlayback = undefined;
  }
}, { flush: 'sync' });

onScopeDispose(() => {
  releasePlayback?.();
  releasePlayback = undefined;
});

watch([() => state.library.root, () => audioIndexStatus.value?.enabled], () => {
  result.value = null;
});

function normalizeLimit(): void {
  const value = typeof limit.value === 'number' ? limit.value : 10;
  limit.value = Math.min(50, Math.max(1, Math.round(value)));
}

async function search(clip?: Blob): Promise<void> {
  normalizeLimit();
  const root = state.library.root;
  const next = await searchLibraryBySound(limit.value, clip);
  if (next && state.library.root === root) result.value = next;
}

function play(track: SoundSearchResult['matches'][number]): void {
  if (!track.key || guardLibraryPlayback()) return;
  const song = state.library.songs.find((item) => item.key === track.key);
  if (song) playSong(song, state.library.songs, true);
}
</script>

<template>
  <section class="sound-search-panel">
    <div class="search-hero">
      <span class="search-hero-icon"><Icon name="search" /></span>
      <div>
        <h2>用声音查找曲库中的相似音频</h2>
        <p>可导入外部录音，或用麦克风录制 5–30 秒清晰的音乐片段。音频在本机识别。</p>
      </div>
      <div class="search-actions">
        <label>最多返回
          <input v-model.number="limit" type="number" min="1" max="50" step="1" :disabled="recording || requesting || soundSearchLoading" @change="normalizeLimit" /> 首
        </label>
        <button type="button" :disabled="!canSearch || recording || requesting" @click="search()">
          <Icon name="folder" /> 选择录音或音频
        </button>
        <button type="button" :disabled="requesting || (recording && !canStop) || (!recording && !canSearch)" @click="recording ? stop() : start()">
          {{ recording ? '结束录音并识别' : requesting ? '等待麦克风权限…' : '麦克风录音' }}
        </button>
      </div>
    </div>
    <div v-if="recording" class="recording-status" role="status" aria-live="polite">
      <span class="recording-dot" aria-hidden="true"></span>
      <div>
        <strong>正在录音，请播放清晰的音乐片段</strong>
        <span>
          已录 {{ formatTime(elapsedMs / 1000) }} / {{ formatTime(maxDurationMs / 1000) }}
          · 至少 {{ Math.ceil(minDurationMs / 1000) }} 秒，可手动结束；满 30 秒后自动检索
        </span>
      </div>
    </div>
    <p v-if="soundSearchLoading" class="query-path" role="status">正在识别音频…</p>
    <div v-if="result" class="query-path"><span>查询来源 · {{ result.matches.length }} 首</span><strong :title="result.queryPath">{{ result.queryPath }}</strong></div>
    <div v-if="result && result.matches.length === 0" class="empty-state">
      <Icon name="search" /><strong>没有找到足够相似的音频</strong><span>可尝试更完整的片段，或先更新音频索引。</span>
    </div>
    <div v-else-if="result" class="match-list">
      <article v-for="match in result.matches" :key="match.id" class="match-row">
        <span class="match-score">{{ Math.round(match.score * 100) }}<small>%</small></span>
        <span class="match-copy">
          <strong :title="match.fileName">{{ match.fileName }}</strong>
          <span :title="match.path">{{ match.path }}</span>
        </span>
        <span class="match-detail">
          <span>{{ match.bpm ? `${match.bpm.toFixed(1)} BPM` : 'BPM —' }} · {{ match.musicalKey ?? '调性 —' }}</span>
          <span>片段约 {{ formatTime(Math.max(0, match.offsetSeconds)) }} · {{ formatBytes(match.sizeBytes) }}</span>
        </span>
        <button type="button" :disabled="!match.key" :title="match.key ? '播放匹配曲目' : '该副本不在曲库播放列表中'" @click="play(match)">
          <Icon name="play" /> 播放
        </button>
      </article>
    </div>
    <div v-else class="empty-state search-placeholder">
      <Icon name="wave" /><strong>选择文件或录音开始检索</strong><span>建议录制背景噪声较少的音乐片段。</span>
    </div>
  </section>
</template>

<style scoped>
.sound-search-panel { display: flex; flex: 1; min-height: 0; flex-direction: column; gap: 12px; overflow-y: auto; }
.search-hero { display: flex; align-items: center; gap: 14px; padding: 18px; border: 1px solid rgba(25, 217, 139, 0.18); border-radius: 15px; background: linear-gradient(120deg, rgba(25, 217, 139, 0.09), rgba(255, 255, 255, 0.025)); }
.search-hero-icon { display: grid; place-items: center; width: 46px; height: 46px; flex: none; border-radius: 15px; color: #9df7cf; background: rgba(25, 217, 139, 0.12); }
.search-hero > div { flex: 1; min-width: 0; }
.search-hero .search-actions { display: flex; flex: 0 0 auto; flex-wrap: wrap; align-items: center; gap: 8px; }
.search-actions label { display: inline-flex; align-items: center; gap: 5px; color: var(--text-2); font-size: 12px; }
.search-actions input { width: 56px; min-height: 36px; padding: 0 5px; border: 1px solid rgba(25, 217, 139, 0.25); border-radius: 8px; color: var(--text); background: rgba(0, 0, 0, 0.2); }
.search-hero h2 { margin: 0 0 6px; color: var(--text); font-size: 16px; }
.search-hero p { margin: 0; color: var(--text-3); font-size: 11px; line-height: 1.7; }
.search-hero button, .match-row button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 36px; padding: 0 14px; border-radius: 9px; color: #baf7dc; background: rgba(25, 217, 139, 0.13); }
.search-hero button:disabled, .match-row button:disabled { opacity: 0.55; cursor: not-allowed; }
.recording-status { display: flex; flex: none; align-items: center; gap: 11px; padding: 11px 14px; border: 1px solid rgba(248, 113, 113, 0.28); border-radius: 11px; color: #fecaca; background: rgba(239, 68, 68, 0.1); }
.recording-status > div { display: flex; min-width: 0; flex-direction: column; gap: 3px; }
.recording-status strong { font-size: 12px; }
.recording-status span:not(.recording-dot) { color: var(--text-3); font-size: 10.5px; line-height: 1.5; }
.recording-dot { width: 10px; height: 10px; flex: none; border-radius: 50%; background: #f87171; box-shadow: 0 0 0 5px rgba(248, 113, 113, 0.12); animation: recording-pulse 1.2s ease-in-out infinite; }
@keyframes recording-pulse { 50% { opacity: 0.45; box-shadow: 0 0 0 8px rgba(248, 113, 113, 0.04); } }
.query-path { display: flex; gap: 10px; padding: 0 4px; color: var(--text-3); font-size: 10.5px; }
.query-path strong { overflow: hidden; color: var(--text-2); font-weight: 500; text-overflow: ellipsis; white-space: nowrap; }
.match-list { display: flex; flex-direction: column; gap: 8px; }
.match-row { display: flex; align-items: center; gap: 13px; padding: 12px 14px; border: 1px solid rgba(255, 255, 255, 0.075); border-radius: 12px; background: rgba(255, 255, 255, 0.028); }
.match-score { width: 48px; flex: none; color: #9df7cf; font-size: 19px; font-weight: 700; text-align: center; }
.match-score small { font-size: 10px; }
.match-copy { display: flex; flex: 1; min-width: 0; flex-direction: column; gap: 4px; }
.match-copy strong { overflow: hidden; color: var(--text); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.match-copy span { overflow: hidden; color: var(--text-3); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
.match-detail { display: flex; flex: none; flex-direction: column; gap: 4px; color: var(--text-3); font-size: 10px; text-align: right; }
.match-row button { min-height: 31px; padding: 0 11px; font-size: 11px; }
.empty-state { display: flex; flex: 1; min-height: 170px; align-items: center; justify-content: center; flex-direction: column; gap: 9px; color: var(--text-3); font-size: 11px; text-align: center; }
.empty-state strong { color: var(--text-2); font-size: 14px; }
.empty-state [data-icon] { width: 30px; height: 30px; color: #6ff3bc; }
@media (max-width: 720px) {
  .search-hero { flex-wrap: wrap; }
  .search-hero button { width: 100%; }
  .match-detail { display: none; }
}
</style>
