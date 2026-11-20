import { PLAY_MODES } from '@/lib/constants';
import {
  openContextMenu,
  separator,
  type ContextMenuEntry,
  type MenuPoint,
} from '@/lib/context-menu';
import type { CategorySummary, Song } from '@/lib/types';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import {
  deleteSongs,
  exportSelection,
  openCategoriesFile,
  openConfigFile,
  openCreateCategory,
  openEditCategory,
  openMove,
  openRenameTrack,
} from '@/stores/actions';
import { clearSelection, selectedKeys, setSelected } from '@/stores/library';
import { clearLoopPoint, loopPointFor, openLoopEditor } from '@/stores/loops';
import { exportSong, playSong, playSongWithMode } from '@/stores/player';
import { setFavorites } from '@/stores/prefs';
import { state, visibleSongs } from '@/stores/state';

export function openCategoryMenu(point: MenuPoint, category: CategorySummary | null): void {
  const items: ContextMenuEntry[] = [];
  if (category) {
    items.push({
      icon: 'edit',
      label: '编辑分类',
      action: () => void openEditCategory(category),
    });
  }
  items.push(
    { icon: 'plus', label: '新增分类', action: () => void openCreateCategory() },
    separator(),
    {
      icon: 'folder',
      label: '打开分类配置文件',
      action: () => void openCategoriesFile(),
    },
    {
      icon: 'edit',
      label: '打开应用配置文件',
      action: () => void openConfigFile(),
    },
  );
  openContextMenu(point, items);
}

export function openSongMenu(
  point: MenuPoint,
  song: Song,
  selectOnOpen = true,
): void {
  if (selectOnOpen && !state.selected.has(song.key)) {
    clearSelection();
    setSelected(song.key, true);
  }
  const keys = state.selected.has(song.key) ? selectedKeys() : [song.key];
  const multiple = keys.length > 1;
  const allFavorite = keys.every((key) => state.favorites.has(key));
  const list = visibleSongs.value;

  const items: ContextMenuEntry[] = [
    {
      icon: 'play',
      label: '播放',
      action: () => {
        if (!guardLibraryPlayback()) playSong(song, list, true);
      },
    },
    {
      icon: 'repeat',
      label: '顺序播放',
      action: () => {
        if (!guardLibraryPlayback()) playSongWithMode(song, list, PLAY_MODES.sequence);
      },
    },
    {
      icon: 'shuffle',
      label: '随机播放',
      action: () => {
        if (!guardLibraryPlayback()) playSongWithMode(song, list, PLAY_MODES.shuffle);
      },
    },
    separator(),
    {
      icon: 'heart',
      label: allFavorite ? '取消收藏' : '收藏',
      action: () => setFavorites(keys, !allFavorite),
    },
  ];

  if (!multiple) {
    items.push({
      icon: 'edit',
      label: '重命名文件',
      action: () => void openRenameTrack(song),
    });
    const loop = loopPointFor(song.key);
    items.push({
      icon: 'repeat-1',
      label: loop ? '编辑循环点…' : '设置循环点…',
      action: () => void openLoopEditor(song),
    });
    if (loop) {
      items.push({
        icon: 'x',
        label: '清除循环点',
        action: () => void clearLoopPoint(song),
      });
    }
  }

  items.push(
    {
      icon: 'folder',
      label: multiple ? '批量移动分类' : '修改分类',
      action: () => void openMove(keys, multiple ? '批量移动分类' : '修改分类'),
    },
    {
      icon: 'download',
      label: multiple ? `导出所选 (${keys.length})` : '导出音频',
      action: () =>
        multiple ? void exportSelection(keys) : void exportSong(song.key),
    },
    separator(),
    {
      icon: 'trash',
      label: multiple ? `删除所选 (${keys.length})` : '删除',
      danger: true,
      action: () => void deleteSongs(keys),
    },
  );

  openContextMenu(point, items);
}

