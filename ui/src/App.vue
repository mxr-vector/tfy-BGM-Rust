<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { applyDocumentCopy } from '@/lib/config';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, isTauri, getCurrentWindow } from '@/lib/tauri';
import type { Bootstrap } from '@/lib/types';
import { installGlobalHotkeys } from '@/composables/useGlobalHotkeys';
import { loadBuiltins, wallpaperView } from '@/composables/useWallpaper';
import { applyBootstrap } from '@/stores/library';
import AudioPlayer from '@/components/AudioPlayer.vue';
import ContextMenu from '@/components/ContextMenu.vue';
import Icon from '@/components/Icon.vue';
import ModalHost from '@/components/ModalHost.vue';
import QueueDrawer from '@/components/QueueDrawer.vue';
import Sidebar from '@/components/Sidebar.vue';
import ToastHost from '@/components/ToastHost.vue';
import TopBar from '@/components/TopBar.vue';
import WallpaperLayer from '@/components/WallpaperLayer.vue';
import WallpaperPopover from '@/components/WallpaperPopover.vue';
import Workspace from '@/components/Workspace.vue';

const fatal = ref<{ title: string; message: string } | null>(null);
const focused = ref(true);

const appWindow = getCurrentWindow();
let unlistenFocus: (() => void) | null = null;
let uninstallHotkeys: (() => void) | null = null;

const shellStyle = computed<Record<string, string>>(() => {
  const view = wallpaperView.value;
  return {
    '--panel-opacity': view.panelOpacity.toFixed(3),
    '--wall-mask-opacity': view.maskOpacity.toFixed(3),
    '--panel-blur': `${view.blur}px`,
    '--table-blur': view.blur > 0 ? `blur(${view.blur}px) saturate(110%)` : 'none',
    '--wallpaper-fit': view.fit,
  };
});

async function bootstrap(): Promise<void> {
  if (!isTauri) {
    fatal.value = {
      title: '请通过 Tauri 启动',
      message:
        '这是桌面客户端界面，需要 Rust 侧提供曲库服务。请运行 cargo tauri dev 或安装打包后的应用。',
    };
    return;
  }

  try {
    const boot = await invoke<Bootstrap>(COMMANDS.bootstrap);
    applyBootstrap(boot);
    uninstallHotkeys ??= installGlobalHotkeys();
  } catch (error) {
    fatal.value = { title: '曲库加载失败', message: normalizeError(error) };
  }
}

function reload(): void {
  window.location.reload();
}

applyDocumentCopy();
void loadBuiltins();

onMounted(async () => {
  void bootstrap();
  if (!appWindow) return;
  try {
    unlistenFocus = await appWindow.onFocusChanged(({ payload }) => {
      focused.value = payload;
    });
  } catch (error) {
    console.warn('[window]', error);
  }
});

onBeforeUnmount(() => {
  unlistenFocus?.();
  uninstallHotkeys?.();
  uninstallHotkeys = null;
});
</script>

<template>
  <div
    class="app-shell"
    :class="[...wallpaperView.shellClass, { 'is-inactive': !focused }]"
    :style="shellStyle"
  >
    <WallpaperLayer :focused="focused" />
    <TopBar />
    <Sidebar />
    <Workspace />
    <AudioPlayer />
    <QueueDrawer />
    <ToastHost />

    <div v-if="fatal" class="fatal-overlay">
      <Icon name="alert" />
      <h1 class="fatal-title">{{ fatal.title }}</h1>
      <p class="fatal-text">{{ fatal.message }}</p>
      <button type="button" class="reset-filter-btn" @click="reload">重新加载</button>
    </div>
  </div>

  <WallpaperPopover />
  <ModalHost />
  <ContextMenu />
</template>