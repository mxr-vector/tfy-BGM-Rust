<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { formatTime } from '@/lib/format';
import { formatMinutes, minutesOf, type Draft, type DraftSong } from '../composables/schedule-draft';
import { categoryById, songByKey } from '@/stores/state';
import Icon from '@/components/Icon.vue';
import SongPickerDialog from './SongPickerDialog.vue';

const props = defineProps<{ draft: Draft }>();

const pickerOpen = ref(false);
const replaceIndex = ref<number | null>(null);
const songsOpen = ref(true);
const dragIndex = ref<number | null>(null);

const pickerVisible = computed(() => pickerOpen.value || replaceIndex.value !== null);
const pickerExisting = computed(() => {
  const keys = props.draft.songs.map((song) => song.key);
  const current =
    replaceIndex.value !== null ? props.draft.songs[replaceIndex.value]?.key : null;
  return current ? keys.filter((key) => key !== current) : keys;
});

function songTitle(key: string): string {
  return songByKey(key)?.title ?? '曲目已移除';
}

function songCategoryLabel(key: string): string {
  const song = songByKey(key);
  if (!song) return '不在曲库';
  return categoryById(song.category)?.nameZh ?? song.category;
}

function songDurationLabel(key: string): string {
  const ms = songByKey(key)?.durationMs ?? 0;
  return ms > 0 ? formatTime(ms / 1000) : '--:--';
}

function draftRange(): { start: number; end: number } | null {
  const start = minutesOf(props.draft.start);
  const end = minutesOf(props.draft.end);
  if (start === null || end === null || end <= start) return null;
  return { start, end };
}

function normalizeSongWindow(song: DraftSong): void {
  const range = draftRange();
  if (!range) return;
  const start = Math.min(
    Math.max(minutesOf(song.start) ?? range.start, range.start),
    range.end - 1,
  );
  const end = Math.min(Math.max(minutesOf(song.end) ?? range.end, start + 1), range.end);
  song.start = formatMinutes(start);
  song.end = formatMinutes(end);
}

watch(
  () => [props.draft.start, props.draft.end] as const,
  () => {
    for (const song of props.draft.songs) normalizeSongWindow(song);
  },
);

/** 按歌曲时长比例把节目时间范围铺满，保证每首歌至少有 1 分钟。 */
function autoFillSongs(): void {
  const range = draftRange();
  if (!range || props.draft.songs.length === 0) return;
  const count = props.draft.songs.length;
  const span = range.end - range.start;

  if (span < count) {
    for (const song of props.draft.songs) {
      song.start = formatMinutes(range.start);
      song.end = formatMinutes(range.end);
    }
    return;
  }

  const weights = props.draft.songs.map((song) => {
    const ms = songByKey(song.key)?.durationMs ?? 0;
    return ms > 0 ? Math.max(1, Math.round(ms / 60_000)) : 3;
  });
  const total = weights.reduce((sum, value) => sum + value, 0) || count;
  let cursor = range.start;

  props.draft.songs.forEach((song, index) => {
    const remaining = range.end - cursor;
    const remainingSongs = count - index;
    const length =
      index === count - 1
        ? remaining
        : Math.max(
            1,
            Math.min(
              Math.round((span * weights[index]!) / total),
              remaining - (remainingSongs - 1),
            ),
          );
    song.start = formatMinutes(cursor);
    cursor = Math.min(range.end, cursor + length);
    song.end = formatMinutes(cursor);
  });
}

function addPickedSongs(keys: string[]): void {
  pickerOpen.value = false;
  const existing = new Set(props.draft.songs.map((song) => song.key));
  const range = draftRange();
  for (const key of keys) {
    if (existing.has(key)) continue;
    existing.add(key);
    props.draft.songs.push({
      key,
      start: range ? formatMinutes(range.start) : props.draft.start,
      end: range ? formatMinutes(range.end) : props.draft.end,
    });
  }
  autoFillSongs();
  songsOpen.value = true;
}

function openAddPicker(): void {
  replaceIndex.value = null;
  pickerOpen.value = true;
}

function openReplacePicker(index: number): void {
  pickerOpen.value = false;
  replaceIndex.value = index;
}

function closePicker(): void {
  pickerOpen.value = false;
  replaceIndex.value = null;
}

function handlePickedSongs(keys: string[]): void {
  const index = replaceIndex.value;
  if (index !== null) {
    const key = keys[0];
    const song = props.draft.songs[index];
    if (key && song) song.key = key;
    closePicker();
    return;
  }
  addPickedSongs(keys);
}

function removeSong(index: number): void {
  props.draft.songs.splice(index, 1);
}

/** 拖拽排序后把原来的时间槽按位置重新分配，保持列表顺序与播放时间一致。 */
function reorderSongs(from: number, to: number): void {
  if (from === to) return;
  const windows = props.draft.songs.map((song) => ({ start: song.start, end: song.end }));
  const [moved] = props.draft.songs.splice(from, 1);
  if (!moved) return;
  props.draft.songs.splice(to, 0, moved);
  props.draft.songs.forEach((song, index) => {
    const window = windows[index];
    if (window) {
      song.start = window.start;
      song.end = window.end;
    }
  });
}

