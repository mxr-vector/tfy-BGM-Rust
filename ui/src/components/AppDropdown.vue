<script setup lang="ts">
/** 通用锚定浮层：统一主题、视口边界、自动上下翻转、外部点击与 Esc 关闭。 */
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { useWindowState } from '@/composables/useWindowState';

const props = withDefaults(
  defineProps<{
    open: boolean;
    anchor: HTMLElement | null;
    align?: 'start' | 'center' | 'end';
    side?: 'top' | 'bottom';
    offset?: number;
    width?: number | string;
    minWidth?: number;
    maxHeight?: number | string;
    panelId?: string;
    role?: 'listbox' | 'dialog' | 'menu';
    label?: string;
    panelClass?: string;
    flex?: boolean;
  }>(),
  {
    align: 'start',
    side: 'bottom',
    offset: 8,
    width: 'auto',
    minWidth: 0,
    maxHeight: 'calc(var(--viewport-height, 100dvh) - 24px)',
    role: undefined,
    panelId: undefined,
    label: undefined,
    panelClass: undefined,
    flex: false,
  },
);

const emit = defineEmits<{ close: [] }>();

const panel = ref<HTMLElement | null>(null);
const ready = ref(false);
const position = ref({ left: 0, top: 0, width: 0, side: props.side });
const { viewportWidth, viewportHeight } = useWindowState();
let observer: ResizeObserver | null = null;

function resolveWidth(anchorWidth: number): number {
  if (typeof props.width === 'number') return props.width;
  if (props.width.endsWith('%')) {
    const ratio = Number.parseFloat(props.width) / 100;
    if (Number.isFinite(ratio)) return anchorWidth * ratio;
  }
  return anchorWidth;
}

async function updatePosition(): Promise<void> {
  if (!props.open || !props.anchor || !panel.value) return;
  const rect = props.anchor.getBoundingClientRect();
  const viewportW = viewportWidth.value || window.innerWidth;
  const viewportH = viewportHeight.value || window.innerHeight;
  const margin = 12;
  const maxWidth = Math.max(0, viewportW - margin * 2);
  const width = Math.min(Math.max(resolveWidth(rect.width), props.minWidth), maxWidth);
  position.value.width = width;
  await nextTick();
  if (!props.open || !panel.value) return;

  const height = panel.value.offsetHeight;
  const spaceBelow = Math.max(0, viewportH - rect.bottom - props.offset - margin);
  const spaceAbove = Math.max(0, rect.top - props.offset - margin);
  const side =
    props.side === 'bottom'
      ? height > spaceBelow && spaceAbove > spaceBelow
        ? 'top'
        : 'bottom'
      : height > spaceAbove && spaceBelow > spaceAbove
        ? 'bottom'
        : 'top';
  const rawLeft =
    props.align === 'end'
      ? rect.right - width
      : props.align === 'center'
        ? rect.left + rect.width / 2 - width / 2
        : rect.left;
  const left = Math.min(Math.max(margin, rawLeft), Math.max(margin, viewportW - width - margin));
  const rawTop = side === 'bottom' ? rect.bottom + props.offset : rect.top - height - props.offset;
  const maxTop = Math.max(margin, viewportH - height - margin);
  position.value = {
    left,
    top: Math.min(Math.max(margin, rawTop), maxTop),
    width,
    side,
  };
  ready.value = true;
}

function onPointerDown(event: PointerEvent): void {
  if (!props.open) return;
  const target = event.target;
  if (target instanceof Node && panel.value?.contains(target)) return;
  if (target instanceof Node && props.anchor?.contains(target)) return;
  emit('close');
}

function onKeydown(event: KeyboardEvent): void {
  if (!props.open || event.key !== 'Escape') return;
  event.preventDefault();
  event.stopImmediatePropagation();
  emit('close');
  if (props.anchor?.isConnected) props.anchor.focus();
}

function observeElements(): void {
  observer?.disconnect();
  if (!props.open || typeof ResizeObserver === 'undefined') return;
  observer = new ResizeObserver(() => void updatePosition());
  if (props.anchor) observer.observe(props.anchor);
  if (panel.value) observer.observe(panel.value);
}

watch(
  [() => props.open, () => props.anchor, () => props.width, () => props.minWidth, viewportWidth, viewportHeight],
  async (values, previous) => {
    if (!props.open) {
      ready.value = false;
      observer?.disconnect();
      return;
    }
    await nextTick();
    observeElements();
    // 宽度变化会先触发布局测量，随后再次按真实面板高度校正上下位置。
    if (previous && values[2] !== previous[2]) await updatePosition();
    await updatePosition();
  },
);

onMounted(() => {
  document.addEventListener('pointerdown', onPointerDown, true);
  document.addEventListener('keydown', onKeydown, true);
  window.addEventListener('scroll', updatePosition, true);
});

onBeforeUnmount(() => {
  observer?.disconnect();
  document.removeEventListener('pointerdown', onPointerDown, true);
  document.removeEventListener('keydown', onKeydown, true);
  window.removeEventListener('scroll', updatePosition, true);
});

defineExpose({ panel });
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      :id="panelId"
      ref="panel"
      class="app-dropdown app-menu-surface"
      :class="[{ 'is-flex': flex, 'is-ready': ready }, panelClass]"
      :role="role"
      :data-side="position.side"
      :aria-label="label"
      :style="{
        left: `${position.left}px`,
        top: `${position.top}px`,
        width: `${position.width}px`,
        maxHeight: typeof maxHeight === 'number' ? `${maxHeight}px` : maxHeight,
      }"
      @click.stop
    >
      <slot />
    </div>
  </Teleport>
</template>

<style scoped>
.app-dropdown {
  position: fixed;
  z-index: var(--dropdown-z);
  overflow-x: hidden;
  overflow-y: auto;
  overscroll-behavior: contain;
  padding: 6px;
  border-radius: 14px;
  opacity: 0;
  pointer-events: none;
  animation: dropdownIn 140ms cubic-bezier(0.16, 1, 0.3, 1);
}

.app-dropdown.is-ready {
  opacity: 1;
  pointer-events: auto;
}

.app-dropdown.is-flex {
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

@keyframes dropdownIn {
  from {
    transform: translateY(-4px) scale(0.985);
  }
  to {
    transform: none;
  }
}

@keyframes dropdownInTop {
  from {
    transform: translateY(4px) scale(0.985);
  }
  to {
    transform: none;
  }
}

.app-dropdown[data-side='top'] {
  animation-name: dropdownInTop;
}

@media (prefers-reduced-motion: reduce) {
  .app-dropdown {
    animation: none;
  }
}
</style>
