<script setup lang="ts">
/**
 * 左侧「字幕生成」页面：搜索曲目 → 加入记录列表 → 试听 / 归一化 → ASR → 预览 → 导出。
 *
 * 音乐选择区域不加载完整曲库：搜索框仅在输入关键词时给出结果；
 * 真正进入生成流程的曲目都来自「字幕生成记录列表」。
 */
import { computed } from 'vue';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { formatTime } from '@/lib/format';
import { showModal } from '@/lib/modal';
import type { Song } from '@/lib/types';
import { loadStatus, modelsInfo } from '@/stores/models';
import { playSong, togglePlay } from '@/stores/player';
import { openSettings } from '@/stores/settings';
import { state } from '@/stores/state';
import {
  addSubtitleRecord,
  clearSubtitleResult,
  deleteSubtitleRecord,
  deletingSubtitle,
  error,
  exportSubtitle,
  exportingFormat,
  generateSubtitles,
  generating,
  hasExportable,
  modelReady,
  previewFormat,
  previewText,
  progress,
  removeSubtitleRecord,
  result,
  savedSubtitles,
  searchResults,
  selectSong,
  selectedKey,
  selectedSong,
  setPreviewFormat,
  subtitleQuery,
  subtitleRecords,
} from '@/stores/subtitles';
import Icon from './Icon.vue';

const modelHint = computed(() => {
  if (modelReady.value) {
    return `已加载 ${modelsInfo.value?.loaded?.name ?? 'ASR 模型'}（sherpa-onnx）`;
  }
  if (loadStatus.value.phase === 'loading') return 'ASR 模型正在加载，请稍候…';
  if (loadStatus.value.phase === 'failed') {
    return `ASR 模型加载失败：${loadStatus.value.error ?? '未知原因'}`;
  }
  return '尚未加载 ASR 模型；下载完成后会自动加载，也可在「设置 → 模型」中手动加载。';
});

const stageLabel = computed(() => {
  const stage = progress.value?.stage;
  switch (stage) {
    case 'normalizing':
      return '音频归一化';
    case 'transcribing':
      return 'ASR 识别';
    case 'done':
      return '生成完成';
    case 'error':
      return '生成失败';
    default:
      return '准备中';
  }
});

const progressPercent = computed(() => {
  const current = progress.value;
  if (!current || current.total <= 0) return null;
  return Math.min(100, Math.round((current.current / current.total) * 100));
});

const savedForSelection = computed(() =>
  selectedKey.value ? savedSubtitles.value[selectedKey.value] ?? null : null,
);

function extensionOf(fileName: string): string {
  const dot = fileName.lastIndexOf('.');
  return dot > 0 ? fileName.slice(dot + 1).toUpperCase() : '—';
}

function durationOf(song: { durationMs?: number }): string {
  return song.durationMs ? formatTime(song.durationMs / 1000) : '--:--';
}

/** 搜索框是否有有效关键词；无关键词时不渲染任何曲库曲目。 */
const hasQuery = computed(() => subtitleQuery.value.trim().length > 0);
/** 正在试听（播放中）的曲目 key。 */
const auditioningKey = computed(() => (state.playing ? state.currentKey : null));

function isAuditioning(key: string): boolean {
  return auditioningKey.value === key;
}

/** 试听：同一曲目再次点击切换暂停 / 继续，其他曲目直接播放。 */
function audition(song: Song | null): void {
  if (!song || !song.path) return;
  if (state.currentKey === song.key) {
    togglePlay();
    return;
  }
  // 与其它播放入口一致：节目单时段内不允许手动顶掉正在播出的曲目。
  if (guardLibraryPlayback()) return;
  playSong(song, null, true, true);
}

/** 选中的曲目是否仍存在于当前曲库（可播放 / 可重新生成）。 */
const selectedInLibrary = computed(() => Boolean(selectedSong.value?.path));

function storedFor(key: string) {
  return savedSubtitles.value[key] ?? null;
}