function startSongDrag(index: number, event: PointerEvent): void {
  if (event.button !== 0) return;
  event.preventDefault();
  const handle = event.currentTarget as HTMLElement;
  handle.setPointerCapture?.(event.pointerId);
  dragIndex.value = index;
  window.addEventListener('pointermove', onSongDragMove);
  window.addEventListener('pointerup', endSongDrag);
  window.addEventListener('pointercancel', endSongDrag);
}

function onSongDragMove(event: PointerEvent): void {
  const from = dragIndex.value;
  if (from === null) return;
  const row = document
    .elementFromPoint(event.clientX, event.clientY)
    ?.closest<HTMLElement>('.editor-song');
  if (!row) return;
  const to = Number(row.dataset.index);
  if (!Number.isInteger(to) || to === from) return;
  reorderSongs(from, to);
  dragIndex.value = to;
}

function endSongDrag(): void {
  dragIndex.value = null;
  window.removeEventListener('pointermove', onSongDragMove);
  window.removeEventListener('pointerup', endSongDrag);
  window.removeEventListener('pointercancel', endSongDrag);
}

onBeforeUnmount(endSongDrag);
</script>

<template>
  <div class="schedule-editor">
    <div class="editor-grid">
      <label class="editor-field">
        <span>节目名称</span>
        <input v-model="draft.name" type="text" placeholder="例如：午后轻快" maxlength="40" />
      </label>
      <label class="editor-field">
        <span>开始时间</span>
        <input v-model="draft.start" type="time" step="60" />
      </label>
      <label class="editor-field">
        <span>结束时间</span>
        <input v-model="draft.end" type="time" step="60" />
      </label>
      <label class="editor-check">
        <input v-model="draft.enabled" type="checkbox" />
        <span>启用这个节目</span>
      </label>
    </div>

    <div class="editor-row">
      <label class="editor-check">
        <input v-model="draft.volumeEnabled" type="checkbox" />
        <span>节目开始时调整音量</span>
      </label>
      <input
        class="schedule-range"
        type="range"
        min="0"
        max="1"
        step="0.01"
        v-model.number="draft.volume"
        :disabled="!draft.volumeEnabled"
      />
      <span class="editor-volume">{{ Math.round(draft.volume * 100) }}%</span>
    </div>

    <div class="editor-songs">
      <div class="editor-songs-head">
        <button type="button" class="editor-collapse" @click="songsOpen = !songsOpen">
          <span class="collapse-caret" :class="{ 'is-open': songsOpen }">
            <Icon name="chevron-down" />
          </span>
          <span>节目歌曲</span>
          <span class="editor-songs-count">{{ draft.songs.length }} 首</span>
        </button>
        <div class="editor-song-actions">
          <button
            type="button"
            class="schedule-ghost is-small"
            :disabled="draft.songs.length === 0"
            title="按歌曲时长重新铺满节目时间范围"
            @click="autoFillSongs"
          >
            自动排期
          </button>
          <button type="button" class="schedule-ghost is-small" @click="openAddPicker">
            <Icon name="plus" /> 添加歌曲
          </button>
        </div>
      </div>

      <div v-if="songsOpen" class="editor-song-list">
        <p v-if="draft.songs.length === 0" class="editor-song-empty">
          还没有歌曲。点「添加歌曲」，在弹窗中按歌单多选后确定。
        </p>
        <div
          v-for="(song, index) in draft.songs"
          :key="song.key"
          class="editor-song"
          :class="{ 'is-dragging': dragIndex === index }"
          :data-index="index"
        >
          <button
            type="button"
            class="drag-handle"
            title="拖拽排序"
            @pointerdown="startSongDrag(index, $event)"
          >
            <Icon name="grip-vertical" />
          </button>
          <span class="editor-song-index">{{ index + 1 }}</span>
          <div class="editor-song-main">
            <span class="editor-song-title" :title="songTitle(song.key)">
              {{ songTitle(song.key) }}
            </span>
            <span class="editor-song-sub">
              <span class="editor-song-cat">{{ songCategoryLabel(song.key) }}</span>
              <span class="editor-song-duration">{{ songDurationLabel(song.key) }}</span>
            </span>
          </div>
          <label class="editor-time">
            <span>开始</span>
            <input
              v-model="song.start"
              type="time"
              step="60"
              :min="draft.start"
              :max="draft.end"
              @change="normalizeSongWindow(song)"
            />
          </label>
          <label class="editor-time">
            <span>结束</span>
            <input
              v-model="song.end"
              type="time"
              step="60"
              :min="draft.start"
              :max="draft.end"
              @change="normalizeSongWindow(song)"
            />
          </label>
          <div class="editor-song-tools">
            <button
              type="button"
              class="replace-btn"
              title="替换为其他歌曲"
              @click="openReplacePicker(index)"
            >
              替换
            </button>
            <button
              type="button"
              class="is-danger"
              title="从节目移除"
              @click="removeSong(index)"
            >
              <Icon name="x" />
            </button>
          </div>
        </div>
        <p v-if="draft.songs.length > 0" class="editor-song-hint">
          每首歌可点「替换」直接换歌；拖动左侧排序图标调整顺序，排序会同步交换时间槽。
        </p>
      </div>
    </div>
  </div>

  <SongPickerDialog
    :open="pickerVisible"
    :existing="pickerExisting"
    :mode="replaceIndex !== null ? 'single' : 'multiple'"
    :title="replaceIndex !== null ? '替换歌曲' : '选择歌曲'"
    @close="closePicker"
    @confirm="handlePickedSongs"
  />
</template>