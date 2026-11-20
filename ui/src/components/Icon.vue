<script setup lang="ts">
import { computed } from 'vue';
import { iconMarkup } from '@/lib/icons';

const props = defineProps<{
  name: string;
  svgClass?: string;
}>();

const html = computed(() => iconMarkup(props.name, props.svgClass));
</script>

<template>
  <span :data-icon="name" v-html="html" />
</template>