import { ref } from 'vue';
import { applyConfig } from '@/lib/config';
import { COMMANDS } from '@/lib/constants';
import { normalizeError } from '@/lib/format';
import { showModal, type ModalOption } from '@/lib/modal';
import { invoke } from '@/lib/tauri';
import { toast } from '@/lib/toast';
import type {
  BatchReport,
  CategoryOutcome,
  CategorySummary,
  ExportBatchResult,
  LibraryMutation,
  LibrarySnapshot,
  Song,
} from '@/lib/types';
import {
  applyMutation,
  clearSelection,
  replaceLibrary,
  setCategory,
  setSearch,
} from './library';
import { state } from './state';

/** 侧栏 / 顶栏同一时间只允许一个曲库任务，避免并发写盘。 */
export const busyAction = ref<string | null>(null);

async function runLibraryTask(name: string, task: () => Promise<void>): Promise<void> {
  if (busyAction.value) return;
  busyAction.value = name;
  try {
    await task();
  } catch (error) {
    toast(normalizeError(error), 'error');
  } finally {
    busyAction.value = null;
  }
}

function categoryOptions(): ModalOption[] {
  return state.library.categories.map((category) => ({
    value: category.id,
    label:
      category.nameEn && category.nameEn !== category.nameZh
        ? `${category.nameZh} · ${category.nameEn}`
        : category.nameZh,
  }));
}

function preferredCategory(): string {
  if (!state.favoritesOnly && state.category !== 'all') return state.category;
  return state.library.categories[0]?.id ?? '';
}

export function reportToast(report: BatchReport | null | undefined, fallback: string): void {
  if (!report) return;
  const parts: string[] = [];
  if (report.succeeded) parts.push(`${report.succeeded} 首成功`);
  if (report.skipped) parts.push(`${report.skipped} 首跳过`);
  if (report.failed) parts.push(`${report.failed} 首失败`);
  let message = parts.length > 0 ? `${fallback}：${parts.join('，')}` : fallback;
  if (report.notice) message += `。${report.notice}`;
  if (report.failed > 0 && report.failures?.length) {
    message += `。${report.failures[0]}`;
  }
  toast(message, report.failed > 0 && report.succeeded === 0 ? 'error' : 'success');
}

