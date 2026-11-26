<script setup lang="ts">
import { useId } from 'vue';

defineProps<{ modelValue: string; busy: boolean }>();
const emit = defineEmits<{
  'update:modelValue': [value: string];
  parse: [];
}>();

const inputId = useId();

function onInput(event: Event): void {
  emit('update:modelValue', (event.target as HTMLTextAreaElement).value);
}
</script>

<template>
  <form class="migration-panel source-form" @submit.prevent="emit('parse')">
    <h2><label :for="inputId">歌单链接</label></h2>
    <p :id="`${inputId}-hint`" class="migration-hint">
      粘贴网易云音乐、QQ 音乐或汽水音乐的公开歌单链接，也可粘贴含链接的分享文案。多个链接可分行输入。
    </p>
    <textarea
      :id="inputId"
      :aria-describedby="`${inputId}-hint`"
      :value="modelValue"
      class="migration-field"
      rows="4"
      spellcheck="false"
      @input="onInput"
    ></textarea>
    <button class="migration-button is-primary" type="submit" :disabled="busy || !modelValue.trim()">
      {{ busy ? '正在读取…' : '解析链接' }}
    </button>
  </form>
</template>

<style scoped>
.source-form { display: grid; gap: 9px; }
textarea { min-height: 76px; }
button { justify-self: start; }
</style>
