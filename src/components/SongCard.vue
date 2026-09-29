<template>
  <div 
    class="song-card glass-panel"
    :class="{ 'is-active': isCurrent, 'is-playing': isCurrent && isPlaying }"
    @click="handleCardClick"
  >
    <!-- Artwork Thumbnail -->
    <div class="thumb-container">
      <img 
        v-if="song.cover" 
        :src="song.cover" 
        :alt="song.title"
        class="card-img"
        loading="lazy"
        @error="song.cover = ''"
      />
      <div 
        v-else 
        class="fallback-art"
        :style="{ background: `linear-gradient(135deg, ${song.color || '#6366f1'}, #0f172a)` }"
      >
        <Music class="fallback-icon" />
        <span class="first-char">{{ song.title.charAt(0) }}</span>
      </div>

      <!-- Play Overlay button -->
      <div class="overlay-mask">
        <button 
          class="play-trigger-btn"
          :style="{ backgroundColor: song.color || '#6366f1' }"
          @click.stop="toggleCurrent"
        >
          <Pause v-if="isCurrent && isPlaying" class="w-6 h-6 fill-white text-white" />
          <Play v-else class="w-6 h-6 fill-white text-white translate-x-0.5" />
        </button>
      </div>

      <!-- Active Playing Equalizer Indicator -->
      <div v-if="isCurrent && isPlaying" class="equalizer-overlay">
        <div class="equalizer">
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
          <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
        </div>
      </div>

      <!-- Category Pill Badge -->
      <span 
        class="category-pill"
        :style="{ 
          backgroundColor: `${song.color || '#6366f1'}cc`,
          boxShadow: `0 2px 8px ${song.color || '#6366f1'}66`
        }"
      >
        {{ song.categoryZh }}
      </span>

      <!-- Duration badge -->
      <span class="duration-pill">
        {{ song.durationFormatted || '03:00' }}
      </span>
    </div>

    <!-- Metadata Content -->
    <div class="card-details">
      <div class="title-section">
        <h4 class="song-title" :title="song.title">{{ song.title }}</h4>
        <p class="song-artist" :title="song.artist">{{ song.artist }}</p>
      </div>

      <!-- Footer action bar -->
      <div class="card-footer">
        <div class="file-info">
          <span class="file-size">{{ formatSize(song.fileSize) }}</span>
          <span class="format-tag">MP3</span>
        </div>

        <div class="card-actions" @click.stop>
          <button 
            class="mini-btn fav-btn" 
            :class="{ 'is-active': isFav(song.id) }"
            @click="toggleFavorite(song.id)"
            title="收藏"
          >
            <Heart class="w-4 h-4" :fill="isFav(song.id) ? '#ec4899' : 'none'" />
          </button>

          <a 
            :href="song.url" 
            :download="`${song.title}.mp3`"
            class="mini-btn download-btn"
            title="下载音频"
          >
            <Download class="w-4 h-4" />
          </a>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup>
import { computed } from 'vue';
import { Play, Pause, Heart, Download, Music } from '@lucide/vue';
import { useAudioPlayer } from '../composables/useAudioPlayer';

const props = defineProps({
  song: {
    type: Object,
    required: true
  }
});

const emit = defineEmits(['play']);

const { currentSong, isPlaying, favorites, toggleFavorite } = useAudioPlayer();

const isCurrent = computed(() => currentSong.value?.id === props.song.id);
const isFav = (id) => favorites.value.has(id);

const formatSize = (bytes) => {
  if (!bytes) return '2.5 MB';
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
};

const handleCardClick = () => {
  emit('play', props.song);
};

const toggleCurrent = () => {
  emit('play', props.song);
};
</script>

<style scoped>
.song-card {
  position: relative;
  display: flex;
  flex-direction: column;
  border-radius: 16px;
  overflow: hidden;
  transition: all 0.25s cubic-bezier(0.4, 0, 0.2, 1);
  cursor: pointer;
  background: rgba(18, 24, 38, 0.6);
  border: 1px solid rgba(255, 255, 255, 0.06);
}

