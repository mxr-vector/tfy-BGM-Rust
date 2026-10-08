<template>
  <div class="app-root">
    <!-- Sidebar Navigation -->
    <Sidebar 
      :categories="categoryList"
      :active-category="currentCategory"
      :show-favorites-only="showFavoritesOnly"
      :total-songs="allSongs.length"
      @select-category="handleCategorySelect"
      @toggle-favorites="handleFavoritesToggle"
    />

    <!-- Main Workspace -->
    <main class="main-workspace">
      <!-- Top Sticky Navigation & Search Bar -->
      <header class="top-nav-bar glass-panel">
        <!-- Search Field -->
        <div class="search-box">
          <Search class="w-4 h-4 text-slate-400 flex-shrink-0" />
          <input 
            ref="searchInput"
            v-model="searchQuery" 
            type="text" 
            placeholder="搜索曲名或分类关键词... (按 / 快速聚焦)"
            class="search-input"
          />
          <button v-if="searchQuery" class="clear-search-btn" @click="searchQuery = ''">
            <X class="w-3.5 h-3.5" />
          </button>
          <kbd class="search-kbd">/</kbd>
        </div>

        <!-- Controls Cluster -->
        <div class="top-controls">
          <!-- Sort Selector -->
          <div class="sort-box">
            <ArrowUpDown class="w-3.5 h-3.5 text-slate-400" />
            <select v-model="sortBy" class="sort-select">
              <option value="default">默认推荐</option>
              <option value="name">按曲名 (A-Z)</option>
            </select>
          </div>

        </div>
      </header>

      <!-- Mobile/Tablet Category Quick Pills -->
      <div class="mobile-cat-pills">
        <button 
          class="mobile-pill" 
          :class="{ 'is-active': currentCategory === 'all' && !showFavoritesOnly }"
          @click="handleCategorySelect('all')"
        >
          全部 ({{ allSongs.length }})
        </button>
        <button 
          class="mobile-pill"
          :class="{ 'is-active': showFavoritesOnly }"
          @click="handleFavoritesToggle"
        >
          收藏 ({{ favCount }})
        </button>
        <button 
          v-for="cat in categoryList"
          :key="cat.id"
          class="mobile-pill"
          :class="{ 'is-active': currentCategory === cat.id && !showFavoritesOnly }"
          :style="{ 
            borderColor: currentCategory === cat.id ? cat.color : 'transparent',
            color: currentCategory === cat.id ? cat.color : ''
          }"
          @click="handleCategorySelect(cat.id)"
        >
          {{ cat.nameZh }} ({{ cat.count }})
        </button>
      </div>

      <!-- Main Content Container -->
      <div class="content-container">
        <!-- Hero Banner for Selected Category -->
        <CategoryHero 
          :category="currentCategory"
          :show-favorites-only="showFavoritesOnly"
          :count="filteredSongs.length"
          @play-all="playAllFiltered"
          @shuffle-play="shufflePlayFiltered"
        />

        <!-- Results Header -->
        <div class="results-header">
          <div class="results-info">
            <span class="results-title">
              {{ currentHeaderTitle }}
            </span>
            <span class="results-count">{{ filteredSongs.length }} 首配乐</span>
          </div>

          <div v-if="searchQuery" class="search-active-hint">
            <span>正在筛选: “{{ searchQuery }}”</span>
            <button @click="searchQuery = ''" class="text-indigo-400 hover:underline text-xs ml-2">清除</button>
          </div>
        </div>

        <!-- Empty State：区分「曲库为空」与「筛选无结果」，避免误导新使用者 -->
        <div v-if="filteredSongs.length === 0" class="empty-results-box glass-panel">
          <Music class="w-16 h-16 text-slate-600 mb-3" />
          <template v-if="allSongs.length === 0">
            <h3 class="text-lg font-bold text-slate-300">曲库为空</h3>
            <p class="text-sm text-slate-500 mt-1">请向 public/bgm/&lt;分类&gt;/ 目录放入 .mp3（或配置 VITE_BGM_BASE 指向云盘音频）</p>
          </template>
          <template v-else>
            <h3 class="text-lg font-bold text-slate-300">没有找到匹配的音频素材</h3>
            <p class="text-sm text-slate-500 mt-1">请尝试更换搜索词，或清除分类与筛选条件</p>
            <button class="reset-filter-btn" @click="resetFilters">
              重置所有筛选
            </button>
          </template>
        </div>

        <!-- List / Table View -->
        <div v-else class="songs-table glass-panel">
          <!-- Table Header -->
          <div class="table-header">
            <span class="th-index">#</span>
            <span class="th-title">曲名</span>
            <span class="th-category">风格分类</span>
            <span class="th-actions">操作</span>
          </div>

          <!-- Table Rows -->
          <div class="table-body">
            <SongRow 
              v-for="(song, idx) in filteredSongs" 
              :key="songKey(song)" 
              :song="song"
              :index="idx"
              draggable="true"
              :class="{ 'is-drag-over': dragOverKey === songKey(song) && dragKey !== songKey(song) }"
              @play="handleSongPlay(song)"
              @dragstart="onDragStart(song)"
              @dragover.prevent
              @dragenter.prevent="dragOverKey = songKey(song)"
              @drop="onDropOn(song)"
              @dragend="onDragEnd"
            />
          </div>
        </div>
      </div>
    </main>

    <!-- Playlist Queue Drawer -->
    <PlaylistDrawer 
      :is-open="isQueueDrawerOpen"
      @close="isQueueDrawerOpen = false"
    />

    <!-- Bottom Persistent Audio Player -->
    <AudioPlayer 
      :show-queue-drawer="isQueueDrawerOpen"
      @toggle-queue="isQueueDrawerOpen = !isQueueDrawerOpen"
    />
  </div>
