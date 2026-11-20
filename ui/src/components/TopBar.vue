<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import { config } from '@/lib/config';
import { SORT_KEYS } from '@/lib/constants';
import type { SortBy } from '@/lib/types';
import { openConfigFile, openImport, busyAction } from '@/stores/actions';
import { setSearch } from '@/stores/library';
import { setSortBy } from '@/stores/prefs';
import { state } from '@/stores/state';
import {
  popoverOpen,
  toggleWallpaperPopover,
  wallpaperView,
} from '@/composables/useWallpaper';
import Icon from './Icon.vue';
import WindowControls from './WindowControls.vue';

const searchInput = ref<HTMLInputElement | null>(null);
const wallpaperTrigger = ref<HTMLButtonElement | null>(null);
const sortOpen = ref(false);
const sortMenu = ref<HTMLElement | null>(null);
const sortButton = ref<HTMLButtonElement | null>(null);

const sortIcon = computed(() =>
  state.sortBy === SORT_KEYS.name ? 'sort-az' : 'sort',
);
const sortLabel = computed(() =>
  state.sortBy === SORT_KEYS.name ? '按曲名 A–Z' : '默认推荐',
);

const importing = computed(() => busyAction.value !== null);

function onSearchInput(event: Event): void {
  setSearch((event.target as HTMLInputElement).value);
}

function clearSearch(): void {
  setSearch('');
  searchInput.value?.focus();
  requestAnimationFrame(() => searchInput.value?.select());
}

function onSearchKeydown(event: KeyboardEvent): void {
  if (event.key === 'Enter' && !event.isComposing && event.keyCode !== 229) {
    event.preventDefault();
    searchInput.value?.blur();
  } else if (event.key === 'Escape') {
    if (state.search) {
      event.preventDefault();
      setSearch('');
    } else {
      searchInput.value?.blur();
    }
  }
}

function toggleSortMenu(): void {
  sortOpen.value = !sortOpen.value;
  if (sortOpen.value) {
    void nextTick(() => {
      sortMenu.value
        ?.querySelector<HTMLElement>('.sort-menu-item.is-active, .sort-menu-item')
        ?.focus();
    });
  }
}

function closeSortMenu(): void {
  sortOpen.value = false;
}

function chooseSort(value: SortBy): void {
  setSortBy(value);
  closeSortMenu();
  sortButton.value?.focus();
}

function onSortMenuKeydown(event: KeyboardEvent): void {
  const items = Array.from(
    sortMenu.value?.querySelectorAll<HTMLButtonElement>('.sort-menu-item') ?? [],
  );
  const index = items.indexOf(document.activeElement as HTMLButtonElement);
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    items[(index + 1) % items.length]?.focus();
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    items[(index - 1 + items.length) % items.length]?.focus();
  } else if (event.key === 'Escape') {
    event.preventDefault();
    closeSortMenu();
    sortButton.value?.focus();
  }
}

function toggleWallpaper(): void {
  if (wallpaperTrigger.value) toggleWallpaperPopover(wallpaperTrigger.value);
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape' && sortOpen.value) {
    closeSortMenu();
    sortButton.value?.focus();
  }
}

onMounted(() => {
  document.addEventListener('click', closeSortMenu);
  document.addEventListener('keydown', onDocumentKeydown);
});

onBeforeUnmount(() => {
  document.removeEventListener('click', closeSortMenu);
  document.removeEventListener('keydown', onDocumentKeydown);
});
</script>

