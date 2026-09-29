<template>
  <transition name="player-slide">
    <footer v-if="currentSong" class="audio-player-container">
      <div class="player-wrapper glass-player">
      
      <!-- Top Scrubber Progress Bar -->
      <div 
        class="progress-bar-container"
        ref="progressContainer"
        @mousedown="startDrag"
        @mousemove="handleHover"
        @mouseleave="hideHover"
        @touchstart.passive="startTouchDrag"
      >
        <!-- Hover timestamp tooltip -->
        <div 
          v-if="hoverVisible" 
          class="hover-tooltip"
          :style="{ left: `${hoverPercent}%` }"
        >
          {{ formatTime(hoverTime) }}
        </div>

        <div class="progress-track">
          <!-- Buffered track -->
          <div class="progress-buffered" :style="{ width: `${bufferedPercent}%` }"></div>
          <!-- Played track -->
          <div 
            class="progress-played" 
            :style="{ 
              width: `${displayProgressPercent}%`,
              background: currentSong.color 
                ? `linear-gradient(90deg, #6366f1, ${currentSong.color})` 
                : 'linear-gradient(90deg, #6366f1, #a855f7)'
            }"
          ></div>
          <!-- Scrubber Thumb -->
          <div 
            class="progress-thumb"
            :class="{ 'is-dragging': isDraggingProgress }"
            :style="{ 
              left: `${displayProgressPercent}%`,
              borderColor: currentSong.color || '#a855f7'
            }"
          ></div>
        </div>
      </div>

      <!-- Player Content Bar -->
      <div class="player-content">
        
        <!-- Left: Track Info -->
        <div class="track-info">
          <!-- Thumbnail with vinyl/cover -->
          <div 
            class="cover-box"
            :class="{ 'playing-glow': isPlaying }"
            :style="{ '--glow-color': currentSong.color || '#6366f1' }"
          >
            <img 
              v-if="currentSong.cover" 
              :src="currentSong.cover" 
              :alt="currentSong.title"
              class="cover-img"
              :class="{ 'spinning-disc': isPlaying, 'spinning-paused': !isPlaying }"
              @error="currentSong.cover = ''"
            />
            <div 
              v-else 
              class="fallback-cover"
              :style="{ background: `linear-gradient(135deg, ${currentSong.color || '#6366f1'}88, #090b10)` }"
            >
              <Music class="w-5 h-5 text-white" />
            </div>

            <!-- Mini play state badge -->
            <div v-if="isPlaying" class="playing-indicator-badge">
              <span class="pulse-dot"></span>
            </div>
          </div>

          <div class="meta-box">
            <div class="title-row">
              <span class="track-title" :title="currentSong.title">{{ currentSong.title }}</span>
              <span 
                class="category-badge"
                :style="{ 
                  backgroundColor: `${currentSong.color || '#6366f1'}22`, 
                  color: currentSong.color || '#6366f1',
                  borderColor: `${currentSong.color || '#6366f1'}44`
                }"
              >
                {{ currentSong.categoryZh }}
              </span>
            </div>
            <div class="artist-row">
              <span class="track-artist">{{ currentSong.artist }}</span>
            </div>
          </div>

          <!-- Like / Favorite button -->
          <button 
            class="action-btn fav-btn" 
            :class="{ 'is-favorited': isFav(currentSong.id) }"
            @click="toggleFavorite(currentSong.id)"
            title="收藏歌曲"
          >
            <Heart class="icon" :fill="isFav(currentSong.id) ? '#ec4899' : 'none'" />
          </button>
        </div>

        <!-- Center: Playback Controls & Timestamps -->
        <div class="controls-section">
          <div class="buttons-row">
            <!-- Play mode toggle -->
            <button 
              class="control-btn" 
              :class="{ 'active': playMode !== 'sequence' }" 
              @click="togglePlayMode"
              :title="playModeTitle"
            >
              <Repeat1 v-if="playMode === 'repeat-one'" class="icon" />
              <Shuffle v-else-if="playMode === 'shuffle'" class="icon" />
              <Repeat v-else class="icon text-muted" />
            </button>

            <!-- Previous track -->
            <button class="control-btn" @click="prevTrack" title="上一首 (←)">
              <SkipBack class="icon" />
            </button>

            <!-- Big Play / Pause Button -->
            <button 
              class="play-main-btn" 
              @click="togglePlay"
              :title="isPlaying ? '暂停 (空格)' : '播放 (空格)'"
              :style="{ 
                boxShadow: isPlaying ? `0 0 24px ${currentSong.color || '#6366f1'}66` : 'none' 
              }"
            >
              <Pause v-if="isPlaying" class="icon-main fill-current" />
              <Play v-else class="icon-main fill-current translate-x-0.5" />
            </button>

            <!-- Next track -->
            <button class="control-btn" @click="nextTrack" title="下一首 (→)">
              <SkipForward class="icon" />
            </button>

            <!-- Speed selector -->
            <div class="speed-selector">
              <button 
                class="speed-btn"
                @click="cycleRate"
                title="调整播放倍速"
              >
                {{ playbackRate }}x
              </button>
            </div>
          </div>

          <!-- Time Display below or inline -->
          <div class="time-display">
            <span class="time-current">{{ formatTime(displayCurrentTime) }}</span>
            <span class="time-separator">/</span>
            <span class="time-total">{{ formatTime(duration) }}</span>
          </div>
        </div>

        <!-- Right: Volume, Download, Queue -->
        <div class="utility-section">
          <!-- Volume control -->
          <div class="volume-container">
            <button class="action-btn" @click="toggleMute" title="静音切换 (M)">
              <VolumeX v-if="isMuted || volume === 0" class="icon" />
              <Volume1 v-else-if="volume < 0.5" class="icon" />
              <Volume2 v-else class="icon" />
            </button>
            <div class="volume-slider-box">
              <input 
                type="range" 
                min="0" 
                max="1" 
                step="0.01" 
                :value="isMuted ? 0 : volume" 
                @input="handleVolumeChange"
                class="volume-range"
              />
            </div>
            <span class="volume-percent">{{ Math.round((isMuted ? 0 : volume) * 100) }}%</span>
          </div>

          <!-- Download MP3 directly -->
          <a 
            :href="currentSong.url" 
            :download="`${currentSong.title}.mp3`"
            class="action-btn download-btn"
            title="下载当前音频文件 (.mp3)"
          >
            <Download class="icon" />
          </a>

          <!-- Playlist Queue Button -->
          <button 
            class="action-btn queue-btn" 
            :class="{ 'is-open': showQueueDrawer }"
            @click="$emit('toggle-queue')"
            title="播放列表队列"
          >
            <ListMusic class="icon" />
            <span class="queue-count">{{ queue.length }}</span>
          </button>
        </div>

      </div>
    </div>
  </footer>