</template>

<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { Search, X, ArrowUpDown, Music } from '@lucide/vue';
import { useAudioPlayer } from './composables/useAudioPlayer';
import { songKey } from './utils/songSchema';
import { CATEGORIES, withCategoryMeta } from './utils/categories';
import bgmManifest from 'virtual:bgm-manifest';

import Sidebar from './components/Sidebar.vue';
import CategoryHero from './components/CategoryHero.vue';
import SongRow from './components/SongRow.vue';
import AudioPlayer from './components/AudioPlayer.vue';
import PlaylistDrawer from './components/PlaylistDrawer.vue';

const { playSong, favorites, currentSong, migrateFavorites } = useAudioPlayer();

// 曲库清单由构建期扫描 public/bgm 目录生成（仅含曲名/分类/路径），
// 分类中文名与主题色运行时从 categories.js 派生，改色不会与构建产物漂移。
const allSongs = ref(bgmManifest.map(withCategoryMeta));
const currentCategory = ref('all');
const showFavoritesOnly = ref(false);
const searchQuery = ref('');
const sortBy = ref('default');
const isQueueDrawerOpen = ref(false);
const searchInput = ref(null);

// 列表视图拖拽排序（仅内存重排 allSongs，不做持久化）
const dragKey = ref(null);
const dragOverKey = ref(null);

const onDragStart = (song) => {
  dragKey.value = songKey(song);
};

const onDropOn = (targetSong) => {
  const fromKey = dragKey.value;
  const toKey = songKey(targetSong);
  if (!fromKey || fromKey === toKey) return;
  const arr = [...allSongs.value];
  const from = arr.findIndex(s => songKey(s) === fromKey);
  const to = arr.findIndex(s => songKey(s) === toKey);
  if (from < 0 || to < 0) return;
  const [moved] = arr.splice(from, 1);
  const insertAt = arr.findIndex(s => songKey(s) === toKey);
  arr.splice(from < to ? insertAt + 1 : insertAt, 0, moved);
  allSongs.value = arr;
};

const onDragEnd = () => {
  dragKey.value = null;
  dragOverKey.value = null;
};

const favCount = computed(() => {
  const loaded = new Set(allSongs.value.map(songKey));
  return [...favorites.value].filter(k => loaded.has(k)).length;
});

