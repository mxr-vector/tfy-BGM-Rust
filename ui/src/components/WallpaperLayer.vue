<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { assetUrl } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import { setWallpaper } from '@/stores/prefs';
import { wallpaperView } from '@/composables/useWallpaper';

const props = defineProps<{ focused: boolean }>();

const video = ref<HTMLVideoElement | null>(null);
const image = ref<HTMLImageElement | null>(null);
const ready = ref(false);
const documentHidden = ref(document.hidden);

const source = computed(() => wallpaperView.value.source);

async function ensureVideoPlayback(): Promise<void> {
  const element = video.value;
  if (!element) return;
  if (props.focused && !documentHidden.value) {
    await element.play().catch(() => {
      // WebView 可能阻止自动播放；静音视频通常可播，失败时保留静态首帧。
      ready.value = true;
    });
  } else {
    element.pause();
  }
}

function onMediaError(): void {
  toast(
    source.value?.kind === 'video'
      ? '动态壁纸加载失败，已切换回默认样式'
      : '图片壁纸加载失败，已切换回默认样式',
    'error',
  );
  setWallpaper('default', null);
}

function onVisibilityChange(): void {
  documentHidden.value = document.hidden;
}

watch(
  () => [source.value?.path, source.value?.kind] as const,
  async () => {
    ready.value = false;
    await nextTick();
    if (image.value?.complete && image.value.naturalWidth > 0) ready.value = true;
    await ensureVideoPlayback();
  },
);

watch([() => props.focused, documentHidden], () => {
  void ensureVideoPlayback();
});

onMounted(() => {
  document.addEventListener('visibilitychange', onVisibilityChange);
  void ensureVideoPlayback();
});

onBeforeUnmount(() => {
  document.removeEventListener('visibilitychange', onVisibilityChange);
  video.value?.pause();
});
</script>

<template>
  <div
    class="wallpaper-media"
    :class="{ 'has-media': Boolean(source) }"
    aria-hidden="true"
  >
    <video
      v-if="source && source.kind === 'video'"
      ref="video"
      class="wallpaper-video"
      :class="{ 'is-ready': ready }"
      autoplay
      loop
      muted
      playsinline
      preload="auto"
      disablepictureinpicture
      :src="assetUrl(source.path)"
      @loadeddata="ready = true"
      @playing="ready = true"
      @error="onMediaError"
    />
    <img
      v-else-if="source"
      ref="image"
      class="wallpaper-image"
      :class="{ 'is-ready': ready }"
      alt=""
      :src="assetUrl(source.path)"
      @load="ready = true"
      @error="onMediaError"
    />
  </div>
</template>