</transition>
</template>

<script setup>
import { ref, computed, onMounted, onUnmounted } from 'vue';
import { 
  Play, Pause, SkipForward, SkipBack, Shuffle, Repeat, Repeat1, 
  Volume2, VolumeX, Volume1, Download, Heart, ListMusic, Music 
} from '@lucide/vue';
import { useAudioPlayer } from '../composables/useAudioPlayer';

const props = defineProps({
  showQueueDrawer: {
    type: Boolean,
    default: false
  }
});

defineEmits(['toggle-queue']);

const {
  currentSong,
  isPlaying,
  currentTime,
  duration,
  bufferedPercent,
  volume,
  isMuted,
  playbackRate,
  playMode,
  queue,
  isDraggingProgress,
  favorites,
  toggleFavorite,
  togglePlay,
  seek,
  setVolume,
  toggleMute,
  setPlaybackRate,
  togglePlayMode,
  nextTrack,
  prevTrack,
  formatTime
} = useAudioPlayer();

const progressContainer = ref(null);
const dragTime = ref(0);
const hoverVisible = ref(false);
const hoverPercent = ref(0);
const hoverTime = ref(0);

const isFav = (id) => favorites.value.has(id);

const displayCurrentTime = computed(() => {
  return isDraggingProgress.value ? dragTime.value : currentTime.value;
});

const displayProgressPercent = computed(() => {
  const dur = duration.value || 1;
  const cur = displayCurrentTime.value;
  return Math.min(100, Math.max(0, (cur / dur) * 100));
});

const playModeTitle = computed(() => {
  switch (playMode.value) {
    case 'repeat-one': return '单曲循环';
    case 'shuffle': return '随机播放';
    default: return '顺序播放';
  }
});

const cycleRate = () => {
  const rates = [1.0, 1.25, 1.5, 2.0, 0.75];
  const next = rates[(rates.indexOf(playbackRate.value) + 1) % rates.length];
  setPlaybackRate(next);
};

// Dragging logic
const calculateTimeFromEvent = (e) => {
  if (!progressContainer.value) return 0;
  const rect = progressContainer.value.getBoundingClientRect();
  const clientX = e.touches ? e.touches[0].clientX : e.clientX;
  const ratio = Math.max(0, Math.min(1, (clientX - rect.left) / rect.width));
  return ratio * (duration.value || 1);
};

const startDrag = (e) => {
  isDraggingProgress.value = true;
  dragTime.value = calculateTimeFromEvent(e);

  const onMouseMove = (moveEvent) => {
    dragTime.value = calculateTimeFromEvent(moveEvent);
  };

  const onMouseUp = () => {
    isDraggingProgress.value = false;
    seek(dragTime.value);
    window.removeEventListener('mousemove', onMouseMove);
    window.removeEventListener('mouseup', onMouseUp);
  };

  window.addEventListener('mousemove', onMouseMove);
  window.addEventListener('mouseup', onMouseUp);
};

