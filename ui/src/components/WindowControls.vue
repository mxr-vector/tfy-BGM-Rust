<script setup lang="ts">
import { getCurrentWindow } from '@/lib/tauri';
import { useWindowState } from '@/composables/useWindowState';
import { openClosePrompt, performCloseAction } from '@/stores/closePrompt';
import { state } from '@/stores/state';
import Icon from './Icon.vue';

const props = withDefaults(
  defineProps<{ idPrefix?: string }>(),
  { idPrefix: 'win' },
);
const appWindow = getCurrentWindow();
const { maximized, refresh } = useWindowState();

function swallow(promise: Promise<unknown>): void {
  void promise.catch((error) => console.warn('[window]', error));
}

function onToggleMaximize(): void {
  if (!appWindow) return;
  void appWindow
    .toggleMaximize()
    .then(refresh)
    .catch((error) => console.warn('[window]', error));
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

</script>

<template>
  <div
    v-if="appWindow"
    :id="props.idPrefix === 'win' ? 'window-controls' : `${props.idPrefix}-controls`"
    class="window-controls"
    role="group"
    aria-label="窗口控制"
  >
    <button
      type="button"
      class="window-btn"
      :id="`${props.idPrefix}-min`"
      title="最小化"
      aria-label="最小化"
      @click="appWindow && swallow(appWindow.minimize())"
    >
      <Icon name="win-min" />
    </button>
    <button
      type="button"
      class="window-btn"
      :id="`${props.idPrefix}-max`"
      :title="maximized ? '还原' : '最大化'"
      :aria-label="maximized ? '还原' : '最大化'"
      @click="onToggleMaximize"
    >
      <Icon :name="maximized ? 'win-restore' : 'win-max'" />
    </button>
    <button
      type="button"
      class="window-btn window-btn-close"
      :id="`${props.idPrefix}-close`"
      title="关闭"
      aria-label="关闭"
      @click="onClose"
    >
      <Icon name="win-close" />
    </button>
  </div>
</template>