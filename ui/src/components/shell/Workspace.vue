<script setup lang="ts">
import { state } from '@/stores/state';
import AudioToolsView from '@/views/audio-tools/index.vue';
import LibraryView from '@/views/library/index.vue';
import PlaylistMigrationView from '@/views/playlist-migration/index.vue';
import ScheduleView from '@/views/schedule/index.vue';
import SubtitlesView from '@/views/subtitles/index.vue';
import TtsView from '@/views/tts/index.vue';
import WarningBanner from './WarningBanner.vue';
</script>

<template>
  <main class="workspace">
    <WarningBanner />
    <ScheduleView v-if="state.view === 'schedule'" />
    <SubtitlesView v-else-if="state.view === 'subtitles'" />
    <TtsView v-else-if="state.view === 'tts'" />
    <AudioToolsView v-else-if="state.view === 'duplicates'" mode="duplicates" />
    <AudioToolsView v-else-if="state.view === 'sound-search'" mode="search" />
    <PlaylistMigrationView v-else-if="state.view === 'playlist-migration'" />
    <LibraryView v-else />
  </main>
</template>
