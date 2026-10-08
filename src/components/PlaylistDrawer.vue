<template>
  <div v-if="isOpen" class="drawer-backdrop" @click="$emit('close')">
    <div class="drawer-panel glass-panel" @click.stop>
      <!-- Header -->
      <div class="drawer-header">
        <div class="header-left">
          <ListMusic class="w-5 h-5 text-indigo-400" />
          <h3 class="drawer-title">播放队列</h3>
          <span class="queue-badge">{{ queue.length }} 首</span>
        </div>

        <div class="header-actions">
          <button 
            v-if="queue.length > 0" 
            class="clear-all-btn" 
            @click="clearQueue"
            title="清空队列"
          >
            <Trash2 class="w-4 h-4" />
            <span>清空</span>
          </button>
          <button class="close-btn" @click="$emit('close')" title="关闭">
            <X class="w-5 h-5" />
          </button>
        </div>
      </div>

      <!-- Queue List -->
      <div class="drawer-content">
        <div v-if="queue.length === 0" class="empty-state">
          <Music class="w-12 h-12 text-slate-600 mb-2" />
          <p>播放队列为空</p>
          <span>在歌曲列表中点击试听添加歌曲</span>
        </div>

        <div 
          v-else 
          v-for="(song, idx) in queue" 
          :key="songKey(song)"
          class="queue-item"
          :class="{ 'is-current': isCurrent(song) }"
          @click="playSong(song)"
        >
          <!-- Current Indicator -->
          <div class="item-left">
            <div v-if="isCurrent(song) && isPlaying" class="equalizer">
              <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
              <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
              <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
              <div class="equalizer-bar" :style="{ backgroundColor: song.color || '#6366f1' }"></div>
            </div>
            <span v-else class="queue-idx">{{ idx + 1 }}</span>

            <div class="queue-meta">
              <span class="song-name" :title="song.title">{{ song.title }}</span>
            </div>
          </div>

          <div class="item-right">
            <span 
              class="cat-chip" 
              :style="{ 
                color: song.color || '#6366f1',
                backgroundColor: `${song.color || '#6366f1'}15`
              }"
            >
              {{ song.categoryZh }}
            </span>
            <button 
              class="item-remove-btn" 
              @click.stop="removeFromQueue(idx)" 
              title="从队列移除"
            >
              <X class="w-3.5 h-3.5" />
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ListMusic, X, Music, Trash2 } from '@lucide/vue';
import { useAudioPlayer } from '../composables/useAudioPlayer.ts';
import { songKey } from '../utils/songSchema.ts';
import type { Song } from '../types/bgm.ts';

withDefaults(defineProps<{
  /** 抽屉是否展开 */
  isOpen?: boolean;
}>(), {
  isOpen: false,
});

defineEmits<{
  close: [];
}>();

const { queue, currentSong, isPlaying, playSong, removeFromQueue, clearQueue } = useAudioPlayer();

const isCurrent = (song: Song) => !!currentSong.value && songKey(currentSong.value) === songKey(song);
</script>

<style scoped>
.drawer-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  backdrop-filter: blur(4px);
  z-index: 1100;
  display: flex;
  justify-content: flex-end;
  animation: fadeIn 0.2s ease;
}

@keyframes fadeIn {
  from { opacity: 0; }
  to { opacity: 1; }
}

.drawer-panel {
  width: 380px;
  max-width: 90vw;
  height: 100%;
  background: #0d1117;
  border-left: 1px solid rgba(255, 255, 255, 0.08);
  border-radius: 0;
  display: flex;
  flex-direction: column;
  padding: 20px;
  box-shadow: -10px 0 30px rgba(0, 0, 0, 0.5);
  animation: slideIn 0.25s cubic-bezier(0.16, 1, 0.3, 1);
}

@keyframes slideIn {
  from { transform: translateX(100%); }
  to { transform: translateX(0); }
}

.drawer-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding-bottom: 16px;
  border-bottom: 1px solid rgba(255, 255, 255, 0.08);
}

.header-left {
  display: flex;
  align-items: center;
  gap: 10px;
}

.drawer-title {
  font-size: 16px;
  font-weight: 700;
  color: #f8fafc;
}

.queue-badge {
  font-size: 11px;
  font-weight: 700;
  color: #818cf8;
  background: rgba(99, 102, 241, 0.15);
  padding: 2px 8px;
  border-radius: 99px;
}

.close-btn {
  color: #94a3b8;
  padding: 6px;
  border-radius: 8px;
}

.close-btn:hover {
  color: #f8fafc;
  background: rgba(255, 255, 255, 0.08);
}

.drawer-content {
  flex: 1;
  overflow-y: auto;
  padding: 12px 0;
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 240px;
  color: #64748b;
  text-align: center;
}

.empty-state p {
  font-size: 14px;
  font-weight: 600;
  color: #94a3b8;
}

.empty-state span {
  font-size: 12px;
  margin-top: 4px;
}

.queue-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 10px 12px;
  border-radius: 10px;
  cursor: pointer;
  transition: all 0.15s ease;
}

.queue-item:hover {
  background: rgba(255, 255, 255, 0.05);
}

.queue-item.is-current {
  background: rgba(99, 102, 241, 0.12);
}

.item-left {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
  flex: 1;
}

.queue-idx {
  font-size: 12px;
  font-weight: 600;
  color: #64748b;
  width: 20px;
  flex-shrink: 0;
}

.queue-meta {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.song-name {
  font-size: 13px;
  font-weight: 600;
  color: #f1f5f9;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.queue-item.is-current .song-name {
  color: #818cf8;
}

.item-right {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-shrink: 0;
}

.cat-chip {
  font-size: 10px;
  font-weight: 700;
  padding: 1px 6px;
  border-radius: 4px;
}

.header-actions {
  display: flex;
  align-items: center;
  gap: 8px;
}

.clear-all-btn {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  color: #94a3b8;
  padding: 4px 8px;
  border-radius: 6px;
  transition: all 0.15s ease;
}

.clear-all-btn:hover {
  color: #ef4444;
  background: rgba(239, 68, 68, 0.1);
}

.item-remove-btn {
  color: #64748b;
  padding: 3px;
  border-radius: 4px;
  display: flex;
  align-items: center;
  justify-content: center;
  opacity: 0;
  transition: all 0.15s ease;
}

.queue-item:hover .item-remove-btn {
  opacity: 1;
}

.item-remove-btn:hover {
  color: #ef4444;
  background: rgba(239, 68, 68, 0.15);
}
</style>
