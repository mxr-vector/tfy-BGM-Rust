<template>
  <div class="category-hero" :style="{ '--hero-color': activeCatInfo.color }">
    <!-- Ambient glowing backdrop blobs -->
    <div class="hero-glow-blob"></div>

    <div class="hero-content">
      <div class="hero-badge-row">
        <span class="hero-tag" :style="{ backgroundColor: `${activeCatInfo.color}22`, color: activeCatInfo.color }">
          <component :is="activeCatInfo.iconComponent" class="w-3.5 h-3.5 inline mr-1" />
          {{ activeCatInfo.labelEn }}
        </span>
        <span class="offline-tag">✓ 离线已下载</span>
      </div>

      <h1 class="hero-title">{{ activeCatInfo.title }}</h1>
      <p class="hero-description">{{ activeCatInfo.description }}</p>

      <!-- Stats & Quick Actions -->
      <div class="hero-footer">
        <div class="action-buttons">
          <button class="primary-play-btn" :disabled="count === 0" @click="count > 0 && $emit('play-all')">
            <Play class="w-5 h-5 fill-white" />
            <span>全部播放 ({{ count }})</span>
          </button>

          <button class="secondary-btn" :disabled="count === 0" @click="count > 0 && $emit('shuffle-play')">
            <Shuffle class="w-4 h-4" />
            <span>随机试听</span>
          </button>
        </div>

        <div class="hero-stats">
          <div class="stat-item">
            <span class="stat-num">{{ count }}</span>
            <span class="stat-label">首曲目</span>
          </div>
          <div class="stat-divider"></div>
          <div class="stat-item">
            <span class="stat-num">320kbps</span>
            <span class="stat-label">高音质 MP3</span>
          </div>
          <div class="stat-divider"></div>
          <div class="stat-item">
            <span class="stat-num">免费</span>
            <span class="stat-label">剪辑可用</span>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue';
import { Play, Shuffle, Sparkles, Coffee, Clock, Flame, Feather, Sun, Music, Heart } from '@lucide/vue';

const props = defineProps({
  category: {
    type: String,
    default: 'all'
  },
  showFavoritesOnly: {
    type: Boolean,
    default: false
  },
  count: {
    type: Number,
    default: 83
  }
});

defineEmits(['play-all', 'shuffle-play']);

const CATEGORY_META = {
  all: {
    title: '全部精选视频剪辑 BGM',
    labelEn: 'ALL BGM SOUNDTRACKS',
    color: '#6366f1',
    iconComponent: Music,
    description: '汇集欢快、独处、回忆、史诗、古风、乡村等 6 大分类优质背景音乐，覆盖 Vlog、宣传片、故事片、卡点等全场景剪辑需求。'
  },
  cheerful: {
    title: '欢快节奏 · Cheerful & Upbeat',
    labelEn: 'CHEERFUL / UPBEAT',
    color: '#f59e0b',
    iconComponent: Sparkles,
    description: '适用于生活 Vlog、开箱解说、美食探店、搞笑趣味、快闪卡点等轻松愉悦的视频剪辑。'
  },
  solitude: {
    title: '独处沉思 · Solitude & Chill',
    labelEn: 'SOLITUDE / PEACEFUL',
    color: '#818cf8',
    iconComponent: Coffee,
    description: '纯音乐、治愈钢琴、深夜独白、静心工作与自律学习视频的最佳背景配乐。'
  },
  memories: {
    title: '时光回忆 · Memories & Nostalgia',
    labelEn: 'MEMORIES / NOSTALGIA',
    color: '#f472b6',
    iconComponent: Clock,
    description: '温情故事、毕业季纪念、告白与重逢、岁月光影、情感纪实剪辑首选配乐。'
  },
  epic: {
    title: '震撼史诗 · Epic & Grand',
    labelEn: 'EPIC / MAJESTIC',
    color: '#ef4444',
    iconComponent: Flame,
    description: '气势磅礴的大片感战歌，适合燃向混剪、企业宣传片、运动高光、游戏集锦、宏大叙事。'
  },
  gufeng: {
    title: '东方古风 · Traditional Chinese',
    labelEn: 'GUFENG / TRADITIONAL',
    color: '#10b981',
    iconComponent: Feather,
    description: '笛子、古筝、二胡等国风雅韵，完美契合汉服国风、古建文旅、武侠唯美剪辑。'
  },
  country: {
    title: '田园乡村 · Country & Rural',
    labelEn: 'COUNTRY / FOLK',
    color: '#84cc16',
    iconComponent: Sun,
    description: '乡野烟火、农家田园、归隐慢生活、悠闲度假风景短视频的绝妙搭档。'
  }
};