<template>
  <header class="topbar" data-tauri-drag-region="deep">
    <div class="brand">
      <div class="brand-logo" id="brand-logo">
        <Icon name="disc" svg-class="disc-spin" />
      </div>
      <div class="brand-text">
        <span class="brand-title" id="brand-title">{{ config.brand.title }}</span>
        <span class="brand-subtitle" id="brand-subtitle">{{ config.brand.subtitle }}</span>
      </div>
    </div>

    <div
      class="search-box"
      :class="{ 'has-value': state.search.length > 0 }"
      data-tauri-drag-region="false"
    >
      <Icon name="search" class="search-icon" />
      <input
        id="search-input"
        ref="searchInput"
        class="search-input"
        type="text"
        autocomplete="off"
        spellcheck="false"
        placeholder="搜索曲名或风格，如 欢快 / 古风 / 史诗"
        :value="state.search"
        @input="onSearchInput"
        @keydown="onSearchKeydown"
      />
      <button
        v-show="state.search.length > 0"
        type="button"
        class="clear-search-btn"
        id="btn-clear-search"
        title="清除搜索"
        @click="clearSearch"
      >
        <Icon name="x" />
      </button>
      <kbd
        class="search-kbd"
        id="search-confirm-hint"
        title="回车确认搜索 · 按 / 快速聚焦"
        aria-label="按 / 快速聚焦搜索"
        >/</kbd
      >
    </div>

    <div class="top-actions">
      <button
        type="button"
        class="toolbar-btn"
        id="btn-import"
        title="导入本地 MP3 到曲库"
        :disabled="importing"
        @click="openImport"
      >
        <Icon name="upload" /><span class="toolbar-label">导入音乐</span>
      </button>
      <div class="sort-wrapper">
        <button
          ref="sortButton"
          type="button"
          class="sort-box"
          :class="{ 'is-open': sortOpen }"
          id="btn-sort"
          aria-haspopup="listbox"
          :aria-expanded="sortOpen"
          title="切换排序方式"
          @click.stop="toggleSortMenu"
        >
          <Icon :name="sortIcon" class="sort-icon" id="sort-current-icon" />
          <span class="sort-value" id="sort-value">{{ sortLabel }}</span>
          <Icon name="chevron-down" class="sort-caret" />
        </button>
        <div
          v-if="sortOpen"
          ref="sortMenu"
          class="sort-menu"
          id="sort-menu"
          role="listbox"
          @click.stop
          @keydown="onSortMenuKeydown"
        >
          <button
            type="button"
            class="sort-menu-item"
            :class="{ 'is-active': state.sortBy === SORT_KEYS.recommended }"
            data-sort="default"
            role="option"
            :aria-selected="state.sortBy === SORT_KEYS.recommended"
            @click="chooseSort(SORT_KEYS.recommended)"
          >
            <Icon name="sparkles" class="sort-menu-icon" />
            <span class="sort-menu-label">默认推荐</span>
            <Icon name="check" class="sort-menu-check" />
          </button>
          <button
            type="button"
            class="sort-menu-item"
            :class="{ 'is-active': state.sortBy === SORT_KEYS.name }"
            data-sort="name"
            role="option"
            :aria-selected="state.sortBy === SORT_KEYS.name"
            @click="chooseSort(SORT_KEYS.name)"
          >
            <Icon name="sort-az" class="sort-menu-icon" />
            <span class="sort-menu-label">按曲名 A–Z</span>
            <Icon name="check" class="sort-menu-check" />
          </button>
        </div>
      </div>
      <span class="top-divider" aria-hidden="true"></span>
      <button
        ref="wallpaperTrigger"
        type="button"
        class="icon-tool"
        :class="{ 'is-active': wallpaperView.hasWallpaper }"
        id="btn-wallpaper"
        title="界面壁纸"
        aria-label="界面壁纸"
        aria-haspopup="dialog"
        :aria-expanded="popoverOpen"
        @click.stop="toggleWallpaper"
      >
        <Icon name="palette" />
      </button>
      <button
        type="button"
        class="icon-tool"
        id="btn-config"
        title="打开应用配置 config.json"
        aria-label="打开应用配置"
        @click="openConfigFile"
      >
        <Icon name="settings" />
      </button>
    </div>

    <span class="top-divider top-divider-window" aria-hidden="true"></span>

    <WindowControls />
  </header>
</template>