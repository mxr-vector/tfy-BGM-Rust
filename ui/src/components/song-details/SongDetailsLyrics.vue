<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { formatTime } from '@/lib/format';
import type { StoredSubtitle, SubtitleSegment } from '@/lib/types';
import { audioState } from '@/stores/audio-engine';
import { seekTo } from '@/stores/player';
import Icon from '@/components/Icon.vue';

const props = defineProps<{
  songKey: string;
  subtitle: StoredSubtitle | null;
}>();

const viewport = ref<HTMLDivElement | null>(null);
const lineRefs = ref<Array<HTMLElement | null>>([]);
const segments = computed(() => props.subtitle?.segments ?? []);
const activeIndex = computed(() => {
  const position = audioState.currentTime;
  let current = -1;
  for (let index = 0; index < segments.value.length; index += 1) {
    if (position < segments.value[index]!.start) break;
    current = index;
  }
  return current;
});

function setLineRef(element: unknown, index: number): void {
  lineRefs.value[index] = element instanceof HTMLElement ? element : null;
}

function scrollToActive(): void {
  const container = viewport.value;
  const line = lineRefs.value[activeIndex.value];
  if (!container || !line) return;
  const top = line.offsetTop - container.clientHeight / 2 + line.clientHeight / 2;
  container.scrollTo({ top: Math.max(0, top), behavior: 'smooth' });
}

function selectSegment(segment: SubtitleSegment): void {
  seekTo(segment.start);
}

watch(
  () => props.songKey,
  () => {
    lineRefs.value = [];
  },
);

watch(
  activeIndex,
  () => {
    void nextTick(scrollToActive);
  },
  { immediate: true },
);

watch(segments, () => {
  lineRefs.value = [];
  void nextTick(scrollToActive);
});
</script>

<template>
  <div ref="viewport" class="details-lyrics" aria-label="歌曲歌词">
    <div v-if="segments.length === 0" class="details-lyrics-empty">
      <span class="details-empty-icon"><Icon name="lyrics" /></span>
      <strong>还没有歌词</strong>
      <p>生成字幕后，歌词会随播放进度自动高亮。</p>
    </div>
    <div v-else class="details-lyrics-spacer" aria-hidden="true"></div>
    <button
      v-for="(segment, index) in segments"
      :key="`${segment.start}-${index}`"
      :ref="(element) => setLineRef(element, index)"
      type="button"
      class="details-lyric-line"
      :class="{ 'is-active': index === activeIndex }"
      :title="`跳转到 ${formatTime(segment.start)}`"
      @click="selectSegment(segment)"
    >
      <span>{{ segment.text }}</span>
      <small>{{ formatTime(segment.start) }}</small>
    </button>
    <div v-if="segments.length > 0" class="details-lyrics-spacer" aria-hidden="true"></div>
  </div>
</template>

<style scoped>
.details-lyrics {
  position: relative;
  height: 100%;
  min-height: 0;
  overflow-y: auto;
  padding: 0 18px 0 6px;
  scrollbar-color: rgba(255, 255, 255, 0.14) transparent;
  scrollbar-width: thin;
  mask-image: linear-gradient(180deg, transparent, #000 9%, #000 91%, transparent);
  -webkit-mask-image: linear-gradient(180deg, transparent, #000 9%, #000 91%, transparent);
}

.details-lyrics-spacer {
  height: 34%;
}

.details-lyric-line {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 20px;
  width: 100%;
  padding: 10px 14px;
  border-radius: 12px;
  color: rgba(255, 255, 255, 0.34);
  font-size: clamp(17px, 1.55vw, 23px);
  font-weight: 550;
  line-height: 1.55;
  text-align: left;
  transition:
    color 180ms ease,
    background 180ms ease,
    transform 180ms ease;
}

.details-lyric-line:hover {
  color: rgba(255, 255, 255, 0.68);
  background: rgba(255, 255, 255, 0.035);
}

.details-lyric-line.is-active {
  color: #f4fff9;
  background: linear-gradient(90deg, rgba(255, 255, 255, 0.09), rgba(255, 255, 255, 0.025));
  box-shadow: inset 3px 0 0 var(--details-accent, #19d98b);
  transform: translateX(4px);
}

.details-lyric-line small {
  flex: none;
  color: rgba(255, 255, 255, 0.2);
  font-size: 10px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
}

.details-lyric-line.is-active small {
  color: rgba(255, 255, 255, 0.42);
}

.details-lyrics-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  height: 100%;
  min-height: 220px;
  color: var(--text-3);
  text-align: center;
}

.details-empty-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 54px;
  height: 54px;
  margin-bottom: 14px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 18px;
  color: var(--details-accent, #19d98b);
  background: rgba(255, 255, 255, 0.035);
}

.details-empty-icon [data-icon] {
  width: 23px;
  height: 23px;
}

.details-lyrics-empty strong {
  color: var(--text-2);
  font-size: 15px;
}

.details-lyrics-empty p {
  margin: 7px 0 0;
  font-size: 12px;
}

@media (max-width: 760px) {
  .details-lyric-line {
    font-size: 18px;
  }
}
</style>
