<template>
  <div 
    class="song-row"
    :class="{ 'is-current': isCurrent, 'is-playing': isCurrent && isPlaying, 'is-unavailable': unavailable }"
    :title="unavailable ? '本地未找到该音频文件，可运行 node scripts/download_all.js 补齐' : ''"
    @click="handleRowClick"
  >
    <!-- Index / Play Button -->
    <div class="col-index" @click.stop="toggleCurrent">
      <div v-if="isCurrent && isPlaying" class="equalizer-box">
        <div class="equalizer">
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
        </div>
      </div>
      <span v-else class="index-number">{{ String(index + 1).padStart(2, '0') }}</span>
      
      <button class="row-play-btn" :style="{ color: song.color || '#6366f1' }">
        <Pause v-if="isCurrent && isPlaying" class="w-4 h-4 fill-current" />
        <Play v-else class="w-4 h-4 fill-current translate-x-0.5" />
      </button>
    </div>

    <!-- Title & Artist & Thumbnail -->
    <div class="col-title">
      <div class="mini-thumb">
        <img 
          v-if="song.cover && !coverFailed" 
          :src="song.cover" 
          :alt="song.title"
          class="thumb-img"
          loading="lazy"
          @error="coverFailed = true"
        />
        <div 
          v-else 
          class="fallback-thumb"
          :style="{ background: `linear-gradient(135deg, ${song.color || '#6366f1'}, #0f172a)` }"
        >
          <Music class="w-3.5 h-3.5 text-white/60" />
        </div>
      </div>

      <div class="title-meta">
        <span class="row-title" :title="song.title">{{ song.title }}</span>
        <span class="row-artist" :title="song.artist">{{ song.artist }}</span>
      </div>
      <span v-if="unavailable" class="unavailable-tag">本地缺失</span>
    </div>

    <!-- Category -->
    <div class="col-category">
      <span 
        class="category-tag"
        :style="{ 
          backgroundColor: `${song.color || '#6366f1'}18`,
          color: song.color || '#6366f1',
          borderColor: `${song.color || '#6366f1'}33`
        }"
      >
        {{ song.categoryZh }}
      </span>
    </div>

    <!-- File Size -->
    <div class="col-size">
      <span class="size-text">{{ formatSize(song.fileSize) }}</span>
    </div>

    <!-- Duration -->
    <div class="col-duration">
      <span class="duration-text">{{ song.durationFormatted || '03:00' }}</span>
    </div>

    <!-- Actions (Favorite & Download) -->
    <div class="col-actions" @click.stop>
      <button 
        class="action-icon-btn fav-btn" 
        :class="{ 'is-active': isFav }"
        @click="toggleFavorite(songKey(song))"
        title="收藏"
      >
        <Heart class="w-4 h-4" :fill="isFav ? '#ec4899' : 'none'" />
      </button>

      <a 
        :href="audioUrl" 
        :download="`${song.title}.mp3`"
        class="action-icon-btn download-btn"
        title="下载音频"
      >
        <Download class="w-4 h-4" />
      </a>
    </div>
  </div>
</template>

<script setup>
import { computed, ref, watch } from 'vue';
import { Play, Pause, Heart, Download, Music } from '@lucide/vue';
import { useAudioPlayer } from '../composables/useAudioPlayer';
import { songKey, resolveSongUrl } from '../utils/songSchema';

const props = defineProps({
  song: {
    type: Object,
    required: true
  },
  index: {
    type: Number,
    required: true
  },
  unavailable: {
    type: Boolean,
    default: false
  }
});

const emit = defineEmits(['play']);

const { currentSong, isPlaying, favorites, toggleFavorite } = useAudioPlayer();

const isCurrent = computed(() => currentSong.value && songKey(currentSong.value) === songKey(props.song));
const isFav = computed(() => favorites.value.has(songKey(props.song)));
const audioUrl = computed(() => resolveSongUrl(props.song));

