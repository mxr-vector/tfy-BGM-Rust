<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useWindowState } from '@/composables/useWindowState';
import {
  closeContextMenu,
  contextMenuState,
  type ContextMenuEntry,
} from '@/lib/context-menu';
import Icon from './Icon.vue';

const menu = ref<HTMLElement | null>(null);
const { viewportWidth, viewportHeight } = useWindowState();

function activate(entry: ContextMenuEntry): void {
  closeContextMenu();
  entry.action?.();
}

function onPointerDown(event: PointerEvent): void {
  if (!contextMenuState.open) return;
  if (menu.value && event.target instanceof Node && menu.value.contains(event.target)) return;
  closeContextMenu();
}

function onKeydown(event: KeyboardEvent): void {
  if (!contextMenuState.open || event.key !== 'Escape') return;
  event.stopPropagation();
  closeContextMenu();
}

function onScroll(event: Event): void {
  // 菜单自身内容溢出时允许滚动，只有背景滚动才关闭菜单。
  if (event.target instanceof Node && menu.value?.contains(event.target)) return;
  closeContextMenu();
}

function clampPosition(): void {
  const rect = menu.value?.getBoundingClientRect();
  if (!rect) return;
  const maxX = Math.max(8, viewportWidth.value - rect.width - 8);
  const maxY = Math.max(8, viewportHeight.value - rect.height - 8);
  contextMenuState.x = Math.min(Math.max(8, contextMenuState.x), maxX);
  contextMenuState.y = Math.min(Math.max(8, contextMenuState.y), maxY);
}

watch(
  () => contextMenuState.open,
  async (open) => {
    if (!open) return;
    await nextTick();
    clampPosition();
  },
);

watch([viewportWidth, viewportHeight], () => {
  if (!contextMenuState.open) return;
  void nextTick(clampPosition);
});

onMounted(() => {
  document.addEventListener('pointerdown', onPointerDown, true);
  document.addEventListener('keydown', onKeydown, true);
  document.addEventListener('scroll', onScroll, true);
  window.addEventListener('blur', onScroll);
});

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', onPointerDown, true);
  document.removeEventListener('keydown', onKeydown, true);
  document.removeEventListener('scroll', onScroll, true);
  window.removeEventListener('blur', onScroll);
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="contextMenuState.open"
      ref="menu"
      class="context-menu"
      role="menu"
      :style="{ left: `${contextMenuState.x}px`, top: `${contextMenuState.y}px` }"
      @click.stop
    >
      <template v-for="(entry, index) in contextMenuState.items" :key="index">
        <div v-if="entry.separator" class="context-menu-separator" />
        <button
          v-else
          type="button"
          class="context-menu-item"
          :class="{ 'is-danger': entry.danger }"
          role="menuitem"
          @click="activate(entry)"
        >
          <Icon v-if="entry.icon" :name="entry.icon" />
          <span>{{ entry.label }}</span>
        </button>
      </template>
    </div>
  </Teleport>
</template>