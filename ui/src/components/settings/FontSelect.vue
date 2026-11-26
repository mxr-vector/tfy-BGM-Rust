<script setup lang="ts">
/**
 * 字体选择下拉框：用自绘列表替代原生 `<select>`，每个选项用自身字体渲染，
 * 与深色界面保持一致；字体清单由系统字体扫描结果提供，支持搜索与重新扫描，
 * 并支持方向键、Home / End、Esc 与点击外部关闭。
 */

import { computed, nextTick, ref, useId, watch } from 'vue';
import { resolveFontOption, type FontOption } from '@/lib/fonts';
import AppDropdown from '@/components/AppDropdown.vue';
import Icon from '@/components/Icon.vue';

const props = defineProps<{
  modelValue: string;
  options: FontOption[];
  loading?: boolean;
}>();
const emit = defineEmits<{
  (event: 'update:modelValue', value: string): void;
  (event: 'refresh'): void;
}>();

const open = ref(false);
const query = ref('');
const panelId = useId();
const menu = ref<HTMLElement | null>(null);
const trigger = ref<HTMLButtonElement | null>(null);
const search = ref<HTMLInputElement | null>(null);

const current = computed(() => resolveFontOption(props.modelValue));

interface TextPart {
  text: string;
  hit: boolean;
}

interface FontResult {
  option: FontOption;
  /** 展示名按关键词切段，命中部分高亮。 */
  labelParts: TextPart[];
  /** 命中的是英文族名、展示名里没有关键词时，补一个族名解释命中原因。 */
  familyParts: TextPart[] | null;
}

const keyword = computed(() => query.value.trim().toLowerCase());

/** 把文本按关键词切成三段，供模板高亮；没有命中时返回整段。 */
function splitByMatch(text: string, needle: string): TextPart[] {
  if (!needle) return [{ text, hit: false }];
  const index = text.toLowerCase().indexOf(needle);
  if (index < 0) return [{ text, hit: false }];
  const end = index + needle.length;
  return [
    { text: text.slice(0, index), hit: false },
    { text: text.slice(index, end), hit: true },
    { text: text.slice(end), hit: false },
  ].filter((part) => part.text.length > 0);
}

/**
 * 搜索结果：展示名命中排在族名命中前面，同分保持后端「中文优先」的顺序。
 * 输入 "yahei" 时让「微软雅黑」靠前，而不是被一大批族名带 yahei 的字体淹没。
 * 同时支持忽略空格的匹配（"notosans" 也能找到 Noto Sans SC）。
 */
const results = computed<FontResult[]>(() => {
  const needle = keyword.value;
  const compact = needle.replace(/\s+/g, '');
  const scored: { option: FontOption; score: number }[] = [];

  for (const option of props.options) {
    const label = option.label.toLowerCase();
    const id = option.id.toLowerCase();
    let score = -1;
    if (!needle) score = 0;
    else if (label.startsWith(needle)) score = 0;
    else if (label.includes(needle)) score = 1;
    else if (id.startsWith(needle)) score = 2;
    else if (id.includes(needle)) score = 3;
    else if (compact && label.replace(/\s+/g, '').includes(compact)) score = 4;
    else if (compact && id.replace(/\s+/g, '').includes(compact)) score = 5;
    if (score >= 0) scored.push({ option, score });
  }

  if (needle) scored.sort((a, b) => a.score - b.score);

  return scored.map(({ option }) => {
    const labelText = option.label.toLowerCase();
    const familyHit = Boolean(needle) && !labelText.includes(needle);
    return {
      option,
      labelParts: splitByMatch(option.label, needle),
      familyParts:
        familyHit && option.id.toLowerCase().includes(needle)
          ? splitByMatch(option.id, needle)
          : null,
    };
  });
});

const optionsCount = computed(() => Math.max(props.options.length - 1, 0));

/** 底部说明：默认显示安装数量，搜索时显示匹配数。 */
const footerHint = computed(() => {
  if (props.loading) return '正在读取系统字体…';
  return keyword.value
    ? `匹配 ${results.value.length} 项 · 共 ${optionsCount.value} 款字体`
    : `系统已安装 ${optionsCount.value} 款字体`;
});

function focusOption(index: number): void {
  const items = Array.from(
    menu.value?.querySelectorAll<HTMLButtonElement>('.font-select-option') ?? [],
  );
  if (items.length === 0) return;
  const target = items[Math.min(Math.max(index, 0), items.length - 1)];
  target?.focus();
}

function openMenu(): void {
  if (open.value) return;
  open.value = true;
  query.value = '';
  void nextTick(() => {
    search.value?.focus();
    // 字体较多时把当前选项滚进可视区域，避免打开列表后看不到已选字体。
    menu.value
      ?.querySelector('.font-select-option.is-active')
      ?.scrollIntoView({ block: 'nearest' });
  });
}

