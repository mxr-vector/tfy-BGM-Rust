<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue';
import { closeModal, modalState } from '@/lib/modal';
import Icon from './Icon.vue';

const values = reactive<Record<string, string>>({});
const errorText = ref('');
const panel = ref<HTMLElement | null>(null);
const cancelButton = ref<HTMLButtonElement | null>(null);
const confirmButton = ref<HTMLButtonElement | null>(null);
let previousFocus: HTMLElement | null = null;

watch(
  () => modalState.current,
  async (current) => {
    if (!current) {
      if (previousFocus?.isConnected) previousFocus.focus();
      previousFocus = null;
      return;
    }
    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    errorText.value = '';
    for (const key of Object.keys(values)) delete values[key];
    for (const field of current.fields ?? []) {
      const fallback =
        field.type === 'select'
          ? (field.options?.[0]?.value ?? '')
          : field.type === 'color'
            ? '#22c55e'
            : '';
      values[field.name] = field.value ?? fallback;
    }

    await nextTick();
    const first = panel.value?.querySelector<HTMLElement>('input, select');
    if (first) {
      first.focus();
      if (first instanceof HTMLInputElement) first.select();
    } else if (current.danger) {
      // 危险操作默认落在「取消」上，避免回车误确认。
      cancelButton.value?.focus();
    } else {
      confirmButton.value?.focus();
    }
  },
);

function submit(): void {
  const current = modalState.current;
  if (!current) return;
  const result: Record<string, string> = {};
  for (const field of current.fields ?? []) result[field.name] = values[field.name] ?? '';

  const error = current.validate?.(result);
  if (error) {
    errorText.value = error;
    return;
  }
  closeModal(result);
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Escape' || !modalState.current) return;
  event.preventDefault();
  event.stopPropagation();
  closeModal(null);
}

onMounted(() => document.addEventListener('keydown', onKeydown, true));
onBeforeUnmount(() => document.removeEventListener('keydown', onKeydown, true));
</script>

<template>
  <Teleport to="body">
    <div
      v-if="modalState.current"
      class="modal-backdrop"
      @mousedown.self="closeModal(null)"
    >
      <div
        ref="panel"
        class="modal-panel"
        role="dialog"
        aria-modal="true"
        :aria-label="modalState.current.title"
      >
        <div class="modal-header">
          <h2 class="modal-title">{{ modalState.current.title }}</h2>
          <button
            type="button"
            class="modal-close"
            title="关闭"
            @click="closeModal(null)"
          >
            <Icon name="x" />
          </button>
        </div>
        <form class="modal-form" @submit.prevent="submit">
          <div class="modal-body">
            <p v-if="modalState.current.description" class="modal-description">
              {{ modalState.current.description }}
            </p>
            <label
              v-for="field in modalState.current.fields ?? []"
              :key="field.name"
              class="modal-field"
            >
              <span class="modal-label">{{ field.label }}</span>
              <select
                v-if="field.type === 'select'"
                v-model="values[field.name]"
                class="modal-select"
                :name="field.name"
              >
                <option
                  v-for="option in field.options ?? []"
                  :key="option.value"
                  :value="option.value"
                >
                  {{ option.label }}
                </option>
              </select>
              <input
                v-else-if="field.type === 'color'"
                v-model="values[field.name]"
                class="modal-input modal-color"
                type="color"
                :name="field.name"
              />
              <input
                v-else
                v-model="values[field.name]"
                class="modal-input"
                type="text"
                :name="field.name"
                :placeholder="field.placeholder ?? ''"
                autocomplete="off"
                spellcheck="false"
              />
            </label>
            <p v-if="errorText" class="modal-error">{{ errorText }}</p>
          </div>
          <div class="modal-footer">
            <button
              ref="cancelButton"
              type="button"
              class="modal-btn ghost"
              @click="closeModal(null)"
            >
              {{ modalState.current.cancelText ?? '取消' }}
            </button>
            <button
              ref="confirmButton"
              type="submit"
              class="modal-btn"
              :class="modalState.current.danger ? 'danger' : 'primary'"
            >
              {{ modalState.current.confirmText ?? '确定' }}
            </button>
          </div>
        </form>
      </div>
    </div>
  </Teleport>
</template>