<script setup lang="ts">
import { ref } from 'vue';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { searchLibraryBySound, soundSearchLoading } from '@/composables/useAudioIndex';
import { formatBytes, formatTime } from '@/lib/format';
import type { SoundSearchResult } from '@/lib/types';
import { playSong } from '@/stores/player';
import { state } from '@/stores/state';
import Icon from '../Icon.vue';

const result = ref<SoundSearchResult | null>(null);

async function chooseQuery(): Promise<void> {
  const next = await searchLibraryBySound();
  if (next) result.value = next;
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
        <p>选择一段本地音频，系统将在已建索引的曲库中匹配相同或相似片段。查询文件不会上传。</p>
      </div>
      <button type="button" :disabled="soundSearchLoading" @click="chooseQuery">
        <Icon name="folder" /> {{ soundSearchLoading ? '正在分析…' : '选择查询音频' }}
      </button>
    </div>
    <div v-if="result" class="query-path"><span>查询文件</span><strong :title="result.queryPath">{{ result.queryPath }}</strong></div>
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
      <Icon name="wave" /><strong>选择一段声音开始检索</strong><span>支持 MP3、M4A、FLAC、WAV、OGG，可匹配较长曲目中的局部片段。</span>
    </div>
  </section>
</template>

<style scoped>
.sound-search-panel { display: flex; flex: 1; min-height: 0; flex-direction: column; gap: 12px; overflow-y: auto; }
.search-hero { display: flex; align-items: center; gap: 14px; padding: 18px; border: 1px solid rgba(25, 217, 139, 0.18); border-radius: 15px; background: linear-gradient(120deg, rgba(25, 217, 139, 0.09), rgba(255, 255, 255, 0.025)); }
.search-hero-icon { display: grid; place-items: center; width: 46px; height: 46px; flex: none; border-radius: 15px; color: #9df7cf; background: rgba(25, 217, 139, 0.12); }
.search-hero > div { flex: 1; min-width: 0; }
.search-hero h2 { margin: 0 0 6px; color: var(--text); font-size: 16px; }
.search-hero p { margin: 0; color: var(--text-3); font-size: 11px; line-height: 1.7; }
.search-hero button, .match-row button { display: inline-flex; align-items: center; justify-content: center; gap: 7px; min-height: 36px; padding: 0 14px; border-radius: 9px; color: #baf7dc; background: rgba(25, 217, 139, 0.13); }
.search-hero button:disabled, .match-row button:disabled { opacity: 0.55; cursor: not-allowed; }
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