// Category configurations with counts
const categoryList = computed(() => {
  const counts = {};
  for (const s of allSongs.value) {
    counts[s.category] = (counts[s.category] || 0) + 1;
  }
  return CATEGORIES.map(cat => ({
    id: cat.id,
    nameZh: cat.nameZh,
    nameEn: cat.nameEn,
    color: cat.color,
    count: counts[cat.id] || 0
  }));
});

// Category header title
const currentHeaderTitle = computed(() => {
  if (showFavoritesOnly.value) return '我的收藏夹';
  if (currentCategory.value === 'all') return '全部音乐素材';
  const found = categoryList.value.find(c => c.id === currentCategory.value);
  return found ? `${found.nameZh}音乐 (${found.nameEn})` : '音频列表';
});

// Filtered and Sorted Songs
const filteredSongs = computed(() => {
  let list = [...allSongs.value];

  // Category filter
  if (currentCategory.value !== 'all' && !showFavoritesOnly.value) {
    list = list.filter(s => s.category === currentCategory.value);
  }

  // Favorites filter
  if (showFavoritesOnly.value) {
    list = list.filter(s => favorites.value.has(songKey(s)));
  }

  // Search filter
  const query = searchQuery.value.trim().toLowerCase();
  if (query) {
    list = list.filter(s => {
      const matchTitle = s.title.toLowerCase().includes(query);
      const matchCategoryZh = s.categoryZh && s.categoryZh.toLowerCase().includes(query);
      const matchCategoryEn = s.categoryEn && s.categoryEn.toLowerCase().includes(query);
      const matchCategoryKey = s.category && s.category.toLowerCase().includes(query);
      return matchTitle || matchCategoryZh || matchCategoryEn || matchCategoryKey;
    });
  }

  // Sorting
  if (sortBy.value === 'name') {
    list.sort((a, b) => a.title.localeCompare(b.title, 'zh'));
  }

  return list;
});

// Handlers
const handleCategorySelect = (catId) => {
  showFavoritesOnly.value = false;
  currentCategory.value = catId;
};

const handleFavoritesToggle = () => {
  showFavoritesOnly.value = !showFavoritesOnly.value;
};

const handleSongPlay = (song) => {
  playSong(song, filteredSongs.value);
};

const playAllFiltered = () => {
  if (filteredSongs.value.length > 0) {
    playSong(filteredSongs.value[0], filteredSongs.value, true);
  }
};

const shufflePlayFiltered = () => {
  if (filteredSongs.value.length > 0) {
    const randomIdx = Math.floor(Math.random() * filteredSongs.value.length);
    playSong(filteredSongs.value[randomIdx], filteredSongs.value, true);
  }
};

const resetFilters = () => {
  searchQuery.value = '';
  currentCategory.value = 'all';
  showFavoritesOnly.value = false;
};

