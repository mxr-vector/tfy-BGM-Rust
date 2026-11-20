<script setup lang="ts">
import { computed, nextTick, reactive, ref } from 'vue';
import {
  currentProgram,
  currentProgramSong,
  nextProgram,
  schedulerNow,
} from '@/composables/useScheduler';
import { showModal } from '@/lib/modal';
import {
  draftFromEntry,
  emptyDraft,
  minutesOf,
  type Draft,
} from '@/lib/schedule-draft';
import type { ScheduleEntry } from '@/lib/types';
import { applyScheduleEntry, persistSchedule } from '@/stores/schedule';
import { categoryById, songByKey, state } from '@/stores/state';
import Icon from './Icon.vue';
import ScheduleEditor from './ScheduleEditor.vue';

const editing = ref(false);
const saving = ref(false);
const editingId = ref<string | null>(null);
const expandedIds = ref(new Set<string>());
const draft = reactive<Draft>(emptyDraft());

const sortedEntries = computed(() =>
  [...state.schedule].sort((a, b) => a.start.localeCompare(b.start)),
);

const nowLabel = computed(() => {
  const date = schedulerNow.value;
  return [date.getHours(), date.getMinutes(), date.getSeconds()]
    .map((value) => String(value).padStart(2, '0'))
    .join(':');
});

const currentLabel = computed(() => {
  const entry = currentProgram.value;
  if (!entry) return '未编排';
  const song = currentProgramSong.value;
  return song ? `${entry.name} · ${songTitle(song.key)}` : entry.name;
});

const nextLabel = computed(() => {
  const next = nextProgram.value;
  if (!next) return '暂无节目';
  return `${next.tomorrow ? '明天 ' : ''}${next.entry.start} ${next.entry.name}`;
});

function songTitle(key: string): string {
  return songByKey(key)?.title ?? '曲目已移除';
}

function songCategoryLabel(key: string): string {
  const song = songByKey(key);
  if (!song) return '不在曲库';
  return categoryById(song.category)?.nameZh ?? song.category;
}

function programHeadline(entry: ScheduleEntry): string {
  if (entry.songs.length === 0) return '未添加歌曲';
  return `${songTitle(entry.songs[0]!.key)} 等 ${entry.songs.length} 首`;
}

function draftRange(): { start: number; end: number } | null {
  const start = minutesOf(draft.start);
  const end = minutesOf(draft.end);
  if (start === null || end === null || end <= start) return null;
  return { start, end };
}

function startCreate(): void {
  editingId.value = null;
  Object.assign(draft, emptyDraft());
  editing.value = true;
}

function startEdit(entry: ScheduleEntry): void {
  editingId.value = entry.id;
  Object.assign(draft, draftFromEntry(entry));
  editing.value = true;
  if (expandedIds.value.has(entry.id)) {
    const next = new Set(expandedIds.value);
    next.delete(entry.id);
    expandedIds.value = next;
  }
}

function isEditing(entry: ScheduleEntry): boolean {
  return editing.value && editingId.value === entry.id;
}

async function toggleEdit(entry: ScheduleEntry): Promise<void> {
  if (isEditing(entry)) {
    cancelEdit();
    return;
  }
  startEdit(entry);
  await nextTick();
  const page = document.querySelector<HTMLElement>('.schedule-page');
  const row = document.querySelector<HTMLElement>('.schedule-row.is-editing');
  const header = document.querySelector<HTMLElement>('.schedule-head');
  if (!page || !row) return;
  const target =
    row.getBoundingClientRect().top -
    page.getBoundingClientRect().top +
    page.scrollTop -
    (header?.offsetHeight ?? 0) -
    12;
  page.scrollTo({ top: Math.max(0, target), behavior: 'smooth' });
}

function cancelEdit(): void {
  editing.value = false;
  editingId.value = null;
}

function isExpanded(id: string): boolean {
  return expandedIds.value.has(id);
}

