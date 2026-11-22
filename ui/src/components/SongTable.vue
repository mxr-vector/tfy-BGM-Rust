<script setup lang="ts">
import { computed, ref, watchEffect } from 'vue';
import { SORT_KEYS } from '@/lib/constants';
import { chooseLibrary } from '@/stores/actions';
import {
  reorderLibrary,
  resetFilters,
  selectMany,
  setSearch,
  setSelected,
} from '@/stores/library';
import { state, visibleSongs } from '@/stores/state';
import Icon from './Icon.vue';
import SongRow from './SongRow.vue';

const selectAllEl = ref<HTMLInputElement | null>(null);
const dragKey = ref<string | null>(null);
const dragOverKey = ref<string | null>(null);
let selectionAnchor: string | null = null;

const list = visibleSongs;
const hasSongs = computed(() => list.value.length > 0);
const libraryEmpty = computed(() => state.library.songs.length === 0);

const visibleSelected = computed(
  () => list.value.reduce((total, song) => total + (state.selected.has(song.key) ? 1 : 0), 0),
);
const allChecked = computed(
  () => list.value.length > 0 && visibleSelected.value === list.value.length,
);
const indeterminate = computed(
  () => visibleSelected.value > 0 && visibleSelected.value < list.value.length,
);

watchEffect(() => {
  if (selectAllEl.value) {
    selectAllEl.value.indeterminate = indeterminate.value;
  }
});

function onSelect(payload: { selected: boolean; shift: boolean }, key: string): void {
  if (payload.shift && selectionAnchor && selectionAnchor !== key) {
    const keys = list.value.map((song) => song.key);
    const from = keys.indexOf(selectionAnchor);
    const to = keys.indexOf(key);
    if (from >= 0 && to >= 0) {
      selectMany(keys.slice(Math.min(from, to), Math.max(from, to) + 1), payload.selected);
      selectionAnchor = key;
      return;
    }
  }
  setSelected(key, payload.selected);
  selectionAnchor = key;
}

function onSelectAll(event: Event): void {
  const checked = (event.target as HTMLInputElement).checked;
  selectMany(
    list.value.map((song) => song.key),
    checked,
  );
  selectionAnchor = null;
}

function onDragStart(key: string, event: DragEvent): void {
  if (state.sortBy !== SORT_KEYS.recommended) {
    event.preventDefault();
    return;
  }
  dragKey.value = key;
  if (event.dataTransfer) {
    event.dataTransfer.effectAllowed = 'move';
    event.dataTransfer.setData('text/plain', key);
  }
}

function onDragOver(key: string, event: DragEvent): void {
  if (!dragKey.value) return;
  event.preventDefault();
  if (event.dataTransfer) event.dataTransfer.dropEffect = 'move';
  dragOverKey.value = key !== dragKey.value ? key : null;
}

function onDrop(key: string, event: DragEvent): void {
  event.preventDefault();
  const from = dragKey.value;
  dragKey.value = null;
  dragOverKey.value = null;
  if (from && from !== key) reorderLibrary(from, key);
}

function onDragEnd(): void {
  dragKey.value = null;
  dragOverKey.value = null;
}

function resetAll(): void {
  resetFilters();
  setSearch('');
}
</script>

<template>
  <section v-if="hasSongs" class="songs-table" id="song-table">
    <div class="table-header">
      <span class="th-check">
        <input
          ref="selectAllEl"
          type="checkbox"
          id="select-all"
          aria-label="全选当前列表"
          :checked="allChecked"
          @change="onSelectAll"
        />
      </span>
      <span class="th-index">#</span>
      <span class="th-title">标题</span>
      <span class="th-duration">时长</span>
      <span class="th-favorite">喜欢</span>
      <span class="th-actions">操作</span>
    </div>
    <div class="table-body" id="song-list">
      <SongRow
        v-for="(song, index) in list"
        :key="song.key"
        :song="song"
        :index="index"
        :dragging="dragKey === song.key"
        :drag-over="dragOverKey === song.key"
        @select="onSelect($event, song.key)"
        @dragstart="onDragStart(song.key, $event)"
        @dragover="onDragOver(song.key, $event)"
        @drop="onDrop(song.key, $event)"
        @dragend="onDragEnd"
      />
    </div>
  </section>

  <section v-else class="empty-results" id="empty-state">
    <Icon name="music" />
    <template v-if="libraryEmpty">
      <h3 class="empty-title">曲库为空</h3>
      <p class="empty-text">
        当前曲库目录：{{ state.library.root || '未找到' }}。请把 mp3 / m4a / flac / wav / ogg
        音频放入对应分类子目录，或选择其他文件夹。
      </p>
      <button
        type="button"
        class="reset-filter-btn"
        data-empty-action="choose"
        @click="chooseLibrary"
      >
        选择曲库文件夹
      </button>
    </template>
    <template v-else>
      <h3 class="empty-title">没有找到匹配的音频素材</h3>
      <p class="empty-text">请尝试更换搜索词，或清除分类与筛选条件。</p>
      <button
        type="button"
        class="reset-filter-btn"
        data-empty-action="reset"
        @click="resetAll"
      >
        重置所有筛选
      </button>
    </template>
  </section>
</template>