export async function openCreateCategory(): Promise<void> {
  const values = await showModal({
    title: '新增分类',
    description:
      '新分类会出现在分类导航中，并在曲库目录下创建同名文件夹；之后可直接向该分类导入音频。',
    fields: [
      { name: 'name', label: '分类名称', type: 'text', placeholder: '例如：旅行' },
      { name: 'id', label: '目录名（可选）', type: 'text', placeholder: '留空则使用分类名称' },
    ],
    confirmText: '创建',
    validate: (input) => (input.name?.trim() ? null : '分类名称不能为空'),
  });
  if (!values) return;

  try {
    const result = await invoke<CategoryOutcome>(COMMANDS.createCategory, {
      name: values.name,
      id: values.id?.trim() || null,
    });
    replaceLibrary(result.library);
    setCategory(result.categoryId);
    const created = result.library.categories.find((item) => item.id === result.categoryId);
    toast(`已新增分类：${created?.nameZh ?? result.categoryId}`, 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function openEditCategory(category: CategorySummary): Promise<void> {
  const values = await showModal({
    title: '编辑分类',
    description:
      '修改分类的显示名称与主题色，不会移动或重命名已导入的音频文件；目录名保持不变。',
    fields: [
      { name: 'name', label: '分类名称', type: 'text', value: category.nameZh },
      { name: 'color', label: '主题色', type: 'color', value: category.color },
    ],
    confirmText: '保存',
    validate: (input) => (input.name?.trim() ? null : '分类名称不能为空'),
  });
  if (!values) return;

  const sameName = values.name?.trim() === category.nameZh;
  const sameColor = values.color?.toLowerCase() === category.color.toLowerCase();
  if (sameName && sameColor) return;

  try {
    const result = await invoke<CategoryOutcome>(COMMANDS.updateCategory, {
      id: category.id,
      name: values.name,
      color: values.color,
    });
    replaceLibrary(result.library);
    toast('分类已更新', 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function openCategoriesFile(): Promise<void> {
  try {
    await invoke(COMMANDS.openCategoriesFile);
    toast('已在文件管理器中定位 categories.json', 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function openConfigFile(): Promise<void> {
  try {
    await invoke(COMMANDS.openConfigFile);
    toast('已在文件管理器中定位 config.json，修改后点「重新扫描」热加载', 'success');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function openRenameTrack(song: Song): Promise<void> {
  const values = await showModal({
    title: '重命名文件',
    description: '修改后会重写曲库中的 mp3 文件名，收藏与播放队列会自动跟随。',
    fields: [
      {
        name: 'title',
        label: '文件名',
        type: 'text',
        value: song.title,
        placeholder: '输入新的文件名（不含扩展名）',
      },
    ],
    confirmText: '重命名',
    validate: (input) => (input.title?.trim() ? null : '文件名不能为空'),
  });
  if (!values || values.title?.trim() === song.title) return;

  try {
    const result = await invoke<LibraryMutation>(COMMANDS.updateTrack, {
      key: song.key,
      title: values.title,
      category: song.category,
    });
    applyMutation(result);
    reportToast(result.report, '文件名已更新');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function openImport(): Promise<void> {
  await runLibraryTask('import', async () => {
    const values = await showModal({
      title: '导入音乐',
      description: '选择本地 MP3 文件，复制到曲库的指定分类目录；原文件不会被移动。',
      fields: [
        {
          name: 'category',
          label: '目标分类',
          type: 'select',
          value: preferredCategory(),
          options: categoryOptions(),
        },
      ],
      confirmText: '选择文件并导入',
    });
    if (!values) return;

    const result = await invoke<LibraryMutation | null>(COMMANDS.importTracks, {
      category: values.category,
    });
    if (!result) return;
    applyMutation(result);
    if (result.report.succeeded > 0) {
      setCategory(values.category);
      setSearch('');
    }
    reportToast(result.report, '导入完成');
  });
}



/**
 * 字幕页快速导入口播素材：音频直接进曲库，视频自动提取音轨后导入。
 * 返回本次新增的曲目，供字幕记录列表直接加入并选中。
 */
export async function openSubtitleImport(): Promise<Song[]> {
  let imported: Song[] = [];
  await runLibraryTask('subtitle-import', async () => {
    const options = categoryOptions();
    if (options.length === 0) {
      toast('请先在曲库中创建分类，再导入音频', 'error');
      return;
    }
    const values = await showModal({
      title: '导入口播音频 / 视频',
      description:
        '音频直接复制到曲库；视频会自动提取首条音轨（AAC / M4A）后导入，原文件不会被修改。导入完成后自动加入字幕生成记录列表。',
      fields: [
        {
          name: 'category',
          label: '目标分类',
          type: 'select',
          value: preferredCategory(),
          options,
        },
      ],
      confirmText: '选择文件并导入',
    });
    if (!values) return;

    const before = new Set(state.library.songs.map((song) => song.key));
    const result = await invoke<LibraryMutation | null>(COMMANDS.importSubtitleMedia, {
      category: values.category,
    });
    if (!result) return;
    applyMutation(result);
    imported = result.library.songs.filter((song) => !before.has(song.key));
    reportToast(
      result.report,
      imported.length > 0 ? `已导入 ${imported.length} 个音频` : '导入完成',
    );
  });
  return imported;
}

export async function openMove(
  keys: string[],
  title: string | null = null,
): Promise<void> {
  if (keys.length === 0) return;
  const values = await showModal({
    title: title ?? `移动 ${keys.length} 首曲目`,
    description: '选择目标分类；目标目录中已有同名文件时会自动追加序号。',
    fields: [
      {
        name: 'category',
        label: '目标分类',
        type: 'select',
        value: preferredCategory(),
        options: categoryOptions(),
      },
    ],
    confirmText: '移动',
  });
  if (!values) return;

  try {
    const result = await invoke<LibraryMutation>(COMMANDS.moveTracks, {
      keys,
      category: values.category,
    });
    applyMutation(result);
    clearSelection();
    reportToast(result.report, '移动完成');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function exportSelection(keys: string[]): Promise<void> {
  if (keys.length === 0) return;
  if (state.prefs?.loudnessNormalization) {
    toast(`正在导出 ${keys.length} 首并做响度归一化，请稍候…`, 'info');
  }
  try {
    const result = await invoke<ExportBatchResult | null>(COMMANDS.exportTracks, { keys });
    if (!result) return;
    reportToast(result.report, `导出完成 · ${result.dir}`);
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export async function deleteSongs(keys: string[]): Promise<void> {
  if (!keys || keys.length === 0) return;
  try {
    const result = await invoke<LibraryMutation | null>(COMMANDS.deleteTracks, { keys });
    if (!result) return;
    applyMutation(result);
    reportToast(result.report, '删除完成');
  } catch (error) {
    toast(normalizeError(error), 'error');
  }
}

export function chooseLibrary(): Promise<void> {
  return runLibraryTask('choose', async () => {
    const snapshot = await invoke<LibrarySnapshot | null>(COMMANDS.chooseLibraryDir);
    if (snapshot) {
      replaceLibrary(snapshot);
      toast(`已切换曲库目录：${snapshot.root}`, 'success');
    }
  });
}

export function rescanLibrary(): Promise<void> {
  return runLibraryTask('rescan', async () => {
    // 重新扫描前先热加载 config.json，文案与播放参数改动无需重启应用。
    const reload = await invoke<{ config: unknown; warnings: string[] }>(COMMANDS.reloadConfig);
    applyConfig(reload.config);
    for (const warning of reload.warnings ?? []) toast(warning, 'error');
    const snapshot = await invoke<LibrarySnapshot>(COMMANDS.rescanLibrary);
    replaceLibrary(snapshot);
    toast(`曲库已刷新：${snapshot.songs.length} 首`, 'success');
  });
}

export function resetLibraryDir(): Promise<void> {
  return runLibraryTask('reset', async () => {
    const snapshot = await invoke<LibrarySnapshot>(COMMANDS.resetLibraryDir);
    replaceLibrary(snapshot);
    toast('已恢复默认曲库', 'success');
  });
}

export function openLibraryDir(): Promise<void> {
  return runLibraryTask('open', async () => {
    await invoke(COMMANDS.openLibraryDir);
  });
}

export async function openTrackLocation(song: Song): Promise<void> {
  try {
    await invoke(COMMANDS.revealTrackInDir, { key: song.key });
  } catch (error) {
    toast(`打开文件所在位置失败：${normalizeError(error)}`, 'error');
  }
}