function toggleExpanded(id: string): void {
  const next = new Set(expandedIds.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expandedIds.value = next;
}

/** 左侧箭头与节目信息区都用于浏览节目歌曲；编辑中点击会先收起编辑面板。 */
function toggleBrowse(entry: ScheduleEntry): void {
  if (isEditing(entry)) cancelEdit();
  toggleExpanded(entry.id);
}

function overlaps(range: { start: number; end: number }, entry: ScheduleEntry): boolean {
  const start = minutesOf(entry.start);
  const end = minutesOf(entry.end);
  if (start === null || end === null) return false;
  return range.start < end && start < range.end;
}

async function saveDraft(): Promise<void> {
  const name = draft.name.trim();
  if (!name) {
    void showModal({ title: '无法保存', description: '节目名称不能为空。', confirmText: '知道了' });
    return;
  }
  const range = draftRange();
  if (!range) {
    void showModal({
      title: '时间范围无效',
      description: '结束时间必须晚于开始时间，且不能跨天。',
      confirmText: '知道了',
    });
    return;
  }
  if (draft.songs.length === 0) {
    void showModal({
      title: '还未添加歌曲',
      description: '请先通过「添加歌曲」从歌单中选择歌曲。',
      confirmText: '知道了',
    });
    return;
  }
  for (const song of draft.songs) {
    const start = minutesOf(song.start);
    const end = minutesOf(song.end);
    if (start === null || end === null || start >= end || start < range.start || end > range.end) {
      void showModal({
        title: '歌曲时间无效',
        description: `《${songTitle(song.key)}》的起止时间必须落在 ${draft.start}–${draft.end} 内，且结束晚于开始。`,
        confirmText: '知道了',
      });
      return;
    }
  }
  const overlap = state.schedule.find(
    (item) => item.id !== editingId.value && overlaps(range, item),
  );
  if (overlap) {
    void showModal({
      title: '时间范围重叠',
      description: `与节目「${overlap.name}」（${overlap.start}–${overlap.end}）时间重叠，请调整后再保存。`,
      confirmText: '知道了',
    });
    return;
  }

  const entry: ScheduleEntry = {
    id: editingId.value ?? `slot-${Date.now().toString(36)}`,
    name,
    start: draft.start,
    end: draft.end,
    songs: draft.songs.map((song) => ({ ...song })),
    volume: draft.volumeEnabled ? Math.round(draft.volume * 100) / 100 : null,
    enabled: draft.enabled,
  };

  const next = editingId.value
    ? state.schedule.map((item) => (item.id === editingId.value ? entry : item))
    : [...state.schedule, entry];

  saving.value = true;
  const ok = await persistSchedule(next);
  saving.value = false;
  if (ok) cancelEdit();
}

async function toggleEntry(entry: ScheduleEntry): Promise<void> {
  const next = state.schedule.map((item) =>
    item.id === entry.id ? { ...item, enabled: !item.enabled } : item,
  );
  await persistSchedule(next);
}

async function removeEntry(entry: ScheduleEntry): Promise<void> {
  const confirmed = await showModal({
    title: '删除节目',
    description: `确定删除「${entry.name}」（${entry.start}–${entry.end}）吗？不会影响曲库中的音频文件。`,
    confirmText: '删除',
    danger: true,
  });
  if (!confirmed) return;
  await persistSchedule(state.schedule.filter((item) => item.id !== entry.id));
}

function applyNow(entry: ScheduleEntry): void {
  applyScheduleEntry(entry);
}
</script>

<template>
  <section class="schedule-page">
    <header class="schedule-head">
      <div class="schedule-copy">
        <span class="schedule-kicker">
          <Icon name="clock" /> SCENE SCHEDULE
        </span>
        <h1 class="schedule-title">节目单 · 场景排程</h1>
        <p class="schedule-desc">
          每天按时间段自动切换节目歌曲与音量。每个节目需设置开始与结束时间，节目之间严禁重叠；
          歌曲按顺序播放，也可以为每首歌曲指定节目范围内的起止时间。
        </p>
      </div>
      <div class="schedule-clock">
        <span class="clock-time">{{ nowLabel }}</span>
        <span class="clock-current">当前：{{ currentLabel }}</span>
        <span class="clock-next">下一档：{{ nextLabel }}</span>
      </div>
    </header>

    <div class="schedule-toolbar">
      <button type="button" class="schedule-primary" @click="startCreate">
        <Icon name="plus" /> 新增节目
      </button>
      <span class="schedule-hint">
        每天执行 · 共 {{ state.schedule.length }} 档 · 时间范围不可重叠
      </span>
    </div>

    <ScheduleEditor
      v-if="editing && editingId === null"
      :draft="draft"
    />

    <ol class="schedule-list">
      <li
        v-for="entry in sortedEntries"
        :key="entry.id"
        class="schedule-row"
        :class="{
          'is-current': entry.id === currentProgram?.id,
          'is-disabled': !entry.enabled,
          'is-editing': isEditing(entry),
        }"
      >
        <div class="schedule-row-main">
          <button
            type="button"
            class="slot-toggle"
            :class="{ 'is-open': isExpanded(entry.id) }"
            :title="isExpanded(entry.id) ? '收起节目歌曲' : '展开节目歌曲'"
            :aria-expanded="isExpanded(entry.id)"
            @click="toggleBrowse(entry)"
          >
            <span class="collapse-caret" :class="{ 'is-open': isExpanded(entry.id) }">
              <Icon name="chevron-down" />
            </span>
          </button>
          <div class="slot-time">
            <span class="slot-start">{{ entry.start }}</span>
            <span class="slot-dash">–</span>
            <span class="slot-end">{{ entry.end }}</span>
          </div>
          <div
            class="slot-body"
            role="button"
            tabindex="0"
            title="点击查看节目歌曲"
            @click="toggleBrowse(entry)"
            @keydown.enter="toggleBrowse(entry)"
            @keydown.space.prevent="toggleBrowse(entry)"
          >
            <div class="slot-title">
              {{ entry.name }}
              <span v-if="entry.id === currentProgram?.id" class="slot-live">进行中</span>
              <span v-if="!entry.enabled" class="slot-off">已停用</span>
            </div>
            <div class="slot-meta">
              <span>{{ programHeadline(entry) }}</span>
              <i class="slot-dot"></i>
              <span>
                {{
                  entry.volume === null
                    ? '保持当前音量'
                    : `音量 ${Math.round(entry.volume * 100)}%`
                }}
              </span>
            </div>
          </div>
          <div class="slot-actions">
            <button
              type="button"
              :class="{ 'is-active': isEditing(entry) }"
              :title="isEditing(entry) ? '收起编辑面板' : '编辑节目'"
              @click="toggleEdit(entry)"
            >
              编辑
            </button>
            <button type="button" :disabled="!entry.enabled" @click="applyNow(entry)">应用</button>
            <button type="button" @click="toggleEntry(entry)">
              {{ entry.enabled ? '停用' : '启用' }}
            </button>
            <button type="button" class="is-danger" @click="removeEntry(entry)">删除</button>
          </div>
        </div>

        <ScheduleEditor
          v-if="isEditing(entry)"
          :draft="draft"
        />

        <div v-if="isExpanded(entry.id) && !isEditing(entry)" class="slot-songs">
          <p v-if="entry.songs.length === 0" class="slot-songs-empty">这个节目还没有歌曲。</p>
          <div
            v-for="(song, index) in entry.songs"
            :key="`${entry.id}-${song.key}`"
            class="slot-song"
            :class="{ 'is-missing': !songByKey(song.key) }"
          >
            <span class="slot-song-index">{{ index + 1 }}</span>
            <span class="slot-song-time">{{ song.start }}–{{ song.end }}</span>
            <span class="slot-song-title">{{ songTitle(song.key) }}</span>
            <span class="slot-song-cat">{{ songCategoryLabel(song.key) }}</span>
          </div>
        </div>
      </li>
      <li v-if="state.schedule.length === 0" class="schedule-empty">
        还没有节目。点「新增节目」，把一天的时间轴排起来。
      </li>
    </ol>

    <div v-if="editing" class="editor-footer is-sticky">
      <span class="editor-footer-hint">
        {{ draft.name.trim() || '新节目' }} · {{ draft.start }}–{{ draft.end }}
      </span>
      <button type="button" class="schedule-ghost" @click="cancelEdit">取消</button>
      <button type="button" class="schedule-primary" :disabled="saving" @click="saveDraft">
        {{ saving ? '保存中…' : '保存节目' }}
      </button>
    </div>
  </section>
</template>