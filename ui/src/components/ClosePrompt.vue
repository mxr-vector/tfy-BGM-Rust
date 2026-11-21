<script setup lang="ts">
/** 关闭窗口确认：关闭程序 / 最小化托盘，可勾选不再提示。 */

import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import {
  closeClosePrompt,
  closePromptOpen,
  performCloseAction,
} from '@/stores/closePrompt';
import Icon from './Icon.vue';

const remember = ref(false);
const busy = ref(false);
const panel = ref<HTMLElement | null>(null);

async function choose(action: 'quit' | 'tray'): Promise<void> {
  if (busy.value) return;
  busy.value = true;
  await performCloseAction(action, remember.value);
  busy.value = false;
}

/** 捕获阶段处理 Esc：关闭按钮在顶栏，焦点可能仍在窗口控件上。 */
function onDocumentKeydown(event: KeyboardEvent): void {
  if (!closePromptOpen.value || event.key !== 'Escape') return;
  event.preventDefault();
  event.stopPropagation();
  closeClosePrompt();
}

onMounted(() => {
  document.addEventListener('keydown', onDocumentKeydown, true);
});

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onDocumentKeydown, true);
});

watch(closePromptOpen, async (open) => {
  if (!open) return;
  remember.value = false;
  busy.value = false;
  await nextTick();
  panel.value?.focus();
});
</script>

<template>
  <Teleport to="body">
    <div
      v-if="closePromptOpen"
      class="close-prompt-backdrop"
      @mousedown.self="closeClosePrompt"
    >
      <section
        ref="panel"
        class="close-prompt"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="close-prompt-title"
        tabindex="-1"
        @keydown.stop
      >
        <header class="close-prompt-head">
          <span class="close-prompt-icon" aria-hidden="true"><Icon name="alert" /></span>
          <div>
            <h2 id="close-prompt-title">关闭听风吟 · BGM？</h2>
            <p>关闭程序会立即退出；最小化到托盘可在后台继续播放，并开启「最小化托盘」设置。</p>
          </div>
        </header>

        <label class="close-prompt-remember">
          <input v-model="remember" type="checkbox" />
          <span>记住我的选择，下次不再提示</span>
        </label>

        <footer class="close-prompt-actions">
          <button type="button" class="settings-btn" @click="closeClosePrompt">取消</button>
          <button
            type="button"
            class="settings-btn is-primary"
            :disabled="busy"
            @click="choose('tray')"
          >
            最小化托盘
          </button>
          <button
            type="button"
            class="settings-btn is-danger"
            :disabled="busy"
            @click="choose('quit')"
          >
            关闭程序
          </button>
        </footer>
      </section>
    </div>
  </Teleport>
</template>