// 封面加载失败只影响本组件展示，不回写共享数据
const coverFailed = ref(false);
watch(() => props.song.cover, () => { coverFailed.value = false; });

const formatSize = (bytes) => {
  if (!bytes) return '—';
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
};

const handleRowClick = () => {
  if (props.unavailable) return;
  emit('play', props.song);
};

const toggleCurrent = () => {
  if (props.unavailable) return;
  emit('play', props.song);
};
</script>

<style scoped>
.song-row {
  display: flex;
  align-items: center;
  padding: 10px 16px;
  border-radius: 12px;
  transition: all 0.15s ease;
  cursor: pointer;
  border: 1px solid transparent;
}

.song-row:hover {
  background: rgba(255, 255, 255, 0.04);
}

.song-row.is-current {
  background: rgba(99, 102, 241, 0.08);
  border-color: rgba(99, 102, 241, 0.2);
}

.song-row.is-current .row-title {
  color: #818cf8;
}

.song-row.is-unavailable {
  opacity: 0.55;
  cursor: not-allowed;
}

.unavailable-tag {
  flex-shrink: 0;
  font-size: 10px;
  font-weight: 700;
  color: #fde68a;
  background: rgba(120, 53, 15, 0.6);
  border: 1px solid rgba(251, 191, 36, 0.35);
  padding: 1px 6px;
  border-radius: 6px;
}

/* Col: Index & Play */
.col-index {
  width: 40px;
  position: relative;
  display: flex;
  align-items: center;
  justify-content: flex-start;
  flex-shrink: 0;
}

.index-number {
  font-size: 13px;
  font-weight: 600;
  color: #64748b;
  font-variant-numeric: tabular-nums;
  transition: opacity 0.15s ease;
}

.row-play-btn {
  position: absolute;
  left: 0;
  display: none;
  align-items: center;
  justify-content: center;
}

.song-row:hover .index-number {
  opacity: 0;
}

.song-row:hover .row-play-btn {
  display: flex;
}

.equalizer-box {
  display: flex;
  align-items: center;
}

/* Col: Title & Thumb */
.col-title {
  display: flex;
  align-items: center;
  gap: 12px;
  flex: 3;
  min-width: 0;
}

.mini-thumb {
  width: 38px;
  height: 38px;
  border-radius: 8px;
  overflow: hidden;
  flex-shrink: 0;
  background: #0f172a;
  border: 1px solid rgba(255, 255, 255, 0.08);
}

.thumb-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}

.fallback-thumb {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
}

.title-meta {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.row-title {
  font-size: 14px;
  font-weight: 600;
  color: #f1f5f9;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.row-artist {
  font-size: 12px;
  color: #94a3b8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* Col: Category */
.col-category {
  flex: 1.2;
  min-width: 0;
}

.category-tag {
  display: inline-block;
  font-size: 11px;
  font-weight: 700;
  padding: 2px 8px;
  border-radius: 99px;
  border: 1px solid;
}

/* Col: Size */
.col-size {
  flex: 0.8;
}

.size-text {
  font-size: 12px;
  color: #64748b;
  font-weight: 600;
}

/* Col: Duration */
.col-duration {
  flex: 0.8;
}

.duration-text {
  font-size: 12px;
  color: #94a3b8;
  font-variant-numeric: tabular-nums;
  font-weight: 600;
}

/* Col: Actions */
.col-actions {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 80px;
  justify-content: flex-end;
  opacity: 0.7;
  transition: opacity 0.15s ease;
}

.song-row:hover .col-actions {
  opacity: 1;
}

.action-icon-btn {
  color: #94a3b8;
  padding: 6px;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  text-decoration: none;
}

.action-icon-btn:hover {
  color: #f8fafc;
  background: rgba(255, 255, 255, 0.08);
}

.fav-btn.is-active {
  color: #ec4899;
}
</style>
