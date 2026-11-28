<script setup lang="ts">
/**
 * 主题化表单下拉：替代原生 select，统一键盘导航、选中态和 v-model 契约。
 *
 * `searchable` 会把搜索框嵌进下拉面板顶部，选项多时（如 Edge 在线音色、
 * 语言列表）无需再在触发器旁边外挂一个输入框；搜索、高亮选中项、方向键
 * 进出列表、回车选中与 Esc 关闭都在同一个组件内完成。
 */
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
    /** 在面板顶部嵌入搜索框；默认关闭，保持既有下拉的紧凑形态。 */
    searchable?: boolean;
    /** 搜索框占位文案。 */
    searchPlaceholder?: string;
    /** 有搜索词但没有命中时的提示。 */
    noMatchText?: string;
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
    searchable: false,
    searchPlaceholder: '搜索…',
    noMatchText: undefined,
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
const query = ref('');
const trigger = ref<HTMLButtonElement | null>(null);
const list = ref<HTMLElement | null>(null);
const search = ref<HTMLInputElement | null>(null);
const listId = useId();
const current = computed(
  () => props.options.find((option) => option.value === props.modelValue) ?? null,
);
const enabledOptions = computed(() =>
  props.options.filter((option) => !option.disabled),
);
const keyword = computed(() => query.value.trim().toLowerCase());
const compactKeyword = computed(() => keyword.value.replace(/\s+/g, ''));
const filteredOptions = computed(() => {
  if (!props.searchable || !keyword.value) return props.options;
  return props.options.filter((option) => {
    const label = option.label.toLowerCase();
    if (label.includes(keyword.value) || option.value.toLowerCase().includes(keyword.value)) {
      return true;
    }
    // 「noto sans」与「notosans」都能命中，避免空格把结果筛空。
    return compactKeyword.value.length > 0
      && label.replace(/\s+/g, '').includes(compactKeyword.value);
  });
});
const emptyText = computed(() =>
  props.options.length === 0
    ? '暂无可选项'
    : props.noMatchText ?? `没有匹配「${query.value.trim()}」的选项`,
);

function focusOption(index: number): void {
  const items = Array.from(
    list.value?.querySelectorAll<HTMLButtonElement>('.app-select-option:not(:disabled)') ?? [],
  );
  if (items.length === 0) return;
  items[Math.min(Math.max(index, 0), items.length - 1)]?.focus();
}

function scrollToSelected(): void {
  list.value
    ?.querySelector('.app-select-option.is-active')
    ?.scrollIntoView({ block: 'nearest' });
}

