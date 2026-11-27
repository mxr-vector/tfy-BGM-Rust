<script setup lang="ts">
import { ALL_CATEGORY } from '@/lib/constants';
import {
  busyAction,
  chooseLibrary,
  openLibraryDir,
  rescanLibrary,
  resetLibraryDir,
} from '@/stores/actions';
import { currentProgram } from '@/composables/useScheduler';
import { setCategory, toggleFavoritesOnly } from '@/stores/library';
import { toggleDrawer } from '@/stores/player';
import { favoriteCount, setView, sourceLabel, state } from '@/stores/state';
import Icon from '@/components/Icon.vue';

function isBusy(): boolean {
  return busyAction.value !== null;
}
</script>

<template>
  <aside class="sidebar">
    <nav class="side-nav" aria-label="曲库导航">
      <span class="nav-section-label">浏览</span>
      <button
        type="button"
        class="nav-item"
        :class="{ 'is-active': state.view === 'library' && !state.favoritesOnly && state.category === ALL_CATEGORY }"
        id="nav-all"
        title="音乐库"
        @click="setCategory(ALL_CATEGORY)"
      >
        <Icon name="music" class="nav-icon" />
        <span class="nav-label">音乐库</span>
        <span class="nav-count" id="count-all">{{ state.library.songs.length }}</span>
      </button>
      <button
        type="button"
        class="nav-item nav-item-fav"
        :class="{ 'is-active': state.view === 'library' && state.favoritesOnly }"
        id="nav-favorites"
        title="我的收藏"
        @click="toggleFavoritesOnly"
      >
        <Icon name="heart" class="nav-icon nav-icon-heart" />
        <span class="nav-label">我的收藏</span>
        <span class="nav-count is-pink" id="count-favorites">{{ favoriteCount }}</span>
      </button>
      <button
        type="button"
        class="nav-item"
        id="nav-queue"
        title="播放队列"
        @click="toggleDrawer"
      >
        <Icon name="list-music" class="nav-icon" />
        <span class="nav-label">播放队列</span>
        <span class="nav-count" id="count-queue">{{ state.queue.length }}</span>
      </button>
      <button
        type="button"
        class="nav-item"
        :class="{ 'is-active': state.view === 'schedule' }"
        :aria-current="state.view === 'schedule' ? 'page' : undefined"
        id="nav-schedule"
        title="按时间段自动切换节目歌曲与音量"
        @click="setView('schedule')"
      >
        <Icon name="clock" class="nav-icon" />
        <span class="nav-label">节目单</span>
        <span class="nav-count" id="count-schedule">{{ state.schedule.length }}</span>
      </button>

      <div class="nav-section">
        <div class="nav-section-heading">
          <span class="nav-section-label">曲库管理</span>
          <button
            id="btn-rescan"
            type="button"
            class="nav-section-action"
            title="重新扫描曲库目录并热加载配置"
            aria-label="重新扫描"
            :disabled="isBusy()"
            @click="rescanLibrary"
          >
            <Icon name="refresh" />
          </button>
          <button
            id="btn-open"
            type="button"
            class="nav-section-action"
            title="在文件管理器中打开曲库文件夹"
            aria-label="打开曲库文件夹"
            :disabled="isBusy()"
            @click="openLibraryDir"
          >
            <Icon name="external" />
          </button>
          <button
            id="btn-choose"
            type="button"
            class="nav-section-action"
            title="切换曲库目录"
            aria-label="切换曲库目录"
            :disabled="isBusy()"
            @click="chooseLibrary"
          >
            <Icon name="folder" />
          </button>
        </div>
        <button
          type="button"
          class="nav-item is-sub"
          :class="{ 'is-active': state.view === 'duplicates' }"
          id="nav-duplicates"
          title="按本地音频指纹查找重复文件"
          @click="setView('duplicates')"
        >
          <Icon name="disc" class="nav-icon" />
          <span class="nav-label">音频指纹去重</span>
        </button>
        <button
          type="button"
          class="nav-item is-sub"
          :class="{ 'is-active': state.view === 'sound-search' }"
          id="nav-sound-search"
          title="选择一段本地音频，离线匹配曲库"
          @click="setView('sound-search')"
        >
          <Icon name="search" class="nav-icon" />
          <span class="nav-label">以声搜声</span>
        </button>
        <button
          type="button"
          class="nav-item is-sub"
          :class="{ 'is-active': state.view === 'playlist-migration' }"
          :aria-current="state.view === 'playlist-migration' ? 'page' : undefined"
          id="nav-playlist-migration"
          title="歌单迁移"
          @click="setView('playlist-migration')"
        >
          <Icon name="list-music" class="nav-icon" />
          <span class="nav-label">歌单迁移</span>
        </button>
        <button
          type="button"
          class="nav-item is-sub"
          :class="{ 'is-active': state.view === 'tts' }"
          :aria-current="state.view === 'tts' ? 'page' : undefined"
          id="nav-tts"
          title="使用 Kokoro 或 ZipVoice 离线合成语音"
          @click="setView('tts')"
        >
          <Icon name="volume-2" class="nav-icon" />
          <span class="nav-label">文生语音</span>
        </button>
        <button
          type="button"
          class="nav-item is-sub"
          :class="{ 'is-active': state.view === 'subtitles' }"
          :aria-current="state.view === 'subtitles' ? 'page' : undefined"
          id="nav-subtitles"
          title="为音乐库曲目生成 SRT / LRC 字幕"
          aria-label="字幕生成"
          @click="setView('subtitles')"
        >
          <Icon name="captions" class="nav-icon" />
          <span class="nav-label">字幕生成</span>
        </button>
        <button
          v-show="state.library.source === 'custom'"
          type="button"
          class="nav-item is-sub"
          id="btn-reset"
          title="清除自定义目录，恢复默认曲库"
          :disabled="isBusy()"
          @click="resetLibraryDir"
        >
          <Icon name="rotate" class="nav-icon" />
          <span class="nav-label">恢复默认曲库</span>
        </button>
      </div>
    </nav>

    <div class="side-status">
      <div class="status-row">
        <span class="status-indicator" :class="{ 'is-warn': state.library.warnings.length > 0 }">
          <i class="status-dot"></i>曲库就绪
        </span>
        <span class="status-count" id="status-count">{{ state.library.songs.length }} 首</span>
      </div>
      <span class="status-path" id="status-path" :title="state.library.root">
        {{ sourceLabel() }} · {{ state.library.root || '未找到曲库目录' }}
      </span>
      <span class="status-path" id="status-program" title="节目单当前时段">
        节目：{{ currentProgram ? currentProgram.name : '未编排' }}
      </span>
    </div>
  </aside>
</template>