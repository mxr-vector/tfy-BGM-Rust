<template>
  <aside class="sidebar-wrapper">
    <!-- Brand Header -->
    <div class="brand-section">
      <div class="brand-logo-box">
        <Disc class="w-6 h-6 text-white spinning-disc" />
      </div>
      <div class="brand-text">
        <h2 class="brand-title">听风吟 · BGM</h2>
        <span class="brand-subtitle">剪辑音频素材库</span>
      </div>
    </div>

    <!-- Navigation Menu -->
    <nav class="nav-menu">
      <div class="nav-group">
        <span class="nav-group-title">发现音乐</span>
        
        <button 
          class="nav-item" 
          :class="{ 'is-active': activeCategory === 'all' && !showFavoritesOnly }"
          @click="selectCategory('all')"
        >
          <Music class="w-4 h-4" />
          <span class="nav-label">全部 BGM</span>
          <span class="nav-badge">{{ totalSongs }}</span>
        </button>

        <button 
          class="nav-item" 
          :class="{ 'is-active': showFavoritesOnly }"
          @click="toggleFavorites"
        >
          <Heart class="w-4 h-4 text-pink-500" :fill="showFavoritesOnly ? '#ec4899' : 'none'" />
          <span class="nav-label">我的收藏</span>
          <span class="nav-badge fav-badge" v-if="favCount > 0">{{ favCount }}</span>
        </button>
      </div>

      <!-- Categories Group -->
      <div class="nav-group">
        <span class="nav-group-title">风格分类 (CATEGORIES)</span>
        
        <button 
          v-for="cat in categories" 
          :key="cat.id"
          class="nav-item category-item"
          :class="{ 'is-active': activeCategory === cat.id && !showFavoritesOnly }"
          @click="selectCategory(cat.id)"
        >
          <span 
            class="cat-dot" 
            :style="{ backgroundColor: cat.color, boxShadow: `0 0 8px ${cat.color}88` }"
          ></span>
          
          <div class="cat-label-box">
            <span class="cat-name-zh">{{ cat.nameZh }}</span>
            <span class="cat-name-en">{{ cat.nameEn }}</span>
          </div>

          <span class="nav-badge">{{ cat.count }}</span>
        </button>
      </div>
    </nav>

    <!-- Bottom Manifest Status Card -->
    <div class="status-card glass-panel">
      <div class="status-header">
        <div class="status-indicator">
          <span class="status-dot"></span>
          <span class="status-text">曲库清单就绪</span>
        </div>
        <span class="status-percent">{{ totalSongs }} 首</span>
      </div>
      <p class="status-detail">构建期由 public/bgm 目录扫描生成，音频本体随仓库入库</p>
    </div>
  </aside>
</template>

<script setup lang="ts">
import { computed } from 'vue';
import { Disc, Music, Heart } from '@lucide/vue';
import { useAudioPlayer } from '../composables/useAudioPlayer.ts';
import type { ActiveCategory, CategoryWithCount } from '../types/bgm.ts';

interface Props {
  /** 侧边栏分类列表（含曲目数） */
  categories: CategoryWithCount[];
  /** 当前选中分类 */
  activeCategory?: ActiveCategory;
  /** 是否只看收藏 */
  showFavoritesOnly?: boolean;
  /** 曲库总数 */
  totalSongs?: number;
}

withDefaults(defineProps<Props>(), {
  activeCategory: 'all',
  showFavoritesOnly: false,
  totalSongs: 0,
});

const emit = defineEmits<{
  'select-category': [catId: ActiveCategory];
  'toggle-favorites': [];
}>();

const { favorites } = useAudioPlayer();
const favCount = computed(() => favorites.value.size);

const selectCategory = (catId: ActiveCategory) => {
  emit('select-category', catId);
};

const toggleFavorites = () => {
  emit('toggle-favorites');
};
</script>

<style scoped>
.sidebar-wrapper {
  width: 260px;
  background: var(--bg-sidebar);
  border-right: 1px solid var(--border-subtle);
  display: flex;
  flex-direction: column;
  height: 100vh;
  position: sticky;
  top: 0;
  padding: 24px 16px 120px;
  flex-shrink: 0;
}

/* Brand */
.brand-section {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 0 8px 24px;
  border-bottom: 1px solid var(--border-subtle);
}

.brand-logo-box {
  width: 40px;
  height: 40px;
  border-radius: 12px;
  background: var(--accent-gradient);
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: 0 4px 16px rgba(99, 102, 241, 0.4);
}

.brand-text {
  display: flex;
  flex-direction: column;
}

.brand-title {
  font-size: 16px;
  font-weight: 800;
  letter-spacing: -0.02em;
  background: linear-gradient(180deg, #ffffff 0%, #cbd5e1 100%);
  -webkit-background-clip: text;
  -webkit-text-fill-color: transparent;
}

.brand-subtitle {
  font-size: 11px;
  font-weight: 600;
  color: var(--text-muted);
}

/* Nav Menu */
.nav-menu {
  display: flex;
  flex-direction: column;
  gap: 24px;
  margin-top: 20px;
  flex: 1;
  overflow-y: auto;
}

.nav-group {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.nav-group-title {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 0.08em;
  color: #475569;
  padding: 0 10px 6px;
  text-transform: uppercase;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  border-radius: 10px;
  color: #94a3b8;
  font-size: 13px;
  font-weight: 600;
  transition: all 0.15s ease;
  position: relative;
}

.nav-item:hover {
  color: #f8fafc;
  background: rgba(255, 255, 255, 0.05);
}

.nav-item.is-active {
  color: #ffffff;
  background: rgba(99, 102, 241, 0.15);
  border: 1px solid rgba(99, 102, 241, 0.3);
}

.nav-label {
  flex: 1;
  text-align: left;
}

.nav-badge {
  font-size: 11px;
  font-weight: 700;
  color: #64748b;
  background: rgba(255, 255, 255, 0.06);
  padding: 2px 7px;
  border-radius: 99px;
  font-variant-numeric: tabular-nums;
}

.nav-item.is-active .nav-badge {
  color: #a5b4fc;
  background: rgba(99, 102, 241, 0.25);
}

.fav-badge {
  background: rgba(236, 72, 153, 0.2);
  color: #f472b6;
}

/* Category item */
.cat-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.cat-label-box {
  display: flex;
  align-items: baseline;
  gap: 6px;
  flex: 1;
  text-align: left;
}

.cat-name-zh {
  font-size: 13px;
  font-weight: 600;
}

.cat-name-en {
  font-size: 11px;
  color: #64748b;
  font-weight: 500;
}

.nav-item.is-active .cat-name-en {
  color: #cbd5e1;
}

/* Status Card */
.status-card {
  margin-top: auto;
  padding: 14px;
  background: rgba(18, 24, 38, 0.5);
  border-radius: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.status-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.status-indicator {
  display: flex;
  align-items: center;
  gap: 6px;
}

.status-dot {
  width: 7px;
  height: 7px;
  background: #10b981;
  border-radius: 50%;
  box-shadow: 0 0 8px #10b981;
}

.status-text {
  font-size: 11px;
  font-weight: 700;
  color: #10b981;
}

.status-percent {
  font-size: 11px;
  font-weight: 800;
  color: #94a3b8;
}

.status-detail {
  font-size: 11px;
  color: #64748b;
  line-height: 1.4;
}

@media (max-width: 900px) {
  .sidebar-wrapper {
    display: none;
  }
}
</style>
