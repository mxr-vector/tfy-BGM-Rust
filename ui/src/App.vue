<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { applyDocumentCopy, playerConfig } from '@/lib/config';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, isTauri, getCurrentWindow } from '@/lib/tauri';
import type { Bootstrap } from '@/lib/types';
import { loadBuiltins, wallpaperView } from '@/composables/useWallpaper';
import {
  adjustVolume,
  closeDrawer,
  drawerOpen,
  playSong,
  seekBy,
  toggleMute,
  togglePlay,
} from '@/stores/player';
import {
  applyBootstrap,
  clearSelection,
  setSearch,
} from '@/stores/library';
import { state, visibleSongs } from '@/stores/state';
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
let hotkeysBound = false;

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

function isTypingTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.isContentEditable)
  );
}

function focusSearch(): void {
  const input = document.querySelector<HTMLInputElement>('#search-input');
  if (!input) return;
  input.focus();
  // 用 / 聚焦时直接选中已有查询，方便整段替换。
  input.select();
}

function handleGlobalKeydown(event: KeyboardEvent): void {
  const target = event.target;

  if (event.key === '/' && !isTypingTarget(target)) {
    event.preventDefault();
    focusSearch();
    return;
  }

  if (isTypingTarget(target)) return;

  if (
    event.code === 'Space' &&
    target instanceof HTMLElement &&
    target.closest('button, a, [role="button"]')
  ) {
    return;
  }

  switch (event.code) {
    case 'Space':
      event.preventDefault();
      if (!state.currentSong) {
        const list = visibleSongs.value;
        if (list.length > 0) playSong(list[0]!, list, true);
      } else {
        togglePlay();
      }
      break;
    case 'ArrowLeft':
      event.preventDefault();
      seekBy(-playerConfig().seekStepSeconds);
      break;
    case 'ArrowRight':
      event.preventDefault();
      seekBy(playerConfig().seekStepSeconds);
      break;
    case 'ArrowUp':
      event.preventDefault();
      adjustVolume(playerConfig().volumeStep);
      break;
    case 'ArrowDown':
      event.preventDefault();
      adjustVolume(-playerConfig().volumeStep);
      break;
    case 'KeyM':
      event.preventDefault();
      toggleMute();
      break;
    case 'Escape':
      if (drawerOpen.value) {
        event.preventDefault();
        closeDrawer();
      } else if (state.selected.size > 0) {
        event.preventDefault();
        clearSelection();
      } else if (state.search) {
        event.preventDefault();
        setSearch('');
      }
      break;
    default:
      break;
  }
}

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
    if (!hotkeysBound) {
      window.addEventListener('keydown', handleGlobalKeydown);
      hotkeysBound = true;
    }
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
  if (hotkeysBound) window.removeEventListener('keydown', handleGlobalKeydown);
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