// Keyboard listener for '/' search focus & Space initial start
const handleGlobalKeyDown = (e) => {
  const isInputActive = ['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target?.tagName) || e.target?.isContentEditable;
  
  if (e.key === '/' && !isInputActive) {
    e.preventDefault();
    searchInput.value?.focus();
  } else if (e.code === 'Space' && !isInputActive && !currentSong.value && filteredSongs.value.length > 0) {
    e.preventDefault();
    playSong(filteredSongs.value[0], filteredSongs.value, true);
  }
};

onMounted(() => {
  migrateFavorites(allSongs.value);   // 旧版收藏（原始 url）-> 稳定 key
  window.addEventListener('keydown', handleGlobalKeyDown);
});

onUnmounted(() => {
  window.removeEventListener('keydown', handleGlobalKeyDown);
});
</script>

<style scoped>
.app-root {
  display: flex;
  min-height: 100vh;
  background-color: var(--bg-main);
}

.main-workspace {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  padding-bottom: 120px;
}

/* Top Navigation Bar */
.top-nav-bar {
  position: sticky;
  top: 16px;
  margin: 16px 28px 0;
  z-index: 100;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 18px;
  gap: 16px;
  background: rgba(13, 17, 23, 0.75);
}

.search-box {
  display: flex;
  align-items: center;
  gap: 10px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 12px;
  padding: 8px 14px;
  flex: 1;
  max-width: 520px;
  transition: all 0.2s ease;
}

.search-box:focus-within {
  background: rgba(255, 255, 255, 0.08);
  border-color: rgba(99, 102, 241, 0.5);
  box-shadow: 0 0 16px rgba(99, 102, 241, 0.15);
}

.search-input {
  background: transparent;
  border: none;
  color: #f8fafc;
  font-size: 13px;
  width: 100%;
  outline: none;
}

.search-input::placeholder {
  color: #64748b;
}

.clear-search-btn {
  color: #64748b;
  padding: 2px;
}

.clear-search-btn:hover {
  color: #f8fafc;
}

.search-kbd {
  font-size: 10px;
  font-family: inherit;
  font-weight: 700;
  color: #64748b;
  background: rgba(255, 255, 255, 0.08);
  padding: 2px 6px;
  border-radius: 4px;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

/* Top Controls */
.top-controls {
  display: flex;
  align-items: center;
  gap: 12px;
}

.sort-box {
  display: flex;
  align-items: center;
  gap: 6px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  padding: 6px 12px;
}

.sort-select {
  background: transparent;
  border: none;
  color: #cbd5e1;
  font-size: 13px;
  font-weight: 600;
  outline: none;
  cursor: pointer;
}

.sort-select option {
  background: #0f172a;
  color: #f8fafc;
}

/* Mobile Quick Pills */
.mobile-cat-pills {
  display: none;
  gap: 8px;
  padding: 12px 20px 4px;
  overflow-x: auto;
}

.mobile-pill {
  padding: 6px 14px;
  border-radius: 99px;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.08);
  font-size: 12px;
  font-weight: 600;
  color: #94a3b8;
  white-space: nowrap;
}

.mobile-pill.is-active {
  background: rgba(255, 255, 255, 0.15);
  color: #ffffff;
}

/* Content Container */
.content-container {
  padding: 24px 28px;
  display: flex;
  flex-direction: column;
}

.results-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 18px;
}

.results-info {
  display: flex;
  align-items: baseline;
  gap: 10px;
}

.results-title {
  font-size: 18px;
  font-weight: 700;
  color: #ffffff;
}

.results-count {
  font-size: 13px;
  font-weight: 600;
  color: #64748b;
}

.search-active-hint {
  font-size: 13px;
  color: #94a3b8;
}

/* Songs Table */
.songs-table {
  border-radius: 16px;
  overflow: hidden;
  padding: 6px;
}

.table-header {
  display: flex;
  align-items: center;
  padding: 10px 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.06);
  font-size: 12px;
  font-weight: 700;
  color: #64748b;
  text-transform: uppercase;
  letter-spacing: 0.05em;
}

.th-index { width: 40px; }
.th-title { flex: 3; }
.th-category { flex: 1.2; }
.th-actions { width: 80px; text-align: right; }

.table-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-top: 4px;
}

/* 拖拽排序落点高亮 */
.table-body :deep(.song-row) {
  cursor: grab;
}

.table-body :deep(.song-row.is-drag-over) {
  box-shadow: inset 0 2px 0 0 #6366f1;
  background: rgba(99, 102, 241, 0.1);
}

.table-body :deep(.song-row:active) {
  cursor: grabbing;
}

/* Empty State */
.empty-results-box {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 60px 20px;
  text-align: center;
  border-radius: 20px;
}

.reset-filter-btn {
  margin-top: 16px;
  padding: 8px 20px;
  border-radius: 99px;
  background: rgba(99, 102, 241, 0.2);
  color: #818cf8;
  font-size: 13px;
  font-weight: 600;
  border: 1px solid rgba(99, 102, 241, 0.3);
  transition: all 0.2s ease;
}

.reset-filter-btn:hover {
  background: rgba(99, 102, 241, 0.3);
  color: #ffffff;
}

@media (max-width: 900px) {
  .top-nav-bar {
    margin: 12px 16px 0;
  }
  .content-container {
    padding: 16px;
  }
  .mobile-cat-pills {
    display: flex;
  }
}

</style>
