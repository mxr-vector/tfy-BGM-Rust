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
import { formatSegments, parseSubtitleText, validateSegments } from '@/lib/subtitle-edit';
import { invoke, listen } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type {
  Song,
  StoredSubtitle,
  SubtitleExportResult,
  SubtitleFormat,
  SubtitleProgress,
  SubtitleResult,
  SubtitleSegment,
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

// ---------------- 字幕生成记录检索 ----------------

/** 记录列表的检索关键词；与「搜索曲目」的输入框互不干扰。 */
export const recordQuery = ref('');
/** 记录状态筛选：全部 / 已生成字幕 / 待生成。 */
export type SubtitleRecordFilter = 'all' | 'generated' | 'pending';
export const recordFilter = ref<SubtitleRecordFilter>('all');

/**
 * 检索用的下界文本：仅在记录列表或已保存字幕变化时重建，
 * 避免每次按键都为每条记录重新拼接并小写化全文。
 */
const recordHaystack = computed<Record<string, string>>(() => {
  const index: Record<string, string> = {};
  for (const song of subtitleRecords.value) {
    const stored = savedSubtitles.value[song.key] ?? null;
    index[song.key] = [
      song.title,
      song.fileName,
      song.category,
      song.artist ?? '',
      song.album ?? '',
      stored ? stored.segments.map((item) => item.text).join(' ') : '',
      stored ? `已有字幕 ${stored.segments.length} 句` : '待生成',
      stored?.updatedAtMs ? '已编辑' : '',
    ]
      .join(' ')
      .toLowerCase();
  }
  return index;
});

/**
 * 记录列表检索结果。检索范围包含曲名、文件名、分类、标签、已生成字幕的正文，
 * 以及「待生成 / 已编辑」等状态词；多个空格分隔的关键词按「全部命中」过滤。
 * 孤立记录（曲目已不在曲库）同样可被字幕内容检索到。
 */
export const filteredRecords = computed<Song[]>(() => {
  const terms = recordQuery.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
  return subtitleRecords.value.filter((song) => {
    const stored = savedSubtitles.value[song.key] ?? null;
    if (recordFilter.value === 'generated' && !stored) return false;
    if (recordFilter.value === 'pending' && stored) return false;
    if (terms.length === 0) return true;
    const haystack = recordHaystack.value[song.key] ?? '';
    return terms.every((term) => haystack.includes(term));
  });
});

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
  if (generating.value) return;
  if (editing.value) {
    // 音乐库右键菜单等入口不经过字幕页的禁用按钮，这里给出明确原因而不是静默失效。
    toast('请先保存或取消字幕编辑，再加入曲目', 'info');
    return;
  }
  if (!subtitleRecords.value.some((item) => item.key === song.key)) {
    manualRecords.value = [song, ...manualRecords.value];
  }
  selectSong(song.key);
  setView('subtitles');
}

export function selectSong(key: string): void {
  // 编辑态禁止切换记录：草稿尚未落盘，切换会丢失未保存的修改。
  if (generating.value || editing.value) return;
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
  if (generating.value || editing.value) return;
  manualRecords.value = manualRecords.value.filter((song) => song.key !== key);
  if (selectedKey.value === key && !savedSubtitles.value[key]) resetWork();
}

/**
 * 曲目重命名 / 移动后的缓存同步：后端已把字幕文件按新 key 迁移，
 * 这里同步内存记录、选中项与桌面歌词缓存，再从磁盘刷新一次。
 */
export function applySubtitleKeyChanges(remap: Map<string, string>): void {
  if (remap.size === 0) return;
  // 曲目 key 变化会让编辑草稿指向旧记录，直接结束编辑避免写回错位置。
  if (editing.value) {
    cancelEditing();
    if (!editing.value) {
      toast('曲目已变更，已退出字幕编辑，未保存的修改已丢弃', 'info');
    }
  }
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
  if (generating.value || deletingSubtitle.value || editing.value) return;
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
  if (!key || generating.value || editing.value) return;
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
  if (!key || exportingFormat.value || editing.value) return;
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
  if (editing.value) return;
  result.value = null;
  error.value = null;
  progress.value = null;
}

// ---------------- 字幕结果编辑 ----------------

export type SubtitleEditMode = 'segments' | 'text';

