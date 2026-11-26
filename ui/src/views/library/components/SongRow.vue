<script setup lang="ts">
import { computed } from 'vue';
import { DEFAULT_ACCENT, SORT_KEYS } from '@/lib/constants';
import { formatTime, withAlpha } from '@/lib/format';
import type { Song } from '@/lib/types';
import { openCategoryMenu, openSongMenu } from '../composables/menus';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { playSong } from '@/stores/player';
import { toggleFavorite } from '@/stores/prefs';
import { categoryById, state, visibleSongs } from '@/stores/state';
import Equalizer from '@/components/Equalizer.vue';
import Icon from '@/components/Icon.vue';

const props = defineProps<{
  song: Song;
  index: number;
  dragging: boolean;
  dragOver: boolean;
}>();

const emit = defineEmits<{
  select: [payload: { selected: boolean; shift: boolean }];
  dragstart: [event: DragEvent];
  dragover: [event: DragEvent];
  drop: [event: DragEvent];
  dragend: [];
}>();

const category = computed(() => categoryById(props.song.category));
const color = computed(() => category.value?.color ?? DEFAULT_ACCENT);
const isCurrent = computed(() => state.currentKey === props.song.key);
const isPlaying = computed(() => isCurrent.value && state.playing);
const isFavorite = computed(() => state.favorites.has(props.song.key));
const canDrag = computed(() => state.sortBy === SORT_KEYS.recommended);
const indexLabel = computed(() => String(props.index + 1).padStart(2, '0'));
/** 时长列；缺少元数据时显示占位符，避免布局跳动。 */
const durationLabel = computed(() =>
  props.song.durationMs ? formatTime(props.song.durationMs / 1000) : '--:--',
);
const styleLabel = computed(() =>
  (category.value?.labelEn || category.value?.nameEn || '').split('/')[0]?.trim() ?? '',
);

function playRow(): void {
  if (guardLibraryPlayback()) return;
  playSong(props.song, visibleSongs.value);
}

function onRowKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' && event.key !== ' ') return;
  const target = event.target instanceof Element ? event.target : null;
  if (target?.closest('button, a, [data-action], input, select, textarea')) return;
  event.preventDefault();
  event.stopPropagation();
  playRow();
}

function onCheckboxClick(event: MouseEvent): void {
  const checkbox = event.target as HTMLInputElement;
  emit('select', { selected: checkbox.checked, shift: event.shiftKey });
}

function onMenuClick(event: MouseEvent): void {
  event.stopPropagation();
  const target = event.currentTarget as HTMLElement;
  const rect = target.getBoundingClientRect();
  openSongMenu(
    { clientX: rect.right - 6, clientY: rect.bottom + 4 },
    props.song,
    false,
  );
}

function onContextMenu(event: MouseEvent): void {
  event.preventDefault();
  openSongMenu(event, props.song);
}

function onCategoryContextMenu(event: MouseEvent): void {
  event.preventDefault();
  event.stopPropagation();
  openCategoryMenu(event, category.value);
}
</script>

<template>
  <div
    class="song-row"
    :class="{
      'is-current': isCurrent,
      'is-selected': state.selected.has(song.key),
      'is-dragging': dragging,
      'is-drag-over': dragOver,
    }"
    :data-key="song.key"
    :draggable="canDrag"
    tabindex="0"
    :title="canDrag ? '拖动可调整顺序' : '按曲名排序时不可拖动'"
    @click="playRow"
    @keydown="onRowKeydown"
    @contextmenu="onContextMenu"
    @dragstart="emit('dragstart', $event)"
    @dragover="emit('dragover', $event)"
    @drop="emit('drop', $event)"
    @dragend="emit('dragend')"
  >
    <span class="row-check">
      <input
        type="checkbox"
        class="row-checkbox"
        :aria-label="`选择 ${song.title}`"
        :checked="state.selected.has(song.key)"
        @click.stop="onCheckboxClick"
      />
    </span>
    <div class="row-index">
      <span class="index-number">{{ indexLabel }}</span>
      <span class="row-play-btn">
        <Icon :name="isPlaying ? 'pause-fill' : 'play-fill'" />
      </span>
    </div>
    <div class="row-main">
      <div
        class="row-cover"
        :class="{ 'is-playing': isPlaying }"
        :style="{
          '--row-color': color,
          '--row-color-soft': withAlpha(color, 0.3),
        }"
      >
        <Equalizer v-if="isPlaying" color="#e9fff6" />
        <Icon v-else :name="category?.icon || 'music'" />
      </div>
      <div class="row-title-box">
        <span class="row-title" :title="song.title">{{ song.title }}</span>
        <div class="row-meta">
          <span
            class="category-tag"
            :data-category-id="song.category"
            :style="{ '--tag-color': withAlpha(color, 0.95) }"
            @contextmenu="onCategoryContextMenu"
          >
            {{ category?.nameZh ?? song.category }}
          </span>
          <span v-if="styleLabel" class="row-sub">{{ styleLabel }}</span>
        </div>
      </div>
    </div>
    <span class="row-duration" :title="`时长 ${durationLabel}`">{{ durationLabel }}</span>
    <div class="row-favorite">
      <button
        type="button"
        class="action-icon-btn fav-btn"
        :class="{ 'is-persistent': isFavorite, 'is-favorite': isFavorite }"
        :title="isFavorite ? '取消收藏' : '收藏'"
        data-action="favorite"
        :data-key="song.key"
        @click.stop="toggleFavorite(song.key)"
      >
        <Icon name="heart" />
      </button>
    </div>
    <div class="row-actions">
      <button
        type="button"
        class="action-icon-btn is-more"
        title="更多操作"
        data-action="menu"
        :data-key="song.key"
        @click="onMenuClick"
      >
        <Icon name="more" />
      </button>
    </div>
  </div>
</template>