const startTouchDrag = (e) => {
  isDraggingProgress.value = true;
  dragTime.value = calculateTimeFromEvent(e);

  const onTouchMove = (moveEvent) => {
    dragTime.value = calculateTimeFromEvent(moveEvent);
  };

  const onTouchEnd = () => {
    isDraggingProgress.value = false;
    seek(dragTime.value);
    window.removeEventListener('touchmove', onTouchMove);
    window.removeEventListener('touchend', onTouchEnd);
  };

  window.addEventListener('touchmove', onTouchMove, { passive: true });
  window.addEventListener('touchend', onTouchEnd);
};

const handleHover = (e) => {
  if (!progressContainer.value) return;
  const rect = progressContainer.value.getBoundingClientRect();
  const ratio = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
  hoverPercent.value = ratio * 100;
  hoverTime.value = ratio * (duration.value || 1);
  hoverVisible.value = true;
};

const hideHover = () => {
  hoverVisible.value = false;
};

const handleVolumeChange = (e) => {
  setVolume(parseFloat(e.target.value));
};

// Global keyboard shortcuts
const handleKeyDown = (e) => {
  // If user is typing or interacting with form elements, don't trigger shortcuts
  if (['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target?.tagName) || e.target?.isContentEditable) return;

  if (e.code === 'Space') {
    e.preventDefault();
    togglePlay();
  } else if (e.code === 'ArrowLeft') {
    e.preventDefault();
    seek(currentTime.value - 5);
  } else if (e.code === 'ArrowRight') {
    e.preventDefault();
    seek(currentTime.value + 5);
  } else if (e.code === 'ArrowUp') {
    e.preventDefault();
    setVolume(Math.min(1, Math.round((volume.value + 0.1) * 100) / 100));
  } else if (e.code === 'ArrowDown') {
    e.preventDefault();
    setVolume(Math.max(0, Math.round((volume.value - 0.1) * 100) / 100));
  } else if (e.key === 'm' || e.key === 'M') {
    toggleMute();
  }
};

onMounted(() => {
  window.addEventListener('keydown', handleKeyDown);
});

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeyDown);
});
</script>

<style scoped>
.audio-player-container {
  position: fixed;
  bottom: 0;
  left: 0;
  right: 0;
  z-index: 1000;
  padding: 0 16px 16px;
  pointer-events: none;
}

.player-wrapper {
  max-width: 1380px;
  margin: 0 auto;
  border-radius: 20px;
  box-shadow: 0 20px 40px -10px rgba(0, 0, 0, 0.7), 0 0 1px 1px rgba(255, 255, 255, 0.1);
  position: relative;
  overflow: hidden;
  pointer-events: auto;
  transition: all 0.3s ease;
}

/* Scrubber Bar on top edge */
.progress-bar-container {
  position: relative;
  height: 12px;
  width: 100%;
  cursor: pointer;
  display: flex;
  align-items: center;
  padding: 0;
  margin-top: -2px;
}

.progress-track {
  position: relative;
  width: 100%;
  height: 4px;
  background: rgba(255, 255, 255, 0.08);
  transition: height 0.15s ease;
}

.progress-bar-container:hover .progress-track {
  height: 6px;
}

.progress-buffered {
  position: absolute;
  top: 0;
  bottom: 0;
  left: 0;
  background: rgba(255, 255, 255, 0.15);
  border-radius: 4px;
}

.progress-played {
  position: absolute;
  top: 0;
  bottom: 0;
  left: 0;
  border-radius: 4px;
  transition: width 0.08s linear;
}

.progress-thumb {
  position: absolute;
  top: 50%;
  width: 14px;
  height: 14px;
  border-radius: 50%;
  background: #ffffff;
  border: 2px solid #6366f1;
  box-shadow: 0 0 10px rgba(0, 0, 0, 0.6);
  transform: translate(-50%, -50%) scale(0);
  transition: transform 0.15s cubic-bezier(0.175, 0.885, 0.32, 1.275);
  pointer-events: none;
}

.progress-bar-container:hover .progress-thumb,
.progress-thumb.is-dragging {
  transform: translate(-50%, -50%) scale(1);
}

.hover-tooltip {
  position: absolute;
  top: -28px;
  transform: translateX(-50%);
  background: #1e293b;
  color: #f8fafc;
  font-size: 11px;
  font-weight: 600;
  padding: 3px 8px;
  border-radius: 6px;
  box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  pointer-events: none;
  white-space: nowrap;
  border: 1px solid rgba(255, 255, 255, 0.1);
}

/* Player Content */
.player-content {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 24px 14px;
  gap: 20px;
}

