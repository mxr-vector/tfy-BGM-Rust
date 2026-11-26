<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import {
  audioIndexBusy,
  audioIndexProgress,
  audioIndexStatus,
  buildAudioIndex,
  deleteAudioDuplicates,
  disableAudioIndex as disableAudioIndexAction,
  duplicatesLoading,
  loadDuplicates,
  progressPercent,
  refreshAudioIndexStatus,
} from '@/composables/useAudioIndex';
import type { DuplicateReport } from '@/lib/types';
import Icon from '@/components/Icon.vue';
import DuplicateList from './components/DuplicateList.vue';
import IndexStatus from './components/IndexStatus.vue';
import SoundSearchPanel from './components/SoundSearchPanel.vue';

const props = defineProps<{ mode: 'duplicates' | 'search' }>();
const report = ref<DuplicateReport | null>(null);
const selected = ref(new Set<string>());
const deleting = ref(false);

const title = computed(() => (props.mode === 'duplicates' ? '音频指纹去重' : '离线以声搜声'));
const description = computed(() =>
  props.mode === 'duplicates'
    ? '按音频内容识别完全一致或高度相似的文件。逐项选择后才会删除，不提供全选或一键全清。'
    : '从本地索引中快速查找匹配或相似音频，支持较长曲目中的局部片段。',
);
const selectedPaths = computed(() => [...selected.value]);
/** 整组都被勾选的分组：后端会拒绝，这里提前禁用按钮并说明原因。 */
const exhaustedGroups = computed(() =>
  (report.value?.groups ?? []).filter(
    (group) => group.tracks.length > 0 && group.tracks.every((track) => selected.value.has(track.path)),
  ),
);

async function refreshDuplicates(): Promise<void> {
  const next = await loadDuplicates();
  if (!next) return;
  report.value = next;
  const available = new Set(next.groups.flatMap((group) => group.tracks.map((track) => track.path)));
  selected.value = new Set([...selected.value].filter((path) => available.has(path)));
}

function toggle(path: string): void {
  if (selected.value.has(path)) selected.value.delete(path);
  else selected.value.add(path);
}

async function removeSelected(): Promise<void> {
  if (selectedPaths.value.length === 0 || deleting.value || exhaustedGroups.value.length > 0) return;
  deleting.value = true;
  try {
    const result = await deleteAudioDuplicates(selectedPaths.value);
    if (result) {
      selected.value.clear();
      await refreshDuplicates();
    }
  } finally {
    deleting.value = false;
  }
}

async function build(): Promise<void> {
  if (await buildAudioIndex() && props.mode === 'duplicates') await refreshDuplicates();
}

async function disable(): Promise<void> {
  if (await disableAudioIndexAction()) {
    report.value = null;
    selected.value.clear();
  }
}

watch(
  [() => audioIndexStatus.value?.root, () => audioIndexStatus.value?.enabled],
  ([, enabled]) => {
    report.value = null;
    selected.value.clear();
    if (enabled && props.mode === 'duplicates') void refreshDuplicates();
  },
);

onMounted(async () => {
  await refreshAudioIndexStatus();
  if (props.mode === 'duplicates' && audioIndexStatus.value?.enabled) await refreshDuplicates();
});
</script>

<template>
  <section class="audio-tools-page">
    <header class="tools-heading">
      <div>
        <span class="tools-kicker">LOCAL AUDIO INTELLIGENCE</span>
        <h1>{{ title }}</h1>
        <p>{{ description }}</p>
      </div>
      <span class="offline-badge"><Icon name="chip" /> 完全离线</span>
    </header>
    <IndexStatus
      :status="audioIndexStatus"
      :busy="audioIndexBusy"
      :progress="audioIndexProgress"
      :percent="progressPercent"
      @build="build"
      @disable="disable"
    />
    <template v-if="mode === 'duplicates'">
      <div class="duplicates-toolbar">
        <span>重复分组 {{ report?.groups.length ?? 0 }} · 已检查 {{ report?.indexedTracks ?? 0 }} 首</span>
        <div>
          <button type="button" :disabled="duplicatesLoading || !audioIndexStatus?.enabled" @click="refreshDuplicates">
            <Icon name="refresh" /> 重新比对
          </button>
          <button
            type="button"
            class="danger-action"
            :disabled="selectedPaths.length === 0 || deleting || exhaustedGroups.length > 0"
            :title="exhaustedGroups.length > 0 ? '每个重复分组至少保留 1 个文件' : '移到曲库回收站'"
            @click="removeSelected"
          >
            <Icon name="trash" /> 删除选中文件（{{ selectedPaths.length }}）
          </button>
        </div>
      </div>
      <p v-if="exhaustedGroups.length > 0" class="selection-hint">
        有 {{ exhaustedGroups.length }} 个分组的全部副本都被勾选；每个分组至少保留 1 个文件才能删除。
      </p>
      <DuplicateList :report="report" :selected="selected" :loading="duplicatesLoading" @toggle="toggle" />
    </template>
    <SoundSearchPanel v-else />
  </section>
</template>

<style scoped>
.audio-tools-page { display: flex; flex: 1; min-height: 0; flex-direction: column; gap: 13px; }
.tools-heading { display: flex; align-items: center; justify-content: space-between; gap: 18px; flex: none; }
.tools-kicker { color: #6ff3bc; font-size: 9px; font-weight: 700; letter-spacing: 0.18em; }
.tools-heading h1 { margin: 5px 0 6px; color: var(--text); font-size: 25px; letter-spacing: -0.03em; }
.tools-heading p { margin: 0; color: var(--text-3); font-size: 11.5px; line-height: 1.6; }
.offline-badge { display: inline-flex; align-items: center; gap: 6px; flex: none; padding: 7px 10px; border: 1px solid rgba(25, 217, 139, 0.16); border-radius: 99px; color: #9df7cf; font-size: 10px; background: rgba(25, 217, 139, 0.07); }
.duplicates-toolbar { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex: none; color: var(--text-3); font-size: 10.5px; }
.duplicates-toolbar > div { display: flex; gap: 8px; }
.duplicates-toolbar button { display: inline-flex; align-items: center; gap: 6px; min-height: 32px; padding: 0 11px; border-radius: 8px; color: var(--text-2); background: rgba(255, 255, 255, 0.055); }
.duplicates-toolbar button.danger-action { color: #ffc4c4; background: rgba(239, 68, 68, 0.13); }
.duplicates-toolbar button:disabled { opacity: 0.42; cursor: not-allowed; }
.selection-hint { flex: none; margin: -4px 0 0; color: #ffc4c4; font-size: 10.5px; line-height: 1.6; }
@media (max-width: 720px) {
  .tools-heading { align-items: flex-start; }
  .offline-badge { display: none; }
  .duplicates-toolbar { align-items: flex-start; flex-direction: column; }
  .duplicates-toolbar > div { width: 100%; }
  .duplicates-toolbar button { flex: 1; justify-content: center; }
}
</style>
