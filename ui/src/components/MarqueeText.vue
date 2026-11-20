<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';

const props = defineProps<{ text: string; paused?: boolean }>();

const host = ref<HTMLElement | null>(null);
const inner = ref<HTMLElement | null>(null);
const shift = ref(0);

let observer: ResizeObserver | null = null;
let frame = 0;

function measure(): void {
  const hostEl = host.value;
  const innerEl = inner.value;
  if (!hostEl || !innerEl) return;
  shift.value = Math.max(0, innerEl.scrollWidth - hostEl.clientWidth - 1);
}

function schedule(): void {
  cancelAnimationFrame(frame);
  frame = requestAnimationFrame(measure);
}

const innerStyle = computed(() => ({
  '--marquee-shift': `${shift.value}px`,
  '--marquee-duration': `${Math.max(6, shift.value / 26 + 4).toFixed(2)}s`,
}));

watch(
  () => props.text,
  () => void nextTick(schedule),
);

onMounted(() => {
  observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(schedule);
  if (observer && host.value) observer.observe(host.value);
  schedule();
});

onBeforeUnmount(() => {
  observer?.disconnect();
  cancelAnimationFrame(frame);
});
</script>

<template>
  <span ref="host" class="marquee-text">
    <span
      ref="inner"
      class="marquee-inner"
      :class="{ 'is-scrolling': shift > 0, 'is-paused': paused }"
      :style="innerStyle"
    >{{ text }}</span>
  </span>
</template>