/** 是否处于编辑态；编辑期间禁止切换记录、生成、导出与删除，避免草稿丢失。 */
export const editing = ref(false);
export const editMode = ref<SubtitleEditMode>('segments');
export const savingSubtitle = ref(false);
/** 后端保存失败 / 模式切换被拒绝时的提示。 */
export const editError = ref<string | null>(null);
/** 分段草稿：编辑器的权威数据，文本模式的输入会实时解析回这里。 */
export const draftSegments = ref<SubtitleSegment[]>([]);
/** 原文草稿；文本模式由用户输入，分段模式由草稿派生。 */
export const draftText = ref('');

interface EditableSource {
  key: string;
  title: string;
  segments: SubtitleSegment[];
}

/** 当前选中记录可作为编辑数据源的字幕（本次生成结果优先，其次历史保存）。 */
function editableSource(): EditableSource | null {
  const key = selectedKey.value;
  if (!key) return null;
  const current = result.value;
  if (current && current.key === key) {
    return { key, title: current.title, segments: current.segments };
  }
  const stored = savedSubtitles.value[key];
  return stored ? { key, title: stored.title, segments: stored.segments } : null;
}

export const hasEditable = computed(() => Boolean(editableSource()));

function draftTitle(): string {
  return editableSource()?.title ?? selectedSong.value?.title ?? '';
}

/** LRC 末句结束时间的推导依据：优先取字幕记录时长，其次曲库时长。 */
function draftDurationSecs(): number | undefined {
  const stored = selectedKey.value ? savedSubtitles.value[selectedKey.value] : null;
  const ms = stored?.durationMs ?? selectedSong.value?.durationMs;
  return ms ? ms / 1000 : undefined;
}

/** 即时校验：分段模式校验时间，文本模式解析原文；返回第一条错误或 `null`。 */
export const editValidation = computed<string | null>(() => {
  if (!editing.value) return null;
  if (editMode.value === 'text') {
    const parsed = parseSubtitleText(draftText.value, previewFormat.value, draftDurationSecs());
    return parsed.ok ? null : parsed.error;
  }
  return validateSegments(draftSegments.value);
});

// 草稿一旦变化就清掉上一次的保存错误 / 模式切换提示，避免旧提示一直挂在编辑器里。
watch([draftText, draftSegments, editMode], () => {
  if (editError.value) editError.value = null;
}, { deep: true });

/** 进入编辑：以当前字幕内容为草稿，默认使用分段编辑。 */
export function startEditing(): void {
  if (editing.value || generating.value || savingSubtitle.value) return;
  const source = editableSource();
  if (!source) return;
  draftSegments.value = source.segments.map((item) => ({ ...item }));
  draftText.value = formatSegments(previewFormat.value, source.title, draftSegments.value);
  editError.value = null;
  editMode.value = 'segments';
  editing.value = true;
}

export function cancelEditing(): void {
  if (savingSubtitle.value) return;
  editing.value = false;
  editError.value = null;
  draftSegments.value = [];
  draftText.value = '';
}

/** 丢弃草稿，恢复为最近一次保存（或本次生成）的内容。 */
export function restoreDraft(): void {
  if (!editing.value || savingSubtitle.value) return;
  const source = editableSource();
  if (!source) return;
  draftSegments.value = source.segments.map((item) => ({ ...item }));
  draftText.value = formatSegments(previewFormat.value, source.title, draftSegments.value);
  editError.value = null;
  toast('已恢复为上次保存的字幕内容', 'info');
}

/** 切换编辑模式；切回分段时先把原文解析成草稿，解析失败则拒绝切换。 */
export function setEditMode(mode: SubtitleEditMode): void {
  if (!editing.value || savingSubtitle.value || editMode.value === mode) return;
  if (mode === 'text') {
    draftText.value = formatSegments(previewFormat.value, draftTitle(), draftSegments.value);
  } else {
    const parsed = parseSubtitleText(draftText.value, previewFormat.value, draftDurationSecs());
    if (!parsed.ok) {
      editError.value = `请先修正原文后再切换：${parsed.error}`;
      return;
    }
    draftSegments.value = parsed.segments;
  }
  editError.value = null;
  editMode.value = mode;
}

