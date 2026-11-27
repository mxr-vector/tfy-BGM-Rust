<script setup lang="ts">
import { ref, watch } from 'vue';
import Icon from '@/components/Icon.vue';
import { COMMANDS } from '@/lib/constants';
import { assetUrl, invoke, isTauri } from '@/lib/tauri';

const props = defineProps<{ path: string }>();

const src = ref<string | null>(null);
const failed = ref(false);

// 视频壁纸预览直接取首帧，不再依赖单独的缩略图文件。
watch(
  () => props.path,
  async (path, _previous, onCleanup) => {
    src.value = null;
    failed.value = false;
    let cancelled = false;
    onCleanup(() => {
      cancelled = true;
    });

    if (!isTauri) {
      src.value = assetUrl(path);
      return;
    }
    try {
      // Linux 的 WebKitGTK 打不开 asset:// 视频，需要用本地媒体服务地址。
      const media = await invoke<string | null>(COMMANDS.wallpaperMediaUrl, { path });
      if (!cancelled) src.value = media ?? assetUrl(path);
    } catch {
      if (!cancelled) src.value = assetUrl(path);
    }
  },
  { immediate: true },
);
</script>

<template>
  <video
    v-if="src && !failed"
    class="wallpaper-preview-video"
    :src="src"
    muted
    playsinline
    preload="auto"
    disablepictureinpicture
    @error="failed = true"
  />
  <span v-else class="preview-icon">
    <Icon name="film" />
  </span>
</template>
