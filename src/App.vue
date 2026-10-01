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
            placeholder="搜索背景音乐、作者、或分类关键词... (按 / 快速聚焦)"
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
              <option value="duration-asc">时长 (短 → 长)</option>
              <option value="duration-desc">时长 (长 → 短)</option>
              <option value="size">文件大小 (大 → 小)</option>
            </select>
          </div>

          <!-- View Mode Toggle -->
          <div class="view-mode-toggle">
            <button 
              class="view-btn" 
              :class="{ 'is-active': viewMode === 'grid' }"
              @click="viewMode = 'grid'"
              title="网格卡片视图"
            >
              <LayoutGrid class="w-4 h-4" />
            </button>
            <button 
              class="view-btn" 
              :class="{ 'is-active': viewMode === 'list' }"
              @click="viewMode = 'list'"
              title="紧凑列表视图"
            >
              <List class="w-4 h-4" />
            </button>
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

      <!-- 曲库质量提示：元数据合法但音频缺失（音频走云盘分发时常见） -->
      <div v-if="unavailableCount > 0 || invalidSongs.length || duplicateKeys.length" class="data-warning glass-panel">
        <AlertTriangle class="w-4 h-4" />
        <span>
          <template v-if="unavailableCount">{{ unavailableCount }} 首素材本地缺失，</template>
          <template v-if="invalidSongs.length">{{ invalidSongs.length }} 条元数据不合法已隐藏，</template>
          <template v-if="duplicateKeys.length">{{ duplicateKeys.length }} 条稳定 key 重复（请检查 scripts/songs.md 是否有同名曲目）。</template>
          可运行 <code>node scripts/download_all.js</code> 修复。
        </span>
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

        <!-- Loading State -->
        <div v-if="isLoading" class="empty-results-box glass-panel">
          <Loader2 class="w-12 h-12 text-indigo-400 animate-spin mb-3" />
          <h3 class="text-lg font-bold text-slate-300">正在加载曲库素材...</h3>
          <p class="text-sm text-slate-500 mt-1">请稍候</p>
        </div>

        <!-- Load Error State -->
        <div v-else-if="loadError" class="empty-results-box glass-panel">
          <Music class="w-16 h-16 text-rose-500 mb-3" />
          <h3 class="text-lg font-bold text-rose-300">曲库加载失败</h3>
          <p class="text-sm text-slate-500 mt-1">{{ loadError }}</p>
          <button class="reset-filter-btn" @click="fetchSongs">
            重新尝试
          </button>
        </div>

        <!-- Empty Search State -->
        <div v-else-if="filteredSongs.length === 0" class="empty-results-box glass-panel">
          <Music class="w-16 h-16 text-slate-600 mb-3" />
          <h3 class="text-lg font-bold text-slate-300">没有找到匹配的音频素材</h3>
          <p class="text-sm text-slate-500 mt-1">请尝试更换搜索词，或清除分类与筛选条件</p>
          <button class="reset-filter-btn" @click="resetFilters">
            重置所有筛选
          </button>
        </div>

        <!-- Grid View -->
        <div v-else-if="viewMode === 'grid'" class="songs-grid">
          <SongCard 
            v-for="song in filteredSongs" 
            :key="songKey(song)" 
            :song="song"
            :unavailable="song.downloaded === false"
            @play="handleSongPlay(song)"
          />
        </div>

        <!-- List / Table View -->
        <div v-else class="songs-table glass-panel">
          <!-- Table Header -->
          <div class="table-header">
            <span class="th-index">#</span>
            <span class="th-title">曲名 / 艺术家</span>
            <span class="th-category">风格分类</span>
            <span class="th-size">大小</span>
            <span class="th-duration">时长</span>
            <span class="th-actions">操作</span>
          </div>

          <!-- Table Rows -->
          <div class="table-body">
            <SongRow 
              v-for="(song, idx) in filteredSongs" 
              :key="songKey(song)" 
              :song="song"
              :index="idx"
              :unavailable="song.downloaded === false"
              @play="handleSongPlay(song)"
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
import { Search, X, ArrowUpDown, LayoutGrid, List, Music, Loader2, AlertTriangle } from '@lucide/vue';
import { useAudioPlayer } from './composables/useAudioPlayer';
import { songKey, validateSong, normalizeSongs } from './utils/songSchema';