/** 切换预览 / 编辑格式；编辑态下同步草稿文本，避免两种格式互相覆盖。 */
export function changePreviewFormat(format: SubtitleFormat): void {
  if (format === previewFormat.value) return;
  if (editing.value && editMode.value === 'text') {
    const parsed = parseSubtitleText(draftText.value, previewFormat.value, draftDurationSecs());
    if (!parsed.ok) {
      editError.value = `请先修正当前 ${previewFormat.value.toUpperCase()} 原文后再切换：${parsed.error}`;
      return;
    }
    draftSegments.value = parsed.segments;
    setPreviewFormat(format);
    draftText.value = formatSegments(format, draftTitle(), draftSegments.value);
    editError.value = null;
    return;
  }
  setPreviewFormat(format);
  if (editing.value) {
    draftText.value = formatSegments(format, draftTitle(), draftSegments.value);
  }
}

/** 文本模式输入：实时把原文解析回分段草稿，保存仍以后端解析结果为准。 */
export function updateDraftText(text: string): void {
  draftText.value = text;
  if (!editing.value || editMode.value !== 'text') return;
  const parsed = parseSubtitleText(text, previewFormat.value, draftDurationSecs());
  if (parsed.ok) draftSegments.value = parsed.segments;
}

/** 在指定行之后插入一条空白分段。 */
export function insertSegment(index: number): void {
  if (!editing.value || savingSubtitle.value) return;
  const next = [...draftSegments.value];
  const previous = next[index];
  const following = next[index + 1];
  const start = previous ? previous.end : 0;
  const proposed = start + 5;
  const end =
    following && following.start > start + 0.1
      ? Math.min(proposed, following.start)
      : proposed;
  next.splice(index + 1, 0, {
    start,
    end: Math.max(end, start + 0.1),
    text: '',
  });
  draftSegments.value = next;
  draftText.value = formatSegments(previewFormat.value, draftTitle(), next);
}

export function removeSegment(index: number): void {
  if (!editing.value || savingSubtitle.value) return;
  const next = draftSegments.value.filter((_, position) => position !== index);
  draftSegments.value = next;
  draftText.value = formatSegments(previewFormat.value, draftTitle(), next);
}

/** 编辑保存成功：用后端快照刷新缓存、结果预览与桌面歌词。 */
function applyUpdatedSubtitle(updated: StoredSubtitle): void {
  const key = updated.key;
  savedSubtitles.value = { ...savedSubtitles.value, [key]: updated };
  generatedSubtitles.value = generatedSubtitles.value.map((item) =>
    item.key === key ? updated : item,
  );
  if (result.value?.key === key) {
    const last = updated.segments[updated.segments.length - 1];
    result.value = {
      ...result.value,
      segments: updated.segments,
      srt: updated.srt,
      lrc: updated.lrc,
      duration: last ? last.end : 0,
    };
  }
  // 主窗口的桌面歌词按 key 缓存了分段，失效后下一次同步会读取修改后的字幕。
  invalidateDesktopLyrics(key);
}

/** 保存编辑：分段模式提交 segments，文本模式提交 SRT / LRC 原文。 */
export async function saveSubtitleEdit(): Promise<void> {
  const key = selectedKey.value;
  if (!key || !editing.value || savingSubtitle.value) return;
  if (editValidation.value) {
    editError.value = editValidation.value;
    return;
  }
  savingSubtitle.value = true;
  editError.value = null;
  try {
    const updated =
      editMode.value === 'segments'
        ? await invoke<StoredSubtitle>(COMMANDS.updateSubtitle, {
            key,
            segments: draftSegments.value.map((item) => ({
              start: item.start,
              end: item.end,
              text: item.text,
            })),
          })
        : await invoke<StoredSubtitle>(COMMANDS.updateSubtitleText, {
            key,
            format: previewFormat.value,
            text: draftText.value,
          });
    applyUpdatedSubtitle(updated);
    editing.value = false;
    draftSegments.value = [];
    draftText.value = '';
    toast(`字幕修改已保存，共 ${updated.segments.length} 条`, 'success');
  } catch (cause) {
    editError.value = normalizeError(cause);
    toast(`保存字幕修改失败：${editError.value}`, 'error');
  } finally {
    savingSubtitle.value = false;
  }
}