function closeMenu(focusTrigger = false): void {
  if (!open.value) return;
  open.value = false;
  query.value = '';
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

/** 列表内的键盘导航：焦点落在选项按钮上。 */
function onListKeydown(event: KeyboardEvent): void {
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

/** 搜索框内的按键：上下键进入列表，回车选中第一项。 */
function onSearchKeydown(event: KeyboardEvent): void {
  // 中文输入法组词期间的 Enter / Esc 属于输入法，不参与选择与关闭。
  if (event.isComposing || event.keyCode === 229) return;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    focusOption(0);
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    focusOption(results.value.length - 1);
  } else if (event.key === 'Enter') {
    event.preventDefault();
    const first = results.value[0];
    if (first) choose(first.option.id);
  } else if (event.key === 'Escape') {
    event.preventDefault();
    closeMenu(true);
  } else if (event.key === 'Tab') {
    closeMenu();
  }
}

/** 清空搜索并保持输入焦点，方便继续输入。 */
function clearQuery(): void {
  query.value = '';
  search.value?.focus();
}

watch(
  () => props.modelValue,
  () => closeMenu(),
);

// 输入即换一批结果时回到列表顶部，否则会停在上一批的滚动位置。
watch(query, () => {
  void nextTick(() => {
    if (menu.value) menu.value.scrollTop = 0;
  });
});

</script>

<template>
  <div class="font-select">
    <button
      ref="trigger"
      type="button"
      class="app-select-trigger font-select-trigger"
      :class="{ 'is-open': open }"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-controls="open ? panelId : undefined"
      aria-label="字体选择"
      :title="current.label"
      @click.stop="toggleMenu"
      @keydown.down.prevent="openMenu"
    >
      <span class="font-select-value" :style="{ fontFamily: current.previewStack }">
        {{ current.label }}
        <em v-if="!current.installed && !loading" class="font-select-missing">未安装</em>
      </span>
      <Icon name="chevron-down" class="font-select-caret" />
    </button>

    <AppDropdown
      :open="open"
      :anchor="trigger"
      align="end"
      :width="380"
      :max-height="380"
      role="dialog"
      label="字体选择"
      flex
      @close="closeMenu"
    >
      <div class="font-select-search">
        <Icon name="search" class="font-select-search-icon" />
        <input
          ref="search"
          v-model="query"
          type="text"
          class="font-select-search-input"
          placeholder="搜索字体名称，如 雅黑 / yahei"
          spellcheck="false"
          autocomplete="off"
          role="searchbox"
          aria-label="搜索字体"
          @keydown.stop="onSearchKeydown"
        />
        <span v-if="query.length > 0" class="font-select-count">{{ results.length }}</span>
        <button
          v-if="query.length > 0"
          type="button"
          class="font-select-clear"
          title="清除搜索"
          aria-label="清除搜索"
          @click.stop="clearQuery"
        >
          <Icon name="x" />
        </button>
      </div>

      <div
        :id="panelId"
        ref="menu"
        class="font-select-list"
        role="listbox"
        aria-label="字体选择"
        @keydown.stop="onListKeydown"
      >
        <button
          v-for="result in results"
          :key="result.option.id"
          type="button"
          class="app-menu-option font-select-option"
          :class="{ 'is-active': result.option.id === current.id }"
          role="option"
          :aria-selected="result.option.id === current.id"
          @click.stop="choose(result.option.id)"
        >
          <span class="font-select-sample" :style="{ fontFamily: result.option.previewStack }">
            <span
              v-for="(part, index) in result.labelParts"
              :key="index"
              :class="{ 'is-hit': part.hit }"
            >{{ part.text }}</span>
            <span v-if="result.familyParts" class="font-select-family">
              <span
                v-for="(part, index) in result.familyParts"
                :key="index"
                :class="{ 'is-hit': part.hit }"
              >{{ part.text }}</span>
            </span>
          </span>
          <span class="font-select-preview" :style="{ fontFamily: result.option.previewStack }">
            听风吟 BGM · 0123
          </span>
          <Icon name="check" class="app-menu-check font-select-check" />
        </button>
        <p v-if="results.length === 0" class="font-select-empty">
          <template v-if="loading">正在读取系统字体…</template>
          <template v-else>
            没有匹配「{{ query.trim() }}」的字体
            <small>试试字体英文名，如 yahei / microsoft</small>
          </template>
        </p>
      </div>

      <div class="font-select-foot">
        <span class="font-select-hint">{{ footerHint }}</span>
        <button
          type="button"
          class="font-select-refresh"
          :disabled="loading"
          @click.stop="emit('refresh')"
        >
          <Icon name="refresh" />
          重新扫描
        </button>
      </div>
    </AppDropdown>
  </div>
</template>
