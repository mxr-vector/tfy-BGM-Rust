import { applyStaticIcons, setIcon } from './icons.js';
import { initLibraryUI } from './library.js';
import {
  adjustVolume,
  closeDrawer,
  hasCurrentSong,
  initPlayer,
  isDrawerOpen,
  playSong,
  seekBy,
  toggleMute,
  togglePlay,
} from './player.js';
import {
  applyBootstrap,
  clearSelection,
  on,
  setSearch,
  state,
  visibleSongs,
} from './store.js';
import { COMMANDS, EVENTS } from './constants.js';
import { applyDocumentCopy, playerConfig } from './config.js';
import { invoke, isTauri } from './tauri.js';
import { el, normalizeError, toast } from './ui.js';
import { initWindowChrome } from './window.js';

applyStaticIcons();
applyDocumentCopy();
// 先把窗口按钮接上，即使曲库加载失败也能正常最小化/关闭窗口。
initWindowChrome();

function showFatal(title, message) {
  const iconHost = el('span');
  setIcon(iconHost, 'alert');
  const retry = el('button', {
    class: 'reset-filter-btn',
    attrs: { type: 'button' },
    text: '重新加载',
  });
  retry.addEventListener('click', () => window.location.reload());
  document.body.append(
    el('div', { class: 'fatal-overlay' }, [
      iconHost,
      el('h1', { class: 'fatal-title', text: title }),
      el('p', { class: 'fatal-text', text: message }),
      retry,
    ]),
  );
}

function isTypingTarget(target) {
  return (
    target instanceof HTMLElement &&
    (['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName) || target.isContentEditable)
  );
}

function focusSearch() {
  const input = document.getElementById('search-input');
  if (!input) return;
  input.focus();
  // 用 / 聚焦时直接选中已有查询，方便整段替换。
  input.select();
}

function clearSearch() {
  const input = document.getElementById('search-input');
  if (input) input.value = '';
  setSearch('');
}

function handleGlobalKeydown(event) {
  const target = event.target;

  if (event.key === '/' && !isTypingTarget(target)) {
    event.preventDefault();
    focusSearch();
    return;
  }

  if (isTypingTarget(target)) {
    if (event.key === 'Escape' && target instanceof HTMLElement) {
      if (target.id === 'search-input') {
        if (state.search) clearSearch();
        else target.blur();
        event.preventDefault();
      }
    }
    return;
  }

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
      if (!hasCurrentSong()) {
        const list = visibleSongs();
        if (list.length > 0) playSong(list[0], list, true);
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
      if (isDrawerOpen()) {
        event.preventDefault();
        closeDrawer();
      } else if (state.selected.size > 0) {
        event.preventDefault();
        clearSelection();
      } else if (state.search) {
        event.preventDefault();
        clearSearch();
      }
      break;
    default:
      break;
  }
}

async function bootstrap() {
  if (!isTauri) {
    showFatal(
      '请通过 Tauri 启动',
      '这是桌面客户端界面，需要 Rust 侧提供曲库服务。请运行 cargo tauri dev 或安装打包后的应用。',
    );
    return;
  }

  let boot;
  try {
    boot = await invoke(COMMANDS.bootstrap);
  } catch (error) {
    showFatal('曲库加载失败', normalizeError(error));
    return;
  }

  applyBootstrap(boot);
  initPlayer();
  initLibraryUI();
  on(EVENTS.error, (message) => toast(message, 'error'));
  window.addEventListener('keydown', handleGlobalKeydown);
}

void bootstrap();