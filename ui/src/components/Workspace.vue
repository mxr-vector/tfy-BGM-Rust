<script setup lang="ts">
import { computed } from 'vue';
import { heroCopy } from '@/lib/config';
import { HERO_KINDS } from '@/lib/constants';
import { setSearch } from '@/stores/library';
import { categoryById, state, visibleSongs } from '@/stores/state';
import AudioToolsPage from './audio-tools/AudioToolsPage.vue';
import BatchBar from './BatchBar.vue';
import CategoryFilter from './CategoryFilter.vue';
import HeroBanner from './HeroBanner.vue';
import PlaylistMigrationPage from './playlist-migration/PlaylistMigrationPage.vue';
import SchedulePage from './SchedulePage.vue';
import SongTable from './SongTable.vue';
import SubtitlePage from './SubtitlePage.vue';
import WarningBanner from './WarningBanner.vue';
import TtsWorkspace from './tts/TtsWorkspace.vue';

const resultsTitle = computed(() => {
  if (state.favoritesOnly) return heroCopy(HERO_KINDS.favorites).resultsTitle;
  if (state.category === 'all') return heroCopy(HERO_KINDS.all).resultsTitle;

  const category = categoryById(state.category);
  if (!category) return '音频列表';
  if (category.nameEn && category.nameEn !== category.nameZh) {
    return `${category.nameZh}音乐 (${category.nameEn})`;
  }
  return `${category.nameZh}音乐`;
});

const resultsCount = computed(() => `${visibleSongs.value.length} 首配乐`);
const query = computed(() => state.search.trim());
</script>

<template>
  <main class="workspace">
    <WarningBanner />
    <SchedulePage v-if="state.view === 'schedule'" />
    <SubtitlePage v-else-if="state.view === 'subtitles'" />
    <TtsWorkspace v-else-if="state.view === 'tts'" />
    <AudioToolsPage v-else-if="state.view === 'duplicates'" mode="duplicates" />
    <AudioToolsPage v-else-if="state.view === 'sound-search'" mode="search" />
    <PlaylistMigrationPage v-else-if="state.view === 'playlist-migration'" />
    <template v-else>
      <HeroBanner />
      <CategoryFilter />

      <div class="results-header">
        <div class="results-info">
          <span class="results-title" id="results-title">{{ resultsTitle }}</span>
          <span class="results-count" id="results-count">{{ resultsCount }}</span>
        </div>
        <div v-show="query.length > 0" class="search-hint" id="search-hint">
          正在筛选：<span id="search-hint-text">“{{ query }}”</span>
          <button type="button" id="btn-clear-search-2" @click="setSearch('')">清除</button>
        </div>
      </div>

      <BatchBar />
      <SongTable />
    </template>
  </main>
</template>