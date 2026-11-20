<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import {
  availableWallpaperOptions,
  builtins,
  chooseCustomWallpaper,
  closeWallpaperPopover,
  fileName,
  kindOf,
  popoverAnchor,
  popoverOpen,
  wallpaperView,
} from '@/composables/useWallpaper';
import { assetUrl } from '@/lib/tauri';
import { setWallpaper, setWallpaperBlur, setWallpaperFit, setWallpaperOpacity } from '@/stores/prefs';
import { state } from '@/stores/state';
import type { WallpaperAsset } from '@/lib/types';
import Icon from './Icon.vue';

const panel = ref<HTMLElement | null>(null);
const position = ref({ left: 12, top: 72 });
const customImageFailed = ref(false);

const savedCustom = computed(() => state.prefs?.customWallpaper ?? null);
const savedKind = computed(() => kindOf(savedCustom.value));

const builtinMap = computed(
  () => new Map(builtins.value.map((item) => [item.id, item])),
);

function assetFor(id: string): WallpaperAsset | undefined {
  return builtinMap.value.get(id);
}

function positionPanel(): void {
  const anchor = popoverAnchor.value;
  if (!anchor) return;
  const rect = anchor.getBoundingClientRect();
  const width = panel.value?.offsetWidth ?? 356;
  const left = Math.min(
    Math.max(12, rect.right - width),
    window.innerWidth - width - 12,
  );
  position.value = { left: Math.max(12, left), top: rect.bottom + 10 };
}

function selectWallpaper(id: string): void {
  const view = wallpaperView.value;
  setWallpaper(id, null, { opacity: view.opacity, blur: view.blur, fit: view.fit });
  closeWallpaperPopover();
}

function previewClass(kind?: 'image' | 'video'): Record<string, boolean> {
  return {
    'has-image': Boolean(kind),
    'is-video': kind === 'video',
  };
}

function onOpacityInput(event: Event): void {
  setWallpaperOpacity(Number((event.target as HTMLInputElement).value) / 100);
}

function onBlurInput(event: Event): void {
  setWallpaperBlur(Number((event.target as HTMLInputElement).value));
}

watch(
  () => popoverOpen.value,
  async (open) => {
    if (!open) return;
    customImageFailed.value = false;
    await nextTick();
    positionPanel();
  },
);

function onWindowResize(): void {
  if (popoverOpen.value) positionPanel();
}

function onDocumentClick(): void {
  if (popoverOpen.value) closeWallpaperPopover();
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape' || !popoverOpen.value) return;
  event.stopPropagation();
  closeWallpaperPopover();
}

onMounted(() => {
  window.addEventListener('resize', onWindowResize);
  document.addEventListener('click', onDocumentClick);
  document.addEventListener('keydown', onKeydown, true);
});

