<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { currentProgram, nextProgram, schedulerNow } from '@/composables/useScheduler';
import { ALL_CATEGORY } from '@/lib/constants';
import { showModal } from '@/lib/modal';
import type { ScheduleEntry } from '@/lib/types';
import { applyScheduleEntry, persistSchedule } from '@/stores/schedule';
import { categoryById, state } from '@/stores/state';
import Icon from './Icon.vue';

interface Draft {
  name: string;
  start: string;
  days: number[];
  category: string;
  volumeEnabled: boolean;
  volume: number;
  enabled: boolean;
}

const DAYS = [
  { value: 1, label: '一' },
  { value: 2, label: '二' },
  { value: 3, label: '三' },
  { value: 4, label: '四' },
  { value: 5, label: '五' },
  { value: 6, label: '六' },
  { value: 7, label: '日' },
];

const editing = ref(false);
const saving = ref(false);
const editingId = ref<string | null>(null);
const draft = reactive<Draft>({
  name: '',
  start: '09:00',
  days: [1, 2, 3, 4, 5, 6, 7],
  category: ALL_CATEGORY,
  volumeEnabled: false,
  volume: 0.5,
  enabled: true,
});

const sortedEntries = computed(() =>
  [...state.schedule].sort((a, b) => a.start.localeCompare(b.start)),
);

const nowLabel = computed(() => {
  const date = schedulerNow.value;
  return [date.getHours(), date.getMinutes(), date.getSeconds()]
    .map((value) => String(value).padStart(2, '0'))
    .join(':');
});

function pad(value: number): string {
  return String(value).padStart(2, '0');
}

function describeDays(days: number[]): string {
  if (days.length === 7) return '每天';
  if (days.length === 5 && days.every((day) => day <= 5)) return '工作日';
  if (days.length === 2 && days.includes(6) && days.includes(7)) return '周末';
  return days.map((day) => `周${DAYS[day - 1]?.label ?? ''}`).join('、');
}

function categoryLabel(category: string | null): string {
  if (!category) return '全部曲目';
  const found = categoryById(category);
  return found ? found.nameZh : category;
}

function categoryOptions(): { id: string; label: string }[] {
  return state.library.categories.map((category) => ({
    id: category.id,
    label: category.nameZh,
  }));
}

function nextHourStart(): string {
  const date = new Date();
  date.setHours(date.getHours() + 1, 0, 0, 0);
  return `${pad(date.getHours())}:00`;
}

function resetDraft(): void {
  draft.name = '';
  draft.start = nextHourStart();
  draft.days = [1, 2, 3, 4, 5, 6, 7];
  draft.category = ALL_CATEGORY;
  draft.volumeEnabled = false;
  draft.volume = 0.5;
  draft.enabled = true;
}

function startCreate(): void {
  editingId.value = null;
  resetDraft();
  editing.value = true;
}

function startEdit(entry: ScheduleEntry): void {
  editingId.value = entry.id;
  draft.name = entry.name;
  draft.start = entry.start;
  draft.days = [...entry.days];
  draft.category = entry.category ?? ALL_CATEGORY;
  draft.volumeEnabled = entry.volume !== null;
  draft.volume = entry.volume ?? 0.5;
  draft.enabled = entry.enabled;
  editing.value = true;
}

function cancelEdit(): void {
  editing.value = false;
  editingId.value = null;
}

function applyPreset(preset: 'daily' | 'workdays' | 'weekend'): void {
  if (preset === 'daily') draft.days = [1, 2, 3, 4, 5, 6, 7];
  else if (preset === 'workdays') draft.days = [1, 2, 3, 4, 5];
  else draft.days = [6, 7];
}

function toggleDay(day: number): void {
  if (draft.days.includes(day)) {
    if (draft.days.length === 1) return;
    draft.days = draft.days.filter((value) => value !== day);
  } else {
    draft.days = [...draft.days, day].sort((a, b) => a - b);
  }
}