import Sidebar from './components/Sidebar.vue';
import CategoryHero from './components/CategoryHero.vue';
import SongCard from './components/SongCard.vue';
import SongRow from './components/SongRow.vue';
import AudioPlayer from './components/AudioPlayer.vue';
import PlaylistDrawer from './components/PlaylistDrawer.vue';

const { playSong, favorites, currentSong, migrateFavorites } = useAudioPlayer();

const allSongs = ref([]);
const invalidSongs = ref([]);
const duplicateKeys = ref([]);
const isLoading = ref(true);
const loadError = ref(null);
const currentCategory = ref('all');
const showFavoritesOnly = ref(false);
const searchQuery = ref('');
const sortBy = ref('default');
const viewMode = ref('grid');
const isQueueDrawerOpen = ref(false);
const searchInput = ref(null);

const favCount = computed(() => {
  const loaded = new Set(allSongs.value.map(songKey));
  return [...favorites.value].filter(k => loaded.has(k)).length;
});

const unavailableCount = computed(() => allSongs.value.filter(s => s.downloaded === false).length);

// Category configurations with counts
const categoryList = computed(() => {
  const counts = {};
  for (const s of allSongs.value) {
    counts[s.category] = (counts[s.category] || 0) + 1;
  }
  return [
    { id: 'cheerful', nameZh: '欢快', nameEn: 'Cheerful', color: '#f59e0b', count: counts['cheerful'] || 0 },
    { id: 'solitude', nameZh: '独处', nameEn: 'Solitude', color: '#818cf8', count: counts['solitude'] || 0 },
    { id: 'memories', nameZh: '回忆', nameEn: 'Memories', color: '#f472b6', count: counts['memories'] || 0 },
    { id: 'epic', nameZh: '史诗', nameEn: 'Epic', color: '#ef4444', count: counts['epic'] || 0 },
    { id: 'gufeng', nameZh: '古风', nameEn: 'GuFeng', color: '#10b981', count: counts['gufeng'] || 0 },
    { id: 'country', nameZh: '乡村', nameEn: 'Country', color: '#84cc16', count: counts['country'] || 0 }
  ];
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
      const matchArtist = s.artist && s.artist.toLowerCase().includes(query);
      const matchCategoryZh = s.categoryZh && s.categoryZh.toLowerCase().includes(query);
      const matchCategoryEn = s.categoryEn && s.categoryEn.toLowerCase().includes(query);
      const matchCategoryKey = s.category && s.category.toLowerCase().includes(query);
      return matchTitle || matchArtist || matchCategoryZh || matchCategoryEn || matchCategoryKey;
    });
  }

  // Sorting
  if (sortBy.value === 'name') {
    list.sort((a, b) => a.title.localeCompare(b.title, 'zh'));
  } else if (sortBy.value === 'duration-asc') {
    list.sort((a, b) => (a.duration || 0) - (b.duration || 0));
  } else if (sortBy.value === 'duration-desc') {
    list.sort((a, b) => (b.duration || 0) - (a.duration || 0));
  } else if (sortBy.value === 'size') {
    list.sort((a, b) => (b.fileSize || 0) - (a.fileSize || 0));
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

let fetchToken = 0;

const fetchSongs = async () => {
  if (isLoading.value) return;          // 重试互斥，避免并发竞态覆盖结果
  const token = ++fetchToken;
  try {
    isLoading.value = true;
    loadError.value = null;

    const url = `${import.meta.env.BASE_URL.replace(/\/$/, '')}/bgm/songs.json`;
    let res;
    try {
      res = await fetch(url, { cache: 'no-cache', signal: AbortSignal.timeout(15000) });
    } catch (err) {
      throw new Error(err?.name === 'TimeoutError' ? '加载曲库超时（15 秒），请检查网络或重试' : `无法请求曲库清单：${err?.message || err}`);
    }
    if (!res.ok) {
      throw new Error(res.status === 404
        ? '未找到 /bgm/songs.json：请先运行 node scripts/download_all.js 生成曲库清单'
        : `加载歌单失败 (HTTP ${res.status})`);
    }

    // SPA 回退会用 200 + index.html 冒充该请求，必须先看 Content-Type
    const contentType = res.headers.get('content-type') || '';
    if (!contentType.includes('json')) {
      throw new Error(`曲库清单响应不是 JSON（Content-Type: ${contentType || '未知'}）`);
    }

    const data = await res.json();
    if (!Array.isArray(data)) {
      throw new Error(`曲库清单格式非法：期望数组，实际 ${data === null ? 'null' : typeof data}`);
    }
    if (data.length === 0) {
      throw new Error('曲库清单为空，请先运行 node scripts/download_all.js 生成');
    }

    const valid = [];
    const invalid = [];
    for (const item of data) {
      const check = validateSong(item);
      if (check.ok) valid.push(item);
      else invalid.push(`${item?.title ?? '<无标题>'}: ${check.reason}`);
    }
    const keys = new Map();
    for (const s of valid) {
      const k = songKey(s);
      keys.set(k, (keys.get(k) || 0) + 1);
    }
    const dups = [...keys.entries()].filter(([, n]) => n > 1).map(([k]) => k);
    if (valid.length === 0) {
      throw new Error(`曲库清单中 ${invalid.length} 条均不符合数据契约（首条原因: ${invalid[0]}），请运行 node scripts/download_all.js 重新生成`);
    }

    // 按契约归一（normalizeSongs 与上面的校验结果一致，作为双保险）
    allSongs.value = normalizeSongs(valid);
    invalidSongs.value = invalid;
    duplicateKeys.value = dups;
    if (invalid.length) console.warn('songs.json 存在不合格条目:', invalid.slice(0, 10));
    if (dups.length) console.warn('songs.json 存在重复 key:', dups);

    migrateFavorites(allSongs.value);   // 旧版收藏（数值 id）-> 稳定 key
  } catch (err) {
    console.error('Failed to load songs:', err);
    if (token === fetchToken) loadError.value = err.message || '加载曲库失败';
  } finally {
    if (token === fetchToken) isLoading.value = false;
  }
};

onMounted(() => {
  fetchSongs();
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

.view-mode-toggle {
  display: flex;
  align-items: center;
  background: rgba(255, 255, 255, 0.05);
  border: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 10px;
  padding: 3px;
}

.view-btn {
  padding: 6px 8px;
  border-radius: 7px;
  color: #64748b;
  display: flex;
  align-items: center;
  justify-content: center;
}

.view-btn:hover {
  color: #f8fafc;
}

.view-btn.is-active {
  color: #ffffff;
  background: rgba(255, 255, 255, 0.1);
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

/* 曲库质量提示条 */
.data-warning {
  display: flex;
  align-items: center;
  gap: 10px;
  margin: 16px 28px 0;
  padding: 10px 16px;
  border-radius: 12px;
  font-size: 12.5px;
  color: #fbbf24;
  background: rgba(180, 83, 9, 0.12);
  border: 1px solid rgba(251, 191, 36, 0.25);
}

.data-warning code {
  font-size: 11.5px;
  color: #fde68a;
}

/* Songs Grid */
.songs-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
  gap: 18px;
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
.th-size { flex: 0.8; }
.th-duration { flex: 0.8; }
.th-actions { width: 80px; text-align: right; }

.table-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-top: 4px;
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

@media (max-width: 600px) {
  .songs-grid {
    grid-template-columns: repeat(2, 1fr);
    gap: 12px;
  }
}
</style>