onBeforeUnmount(() => {
  window.removeEventListener('resize', onWindowResize);
  document.removeEventListener('click', onDocumentClick);
  document.removeEventListener('keydown', onKeydown, true);
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="popoverOpen"
      ref="panel"
      class="wallpaper-popover"
      id="wallpaper-popover"
      role="dialog"
      aria-label="界面壁纸"
      :style="{ left: `${position.left}px`, top: `${position.top}px` }"
      @click.stop
    >
      <div class="wallpaper-pop-head">
        <div class="wallpaper-pop-title">
          <strong>界面壁纸</strong>
          <span>选择后自动保存，支持动态壁纸</span>
        </div>
        <Icon name="palette" class="wallpaper-pop-icon" />
      </div>

      <div class="wallpaper-grid">
        <button
          v-for="option in availableWallpaperOptions"
          :key="option.id"
          type="button"
          class="wallpaper-option"
          :class="{ 'is-active': wallpaperView.id === option.id }"
          :data-wallpaper="option.id"
          :aria-label="`使用${option.label}壁纸`"
          :aria-pressed="wallpaperView.id === option.id"
          @click="selectWallpaper(option.id)"
        >
          <span
            class="wallpaper-preview"
            :class="[
              `preview-${option.preview}`,
              previewClass(assetFor(option.id)?.kind),
            ]"
            aria-hidden="true"
          >
            <template v-if="assetFor(option.id)">
              <img
                v-if="assetFor(option.id)!.thumb"
                :src="assetUrl(assetFor(option.id)!.thumb)"
                alt=""
              />
              <span v-else class="preview-icon">
                <Icon :name="assetFor(option.id)!.kind === 'video' ? 'film' : 'image'" />
              </span>
              <span v-if="assetFor(option.id)!.kind === 'video'" class="wallpaper-badge">
                <Icon name="play-fill" />
              </span>
            </template>
          </span>
          <span class="wallpaper-meta">
            <span class="wallpaper-name">{{ option.label }}</span>
            <span class="wallpaper-desc">{{ option.hint }}</span>
          </span>
          <span class="wallpaper-check">
            <Icon name="check" />
          </span>
        </button>
      </div>

      <div class="wallpaper-pop-foot">
        <button
          type="button"
          class="wallpaper-option wallpaper-option-custom"
          :class="{ 'is-active': wallpaperView.id === 'custom' }"
          id="wallpaper-custom-btn"
          aria-label="选择本地自定义壁纸"
          :aria-pressed="wallpaperView.id === 'custom'"
          @click="chooseCustomWallpaper"
        >
          <span
            class="wallpaper-preview is-custom"
            :class="{
              'has-image': Boolean(savedCustom) && savedKind === 'image' && !customImageFailed,
              'is-video': savedKind === 'video',
            }"
            aria-hidden="true"
          >
            <img
              v-if="savedCustom && savedKind === 'image' && !customImageFailed"
              :src="assetUrl(savedCustom)"
              alt=""
              @error="customImageFailed = true"
            />
            <span v-else class="preview-icon">
              <Icon :name="savedKind === 'video' ? 'play-fill' : 'image'" />
            </span>
          </span>
          <span class="wallpaper-meta">
            <span class="wallpaper-name">自定义壁纸</span>
            <span class="wallpaper-desc" id="wallpaper-custom-hint">
              {{
                savedCustom
                  ? `上次使用：${fileName(savedCustom)}`
                  : '支持 JPG / PNG / WebP / MP4'
              }}
            </span>
          </span>
          <span class="wallpaper-check">
            <Icon name="check" />
          </span>
        </button>

        <div class="wallpaper-control-row">
          <span class="wallpaper-control-label">画面展示</span>
          <div class="wallpaper-fit-tabs">
            <button
              type="button"
              class="wallpaper-fit-btn"
              :class="{ 'is-active': wallpaperView.fit === 'cover' }"
              aria-label="铺满裁剪（填满窗口，裁剪溢出边缘）"
              :aria-pressed="wallpaperView.fit === 'cover'"
              @click="setWallpaperFit('cover')"
            >
              铺满裁剪
            </button>
            <button
              type="button"
              class="wallpaper-fit-btn"
              :class="{ 'is-active': wallpaperView.fit === 'contain' }"
              aria-label="完整展示（保留完整画面，不裁切任何内容）"
              :aria-pressed="wallpaperView.fit === 'contain'"
              @click="setWallpaperFit('contain')"
            >
              完整展示
            </button>
          </div>
        </div>

        <div class="wallpaper-control-group">
          <div class="wallpaper-control-head">
            <span class="wallpaper-control-label">界面透明度</span>
            <span class="wallpaper-control-val" id="wallpaper-opacity-val">
              {{ Math.round(wallpaperView.opacity * 100) }}%
            </span>
          </div>
          <div class="wallpaper-slider-track">
            <span class="slider-hint">实底</span>
            <input
              class="wallpaper-slider"
              id="wallpaper-opacity-slider"
              type="range"
              min="0"
              max="90"
              step="5"
              :value="Math.round(wallpaperView.opacity * 100)"
              aria-label="调整界面透明度"
              @input="onOpacityInput"
            />
            <span class="slider-hint">通透</span>
          </div>
        </div>

        <div class="wallpaper-control-group">
          <div class="wallpaper-control-head">
            <span class="wallpaper-control-label">内容区模糊</span>
            <span class="wallpaper-control-val" id="wallpaper-blur-val">
              {{ wallpaperView.blur }}px
            </span>
          </div>
          <div class="wallpaper-slider-track">
            <span class="slider-hint">清晰 (0px)</span>
            <input
              class="wallpaper-slider"
              id="wallpaper-blur-slider"
              type="range"
              min="0"
              max="16"
              step="1"
              :value="wallpaperView.blur"
              aria-label="调整内容区毛玻璃模糊度"
              @input="onBlurInput"
            />
            <span class="slider-hint">柔化</span>
          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>