const activeCatInfo = computed(() => {
  if (props.showFavoritesOnly) {
    return {
      title: '我的收藏夹 · Favorite Tracks',
      labelEn: 'FAVORITES',
      color: '#ec4899',
      iconComponent: Heart,
      description: '你标记喜欢的配乐都在这里，方便在剪辑过程中随时试听与下载使用。'
    };
  }
  return CATEGORY_META[props.category] || CATEGORY_META.all;
});
</script>

<style scoped>
.category-hero {
  position: relative;
  border-radius: 24px;
  padding: 36px 36px 32px;
  background: linear-gradient(135deg, rgba(255, 255, 255, 0.04) 0%, rgba(18, 24, 38, 0.6) 100%);
  border: 1px solid rgba(255, 255, 255, 0.08);
  overflow: hidden;
  margin-bottom: 28px;
}

.hero-glow-blob {
  position: absolute;
  top: -60px;
  right: -40px;
  width: 320px;
  height: 320px;
  border-radius: 50%;
  background: var(--hero-color);
  opacity: 0.18;
  filter: blur(80px);
  pointer-events: none;
}

.hero-content {
  position: relative;
  z-index: 1;
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.hero-badge-row {
  display: flex;
  align-items: center;
  gap: 10px;
}

.hero-tag {
  font-size: 11px;
  font-weight: 800;
  letter-spacing: 0.06em;
  padding: 3px 10px;
  border-radius: 99px;
  text-transform: uppercase;
}

.offline-tag {
  font-size: 11px;
  font-weight: 700;
  color: #10b981;
  background: rgba(16, 185, 129, 0.12);
  padding: 3px 10px;
  border-radius: 99px;
}

.hero-title {
  font-size: 30px;
  font-weight: 800;
  color: #ffffff;
  letter-spacing: -0.02em;
  line-height: 1.2;
}

.hero-description {
  font-size: 14px;
  color: #94a3b8;
  max-width: 680px;
  line-height: 1.6;
}

.hero-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-top: 14px;
  flex-wrap: wrap;
  gap: 18px;
}

.action-buttons {
  display: flex;
  align-items: center;
  gap: 12px;
}

.primary-play-btn {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 24px;
  border-radius: 99px;
  background: var(--hero-color);
  color: #ffffff;
  font-size: 14px;
  font-weight: 700;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.4);
  transition: all 0.2s cubic-bezier(0.34, 1.56, 0.64, 1);
}

.primary-play-btn:hover {
  transform: scale(1.04);
  filter: brightness(1.15);
}

.secondary-btn {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 20px;
  border-radius: 99px;
  background: rgba(255, 255, 255, 0.08);
  color: #f8fafc;
  font-size: 14px;
  font-weight: 600;
  border: 1px solid rgba(255, 255, 255, 0.1);
  transition: all 0.2s ease;
}

.secondary-btn:hover {
  background: rgba(255, 255, 255, 0.15);
}

.primary-play-btn:disabled,
.secondary-btn:disabled {
  opacity: 0.35;
  cursor: not-allowed;
  transform: none !important;
  filter: none !important;
}

.hero-stats {
  display: flex;
  align-items: center;
  gap: 18px;
}

.stat-item {
  display: flex;
  flex-direction: column;
}

.stat-num {
  font-size: 15px;
  font-weight: 800;
  color: #f1f5f9;
}

.stat-label {
  font-size: 11px;
  color: #64748b;
  font-weight: 600;
}

.stat-divider {
  width: 1px;
  height: 24px;
  background: rgba(255, 255, 255, 0.1);
}

@media (max-width: 768px) {
  .category-hero {
    padding: 24px 20px;
  }
  .hero-title {
    font-size: 22px;
  }
  .hero-stats {
    display: none;
  }
}
</style>