/* Left: Track info */
.track-info {
  display: flex;
  align-items: center;
  gap: 14px;
  flex: 1;
  min-width: 0;
  max-width: 340px;
}

.cover-box {
  width: 50px;
  height: 50px;
  border-radius: 12px;
  overflow: hidden;
  position: relative;
  flex-shrink: 0;
  background: #111827;
  border: 1px solid rgba(255, 255, 255, 0.1);
  display: flex;
  align-items: center;
  justify-content: center;
}

.cover-box.playing-glow {
  box-shadow: 0 0 20px var(--glow-color);
}

.cover-img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  border-radius: 50%;
  padding: 2px;
}

.fallback-cover {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
}

.playing-indicator-badge {
  position: absolute;
  bottom: 2px;
  right: 2px;
  width: 8px;
  height: 8px;
  background: #10b981;
  border-radius: 50%;
  box-shadow: 0 0 6px #10b981;
}

.meta-box {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.title-row {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.track-title {
  font-size: 14px;
  font-weight: 700;
  color: #f8fafc;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.category-badge {
  font-size: 11px;
  font-weight: 600;
  padding: 1px 7px;
  border-radius: 99px;
  border: 1px solid;
  flex-shrink: 0;
}

.artist-row {
  display: flex;
  align-items: center;
}

.track-artist {
  font-size: 12px;
  color: #94a3b8;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.fav-btn {
  color: #64748b;
  padding: 6px;
  border-radius: 8px;
  flex-shrink: 0;
}

.fav-btn:hover {
  color: #ec4899;
  background: rgba(236, 72, 153, 0.1);
}

.fav-btn.is-favorited {
  color: #ec4899;
}

/* Center Controls */
.controls-section {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  flex: 1.2;
}

.buttons-row {
  display: flex;
  align-items: center;
  gap: 16px;
}

.control-btn {
  color: #94a3b8;
  padding: 8px;
  border-radius: 50%;
  display: flex;
  align-items: center;
  justify-content: center;
}

.control-btn:hover {
  color: #f8fafc;
  background: rgba(255, 255, 255, 0.08);
}

.control-btn.active {
  color: #8b5cf6;
}

.play-main-btn {
  width: 44px;
  height: 44px;
  border-radius: 50%;
  background: #ffffff;
  color: #090b10;
  display: flex;
  align-items: center;
  justify-content: center;
  transition: all 0.2s cubic-bezier(0.34, 1.56, 0.64, 1);
}

.play-main-btn:hover {
  transform: scale(1.08);
  background: #f1f5f9;
}

.play-main-btn:active {
  transform: scale(0.95);
}

.icon-main {
  width: 20px;
  height: 20px;
}

.speed-selector {
  position: relative;
}

.speed-btn {
  font-size: 11px;
  font-weight: 700;
  color: #94a3b8;
  padding: 3px 7px;
  border-radius: 6px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  background: rgba(255, 255, 255, 0.04);
}

.speed-btn:hover {
  color: #f8fafc;
  border-color: rgba(255, 255, 255, 0.2);
}

.time-display {
  display: flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
  color: #64748b;
}

.time-current {
  color: #cbd5e1;
}

/* Right: Utilities */
.utility-section {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 12px;
  flex: 1;
  max-width: 340px;
}

.volume-container {
  display: flex;
  align-items: center;
  gap: 6px;
}

.volume-slider-box {
  width: 80px;
  display: flex;
  align-items: center;
}

.volume-range {
  width: 100%;
}

.volume-percent {
  font-size: 11px;
  font-weight: 600;
  color: #64748b;
  min-width: 32px;
}

.action-btn {
  color: #94a3b8;
  padding: 8px;
  border-radius: 10px;
  display: flex;
  align-items: center;
  justify-content: center;
  position: relative;
}

.action-btn:hover {
  color: #f8fafc;
  background: rgba(255, 255, 255, 0.08);
}

.download-btn {
  text-decoration: none;
}

.queue-btn.is-open {
  color: #8b5cf6;
  background: rgba(139, 92, 246, 0.15);
}

.queue-count {
  position: absolute;
  top: 2px;
  right: 2px;
  background: #6366f1;
  color: #ffffff;
  font-size: 9px;
  font-weight: 700;
  padding: 1px 4px;
  border-radius: 99px;
  line-height: 1;
}

.icon {
  width: 18px;
  height: 18px;
}

@media (max-width: 860px) {
  .player-content {
    padding: 8px 16px 12px;
  }
  .utility-section {
    display: none;
  }
}

/* Slide up/down transition for player */
.player-slide-enter-active,
.player-slide-leave-active {
  transition: all 0.35s cubic-bezier(0.16, 1, 0.3, 1);
}

.player-slide-enter-from,
.player-slide-leave-to {
  opacity: 0;
  transform: translateY(100%);
}
</style>
