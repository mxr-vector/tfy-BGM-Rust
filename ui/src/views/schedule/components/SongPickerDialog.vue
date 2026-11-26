<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { formatTime } from '@/lib/format';
import type { Song } from '@/lib/types';
import { state } from '@/stores/state';
import Icon from '@/components/Icon.vue';

const props = withDefaults(
  defineProps<{
    open: boolean;
    existing?: string[];
    mode?: 'multiple' | 'single';
    title?: string;
  }>(),
  {
    existing: () => [],
    mode: 'multiple',
    title: '选择歌曲',
  },
);
const emit = defineEmits<{ close: []; confirm: [keys: string[]] }>();

const single = computed(() => props.mode === 'single');

interface Group {
  id: string;
  label: string;
  color: string;
  songs: Song[];
}

const query = ref('');
const selected = ref(new Set<string>());
const expanded = ref(new Set<string>());
const searchRef = ref<HTMLInputElement | null>(null);

const groups = computed<Group[]>(() => {
  const keyword = query.value.trim().toLowerCase();
  const existing = new Set(props.existing);
  const result: Group[] = [];

  for (const category of state.library.categories) {
    const names = [category.nameZh, category.nameEn, category.labelEn, category.id]
      .join(' ')
      .toLowerCase();
    const categoryMatched = keyword.length > 0 && names.includes(keyword);
    const songs = state.library.songs.filter((song) => {
      if (song.category !== category.id || existing.has(song.key)) return false;
      if (!keyword || categoryMatched) return true;
      return [song.title, song.fileName, song.artist, song.album, song.genre]
        .filter(Boolean)
        .some((text) => String(text).toLowerCase().includes(keyword));
    });
    if (songs.length > 0) {
      result.push({ id: category.id, label: category.nameZh, color: category.color, songs });
    }
  }
  return result;
});

const visibleSongs = computed(() => groups.value.flatMap((group) => group.songs));

const allVisibleSelected = computed(
  () =>
    visibleSongs.value.length > 0 &&
    visibleSongs.value.every((song) => selected.value.has(song.key)),
);

const selectedCount = computed(() => selected.value.size);

watch(
  () => props.open,
  async (open) => {
    if (!open) return;
    query.value = '';
    selected.value = new Set();
    expanded.value = new Set();
    await nextTick();
    searchRef.value?.focus();
  },
);

function isOpen(group: Group): boolean {
  return expanded.value.has(group.id) || query.value.trim().length > 0;
}

