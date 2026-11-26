<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { withAlpha } from '@/lib/format';

const props = defineProps<{ waveform: number[]; accent?: string }>();
const canvas = ref<HTMLCanvasElement | null>(null);
let observer: ResizeObserver | null = null;

function draw(): void {
  const element = canvas.value;
  if (!element) return;
  const rect = element.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  element.width = Math.round(rect.width * dpr);
  element.height = Math.round(rect.height * dpr);
  const context = element.getContext('2d');
  if (!context) return;
  context.setTransform(dpr, 0, 0, dpr, 0, 0);
  context.clearRect(0, 0, rect.width, rect.height);
  context.fillStyle = withAlpha(props.accent ?? '#19d98b', 0.72);
  const step = rect.width / Math.max(1, props.waveform.length);
  for (let index = 0; index < props.waveform.length; index += 1) {
    const amplitude = Math.min(1, props.waveform[index] ?? 0) * rect.height * 0.46;
    context.fillRect(index * step, rect.height / 2 - amplitude, Math.max(1, step * 0.75), amplitude * 2);
  }
}

// 波形是异步从索引取回的：只在挂载时画一次会留下空白画布。
watch(
  () => props.waveform,
  async () => {
    await nextTick();
    draw();
  },
);

onMounted(async () => {
  await nextTick();
  draw();
  if ('ResizeObserver' in window) {
    observer = new ResizeObserver(draw);
    if (canvas.value) observer.observe(canvas.value);
  }
});
onBeforeUnmount(() => observer?.disconnect());
</script>

<template>
  <canvas ref="canvas" class="waveform-preview" aria-label="音频索引波形"></canvas>
</template>

<style scoped>
.waveform-preview { display: block; width: 100%; height: 44px; }
</style>
