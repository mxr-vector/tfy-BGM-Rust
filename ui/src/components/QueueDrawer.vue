<script setup lang="ts">
import { computed } from 'vue';
import { DEFAULT_ACCENT } from '@/lib/constants';
import { withAlpha } from '@/lib/format';
import {
  clearQueue,
  closeDrawer,
  drawerOpen,
  playSong,
  removeFromQueue,
} from '@/stores/player';
import { categoryById, state } from '@/stores/state';
import Equalizer from './Equalizer.vue';
import Icon from './Icon.vue';

const queue = computed(() => state.queue);

function songColor(categoryId: string): string {
  return categoryById(categoryId)?.color ?? DEFAULT_ACCENT;
}

function categoryName(categoryId: string): string {
  return categoryById(categoryId)?.nameZh ?? categoryId;
}
</script>

<template>
  <div
    v-if="drawerOpen"
    class="drawer-backdrop"
    id="drawer-backdrop"
    @click.self="closeDrawer"
  >
    <aside class="drawer-panel" role="dialog" aria-label="播放队列">
      <div class="drawer-header">
        <div class="drawer-heading">
          <Icon name="list-music" class="drawer-icon" />
          <h2 class="drawer-title">播放队列</h2>
          <span class="queue-badge" id="drawer-count">{{ queue.length }} 首</span>
        </div>
        <div class="drawer-actions">
          <button
            v-if="queue.length > 0"
            type="button"
            class="clear-all-btn"
            id="btn-clear-queue"
            @click="clearQueue"
          >
            <Icon name="trash" /><span>清空</span>
          </button>
          <button
            type="button"
            class="close-btn"
            id="btn-close-drawer"
            title="关闭队列"
            @click="closeDrawer"
          >
            <Icon name="x" />
          </button>
        </div>
      </div>
      <div class="drawer-body" id="queue-list">
        <div v-if="queue.length === 0" class="queue-empty">
          <Icon name="music" />
          <span>播放队列为空</span>
          <small>在曲库列表中点击曲目即可加入队列</small>
        </div>
        <template v-else>
          <div
            v-for="(song, index) in queue"
            :key="song.key"
            class="queue-item"
            :class="{ 'is-current': state.currentKey === song.key }"
            :data-key="song.key"
            @click="playSong(song)"
          >
            <div class="queue-item-left">
              <Equalizer
                v-if="state.currentKey === song.key && state.playing"
                :color="songColor(song.category)"
              />
              <span v-else class="queue-index">{{ index + 1 }}</span>
              <span class="queue-name" :title="song.title">{{ song.title }}</span>
            </div>
            <div class="queue-item-right">
              <span
                class="cat-chip"
                :style="{
                  color: songColor(song.category),
                  backgroundColor: withAlpha(songColor(song.category), 0.12),
                }"
              >
                {{ categoryName(song.category) }}
              </span>
              <button
                type="button"
                class="item-remove-btn"
                title="从队列移除"
                @click.stop="removeFromQueue(index)"
              >
                <Icon name="x" />
              </button>
            </div>
          </div>
        </template>
      </div>
    </aside>
  </div>
</template>