function openMenu(focus: 'selected' | 'first' | 'last' = 'selected'): void {
  if (props.disabled || open.value) return;
  open.value = true;
  query.value = '';
  void nextTick(() => {
    // 可搜索时焦点直接落在输入框，打开即可输入；否则按方向键语义定位选项。
    if (props.searchable) {
      search.value?.focus();
      scrollToSelected();
      return;
    }
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
  query.value = '';
  if (focusTrigger) void nextTick(() => trigger.value?.focus());
}

function toggleMenu(): void {
  if (open.value) closeMenu(true);
  else openMenu();
}

function choose(value: string): void {
  emit('update:modelValue', value);
  closeMenu(true);
}

function clearQuery(): void {
  query.value = '';
  search.value?.focus();
}

function onTriggerKeydown(event: KeyboardEvent): void {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    openMenu(event.key === 'ArrowUp' ? 'last' : 'first');
  }
}

/** 搜索框内的按键：上下键进入列表，回车选中第一个可用项。 */
function onSearchKeydown(event: KeyboardEvent): void {
  // 中文输入法组词期间的 Enter / Esc 属于输入法，不参与选择与关闭。
  if (event.isComposing || event.keyCode === 229) return;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    focusOption(0);
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    focusOption(filteredOptions.value.length - 1);
  } else if (event.key === 'Enter') {
    event.preventDefault();
    const first = filteredOptions.value.find((option) => !option.disabled);
    if (first) choose(first.value);
  } else if (event.key === 'Tab') {
    closeMenu();
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

// 选项异步到达时（如在线音色）不抢走搜索框的焦点。
watch(
  () => props.options.length,
  () => {
    if (open.value && !props.searchable) void nextTick(() => focusOption(0));
  },
);

// 每次换关键词回到列表顶部，避免停在上一批结果的滚动位置。
watch(query, () => {
  void nextTick(() => {
    if (list.value) list.value.scrollTop = 0;
  });
});
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
      :flex="searchable"
      @close="closeMenu(true)"
    >
      <div v-if="searchable" class="app-select-search">
        <Icon name="search" class="app-select-search-icon" />
        <input
          ref="search"
          v-model="query"
          type="text"
          class="app-select-search-input"
          :placeholder="searchPlaceholder"
          spellcheck="false"
          autocomplete="off"
          role="searchbox"
          :aria-label="`${label}：搜索`"
          :aria-controls="listId"
          aria-autocomplete="list"
          @keydown.stop="onSearchKeydown"
        />
        <button
          v-if="query.length > 0"
          type="button"
          class="app-select-clear"
          title="清除搜索"
          aria-label="清除搜索"
          @click.stop="clearQuery"
        >
          <Icon name="x" />
        </button>
      </div>
      <div
        :id="listId"
        ref="list"
        class="app-menu-list app-select-list"
        :class="{ 'is-searchable': searchable }"
        role="listbox"
        :aria-label="label"
        @keydown="onListKeydown"
      >
        <button
          v-for="option in filteredOptions"
          :key="option.value"
          type="button"
          class="app-menu-option app-select-option"
          :class="{ 'is-active': option.value === modelValue }"
          role="option"
          :aria-selected="option.value === modelValue"
          :disabled="option.disabled"
          :title="option.label"
          @click="choose(option.value)"
        >
          <span class="app-menu-option-label">{{ option.label }}</span>
          <Icon name="check" class="app-menu-check" />
        </button>
        <p v-if="filteredOptions.length === 0" class="app-select-empty">{{ emptyText }}</p>
      </div>
    </AppDropdown>
  </div>
</template>

<style scoped>
.app-select-search {
  display: flex;
  flex: none;
  align-items: center;
  gap: 7px;
  min-height: 32px;
  margin: 0 0 4px;
  padding: 0 8px;
  border: 1px solid var(--line);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.04);
}

.app-select-search:focus-within {
  border-color: rgba(25, 217, 139, 0.32);
  background: rgba(25, 217, 139, 0.06);
}

.app-select-search-icon {
  flex: none;
  width: 14px;
  height: 14px;
  color: var(--text-3);
}

.app-select-search-input {
  flex: 1;
  min-width: 0;
  height: 30px;
  padding: 0;
  border: 0;
  outline: none;
  background: transparent;
  color: var(--text);
  font: inherit;
  font-size: 12px;
}

.app-select-search-input::placeholder {
  color: var(--text-3);
}

.app-select-clear {
  display: grid;
  flex: none;
  place-items: center;
  width: 20px;
  height: 20px;
  border-radius: 5px;
  color: var(--text-3);
}

.app-select-clear:hover {
  background: var(--hover);
  color: var(--text);
}

.app-select-clear :deep([data-icon]) {
  width: 12px;
  height: 12px;
}

/* 面板固定高度时只让选项列表滚动，搜索框始终可见。 */
.app-select-list.is-searchable {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  overscroll-behavior: contain;
  scrollbar-width: thin;
}

/* 可搜索列表通常是长名称（音色、语言），单行省略比折行更好扫读；
   完整名称仍可通过 title 查看。 */
.app-select-list.is-searchable :deep(.app-menu-option-label) {
  overflow: hidden;
  overflow-wrap: normal;
  text-overflow: ellipsis;
  white-space: nowrap;
}
</style>
