<script setup lang="ts">
import { computed } from 'vue';
import {
  deleteSongs,
  exportSelection,
  openMove,
} from '@/stores/actions';
import { clearSelection, selectedKeys } from '@/stores/library';
import { state, visibleSongs } from '@/stores/state';
import Icon from '@/components/Icon.vue';

const count = computed(() => state.selected.size);
const visibleSelected = computed(
  () =>
    visibleSongs.value.reduce(
      (total, song) => total + (state.selected.has(song.key) ? 1 : 0),
      0,
    ),
);
const hint = computed(() =>
  count.value > visibleSelected.value
    ? `当前筛选内 ${visibleSelected.value} 首`
    : '对选中曲目执行批量操作',
);

function onBatchAction(action: string | undefined): void {
  switch (action) {
    case 'export':
      void exportSelection(selectedKeys());
      break;
    case 'move':
      void openMove(selectedKeys());
      break;
    case 'delete':
      void deleteSongs(selectedKeys());
      break;
    case 'clear':
      clearSelection();
      break;
    default:
      break;
  }
}
</script>

<template>
  <section v-if="count > 0" class="batch-bar" id="batch-bar">
    <div class="batch-info">
      <span class="batch-count" id="batch-count">已选 {{ count }} 首</span>
      <span class="batch-hint">{{ hint }}</span>
    </div>
    <div class="batch-actions">
      <button type="button" class="batch-btn" data-batch-action="export" @click="onBatchAction('export')">
        <Icon name="download" /><span>导出</span>
      </button>
      <button type="button" class="batch-btn" data-batch-action="move" @click="onBatchAction('move')">
        <Icon name="folder" /><span>移动分类</span>
      </button>
      <button
        type="button"
        class="batch-btn is-danger"
        data-batch-action="delete"
        @click="onBatchAction('delete')"
      >
        <Icon name="trash" /><span>删除</span>
      </button>
      <button type="button" class="batch-btn" data-batch-action="clear" @click="onBatchAction('clear')">
        <Icon name="x" /><span>取消选择</span>
      </button>
    </div>
  </section>
</template>