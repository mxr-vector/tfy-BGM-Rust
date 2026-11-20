<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import {
  closeContextMenu,
  contextMenuState,
  type ContextMenuEntry,
} from '@/lib/context-menu';
import Icon from './Icon.vue';

const menu = ref<HTMLElement | null>(null);

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

function onScroll(): void {
  closeContextMenu();
}

watch(
  () => contextMenuState.open,
  async (open) => {
    if (!open) return;
    await nextTick();
    const rect = menu.value?.getBoundingClientRect();
    if (!rect) return;
    if (contextMenuState.x + rect.width > window.innerWidth - 8) {
      contextMenuState.x = Math.max(8, contextMenuState.x - rect.width);
    }
    if (contextMenuState.y + rect.height > window.innerHeight - 8) {
      contextMenuState.y = Math.max(8, contextMenuState.y - rect.height);
    }
  },
);

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