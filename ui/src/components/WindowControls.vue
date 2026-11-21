<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue';
import { getCurrentWindow } from '@/lib/tauri';
import { openClosePrompt, performCloseAction } from '@/stores/closePrompt';
import { state } from '@/stores/state';
import Icon from './Icon.vue';

const appWindow = getCurrentWindow();
const maximized = ref(false);
let unlisten: (() => void) | null = null;

function swallow(promise: Promise<unknown>): void {
  void promise.catch((error) => console.warn('[window]', error));
}

/** 关闭按钮：默认询问「关闭程序 / 最小化托盘」，记住选择后直接执行。 */
function onClose(): void {
  if (!appWindow) return;
  const action = state.prefs?.closeAction ?? 'ask';
  if (action === 'ask') {
    openClosePrompt();
    return;
  }
  void performCloseAction(action, false);
}

async function syncMaximize(): Promise<void> {
  if (!appWindow) return;
  try {
    maximized.value = await appWindow.isMaximized();
  } catch {
    // 窗口关闭或权限不足时保持原图标。
  }
}

onMounted(() => {
  if (!appWindow) return;
  void syncMaximize();
  appWindow
    .onResized(() => void syncMaximize())
    .then((off) => {
      unlisten = off;
    })
    .catch((error) => console.warn('[window]', error));
});

onBeforeUnmount(() => unlisten?.());
</script>

<template>
  <div v-if="appWindow" class="window-controls" id="window-controls" role="group" aria-label="窗口控制">
    <button
      type="button"
      class="window-btn"
      id="win-min"
      title="最小化"
      aria-label="最小化"
      @click="appWindow && swallow(appWindow.minimize())"
    >
      <Icon name="win-min" />
    </button>
    <button
      type="button"
      class="window-btn"
      id="win-max"
      :title="maximized ? '还原' : '最大化'"
      :aria-label="maximized ? '还原' : '最大化'"
      @click="appWindow && swallow(appWindow.toggleMaximize())"
    >
      <Icon :name="maximized ? 'win-restore' : 'win-max'" />
    </button>
    <button
      type="button"
      class="window-btn window-btn-close"
      id="win-close"
      title="关闭"
      aria-label="关闭"
      @click="onClose"
    >
      <Icon name="win-close" />
    </button>
  </div>
</template>