/**
 * 「字幕生成」状态：曲目搜索、生成记录列表、生成进度、结果预览与 SRT / LRC 导出。
 *
 * 页面不加载也不渲染完整音乐库：搜索框仅在输入关键词时给出匹配结果，
 * 真正进入生成流程的曲目统一保存在「字幕生成记录列表」中（由音乐库菜单入口
 * 或搜索结果加入）。后端 `generate_subtitles` 完成归一化与分片 ASR 后返回
 * SRT / LRC 文本，并把字幕持久化到应用配置目录；`export_subtitle` 直接读取
 * 该结果，因此重新进入页面后仍可导出上次生成的字幕。
 */

import { computed, ref, watch } from 'vue';
import { COMMANDS, EVENTS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type {
  Song,
  StoredSubtitle,
  SubtitleExportResult,
  SubtitleFormat,
  SubtitleProgress,
  SubtitleResult,
} from '@/lib/types';
import { invalidateDesktopLyrics } from './desktop-lyrics';
import { modelsInfo } from './models';
import { persistPrefs } from './prefs';
import { setView, state } from './state';

export const subtitleQuery = ref('');
export const selectedKey = ref<string | null>(null);
export const generating = ref(false);
export const progress = ref<SubtitleProgress | null>(null);
export const result = ref<SubtitleResult | null>(null);
export const error = ref<string | null>(null);
export const exportingFormat = ref<SubtitleFormat | null>(null);
/** 预览格式；同样写进偏好，重启后保持上次选择。 */
export const previewFormat = ref<SubtitleFormat>('srt');

watch(
  () => state.prefs?.subtitleFormat,
  (format) => {
    if (format) previewFormat.value = format;
  },
  { immediate: true },
);

export function setPreviewFormat(format: SubtitleFormat): void {
  previewFormat.value = format;
  if (!state.prefs || state.prefs.subtitleFormat === format) return;
  state.prefs = { ...state.prefs, subtitleFormat: format };
  persistPrefs(true);
}
/** key → 已保存字幕；用于进入页面后仍可预览 / 导出。 */
export const savedSubtitles = ref<Record<string, StoredSubtitle>>({});
/**
 * 后端配置目录中的全部已生成字幕，按生成时间倒序。它不依赖当前曲库：
 * 曲目被移出曲库后，只要字幕文件还在，记录表里仍然能看到并删除。
 */
export const generatedSubtitles = ref<StoredSubtitle[]>([]);
/**
 * 用户显式加入、但尚未生成字幕的曲目。已生成字幕的曲目不再保留在这里，
 * 而是由 `generatedSubtitles` 统一驱动记录表。
 */
const manualRecords = ref<Song[]>([]);

/**
 * 「字幕生成记录列表」：已生成字幕（持久化）在前，待生成曲目在后。
 * 曲库加载完成前后都能正确映射：曲库里有同名 key 时用最新 Song，
 * 没有时用字幕文件里的标题 / 文件名兜底。
 */
export const subtitleRecords = computed<Song[]>(() => {
  const byKey = new Map(state.library.songs.map((song) => [song.key, song]));
  const seen = new Set<string>();
  const records: Song[] = [];

  for (const stored of generatedSubtitles.value) {
    if (seen.has(stored.key)) continue;
    seen.add(stored.key);
    records.push(byKey.get(stored.key) ?? songFromStored(stored));
  }
  for (const song of manualRecords.value) {
    if (seen.has(song.key)) continue;
    const fresh = byKey.get(song.key);
    if (fresh) {
      seen.add(song.key);
      records.push(fresh);
    } else if (state.library.songs.length === 0) {
      // 曲库快照尚未加载时先展示用户刚加入的曲目。
      seen.add(song.key);
      records.push(song);
    }
  }
  return records;
});

/** 已生成字幕但曲目不在当前曲库时，构造一条只读展示记录。 */
function songFromStored(stored: StoredSubtitle): Song {
  const slash = stored.key.lastIndexOf('/');
  return {
    key: stored.key,
    title: stored.title,
    fileName: stored.fileName ?? (slash >= 0 ? stored.key.slice(slash + 1) : stored.key),
    category: slash >= 0 ? stored.key.slice(0, slash) : '',
    path: '',
    durationMs: stored.durationMs,
  };
}

let loaded = false;

/** 应用启动时调用：恢复后端已生成的字幕记录。 */
export async function installSubtitles(): Promise<void> {
  if (loaded) return;
  loaded = true;
  await refreshGeneratedSubtitles();
}

/** 重新读取已生成字幕列表；失败时保留现有数据，不阻塞页面。 */
export async function refreshGeneratedSubtitles(): Promise<void> {
  try {
    const list = await invoke<StoredSubtitle[]>(COMMANDS.listSubtitles);
    generatedSubtitles.value = list;
    savedSubtitles.value = Object.fromEntries(list.map((item) => [item.key, item]));
    // 选中的记录可能刚被别处删除。
    if (selectedKey.value && !list.some((item) => item.key === selectedKey.value)) {
      result.value = null;
    }
  } catch {
    // 列表读取失败时保留当前状态，生成 / 导出仍可用。
  }
}

let listening = false;

/**
 * 搜索结果：仅在输入关键词时返回匹配曲目。
 * 关键词为空时返回空数组，页面因此不会请求或渲染完整音乐库列表。
 */
export const searchResults = computed<Song[]>(() => {
  const query = subtitleQuery.value.trim().toLowerCase();
  if (!query) return [];
  return state.library.songs.filter((song) =>
    [song.title, song.fileName, song.artist, song.album, song.category]
      .filter(Boolean)
      .some((text) => String(text).toLowerCase().includes(query)),
  );
});

/** 当前选中曲目取自记录列表，避免依赖完整曲库快照。 */
export const selectedSong = computed<Song | null>(
  () => subtitleRecords.value.find((song) => song.key === selectedKey.value) ?? null,
);

export const modelReady = computed(() => Boolean(modelsInfo.value?.loaded));

/** 当前曲目是否已有可导出的字幕（本次生成或历史保存）。 */
export const hasExportable = computed(() => {
  const key = selectedKey.value;
  if (!key) return false;
  return Boolean(result.value?.key === key || savedSubtitles.value[key]);
});

export const previewText = computed(() => {
  const current = result.value;
  if (current && current.key === selectedKey.value) {
    return previewFormat.value === 'srt' ? current.srt : current.lrc;
  }
  const stored = selectedKey.value ? savedSubtitles.value[selectedKey.value] : null;
  if (!stored) return '';
  // 历史字幕直接使用后端格式化好的文本，避免前后端两套规则漂移。
  return previewFormat.value === 'srt' ? stored.srt : stored.lrc;
});

async function ensureProgressListener(): Promise<void> {
  if (listening) return;
  listening = true;
  await listen<SubtitleProgress>(EVENTS.subtitleProgress, (payload) => {
    progress.value = payload;
    if (payload.stage === 'error' && payload.error) {
      error.value = payload.error;
    }
  });
}

/** 读取曲目已保存的字幕；不存在时静默返回。 */
export async function loadSavedSubtitle(key: string): Promise<void> {
  try {
    const stored = await invoke<StoredSubtitle | null>(COMMANDS.subtitleForTrack, { key });
    if (stored) {
      savedSubtitles.value = { ...savedSubtitles.value, [key]: stored };
    }
  } catch {
    // 查询失败不阻塞选择，生成流程会给出明确错误。
  }
}

/**
 * 把曲目加入「字幕生成记录列表」并选中，然后切到字幕生成页。
 * 音乐库右键菜单 / 「更多」菜单与字幕页搜索结果共用此入口；按 key 去重，
 * 保证同一曲目不会重复出现在记录列表里。
 */
export function addSubtitleRecord(song: Song): void {
  if (!subtitleRecords.value.some((item) => item.key === song.key)) {
    manualRecords.value = [song, ...manualRecords.value];
  }
  selectSong(song.key);
  setView('subtitles');
}

export function selectSong(key: string): void {
  if (generating.value) return;
  selectedKey.value = key;
  result.value = null;
  error.value = null;
  progress.value = null;
  void loadSavedSubtitle(key);
}

function resetWork(): void {
  selectedKey.value = null;
  result.value = null;
  error.value = null;
  progress.value = null;
}

/** 从记录列表移除尚未生成的曲目；已生成字幕的记录请使用 `deleteSubtitleRecord`。 */
export function removeSubtitleRecord(key: string): void {
  if (generating.value) return;
  manualRecords.value = manualRecords.value.filter((song) => song.key !== key);
  if (selectedKey.value === key && !savedSubtitles.value[key]) resetWork();
}

/**
 * 曲目重命名 / 移动后的缓存同步：后端已把字幕文件按新 key 迁移，
 * 这里同步内存记录、选中项与桌面歌词缓存，再从磁盘刷新一次。
 */
export function applySubtitleKeyChanges(remap: Map<string, string>): void {
  if (remap.size === 0) return;
  manualRecords.value = manualRecords.value.map((song) => {
    const key = remap.get(song.key);
    return key ? { ...song, key } : song;
  });
  generatedSubtitles.value = generatedSubtitles.value.map((stored) => {
    const key = remap.get(stored.key);
    return key ? { ...stored, key } : stored;
  });
  const saved: Record<string, StoredSubtitle> = {};
  for (const [key, stored] of Object.entries(savedSubtitles.value)) {
    saved[remap.get(key) ?? key] = stored;
  }
  savedSubtitles.value = saved;
  for (const [from, to] of remap) {
    invalidateDesktopLyrics(from);
    invalidateDesktopLyrics(to);
  }
  if (selectedKey.value) {
    const key = remap.get(selectedKey.value);
    if (key) {
      selectedKey.value = key;
      result.value = null;
      error.value = null;
      progress.value = null;
      void loadSavedSubtitle(key);
    }
  }
  void refreshGeneratedSubtitles();
}

/** 正在删除的字幕 key；用于禁用重复点击。 */
export const deletingSubtitle = ref<string | null>(null);

/**
 * 删除已生成的字幕记录：同时清理后端字幕文件、前端缓存与记录表。
 * 删除失败时重新拉取列表，保证界面与磁盘状态一致。
 */
export async function deleteSubtitleRecord(key: string): Promise<void> {
  if (generating.value || deletingSubtitle.value) return;
  deletingSubtitle.value = key;
  try {
    await invoke<boolean>(COMMANDS.deleteSubtitle, { key });
    generatedSubtitles.value = generatedSubtitles.value.filter((item) => item.key !== key);
    const nextSaved = { ...savedSubtitles.value };
    delete nextSaved[key];
    savedSubtitles.value = nextSaved;
    manualRecords.value = manualRecords.value.filter((song) => song.key !== key);
    if (selectedKey.value === key) resetWork();
    invalidateDesktopLyrics(key);
    toast('已删除字幕记录', 'success');
  } catch (cause) {
    toast(`删除字幕记录失败：${normalizeError(cause)}`, 'error');
    void refreshGeneratedSubtitles();
  } finally {
    deletingSubtitle.value = null;
  }
}

/** 为选中曲目生成字幕。 */
export async function generateSubtitles(): Promise<void> {
  const key = selectedKey.value;
  if (!key || generating.value) return;
  if (!modelReady.value) {
    toast('请先完成 ASR 模型加载，再生成字幕', 'error');
    return;
  }
  await ensureProgressListener();
  generating.value = true;
  error.value = null;
  result.value = null;
  progress.value = {
    stage: 'normalizing',
    message: '准备生成字幕…',
    current: 0,
    total: 0,
    error: null,
  };
  try {
    const generated = await invoke<SubtitleResult>(COMMANDS.generateSubtitles, { key });
    result.value = generated;
    const song = selectedSong.value;
    const stored: StoredSubtitle = {
      key,
      title: generated.title,
      segments: generated.segments,
      createdAtMs: Date.now(),
      fileName: song?.fileName,
      durationMs: song?.durationMs,
      srt: generated.srt,
      lrc: generated.lrc,
    };
    savedSubtitles.value = { ...savedSubtitles.value, [key]: stored };
    generatedSubtitles.value = [
      stored,
      ...generatedSubtitles.value.filter((item) => item.key !== key),
    ];
    manualRecords.value = manualRecords.value.filter((item) => item.key !== key);
    // 后端已落盘；再拉一次列表确保时间戳 / 文件名与磁盘一致。
    void refreshGeneratedSubtitles();
    // 新字幕立即对已开启的桌面歌词生效。
    invalidateDesktopLyrics(key);
    toast(`字幕生成完成，共 ${generated.segments.length} 条`, 'success');
  } catch (cause) {
    error.value = normalizeError(cause);
    toast(`字幕生成失败：${error.value}`, 'error');
  } finally {
    generating.value = false;
  }
}

/** 导出 SRT / LRC；默认文件名由后端按曲目名生成。 */
export async function exportSubtitle(format: 'srt' | 'lrc'): Promise<void> {
  const key = selectedKey.value;
  if (!key || exportingFormat.value) return;
  exportingFormat.value = format;
  try {
    const outcome = await invoke<SubtitleExportResult | null>(COMMANDS.exportSubtitle, {
      key,
      format,
    });
    if (!outcome) return; // 用户取消保存对话框
    toast(`已导出 ${outcome.format.toUpperCase()} 字幕：${outcome.path}`, 'success');
  } catch (cause) {
    toast(`导出字幕失败：${normalizeError(cause)}`, 'error');
  } finally {
    exportingFormat.value = null;
  }
}

export function clearSubtitleResult(): void {
  result.value = null;
  error.value = null;
  progress.value = null;
}
