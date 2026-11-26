<script setup lang="ts">
/** 复用「播放输出」面板的开关样式，保证设置界面的交互密度一致。 */
const props = defineProps<{
  modelValue: boolean;
  label: string;
  disabled?: boolean;
}>();

const emit = defineEmits<{ (event: 'update:modelValue', value: boolean): void }>();

function toggle(): void {
  if (props.disabled) return;
  emit('update:modelValue', !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    class="output-switch"
    :class="{ 'is-on': modelValue }"
    role="switch"
    :aria-checked="modelValue"
    :aria-label="label"
    :disabled="disabled"
    @click="toggle"
  >
    <span class="output-switch-thumb"></span>
  </button>
</template>