.song-card:hover {
  transform: translateY(-4px);
  background: rgba(30, 41, 59, 0.7);
  border-color: rgba(255, 255, 255, 0.15);
  box-shadow: 0 12px 28px -6px rgba(0, 0, 0, 0.5);
}

.song-card.is-active {
  border-color: rgba(99, 102, 241, 0.6);
  background: rgba(30, 41, 59, 0.85);
  box-shadow: 0 0 20px rgba(99, 102, 241, 0.2);
}

/* Thumbnail */
.thumb-container {
  position: relative;
  width: 100%;
  aspect-ratio: 16 / 10;
  overflow: hidden;
  background: #0f172a;
}

.card-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  transition: transform 0.4s ease;
}

.song-card:hover .card-img {
  transform: scale(1.05);
}

.fallback-art {
  width: 100%;
  height: 100%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  position: relative;
}

.fallback-icon {
  width: 32px;
  height: 32px;
  color: rgba(255, 255, 255, 0.3);
}

.first-char {
  position: absolute;
  font-size: 42px;
  font-weight: 800;
  color: rgba(255, 255, 255, 0.08);
  user-select: none;
}

.overlay-mask {
  position: absolute;
  inset: 0;
  background: rgba(0, 0, 0, 0.4);
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0;
  transition: opacity 0.2s ease;
}

.song-card:hover .overlay-mask,
.song-card.is-active .overlay-mask {
  opacity: 1;
}

.play-trigger-btn {
  width: 48px;
  height: 48px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: 0 8px 16px rgba(0, 0, 0, 0.4);
  transform: scale(0.9);
  transition: transform 0.2s cubic-bezier(0.34, 1.56, 0.64, 1);
}

.song-card:hover .play-trigger-btn {
  transform: scale(1);
}

.equalizer-overlay {
  position: absolute;
  top: 10px;
  left: 10px;
  background: rgba(0, 0, 0, 0.7);
  backdrop-filter: blur(8px);
  padding: 6px 8px;
  border-radius: 8px;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

.category-pill {
  position: absolute;
  bottom: 8px;
  left: 8px;
  color: #ffffff;
  font-size: 11px;
  font-weight: 700;
  padding: 2px 8px;
  border-radius: 6px;
  backdrop-filter: blur(4px);
}

.duration-pill {
  position: absolute;
  bottom: 8px;
  right: 8px;
  background: rgba(0, 0, 0, 0.7);
  backdrop-filter: blur(6px);
  color: #e2e8f0;
  font-size: 11px;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 6px;
  font-variant-numeric: tabular-nums;
  border: 1px solid rgba(255, 255, 255, 0.08);
}

/* Card details */
.card-details {
  padding: 12px 14px 14px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.title-section {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

.song-title {
  font-size: 14px;
  font-weight: 700;
  color: #f8fafc;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.song-artist {
  font-size: 12px;
  color: #94a3b8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.card-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-top: 8px;
  border-top: 1px solid rgba(255, 255, 255, 0.05);
}

.file-info {
  display: flex;
  align-items: center;
  gap: 6px;
}

.file-size {
  font-size: 11px;
  color: #64748b;
  font-weight: 600;
}

.format-tag {
  font-size: 9px;
  font-weight: 800;
  color: #94a3b8;
  background: rgba(255, 255, 255, 0.08);
  padding: 1px 4px;
  border-radius: 4px;
  letter-spacing: 0.05em;
}

.card-actions {
  display: flex;
  align-items: center;
  gap: 6px;
}

.mini-btn {
  color: #94a3b8;
  padding: 6px;
  border-radius: 6px;
  display: flex;
  align-items: center;
  justify-content: center;
  text-decoration: none;
}

.mini-btn:hover {
  color: #f8fafc;
  background: rgba(255, 255, 255, 0.1);
}

.fav-btn.is-active {
  color: #ec4899;
}
</style>