/** 记录行右侧动作：已生成字幕 → 删除记录；待生成曲目 → 从列表移除。 */
async function removeRecord(song: Song): Promise<void> {
  const stored = storedFor(song.key);
  if (!stored) {
    removeSubtitleRecord(song.key);
    return;
  }
  const confirmed = await showModal({
    title: '删除字幕记录',
    description: `将删除「${song.title}」的 ${stored.segments.length} 条字幕，删除后不可恢复。`,
    confirmText: '删除',
    danger: true,
  });
  if (confirmed === null) return;
  await deleteSubtitleRecord(song.key);
}
</script>

<template>
  <section class="subtitle-page" aria-label="字幕生成">
    <header class="subtitle-header">
      <div>
        <h2>字幕生成</h2>
        <p>
          选择音乐库曲目，自动归一化为 WAV 后调用 ASR 生成字幕，可预览并导出 SRT / LRC。
        </p>
      </div>
      <span class="subtitle-header-badge">
        <Icon name="captions" /> ASR 字幕
      </span>
    </header>

    <div class="subtitle-grid">
      <!-- 音乐选择：仅搜索，不加载 / 渲染完整曲库 -->
      <div class="subtitle-left">
        <div class="subtitle-panel subtitle-picker">
          <div class="subtitle-panel-head">
            <h3>搜索曲目</h3>
            <span class="subtitle-muted">
              {{ hasQuery ? `${searchResults.length} 条结果` : '仅搜索，不加载曲库' }}
            </span>
          </div>
          <label class="subtitle-search">
            <Icon name="search" />
            <input
              v-model="subtitleQuery"
              type="search"
              placeholder="搜索曲名、文件名、艺人…"
              autocomplete="off"
              spellcheck="false"
              aria-label="搜索音乐库曲目"
            />
            <button
              v-if="subtitleQuery"
              type="button"
              class="subtitle-search-clear"
              title="清空搜索"
              @click="subtitleQuery = ''"
            >
              <Icon name="x" />
            </button>
          </label>

          <div class="subtitle-song-list">
            <p v-if="!hasQuery" class="subtitle-empty">
              输入曲名、文件名或艺人进行搜索；此处不展示完整音乐库。
            </p>
            <p v-else-if="searchResults.length === 0" class="subtitle-empty">
              没有匹配的曲目
            </p>
            <button
              v-for="song in searchResults"
              :key="song.key"
              type="button"
              class="subtitle-song"
              :disabled="generating"
              :title="`加入字幕生成记录：${song.title}`"
              @click="addSubtitleRecord(song)"
            >
              <span class="subtitle-song-main">
                <span class="subtitle-song-title" :title="song.title">{{ song.title }}</span>
                <span class="subtitle-song-file" :title="song.fileName">{{ song.fileName }}</span>
              </span>
              <span class="subtitle-song-ext">{{ extensionOf(song.fileName) }}</span>
              <span class="subtitle-song-time">{{ durationOf(song) }}</span>
              <span class="subtitle-song-add" title="加入字幕生成记录"><Icon name="plus" /></span>
            </button>
          </div>
        </div>

        <!-- 字幕生成记录列表 -->
        <div class="subtitle-panel subtitle-records">
          <div class="subtitle-panel-head">
            <h3>字幕生成记录列表</h3>
            <span class="subtitle-muted">{{ subtitleRecords.length }} 首</span>
          </div>
          <p v-if="subtitleRecords.length === 0" class="subtitle-empty">
            已生成的字幕会自动出现在这里；也可在音乐库中右键曲目或点击「更多」→「字幕生成」加入待生成曲目。
          </p>
          <div v-else class="subtitle-record-list">
            <div
              v-for="song in subtitleRecords"
              :key="song.key"
              class="subtitle-record"
              :class="{ 'is-selected': song.key === selectedKey }"
            >
              <button
                type="button"
                class="subtitle-record-main"
                :disabled="generating"
                :title="`选择 ${song.title}`"
                @click="selectSong(song.key)"
              >
                <span class="subtitle-record-title">{{ song.title }}</span>
                <span class="subtitle-record-meta">
                  <template v-if="storedFor(song.key)">
                    已有字幕 · {{ storedFor(song.key)!.segments.length }} 句
                  </template>
                  <template v-else>
                    {{ extensionOf(song.fileName) }} · {{ durationOf(song) }}
                  </template>
                </span>
              </button>
              <button
                type="button"
                class="subtitle-audition-btn"
                :class="{ 'is-playing': isAuditioning(song.key) }"
                :disabled="!song.path"
                :title="!song.path ? '曲目已不在曲库，无法试听' : (isAuditioning(song.key) ? '暂停试听' : '试听')"
                :aria-label="`${isAuditioning(song.key) ? '暂停试听' : '试听'} ${song.title}`"
                @click="audition(song)"
              >
                <Icon :name="isAuditioning(song.key) ? 'pause-fill' : 'play'" />
              </button>
              <button
                v-if="storedFor(song.key)"
                type="button"
                class="subtitle-audition-btn subtitle-record-remove"
                :disabled="generating || deletingSubtitle === song.key"
                :title="`删除字幕记录：${song.title}`"
                :aria-label="`删除 ${song.title} 的字幕记录`"
                @click="removeRecord(song)"
              >
                <Icon :name="deletingSubtitle === song.key ? 'refresh' : 'trash'" />
              </button>
              <button
                v-else
                type="button"
                class="subtitle-audition-btn subtitle-record-remove"
                :disabled="generating"
                :title="`从列表移除：${song.title}`"
                :aria-label="`从字幕生成记录列表移除 ${song.title}`"
                @click="removeRecord(song)"
              >
                <Icon name="x" />
              </button>
            </div>
          </div>
        </div>
      </div>

      <!-- 生成与结果 -->
      <div class="subtitle-panel subtitle-work">
        <div class="subtitle-panel-head">
          <h3>生成与导出</h3>
          <span class="subtitle-muted" :class="{ 'is-ready': modelReady }">
            {{ modelHint }}
          </span>
        </div>

        <div v-if="!modelReady" class="subtitle-notice is-warn">
          <Icon name="alert" />
          <span>{{ modelHint }}</span>
          <button type="button" class="subtitle-btn is-ghost" @click="openSettings('models')">
            打开模型设置
          </button>
        </div>

        <div v-if="selectedSong" class="subtitle-track">
          <div class="subtitle-track-title">
            <Icon name="music" />
            <strong :title="selectedSong.title">{{ selectedSong.title }}</strong>
            <span class="subtitle-badge">{{ extensionOf(selectedSong.fileName) }}</span>
          </div>
          <p class="subtitle-track-path" :title="selectedSong.path">
            {{ selectedInLibrary ? selectedSong.path : '该曲目已不在当前曲库，仅保留已生成的字幕记录' }}
          </p>
          <p class="subtitle-track-meta">
            <template v-if="selectedInLibrary">
              分类 {{ selectedSong.category }} · 时长 {{ durationOf(selectedSong) }}
            </template>
            <template v-else>
              字幕 {{ savedForSelection?.segments.length ?? 0 }} 条 · 可预览 / 导出 / 删除
            </template>
            <template v-if="selectedInLibrary && savedForSelection && !result">
              · 已有 {{ savedForSelection.segments.length }} 条字幕，可直接导出或重新生成
            </template>
          </p>
          <div v-if="selectedInLibrary" class="subtitle-audition-bar">
            <button
              type="button"
              class="subtitle-btn"
              :title="isAuditioning(selectedSong.key) ? '暂停试听' : '试听当前曲目'"
              @click="audition(selectedSong)"
            >
              <Icon :name="isAuditioning(selectedSong.key) ? 'pause-fill' : 'play'" />
              {{ isAuditioning(selectedSong.key) ? '暂停试听' : '试听这首' }}
            </button>
            <span class="subtitle-audition-status">
              {{ state.currentKey === selectedSong.key
                ? (state.playing ? '正在试听' : '试听已暂停')
                : '点击试听当前曲目' }}
            </span>
          </div>
        </div>
        <div v-else class="subtitle-placeholder">
          <Icon name="captions" />
          <span>请从左侧选择一首曲目</span>
        </div>

        <div class="subtitle-actions">
          <button
            type="button"
            class="subtitle-btn is-primary"
            :disabled="!selectedSong || !selectedInLibrary || !modelReady || generating"
            @click="generateSubtitles"
          >
            <Icon :name="generating ? 'refresh' : 'sparkles'" />
            {{ generating ? '生成中…' : '生成字幕' }}
          </button>
          <button
            type="button"
            class="subtitle-btn"
            :disabled="generating || (!result && !error && !progress)"
            @click="clearSubtitleResult"
          >
            清空结果
          </button>
        </div>

        <!-- 进度 / 状态 -->
        <div v-if="progress" class="subtitle-progress" :class="`is-${progress.stage}`">
          <div class="subtitle-progress-head">
            <strong>{{ stageLabel }}</strong>
            <span>{{ progress.message }}</span>
            <span v-if="progressPercent !== null" class="subtitle-progress-percent">
              {{ progressPercent }}%
            </span>
          </div>
          <div class="subtitle-progress-bar">
            <span
              v-if="progressPercent !== null"
              :style="{ width: `${progressPercent}%` }"
            ></span>
            <i v-else class="subtitle-progress-indeterminate"></i>
          </div>
        </div>

        <div v-if="error" class="subtitle-notice is-error">
          <Icon name="alert" />
          <div>
            <strong>字幕生成失败</strong>
            <p>{{ error }}</p>
            <small>常见原因：未加载 ASR 模型、缺少 ffmpeg、音频格式不受支持或文件不可读。</small>
          </div>
        </div>

        <div v-if="result" class="subtitle-result">
          <div class="subtitle-result-head">
            <Icon name="check-circle" />
            <strong>生成完成</strong>
            <span>{{ result.segments.length }} 条 · {{ formatTime(result.duration) }}</span>
          </div>
          <p class="subtitle-result-meta">
            {{ result.converted ? `已由 ${result.sourceFormat.toUpperCase()} 转换为 WAV` : '源格式即 WAV' }}
            · 识别音频统一为 16 kHz 单声道 WAV（转换产生的临时文件已清理）
          </p>
          <p v-if="result.note" class="subtitle-result-note">{{ result.note }}</p>
        </div>

        <!-- 预览 -->
        <div v-if="previewText" class="subtitle-preview">
          <div class="subtitle-preview-head">
            <h4>结果预览</h4>
            <div class="subtitle-tabs" role="tablist" aria-label="字幕格式">
              <button
                type="button"
                role="tab"
                :aria-selected="previewFormat === 'srt'"
                :class="{ 'is-active': previewFormat === 'srt' }"
                @click="setPreviewFormat('srt')"
              >
                SRT
              </button>
              <button
                type="button"
                role="tab"
                :aria-selected="previewFormat === 'lrc'"
                :class="{ 'is-active': previewFormat === 'lrc' }"
                @click="setPreviewFormat('lrc')"
              >
                LRC
              </button>
            </div>
          </div>
          <pre class="subtitle-preview-text">{{ previewText }}</pre>
        </div>

        <!-- 导出 -->
        <div class="subtitle-export">
          <div class="subtitle-export-head">
            <h4>导出字幕</h4>
            <span class="subtitle-muted">默认文件名使用曲目名</span>
          </div>
          <div class="subtitle-export-actions">
            <button
              type="button"
              class="subtitle-btn"
              :disabled="!hasExportable || exportingFormat !== null"
              @click="exportSubtitle('srt')"
            >
              <Icon name="download" />
              {{ exportingFormat === 'srt' ? '导出中…' : '导出 SRT' }}
            </button>
            <button
              type="button"
              class="subtitle-btn"
              :disabled="!hasExportable || exportingFormat !== null"
              @click="exportSubtitle('lrc')"
            >
              <Icon name="download" />
              {{ exportingFormat === 'lrc' ? '导出中…' : '导出 LRC' }}
            </button>
          </div>
          <p v-if="!hasExportable" class="subtitle-hint">
            尚未生成字幕。生成成功后即可导出；文件名默认为「{{ selectedSong?.title ?? '曲目名' }}」。
          </p>
        </div>
      </div>
    </div>
  </section>
