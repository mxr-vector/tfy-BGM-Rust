/** 全局键盘快捷键：空格播放/暂停、方向键跳转与音量、M 静音、Esc 逐层退出、/ 聚焦搜索。 */

import { playerConfig } from '@/lib/config';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { clearSelection, setSearch } from '@/stores/library';
import {
  adjustVolume,
  closeDrawer,
  drawerOpen,
  playSong,
  seekBy,
  toggleMute,
  togglePlay,
} from '@/stores/player';
import { closeSongDetails, songDetailsOpen } from '@/stores/song-details';
import { state, visibleSongs } from '@/stores/state';

function isTypingTarget(target: EventTarget | null): boolean {
  return (
    target instanceof HTMLElement &&
    (['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.isContentEditable)
  );
}

function focusSearch(): void {
  const input = document.querySelector<HTMLInputElement>('#search-input');
  if (!input) return;
  input.focus();
  // 用 / 聚焦时直接选中已有查询，方便整段替换。
  input.select();
}

function handleGlobalKeydown(event: KeyboardEvent): void {
  const target = event.target;

  if (event.key === '/' && !isTypingTarget(target)) {
    if (songDetailsOpen.value) return;
    event.preventDefault();
    focusSearch();
    return;
  }

  if (isTypingTarget(target)) return;

  if (
    event.code === 'Space' &&
    target instanceof HTMLElement &&
    target.closest('button, a, [role="button"]')
  ) {
    return;
  }

  switch (event.code) {
    case 'Space':
      event.preventDefault();
      if (!state.currentSong) {
        if (guardLibraryPlayback()) break;
        const list = visibleSongs.value;
        if (list.length > 0) playSong(list[0]!, list, true);
      } else {
        togglePlay();
      }
      break;
    case 'ArrowLeft':
      event.preventDefault();
      seekBy(-playerConfig().seekStepSeconds);
      break;
    case 'ArrowRight':
      event.preventDefault();
      seekBy(playerConfig().seekStepSeconds);
      break;
    case 'ArrowUp':
      event.preventDefault();
      adjustVolume(playerConfig().volumeStep);
      break;
    case 'ArrowDown':
      event.preventDefault();
      adjustVolume(-playerConfig().volumeStep);
      break;
    case 'KeyM':
      event.preventDefault();
      toggleMute();
      break;
    case 'Escape':
      if (drawerOpen.value) {
        event.preventDefault();
        closeDrawer();
      } else if (songDetailsOpen.value) {
        event.preventDefault();
        closeSongDetails();
      } else if (state.selected.size > 0) {
        event.preventDefault();
        clearSelection();
      } else if (state.search) {
        event.preventDefault();
        setSearch('');
      }
      break;
    default:
      break;
  }
}

/** 注册全局快捷键，返回卸载函数。 */
export function installGlobalHotkeys(): () => void {
  window.addEventListener('keydown', handleGlobalKeydown);
  return () => window.removeEventListener('keydown', handleGlobalKeydown);
}