function toggleExpand(id: string): void {
  const next = new Set(expanded.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expanded.value = next;
}

function groupState(group: Group): 'none' | 'partial' | 'all' {
  const picked = group.songs.filter((song) => selected.value.has(song.key)).length;
  if (picked === 0) return 'none';
  return picked === group.songs.length ? 'all' : 'partial';
}

function toggleGroup(group: Group): void {
  const next = new Set(selected.value);
  const all = groupState(group) === 'all';
  for (const song of group.songs) {
    if (all) next.delete(song.key);
    else next.add(song.key);
  }
  selected.value = next;
}

function toggleSong(key: string): void {
  if (single.value) {
    selected.value = selected.value.has(key) ? new Set() : new Set([key]);
    return;
  }
  const next = new Set(selected.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  selected.value = next;
}

function toggleAll(): void {
  const next = new Set(selected.value);
  if (allVisibleSelected.value) {
    for (const song of visibleSongs.value) next.delete(song.key);
  } else {
    for (const song of visibleSongs.value) next.add(song.key);
  }
  selected.value = next;
}

function groupClick(group: Group): void {
  if (single.value) toggleExpand(group.id);
  else toggleGroup(group);
}

function confirm(): void {
  if (selectedCount.value === 0 || (single.value && selectedCount.value !== 1)) return;
  const keys = state.library.songs
    .filter((song) => selected.value.has(song.key))
    .map((song) => song.key);
  emit('confirm', keys);
}

function duration(song: Song): string {
  return song.durationMs ? formatTime(song.durationMs / 1000) : '--:--';
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape' || !props.open) return;
  event.preventDefault();
  event.stopPropagation();
  emit('close');
}

onMounted(() => document.addEventListener('keydown', onKeydown, true));
onBeforeUnmount(() => document.removeEventListener('keydown', onKeydown, true));
</script>

<template>
  <Teleport to="body">
    <div v-if="open" class="modal-backdrop" @mousedown.self="emit('close')">
      <div class="modal-panel picker-panel" role="dialog" aria-modal="true" :aria-label="title">
        <div class="modal-header">
          <div class="picker-heading">
            <h2 class="modal-title">{{ title }}</h2>
            <span class="picker-heading-hint">
              {{ single ? '选择一首歌曲替换当前曲目' : '歌单 → 歌曲逐级展开' }}
            </span>
          </div>
          <button type="button" class="modal-close" title="关闭" @click="emit('close')">
            <Icon name="x" />
          </button>
        </div>

        <div class="picker-toolbar">
          <label class="picker-search">
            <Icon name="search" />
            <input
              ref="searchRef"
              v-model="query"
              type="search"
              placeholder="搜索歌单、歌曲、艺人…"
              autocomplete="off"
              spellcheck="false"
            />
            <button
              v-if="query"
              type="button"
              class="picker-search-clear"
              title="清空搜索"
              @click="query = ''"
            >
              <Icon name="x" />
            </button>
          </label>
          <button
            v-if="!single"
            type="button"
            class="picker-link"
            :disabled="visibleSongs.length === 0"
            @click="toggleAll"
          >
            {{ allVisibleSelected ? '取消全选' : '全选结果' }}
          </button>
        </div>

        <div class="picker-body">
          <p v-if="groups.length === 0" class="picker-empty">
            {{
              query
                ? '没有匹配的歌曲'
                : single
                  ? '曲库中没有可替换的歌曲'
                  : '曲库中没有可添加的歌曲'
            }}
          </p>

          <div v-for="group in groups" :key="group.id" class="picker-group">
            <div class="picker-group-head">
              <button
                type="button"
                class="picker-caret"
                :class="{ 'is-open': isOpen(group) }"
                :title="isOpen(group) ? '折叠歌单' : '展开歌单'"
                @click="toggleExpand(group.id)"
              >
                <Icon name="chevron-down" />
              </button>
              <button
                v-if="!single"
                type="button"
                class="picker-check"
                :class="{ 'is-on': groupState(group) === 'all', 'is-partial': groupState(group) === 'partial' }"
                :title="groupState(group) === 'all' ? '取消选择该歌单' : '选择该歌单全部歌曲'"
                @click="toggleGroup(group)"
              >
                <Icon v-if="groupState(group) === 'all'" name="check" />
                <i v-else-if="groupState(group) === 'partial'" class="picker-dash"></i>
              </button>
              <button type="button" class="picker-group-title" @click="groupClick(group)">
                <span class="picker-dot" :style="{ background: group.color }"></span>
                <span class="picker-group-name">{{ group.label }}</span>
                <span class="picker-count">{{ group.songs.length }}</span>
              </button>
            </div>

            <div v-if="isOpen(group)" class="picker-songs">
              <button
                v-for="song in group.songs"
                :key="song.key"
                type="button"
                class="picker-song"
                :class="{ 'is-on': selected.has(song.key) }"
                @click="toggleSong(song.key)"
              >
                <span class="picker-check is-small" :class="{ 'is-on': selected.has(song.key) }">
                  <Icon v-if="selected.has(song.key)" name="check" />
                </span>
                <span class="picker-song-title" :title="song.title">{{ song.title }}</span>
                <span class="picker-song-artist">{{ song.artist ?? '' }}</span>
                <span class="picker-song-time">{{ duration(song) }}</span>
              </button>
            </div>
          </div>
        </div>

        <div class="modal-footer picker-footer">
          <span class="picker-selected">已选 {{ selectedCount }} 首</span>
          <button type="button" class="modal-btn ghost" @click="emit('close')">取消</button>
          <button
            type="button"
            class="modal-btn primary"
            :disabled="single ? selectedCount !== 1 : selectedCount === 0"
            @click="confirm"
          >
            确定
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>