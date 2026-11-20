<script setup lang="ts">
withDefaults(defineProps<{ color?: string }>(), { color: '#19d98b' });
</script>

<template>
  <div class="equalizer">
    <i
      v-for="index in 4"
      :key="index"
      class="equalizer-bar"
      :style="{ backgroundColor: color }"
    />
  </div>
</template>