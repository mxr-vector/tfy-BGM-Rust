<script setup lang="ts">
import { ref, watch } from 'vue';
import { state } from '@/stores/state';
import Icon from '@/components/Icon.vue';

const dismissed = ref(false);

watch(
  () => state.library.warnings,
  () => {
    dismissed.value = false;
  },
);
</script>

<template>
  <section
    v-if="state.library.warnings.length > 0 && !dismissed"
    class="warning-banner"
    id="warning-banner"
  >
    <Icon name="alert" />
    <div class="warning-list">
      <div v-for="(warning, index) in state.library.warnings" :key="index">
        {{ warning }}
      </div>
    </div>
    <button
      type="button"
      class="warning-close"
      title="暂时隐藏"
      @click="dismissed = true"
    >
      <Icon name="x" />
    </button>
  </section>
</template>