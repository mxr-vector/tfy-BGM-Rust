<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { assetUrl, invoke, isTauri } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import { setWallpaper } from '@/stores/prefs';
import { wallpaperView } from '@/composables/useWallpaper';

const props = defineProps<{ focused: boolean }>();

const video = ref<HTMLVideoElement | null>(null);
const image = ref<HTMLImageElement | null>(null);
const ready = ref(false);
const documentHidden = ref(document.hidden);
const videoSource = ref<string | null>(null);
let sourceRevision = 0;

const source = computed(() => wallpaperView.value.source);

async function ensureVideoPlayback(): Promise<void> {
  const element = video.value;
  if (!element || !videoSource.value) return;
  if (props.focused && !documentHidden.value) {
    await element.play().catch(() => {
      // WebView 可能阻止自动播放；已有首帧时仍显示静态画面。
      ready.value = element.readyState >= 2;
    });
  } else {
    element.pause();
  }
}

function fallbackWallpaper(message: string): void {
  videoSource.value = null;
  toast(message, 'error');
  setWallpaper('default', null);
}

function onMediaError(): void {
  // Linux 视频地址解析完成前不设置 src，避免 asset:// 的失败事件抢先触发回退。
  if (source.value?.kind === 'video' && !videoSource.value) return;
  fallbackWallpaper(
    source.value?.kind === 'video'
      ? '动态壁纸加载失败，已切换回默认样式'
      : '图片壁纸加载失败，已切换回默认样式',
  );
}

function onVisibilityChange(): void {
  documentHidden.value = document.hidden;
}

watch(
  () => source.value?.path ?? null,
  async (path, _previous, onCleanup) => {
    const revision = ++sourceRevision;
    ready.value = false;
    videoSource.value = null;
    video.value?.pause();
    onCleanup(() => video.value?.pause());

    await nextTick();
    if (revision !== sourceRevision || !path) return;
    if (source.value?.kind !== 'video') {
      if (image.value?.complete && image.value.naturalWidth > 0) ready.value = true;
      return;
    }

    try {
      const mediaUrl = isTauri
        ? await invoke<string | null>(COMMANDS.wallpaperMediaUrl, { path })
        : null;
      if (revision !== sourceRevision) return;
      videoSource.value = mediaUrl ?? assetUrl(path);
      await nextTick();
      await ensureVideoPlayback();
    } catch (error) {
      if (revision !== sourceRevision) return;
      fallbackWallpaper(`动态壁纸加载失败：${normalizeError(error)}`);
    }
  },
  { immediate: true },
);

watch([() => props.focused, documentHidden], () => {
  void ensureVideoPlayback();
});

onMounted(() => {
  document.addEventListener('visibilitychange', onVisibilityChange);
  void ensureVideoPlayback();
});

onBeforeUnmount(() => {
  sourceRevision += 1;
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
      :key="source.path"
      ref="video"
      class="wallpaper-video"
      :class="{ 'is-ready': ready }"
      autoplay
      loop
      muted
      playsinline
      preload="auto"
      disablepictureinpicture
      :src="videoSource || undefined"
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