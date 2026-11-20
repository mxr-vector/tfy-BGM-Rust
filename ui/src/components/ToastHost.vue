<script setup lang="ts">
import { toastState } from '@/lib/toast';
</script>

<template>
  <div
    v-if="toastState.shown"
    id="toast"
    class="toast"
    :class="{
      'is-error': toastState.kind === 'error',
      'is-success': toastState.kind === 'success',
      'is-visible': toastState.visible,
    }"
  >
    {{ toastState.message }}
  </div>
</template>