</template>

<style scoped>
.subtitle-page {
  display: flex;
  flex-direction: column;
  gap: 18px;
  padding: 22px 26px 28px;
  overflow-y: auto;
  height: 100%;
  box-sizing: border-box;
}
.subtitle-header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}
.subtitle-header h2 {
  margin: 0 0 6px;
  font-size: 22px;
  color: var(--text);
}
.subtitle-header p {
  margin: 0;
  font-size: 13px;
  color: var(--text-2);
  line-height: 1.6;
}
.subtitle-header-badge {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  flex: none;
  padding: 6px 12px;
  border: 1px solid rgba(25, 217, 139, 0.3);
  border-radius: 999px;
  color: var(--accent-bright);
  font-size: 12px;
  background: var(--accent-soft);
}
.subtitle-header-badge :deep([data-icon]) { display: inline-flex; }
.subtitle-header-badge :deep(svg) { width: 14px; height: 14px; }

.subtitle-grid {
  display: grid;
  grid-template-columns: minmax(300px, 0.85fr) minmax(380px, 1.15fr);
  gap: 16px;
  align-items: start;
}
/* 左列：搜索区 + 字幕生成记录列表，两个面板纵向排列。 */
.subtitle-left {
  display: flex;
  flex-direction: column;
  gap: 16px;
  min-width: 0;
}
.subtitle-panel {
  border: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(10, 22, 18, 0.55);
  border-radius: 14px;
  padding: 16px;
  box-sizing: border-box;
}
.subtitle-panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 12px;
}
.subtitle-panel-head h3 {
  margin: 0;
  font-size: 15px;
  color: var(--text);
}
.subtitle-muted {
  font-size: 12px;
  color: var(--text-3);
  text-align: right;
  line-height: 1.5;
}
.subtitle-muted.is-ready { color: var(--accent-bright); }