async function saveDraft(): Promise<void> {
  const name = draft.name.trim();
  if (!name) {
    void showModal({ title: '无法保存', description: '节目名称不能为空。', confirmText: '知道了' });
    return;
  }
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(draft.start)) return;
  if (draft.days.length === 0) return;

  const entry: ScheduleEntry = {
    id: editingId.value ?? `slot-${Date.now().toString(36)}`,
    name,
    start: draft.start,
    days: [...draft.days].sort((a, b) => a - b),
    category: draft.category === ALL_CATEGORY ? null : draft.category,
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
    description: `确定删除「${entry.name}」（${entry.start}）吗？不会影响曲库中的音频文件。`,
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
          按系统时间自动切换分类歌单与音量，例如 9:00 轻快、12:00 中等能量、20:00 舒缓。
          支持每天或按周循环；应用保持运行即可，到点自动生效。
        </p>
      </div>
      <div class="schedule-clock">
        <span class="clock-time">{{ nowLabel }}</span>
        <span class="clock-current">
          当前：{{ currentProgram ? currentProgram.name : '未编排' }}
        </span>
        <span class="clock-next">
          下一档：{{ nextProgram ? `${nextProgram.start} ${nextProgram.name}` : '今天已无节目' }}
        </span>
      </div>
    </header>

    <div class="schedule-toolbar">
      <button type="button" class="schedule-primary" @click="startCreate">
        <Icon name="plus" /> 新增节目
      </button>
      <span class="schedule-hint">
        共 {{ state.schedule.length }} 个节目 · 同一分钟只触发一次
      </span>
    </div>

    <div v-if="editing" class="schedule-editor">
      <div class="editor-grid">
        <label class="editor-field">
          <span>节目名称</span>
          <input v-model="draft.name" type="text" placeholder="例如：午后轻快" maxlength="40" />
        </label>
        <label class="editor-field">
          <span>开始时间</span>
          <input v-model="draft.start" type="time" step="60" />
        </label>
        <label class="editor-field">
          <span>目标歌单</span>
          <select v-model="draft.category">
            <option :value="ALL_CATEGORY">全部曲目</option>
            <option v-for="option in categoryOptions()" :key="option.id" :value="option.id">
              {{ option.label }}
            </option>
          </select>
        </label>
        <label class="editor-check">
          <input v-model="draft.enabled" type="checkbox" />
          <span>启用这个节目</span>
        </label>
      </div>

      <div class="editor-row">
        <span class="editor-label">重复</span>
        <div class="preset-group">
          <button type="button" @click="applyPreset('daily')">每天</button>
          <button type="button" @click="applyPreset('workdays')">工作日</button>
          <button type="button" @click="applyPreset('weekend')">周末</button>
        </div>
        <div class="day-group">
          <button
            v-for="day in DAYS"
            :key="day.value"
            type="button"
            class="day-chip"
            :class="{ 'is-on': draft.days.includes(day.value) }"
            @click="toggleDay(day.value)"
          >
            {{ day.label }}
          </button>
        </div>
      </div>

      <div class="editor-row">
        <label class="editor-check">
          <input v-model="draft.volumeEnabled" type="checkbox" />
          <span>到点调整音量</span>
        </label>
        <input
          class="schedule-range"
          type="range"
          min="0"
          max="1"
          step="0.01"
          v-model.number="draft.volume"
          :disabled="!draft.volumeEnabled"
        />
        <span class="editor-volume">{{ Math.round(draft.volume * 100) }}%</span>
      </div>

      <div class="editor-footer">
        <button type="button" class="schedule-ghost" @click="cancelEdit">取消</button>
        <button type="button" class="schedule-primary" :disabled="saving" @click="saveDraft">
          {{ saving ? '保存中…' : '保存节目' }}
        </button>
      </div>
    </div>

    <ol class="schedule-list">
      <li
        v-for="entry in sortedEntries"
        :key="entry.id"
        class="schedule-row"
        :class="{
          'is-current': entry.id === currentProgram?.id,
          'is-disabled': !entry.enabled,
        }"
      >
        <div class="slot-time">{{ entry.start }}</div>
        <div class="slot-body">
          <div class="slot-title">
            {{ entry.name }}
            <span v-if="entry.id === currentProgram?.id" class="slot-live">进行中</span>
            <span v-if="!entry.enabled" class="slot-off">已停用</span>
          </div>
          <div class="slot-meta">
            <span>{{ describeDays(entry.days) }}</span>
            <i class="slot-dot"></i>
            <span>{{ categoryLabel(entry.category) }}</span>
            <i class="slot-dot"></i>
            <span>{{ entry.volume === null ? '保持当前音量' : `音量 ${Math.round(entry.volume * 100)}%` }}</span>
          </div>
        </div>
        <div class="slot-actions">
          <button type="button" :disabled="!entry.enabled" @click="applyNow(entry)">应用</button>
          <button type="button" @click="toggleEntry(entry)">{{ entry.enabled ? '停用' : '启用' }}</button>
          <button type="button" @click="startEdit(entry)">编辑</button>
          <button type="button" class="is-danger" @click="removeEntry(entry)">删除</button>
        </div>
      </li>
      <li v-if="state.schedule.length === 0" class="schedule-empty">
        还没有节目。点「新增节目」，把一天的时间轴排起来。
      </li>
    </ol>
  </section>
</template>