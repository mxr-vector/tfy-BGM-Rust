<script setup lang="ts">
/** 主题化表单下拉：替代原生 select，统一键盘导航、选中态和 v-model 契约。 */
import { computed, nextTick, ref, useId, watch } from 'vue';
import AppDropdown from './AppDropdown.vue';
import Icon from './Icon.vue';

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

const props = withDefaults(
  defineProps<{
    modelValue: string;
    options: SelectOption[];
    label: string;
    name?: string;
    id?: string;
    placeholder?: string;
    disabled?: boolean;
    align?: 'start' | 'center' | 'end';
    side?: 'top' | 'bottom';
    menuWidth?: number | string;
    minMenuWidth?: number;
    maxHeight?: number;
    triggerClass?: string;
  }>(),
  {
    name: undefined,
    id: undefined,
    placeholder: '请选择',
    disabled: false,
    align: 'start',
    side: 'bottom',
    menuWidth: '100%',
    minMenuWidth: 200,
    maxHeight: 280,
    triggerClass: undefined,
  },
);

const emit = defineEmits<{
  (event: 'update:modelValue', value: string): void;
}>();

const open = ref(false);
const trigger = ref<HTMLButtonElement | null>(null);
const list = ref<HTMLElement | null>(null);
const listId = useId();
const current = computed(
  () => props.options.find((option) => option.value === props.modelValue) ?? null,
);
const enabledOptions = computed(() =>
  props.options.filter((option) => !option.disabled),
);

function focusOption(index: number): void {
  const items = Array.from(
    list.value?.querySelectorAll<HTMLButtonElement>('.app-select-option:not(:disabled)') ?? [],
  );
  if (items.length === 0) return;
  items[Math.min(Math.max(index, 0), items.length - 1)]?.focus();
}

function openMenu(focus = 'selected'): void {
  if (props.disabled || open.value) return;
  open.value = true;
  void nextTick(() => {
    const index = enabledOptions.value.findIndex(
      (option) => option.value === props.modelValue,
    );
    if (focus === 'first') focusOption(0);
    else if (focus === 'last') focusOption(enabledOptions.value.length - 1);
    else focusOption(Math.max(0, index));
  });
}

function closeMenu(focusTrigger = false): void {
  if (!open.value) return;
  open.value = false;
  if (focusTrigger) void nextTick(() => trigger.value?.focus());
}

function toggleMenu(): void {
  if (open.value) closeMenu();
  else openMenu();
}

function choose(value: string): void {
  emit('update:modelValue', value);
  closeMenu(true);
}

function onTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    openMenu(event.key === 'ArrowUp' ? 'last' : 'first');
  }
}

function onListKeydown(event: KeyboardEvent): void {
  const items = Array.from(
    list.value?.querySelectorAll<HTMLButtonElement>('.app-select-option:not(:disabled)') ?? [],
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
  } else if (event.key === 'Tab') {
    closeMenu();
  }
}

watch(
  () => props.options.length,
  () => {
    if (open.value) void nextTick(() => focusOption(0));
  },
);
</script>

<template>
  <div class="app-select">
    <input v-if="name" type="hidden" :name="name" :value="modelValue" />
    <button
      :id="id"
      ref="trigger"
      type="button"
      class="app-select-trigger"
      :class="[{ 'is-open': open }, triggerClass]"
      :disabled="disabled"
      :title="current?.label ?? placeholder"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-controls="open ? listId : undefined"
      :aria-label="label"
      @click="toggleMenu"
      @keydown="onTriggerKeydown"
    >
      <span class="app-select-value">{{ current?.label ?? placeholder }}</span>
      <Icon name="chevron-down" class="app-select-caret" />
    </button>
    <AppDropdown
      :open="open"
      :anchor="trigger"
      :align="align"
      :side="side"
      :width="menuWidth"
      :min-width="minMenuWidth"
      :max-height="maxHeight"
      :label="label"
      @close="closeMenu(true)"
    >
      <div
        :id="listId"
        ref="list"
        class="app-menu-list"
        role="listbox"
        :aria-label="label"
        @keydown="onListKeydown"
      >
        <button
          v-for="option in options"
          :key="option.value"
          type="button"
          class="app-menu-option app-select-option"
          :class="{ 'is-active': option.value === modelValue }"
          role="option"
          :aria-selected="option.value === modelValue"
          :disabled="option.disabled"
          @click="choose(option.value)"
        >
          <span class="app-menu-option-label">{{ option.label }}</span>
          <Icon name="check" class="app-menu-check" />
        </button>
        <p v-if="options.length === 0" class="app-select-empty">暂无可选项</p>
      </div>
    </AppDropdown>
  </div>
</template>
