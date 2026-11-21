<script setup lang="ts">
/**
 * 字体选择下拉框：用自绘列表替代原生 `<select>`，每个选项用自身字体渲染，
 * 与深色界面保持一致；支持方向键、Home / End、Esc 与点击外部关闭。
 */

import { computed, nextTick, onBeforeUnmount, onMounted, ref } from 'vue';
import { FONT_OPTIONS } from '@/lib/constants';
import Icon from './Icon.vue';

const props = defineProps<{ modelValue: string }>();
const emit = defineEmits<{ (event: 'update:modelValue', value: string): void }>();

const open = ref(false);
const wrap = ref<HTMLElement | null>(null);
const menu = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);

const current = computed(
  () => FONT_OPTIONS.find((option) => option.id === props.modelValue) ?? FONT_OPTIONS[0],
);

function openMenu(focusOption = false): void {
  if (open.value) return;
  open.value = true;
  if (!focusOption) return;
  void nextTick(() => {
    const target =
      menu.value?.querySelector<HTMLButtonElement>('.font-select-option.is-active') ??
      menu.value?.querySelector<HTMLButtonElement>('.font-select-option');
    target?.focus();
  });
}

function closeMenu(focusTrigger = false): void {
  if (!open.value) return;
  open.value = false;
  if (focusTrigger) trigger.value?.focus();
}

function toggleMenu(): void {
  if (open.value) closeMenu();
  else openMenu();
}

function choose(id: string): void {
  emit('update:modelValue', id);
  closeMenu(true);
}

function onMenuKeydown(event: KeyboardEvent): void {
  const items = Array.from(
    menu.value?.querySelectorAll<HTMLButtonElement>('.font-select-option') ?? [],
  );
  if (items.length === 0) return;
  const index = items.indexOf(document.activeElement as HTMLButtonElement);

  if (event.key === 'ArrowDown') {
    event.preventDefault();
    items[(index + 1) % items.length]?.focus();
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    items[(index - 1 + items.length) % items.length]?.focus();
  } else if (event.key === 'Home') {
    event.preventDefault();
    items[0]?.focus();
  } else if (event.key === 'End') {
    event.preventDefault();
    items[items.length - 1]?.focus();
  } else if (event.key === 'Escape') {
    event.preventDefault();
    event.stopPropagation();
    closeMenu(true);
  } else if (event.key === 'Tab') {
    closeMenu();
  }
}

function onDocumentClick(event: MouseEvent): void {
  if (!open.value) return;
  const target = event.target as Node | null;
  if (target && wrap.value?.contains(target)) return;
  closeMenu();
}

function onDocumentKeydown(event: KeyboardEvent): void {
  if (open.value && event.key === 'Escape') closeMenu(true);
}

onMounted(() => {
  document.addEventListener('click', onDocumentClick);
  document.addEventListener('keydown', onDocumentKeydown);
});

onBeforeUnmount(() => {
  document.removeEventListener('click', onDocumentClick);
  document.removeEventListener('keydown', onDocumentKeydown);
});
</script>

<template>
  <div ref="wrap" class="font-select" @keydown="onMenuKeydown">
    <button
      ref="trigger"
      type="button"
      class="font-select-trigger"
      :class="{ 'is-open': open }"
      aria-haspopup="listbox"
      :aria-expanded="open"
      aria-label="字体选择"
      @click.stop="toggleMenu"
      @keydown.down.prevent="openMenu(true)"
    >
      <span class="font-select-value" :style="{ fontFamily: current.stack }">
        {{ current.label }}
      </span>
      <Icon name="chevron-down" class="font-select-caret" />
    </button>

    <div
      v-if="open"
      ref="menu"
      class="font-select-menu"
      role="listbox"
      aria-label="字体选择"
      @click.stop
    >
      <button
        v-for="option in FONT_OPTIONS"
        :key="option.id"
        type="button"
        class="font-select-option"
        :class="{ 'is-active': option.id === modelValue }"
        role="option"
        :aria-selected="option.id === modelValue"
        @click="choose(option.id)"
      >
        <span class="font-select-sample" :style="{ fontFamily: option.stack }">
          {{ option.label }}
        </span>
        <span class="font-select-preview" :style="{ fontFamily: option.stack }">
          听风吟 BGM · 0123
        </span>
        <Icon name="check" class="font-select-check" />
      </button>
    </div>
  </div>
</template>
