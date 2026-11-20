<script setup lang="ts">
import { ALL_CATEGORY } from '@/lib/constants';
import { withAlpha } from '@/lib/format';
import { openCategoryMenu } from '@/composables/menus';
import { openCreateCategory } from '@/stores/actions';
import { setCategory } from '@/stores/library';
import { categoryById, state } from '@/stores/state';
import Icon from './Icon.vue';

function onStripContextMenu(event: MouseEvent): void {
  event.preventDefault();
  const target = event.target instanceof Element ? event.target : null;
  const tab = target?.closest<HTMLElement>('.category-item');
  const category = tab?.dataset.categoryId
    ? categoryById(tab.dataset.categoryId)
    : null;
  openCategoryMenu(event, category);
}
</script>

<template>
  <div class="filter-row">
    <div
      class="filter-strip"
      role="tablist"
      aria-label="风格筛选"
      @contextmenu="onStripContextMenu"
    >
      <button
        type="button"
        class="filter-chip"
        :class="{ 'is-active': state.category === ALL_CATEGORY && !state.favoritesOnly }"
        id="chip-all"
        role="tab"
        :aria-selected="state.category === ALL_CATEGORY && !state.favoritesOnly"
        @click="setCategory(ALL_CATEGORY)"
      >
        <Icon name="music" class="chip-icon" />
        <span class="chip-label">全部风格</span>
      </button>
      <div id="category-nav" class="filter-categories">
        <button
          v-for="category in state.library.categories"
          :key="category.id"
          type="button"
          class="filter-chip category-item"
          :class="{
            'is-active': state.category === category.id && !state.favoritesOnly,
          }"
          role="tab"
          :aria-selected="state.category === category.id && !state.favoritesOnly"
          :title="`${category.nameZh} · ${category.nameEn}`"
          :data-category-id="category.id"
          @click="setCategory(category.id)"
        >
          <span
            v-if="category.icon"
            class="chip-icon"
            :style="{ '--chip-color': withAlpha(category.color, 0.9) }"
          >
            <Icon :name="category.icon" />
          </span>
          <span
            v-else
            class="chip-dot"
            :style="{ backgroundColor: category.color }"
          ></span>
          <span class="chip-label">{{ category.nameZh }}</span>
          <span class="chip-count">{{ category.count }}</span>
        </button>
      </div>
      <button
        type="button"
        class="filter-chip is-ghost"
        id="btn-add-category"
        title="新增分类（也可右键分类）"
        aria-label="新增分类"
        @click="openCreateCategory"
      >
        <Icon name="plus" class="nav-icon" />
        <span class="chip-label">新增分类</span>
      </button>
    </div>
  </div>
</template>