.subtitle-search {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px;
  border: 1px solid rgba(255, 255, 255, 0.1);
  border-radius: 9px;
  background: rgba(0, 0, 0, 0.18);
  color: var(--text-3);
  margin-bottom: 10px;
}
.subtitle-search :deep(svg) { width: 15px; height: 15px; flex: none; }
.subtitle-search input {
  flex: 1;
  min-width: 0;
  border: 0;
  outline: 0;
  background: transparent;
  color: var(--text);
  font-size: 13px;
}
.subtitle-search input::placeholder { color: var(--text-3); }
.subtitle-search-clear {
  border: 0;
  background: transparent;
  color: var(--text-3);
  cursor: pointer;
  display: inline-flex;
  padding: 2px;
}
.subtitle-search-clear :deep(svg) { width: 13px; height: 13px; }

.subtitle-song-list {
  max-height: 220px;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.subtitle-empty {
  margin: 24px 0;
  text-align: center;
  font-size: 13px;
  color: var(--text-3);
}
.subtitle-song {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  padding: 9px 10px;
  border: 1px solid transparent;
  border-radius: 9px;
  background: transparent;
  color: var(--text-2);
  text-align: left;
  cursor: pointer;
}
.subtitle-song:hover:not(:disabled) { background: rgba(255, 255, 255, 0.05); color: var(--text); }
.subtitle-song.is-selected {
  border-color: rgba(25, 217, 139, 0.4);
  background: var(--accent-soft);
  color: var(--text);
}
.subtitle-song:disabled { opacity: 0.55; cursor: not-allowed; }
.subtitle-song-main { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 2px; }
.subtitle-song-title, .subtitle-song-file {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.subtitle-song-title { font-size: 13px; }
.subtitle-song-file { font-size: 11px; color: var(--text-3); }
.subtitle-song-ext {
  flex: none;
  font-size: 10px;
  padding: 2px 6px;
  border-radius: 5px;
  border: 1px solid rgba(255, 255, 255, 0.14);
  color: var(--text-3);
}
.subtitle-song-time { flex: none; font-size: 11px; color: var(--text-3); }

.subtitle-notice {
  display: flex;
  align-items: flex-start;
  gap: 9px;
  padding: 10px 12px;
  border-radius: 9px;
  font-size: 12px;
  line-height: 1.55;
  margin-bottom: 12px;
}
.subtitle-notice :deep(svg) { width: 15px; height: 15px; flex: none; margin-top: 1px; }
.subtitle-notice.is-warn {
  border: 1px solid rgba(250, 204, 21, 0.32);
  background: rgba(250, 204, 21, 0.08);
  color: #fde68a;
}
.subtitle-notice.is-error {
  border: 1px solid rgba(248, 113, 113, 0.38);
  background: rgba(248, 113, 113, 0.08);
  color: #fca5a5;
}
.subtitle-notice strong { display: block; margin-bottom: 3px; }
.subtitle-notice p { margin: 0 0 4px; word-break: break-word; }
.subtitle-notice small { color: var(--text-3); }
.subtitle-notice > span { flex: 1; }
.subtitle-notice .subtitle-btn { flex: none; }

.subtitle-track {
  border: 1px solid rgba(255, 255, 255, 0.09);
  border-radius: 10px;
  padding: 11px 13px;
  margin-bottom: 12px;
}
.subtitle-track-title { display: flex; align-items: center; gap: 8px; }
.subtitle-track-title :deep(svg) { width: 15px; height: 15px; color: var(--accent-bright); }
.subtitle-track-title strong { font-size: 14px; color: var(--text); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.subtitle-badge {
  margin-left: auto;
  font-size: 10px;
  padding: 2px 7px;
  border-radius: 5px;
  background: var(--accent-soft);
  color: var(--accent-bright);
}
.subtitle-track-path, .subtitle-track-meta, .subtitle-result-meta, .subtitle-result-note, .subtitle-hint {
  margin: 6px 0 0;
  font-size: 11px;
  color: var(--text-3);
  line-height: 1.55;
  word-break: break-all;
}
.subtitle-track-path code, .subtitle-result-meta code { color: var(--text-2); }
.subtitle-placeholder {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 22px;
  border: 1px dashed rgba(255, 255, 255, 0.12);
  border-radius: 10px;
  color: var(--text-3);
  font-size: 13px;
  margin-bottom: 12px;
}
.subtitle-placeholder :deep(svg) { width: 26px; height: 26px; }

.subtitle-actions { display: flex; gap: 8px; margin-bottom: 12px; }
.subtitle-btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 8px 14px;
  border: 1px solid rgba(255, 255, 255, 0.14);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
  color: var(--text);
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}
.subtitle-btn :deep(svg) { width: 14px; height: 14px; }
.subtitle-btn:hover:not(:disabled) { border-color: rgba(25, 217, 139, 0.45); }
.subtitle-btn:disabled { opacity: 0.45; cursor: not-allowed; }
.subtitle-btn.is-primary {
  background: var(--accent);
  border-color: var(--accent);
  color: var(--accent-ink);
  font-weight: 600;
}
.subtitle-btn.is-ghost { background: transparent; }

.subtitle-progress { margin-bottom: 12px; }
.subtitle-progress-head {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text-2);
  margin-bottom: 6px;
}
.subtitle-progress-head strong { color: var(--text); }
.subtitle-progress-head > span:nth-child(2) { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.subtitle-progress-percent { color: var(--accent-bright); }
.subtitle-progress-bar {
  height: 6px;
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
  position: relative;
}
.subtitle-progress-bar > span {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.2s ease;
}
.subtitle-progress-indeterminate {
  position: absolute;
  inset: 0;
  background: linear-gradient(90deg, transparent, var(--accent), transparent);
  animation: subtitle-sweep 1.2s linear infinite;
}
@keyframes subtitle-sweep {
  from { transform: translateX(-100%); }
  to { transform: translateX(100%); }
}
.subtitle-progress.is-error .subtitle-progress-bar { background: rgba(248, 113, 113, 0.15); }

.subtitle-result {
  border: 1px solid rgba(25, 217, 139, 0.3);
  background: rgba(25, 217, 139, 0.07);
  border-radius: 10px;
  padding: 10px 12px;
  margin-bottom: 12px;
}
.subtitle-result-head { display: flex; align-items: center; gap: 7px; font-size: 13px; color: var(--text); }
.subtitle-result-head :deep(svg) { width: 15px; height: 15px; color: var(--accent-bright); }
.subtitle-result-head span { margin-left: auto; font-size: 11px; color: var(--text-3); }

.subtitle-preview { margin-bottom: 14px; }
.subtitle-preview-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 7px; }
.subtitle-preview-head h4, .subtitle-export-head h4 { margin: 0; font-size: 13px; color: var(--text); }
.subtitle-tabs { display: flex; gap: 4px; }
.subtitle-tabs button {
  border: 1px solid rgba(255, 255, 255, 0.12);
  background: transparent;
  color: var(--text-3);
  border-radius: 6px;
  padding: 3px 10px;
  font-size: 11px;
  cursor: pointer;
}
.subtitle-tabs button.is-active { color: var(--accent-bright); border-color: rgba(25, 217, 139, 0.45); background: var(--accent-soft); }
.subtitle-preview-text {
  margin: 0;
  max-height: 200px;
  overflow: auto;
  padding: 10px 12px;
  border: 1px solid rgba(255, 255, 255, 0.09);
  border-radius: 9px;
  background: rgba(0, 0, 0, 0.22);
  color: var(--text-2);
  font-size: 12px;
  line-height: 1.65;
  white-space: pre-wrap;
  word-break: break-word;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
.subtitle-export-head { display: flex; align-items: baseline; justify-content: space-between; gap: 10px; margin-bottom: 8px; }
.subtitle-export-actions { display: flex; gap: 8px; }

.subtitle-song-add {
  flex: none;
  display: inline-flex;
  color: var(--accent-bright);
}
.subtitle-song-add :deep(svg) { width: 13px; height: 13px; }

.subtitle-record-list {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 260px;
  overflow-y: auto;
}
.subtitle-record {
  display: flex;
  align-items: center;
  gap: 6px;
  border: 1px solid transparent;
  border-radius: 9px;
  padding: 6px 8px;
}
.subtitle-record:hover { background: rgba(255, 255, 255, 0.04); }
.subtitle-record.is-selected {
  border-color: rgba(25, 217, 139, 0.4);
  background: var(--accent-soft);
}
.subtitle-record-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 2px 0;
  border: 0;
  background: transparent;
  color: var(--text-2);
  text-align: left;
  cursor: pointer;
}
.subtitle-record-main:disabled { opacity: 0.6; cursor: not-allowed; }
.subtitle-record-title {
  font-size: 13px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.subtitle-record-meta { font-size: 11px; color: var(--text-3); }
.subtitle-audition-btn {
  flex: none;
  width: 28px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border: 1px solid rgba(255, 255, 255, 0.14);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.05);
  color: var(--text-2);
  cursor: pointer;
}
.subtitle-audition-btn:hover,
.subtitle-audition-btn.is-playing {
  color: var(--accent-bright);
  border-color: rgba(25, 217, 139, 0.45);
}
.subtitle-audition-btn :deep(svg) { width: 13px; height: 13px; }
.subtitle-record-remove { color: var(--text-3); }
.subtitle-record-remove:hover:not(:disabled) { color: #fca5a5; border-color: rgba(248, 113, 113, 0.45); }
.subtitle-record-remove:disabled { opacity: 0.4; cursor: not-allowed; }

.subtitle-audition-bar {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 10px;
}
.subtitle-audition-status { font-size: 11px; color: var(--text-3); }
</style>
