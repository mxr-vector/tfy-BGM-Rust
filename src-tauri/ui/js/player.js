import { assetUrl, invoke } from './tauri.js';
import { emit, on, persistPrefs, songByKey, state, toggleFavorite } from './store.js';
import { COMMANDS, DEFAULT_ACCENT, EVENTS, PLAY_MODES } from './constants.js';
import { playerConfig } from './config.js';
import { equalizer, setIcon } from './icons.js';
import { showModal } from './modal.js';
import { clamp, el, formatTime, normalizeError, toast, withAlpha } from './ui.js';

const audio = new Audio();
audio.preload = 'metadata';

const els = {};

let initialized = false;
let dragging = false;
let dragTime = 0;
let drawerOpen = false;
let lastError = '';
let exporting = false;
let failureStreak = 0;
let sourceToken = 0;
let sourceUrl = '';
let loadedPath = '';

const MODE_CYCLE = [PLAY_MODES.sequence, PLAY_MODES.repeatOne, PLAY_MODES.shuffle];

function $(id) {
  return document.getElementById(id);
}

export function initPlayer() {
  if (initialized) return;
  initialized = true;

  Object.assign(els, {
    player: $('player'),
    progress: $('progress'),
    buffered: $('progress-buffered'),
    played: $('progress-played'),
    thumb: $('progress-thumb'),
    tooltip: $('hover-tooltip'),
    title: $('now-title'),
    category: $('now-category'),
    sub: $('now-sub'),
    cover: $('now-cover'),
    coverIcon: $('now-cover-icon'),
    favorite: $('btn-now-favorite'),
    play: $('btn-play'),
    prev: $('btn-prev'),
    next: $('btn-next'),
    mode: $('btn-mode'),
    rate: $('btn-rate'),
    timeCurrent: $('time-current'),
    timeTotal: $('time-total'),
    mute: $('btn-mute'),
    volume: $('volume-range'),
    volumePercent: $('volume-percent'),
    download: $('btn-download'),
    queueBtn: $('btn-queue'),
    queueCount: $('queue-count'),
    errorBar: $('playback-error'),
    errorText: $('playback-error-text'),
    drawerBackdrop: $('drawer-backdrop'),
    drawerCount: $('drawer-count'),
    queueList: $('queue-list'),
    clearQueue: $('btn-clear-queue'),
    closeDrawer: $('btn-close-drawer'),
  });

  applyPrefsToAudio();
  bindAudio();
  bindControls();
  bindProgress();
  bindDrawer();

  on(EVENTS.library, () => {
    if (!state.currentSong && audio.hasAttribute('src')) {
      stopPlayback();
    } else if (state.currentSong) {
      syncAudioSource();
    }
    renderPlayer();
    renderQueueBadge();
    if (drawerOpen) renderDrawer();
  });
  on(EVENTS.favorites, renderPlayer);

  renderPlayer();
  renderQueueBadge();
}

function applyPrefsToAudio() {
  if (!state.prefs) return;
  audio.volume = state.prefs.muted ? 0 : state.prefs.volume;
  audio.playbackRate = state.prefs.playbackRate;
}

function bindAudio() {
  audio.addEventListener('loadstart', () => {
    setProgressUi();
    updateBuffered();
  });
  audio.addEventListener('loadedmetadata', setProgressUi);
  audio.addEventListener('durationchange', setProgressUi);
  audio.addEventListener('canplay', setProgressUi);
  audio.addEventListener('progress', updateBuffered);
  audio.addEventListener('timeupdate', () => {
    if (!dragging) setProgressUi();
  });

  audio.addEventListener('play', () => {
    state.playing = true;
    setError('');
    renderPlayer();
    emit(EVENTS.player);
  });
  audio.addEventListener('playing', () => {
    failureStreak = 0;
  });
  audio.addEventListener('pause', () => {
    state.playing = false;
    renderPlayer();
    emit(EVENTS.player);
  });
  audio.addEventListener('ended', handleEnded);
  audio.addEventListener('error', handleAudioError);
}

function bindControls() {
  els.play.addEventListener('click', togglePlay);
  els.prev.addEventListener('click', () => prevTrack());
  els.next.addEventListener('click', () => nextTrack());
  els.mode.addEventListener('click', cyclePlayMode);
  els.rate.addEventListener('click', cycleRate);
  els.mute.addEventListener('click', toggleMute);
  els.favorite.addEventListener('click', () => toggleCurrentFavorite());
  els.download.addEventListener('click', () => exportSong(state.currentKey));
  els.queueBtn.addEventListener('click', toggleDrawer);
  els.volume.addEventListener('input', () => setVolume(Number(els.volume.value)));
}

function bindProgress() {
  els.progress.addEventListener('pointerdown', (event) => {
    if (!Number.isFinite(audio.duration) || audio.duration <= 0) return;
    dragging = true;
    dragTime = ratioFromEvent(event) * audio.duration;
    els.progress.classList.add('is-dragging');
    els.progress.setPointerCapture(event.pointerId);
    hideTooltip();
    setProgressUi();
  });

  els.progress.addEventListener('pointermove', (event) => {
    if (!Number.isFinite(audio.duration) || audio.duration <= 0) return;
    if (dragging) {
      dragTime = ratioFromEvent(event) * audio.duration;
      setProgressUi();
    } else {
      const ratio = ratioFromEvent(event);
      els.tooltip.hidden = false;
      els.tooltip.style.left = `${ratio * 100}%`;
      els.tooltip.textContent = formatTime(ratio * audio.duration);
    }
  });

  const finishDrag = (event) => {
    if (!dragging) return;
    if (els.progress.hasPointerCapture(event.pointerId)) {
      els.progress.releasePointerCapture(event.pointerId);
    }
    dragging = false;
    els.progress.classList.remove('is-dragging');
    seek(dragTime);
  };
  els.progress.addEventListener('pointerup', finishDrag);
  els.progress.addEventListener('pointercancel', finishDrag);
  els.progress.addEventListener('pointerleave', hideTooltip);
}

function bindDrawer() {
  els.closeDrawer.addEventListener('click', closeDrawer);
  els.drawerBackdrop.addEventListener('click', (event) => {
    if (event.target === els.drawerBackdrop) closeDrawer();
  });
  els.clearQueue.addEventListener('click', () => void clearQueue());
}

function ratioFromEvent(event) {
  const rect = els.progress.getBoundingClientRect();
  if (rect.width === 0) return 0;
  return clamp((event.clientX - rect.left) / rect.width, 0, 1);
}

function hideTooltip() {
  els.tooltip.hidden = true;
}

export function hasCurrentSong() {
  return Boolean(state.currentSong);
}

export function playSong(song, newQueue = null, forcePlay = false) {
  if (!song) return;

  if (Array.isArray(newQueue)) {
    state.queue = [...newQueue];
    const index = state.queue.findIndex((queued) => queued.key === song.key);
    state.queueIndex = index >= 0 ? index : 0;
  } else if (!state.queue.some((queued) => queued.key === song.key)) {
    state.queue.push(song);
    state.queueIndex = state.queue.length - 1;
  } else {
    state.queueIndex = state.queue.findIndex((queued) => queued.key === song.key);
  }

  if (state.currentKey === song.key && state.currentSong) {
    if (!sourceUrl) {
      void loadSongSource(song, { autoplay: true });
    } else if (forcePlay) {
      audio.currentTime = 0;
      setProgressUi();
      audio.play().catch(() => {});
    } else {
      togglePlay();
    }
    renderPlayer();
    renderQueueBadge();
    if (drawerOpen) renderDrawer();
    emit(EVENTS.queue);
    return;
  }

  state.currentKey = song.key;
  state.currentSong = song;
  state.playing = false;
  setError('');

  renderPlayer();
  renderQueueBadge();
  if (drawerOpen) renderDrawer();
  emit(EVENTS.player);
  emit(EVENTS.queue);
  void loadSongSource(song, { autoplay: true });
}

export function setPlayMode(mode) {
  if (!MODE_CYCLE.includes(mode) || state.prefs.playMode === mode) return;
  state.prefs.playMode = mode;
  persistPrefs(true);
  renderPlayer();
}

export function playSongWithMode(song, list, mode) {
  if (!song) return;
  setPlayMode(mode);
  playSong(song, Array.isArray(list) ? list : null, true);
}

/**
 * WebKitGTK 的媒体管线无法直接播放 `asset://` 自定义协议（会报
 * MEDIA_ERR_SRC_NOT_SUPPORTED），这里先经 Asset Protocol 读取字节，
 * 再交给 Blob URL 播放；仍保持本地读取，不经过网络。
 */
async function loadSongSource(song, { autoplay = true, restoreTime = 0 } = {}) {
  const token = (sourceToken += 1);
  try {
    const blob = await fetchTrack(song.path);
    if (token !== sourceToken) return;

    const url = URL.createObjectURL(blob);
    if (sourceUrl) URL.revokeObjectURL(sourceUrl);
    sourceUrl = url;
    loadedPath = song.path;

    applyPrefsToAudio();
    if (restoreTime > 0) {
      audio.addEventListener(
        'loadedmetadata',
        () => {
          if (Number.isFinite(audio.duration) && audio.duration > 0) {
            audio.currentTime = Math.min(restoreTime, audio.duration);
          }
        },
        { once: true },
      );
    }
    audio.src = url;
    if (autoplay) {
      audio.play().catch(() => {
        if (token !== sourceToken) return;
        state.playing = false;
        renderPlayer();
      });
    }
  } catch (error) {
    if (token !== sourceToken) return;
    failCurrent(`无法读取音频（${normalizeError(error)}）`);
  }
}

async function fetchTrack(path) {
  const controller = new AbortController();
  const timer = window.setTimeout(() => controller.abort(), playerConfig().loadTimeoutMs);
  try {
    const response = await fetch(assetUrl(path), { signal: controller.signal });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const blob = await response.blob();
    if (blob.size === 0) throw new Error('音频文件为空');
    return blob.type === 'audio/mpeg'
      ? blob
      : new Blob([blob], { type: 'audio/mpeg' });
  } finally {
    window.clearTimeout(timer);
  }
}

export function togglePlay() {
  if (!state.currentSong) return;
  if (audio.paused) {
    audio.play().catch(() => {});
  } else {
    audio.pause();
  }
}

export function seekBy(delta) {
  seek(audio.currentTime + delta);
}

function seek(seconds) {
  const duration = audio.duration;
  if (!Number.isFinite(duration) || duration <= 0) return;
  audio.currentTime = clamp(seconds, 0, duration);
  setProgressUi();
}

export function nextTrack(forcePlay = true) {
  if (state.queue.length === 0) return;
  if (state.queue.length === 1) {
    audio.currentTime = 0;
    if (forcePlay) audio.play().catch(() => {});
    setProgressUi();
    return;
  }

  if (state.prefs.playMode === PLAY_MODES.shuffle) {
    let nextIndex = state.queueIndex;
    while (nextIndex === state.queueIndex) {
      nextIndex = Math.floor(Math.random() * state.queue.length);
    }
    state.queueIndex = nextIndex;
    playSong(state.queue[nextIndex], null, forcePlay);
    return;
  }

  const nextIndex = (state.queueIndex + 1) % state.queue.length;
  state.queueIndex = nextIndex;
  playSong(state.queue[nextIndex], null, forcePlay);
}

export function prevTrack(forcePlay = true) {
  if (state.queue.length === 0) return;
  if (audio.currentTime > playerConfig().restartThresholdSeconds) {
    seek(0);
    if (!state.playing && forcePlay) audio.play().catch(() => {});
    return;
  }
  if (state.queue.length === 1) {
    seek(0);
    if (!state.playing && forcePlay) audio.play().catch(() => {});
    return;
  }

  const prevIndex = (state.queueIndex - 1 + state.queue.length) % state.queue.length;
  state.queueIndex = prevIndex;
  playSong(state.queue[prevIndex], null, forcePlay);
}

function handleEnded() {
  if (state.prefs.playMode === PLAY_MODES.repeatOne) {
    audio.currentTime = 0;
    setProgressUi();
    audio.play().catch(() => {});
    return;
  }
  nextTrack(true);
}

function handleAudioError() {
  failCurrent('音频加载失败');
}

/** 播放失败：连续失败不超过上限时自动跳下一首，否则停下来提示。 */
function failCurrent(reason) {
  const song = state.currentSong;
  if (!song) return;
  state.playing = false;
  failureStreak += 1;

  if (state.queue.length > 1 && failureStreak <= playerConfig().maxAutoSkips) {
    setError(`《${song.title}》${reason}，已自动跳到下一首`);
    renderPlayer();
    emit(EVENTS.player);
    nextTrack(true);
    return;
  }

  setError(`《${song.title}》${reason}：文件可能已损坏或被移动`);
  renderPlayer();
  emit(EVENTS.player);
}

export async function removeFromQueue(index) {
  if (index < 0 || index >= state.queue.length) return;
  const song = state.queue[index];
  const key = song.key;
  const wasCurrent = state.queueIndex === index;
  const confirmed = await showModal({
    title: '移出播放队列',
    description: wasCurrent
      ? `确定将正在播放的《${song.title}》移出队列吗？${
          state.queue.length > 1 ? '移除后会切到下一首，' : '移除后会停止播放，'
        }本地文件不会被删除。`
      : `确定将《${song.title}》移出播放队列吗？本地文件不会被删除。`,
    confirmText: '移出队列',
  });
  if (!confirmed) return;

  // 确认期间播放可能自动切歌，按 key 重新定位，避免误删别的曲目。
  const liveIndex = state.queue.findIndex((item) => item.key === key);
  if (liveIndex < 0) return;
  const liveCurrent = state.queueIndex === liveIndex;
  const wasPlaying = state.playing;
  state.queue.splice(liveIndex, 1);

  if (state.queue.length === 0) {
    stopPlayback();
  } else if (liveCurrent) {
    state.queueIndex = liveIndex < state.queue.length ? liveIndex : 0;
    playSong(state.queue[state.queueIndex], null, wasPlaying);
  } else if (state.queueIndex > liveIndex) {
    state.queueIndex -= 1;
  }

  renderPlayer();
  renderQueueBadge();
  renderDrawer();
  emit(EVENTS.queue);
  emit(EVENTS.player);
}

async function clearQueue() {
  if (state.queue.length === 0) return;
  const confirmed = await showModal({
    title: '清空播放队列',
    description: `确定清空播放队列中的 ${state.queue.length} 首曲目吗？当前播放会停止，本地文件不会被删除。`,
    confirmText: '清空队列',
    danger: true,
  });
  if (!confirmed) return;

  state.queue = [];
  state.queueIndex = -1;
  stopPlayback();
  renderQueueBadge();
  renderDrawer();
  emit(EVENTS.queue);
  emit(EVENTS.player);
}

function stopPlayback() {
  sourceToken += 1;
  audio.pause();
  audio.removeAttribute('src');
  audio.load();
  releaseSource();
  state.currentSong = null;
  state.currentKey = null;
  state.playing = false;
  setError('');
  setProgressUi();
}

function releaseSource() {
  if (sourceUrl) {
    URL.revokeObjectURL(sourceUrl);
    sourceUrl = '';
  }
  loadedPath = '';
}

/** 曲目被重命名或移动后，文件路径变化，按原进度重新加载。 */
function syncAudioSource() {
  const song = state.currentSong;
  if (!song || song.path === loadedPath) return;
  const time = Number.isFinite(audio.currentTime) ? audio.currentTime : 0;
  const wasPlaying = !audio.paused;
  void loadSongSource(song, { autoplay: wasPlaying, restoreTime: time });
}

export function setVolume(value) {
  const volume = clamp(Number.isFinite(value) ? value : 0.85, 0, 1);
  state.prefs.volume = volume;
  if (volume > 0) state.prefs.muted = false;
  applyPrefsToAudio();
  persistPrefs();
  renderPlayer();
}

export function adjustVolume(delta) {
  setVolume(Math.round((state.prefs.volume + delta) * 100) / 100);
}

export function toggleMute() {
  state.prefs.muted = !state.prefs.muted;
  if (!state.prefs.muted && state.prefs.volume === 0) {
    state.prefs.volume = 0.5;
  }
  applyPrefsToAudio();
  persistPrefs(true);
  renderPlayer();
}

function cycleRate() {
  const rates = playerConfig().rates;
  const currentIndex = rates.findIndex(
    (rate) => Math.abs(rate - state.prefs.playbackRate) < 0.001,
  );
  const next = rates[(currentIndex + 1) % rates.length];
  state.prefs.playbackRate = next;
  audio.playbackRate = next;
  persistPrefs(true);
  renderPlayer();
}

function cyclePlayMode() {
  const currentIndex = MODE_CYCLE.indexOf(state.prefs.playMode);
  state.prefs.playMode = MODE_CYCLE[(currentIndex + 1) % MODE_CYCLE.length];
  persistPrefs(true);
  renderPlayer();
}

function toggleCurrentFavorite() {
  if (!state.currentKey) return;
  toggleFavorite(state.currentKey);
}

export async function exportSong(key) {
  if (!key || exporting) return;
  const song = songByKey(key);
  if (!song) {
    toast('曲目不存在或已从曲库移除', 'error');
    return;
  }
  exporting = true;
  renderPlayer();
  try {
    const result = await invoke(COMMANDS.exportTrack, { key });
    if (result) {
      toast(`已导出到 ${result.path}`, 'success');
    }
  } catch (error) {
    toast(`导出失败：${normalizeError(error)}`, 'error');
  } finally {
    exporting = false;
    renderPlayer();
  }
}

export function toggleDrawer() {
  if (drawerOpen) closeDrawer();
  else openDrawer();
}

function openDrawer() {
  drawerOpen = true;
  els.drawerBackdrop.hidden = false;
  els.queueBtn.classList.add('is-open');
  renderDrawer();
}

export function closeDrawer() {
  drawerOpen = false;
  els.drawerBackdrop.hidden = true;
  els.queueBtn.classList.remove('is-open');
}

export function isDrawerOpen() {
  return drawerOpen;
}

function renderPlayer() {
  const song = state.currentSong;
  els.player.hidden = !song;
  els.player.classList.toggle('is-playing', Boolean(song) && state.playing);
  if (!song) {
    renderQueueBadge();
    return;
  }

  const category = state.library.categories.find((item) => item.id === song.category);
  const color = category?.color ?? DEFAULT_ACCENT;

  if (els.cover) {
    els.cover.style.background = `radial-gradient(130% 130% at 22% 16%, ${withAlpha(color, 0.6)} 0%, rgba(9, 24, 16, 0.94) 56%, rgba(4, 10, 7, 1) 100%)`;
    els.cover.style.boxShadow = `inset 0 0 0 1px ${withAlpha(color, 0.3)}, 0 10px 22px -16px rgba(0, 0, 0, 0.9)`;
    setIcon(els.coverIcon, category?.icon || 'music');
  }

  els.title.textContent = song.title;
  els.title.title = song.title;
  els.category.hidden = false;
  els.category.textContent = category?.nameZh ?? song.category;
  els.category.style.color = color;
  els.category.style.backgroundColor = withAlpha(color, 0.13);
  els.category.style.borderColor = withAlpha(color, 0.32);
  els.sub.textContent = category?.labelEn ?? song.category;
  els.sub.title = song.fileName;

  els.favorite.hidden = false;
  els.favorite.classList.toggle('is-favorite', state.favorites.has(song.key));

  setIcon(els.play, state.playing ? 'pause-fill' : 'play-fill');
  els.play.title = state.playing ? '暂停 (空格)' : '播放 (空格)';
  els.play.style.boxShadow = state.playing
    ? `inset 0 1px 0 rgba(255, 255, 255, 0.6), 0 6px 20px -6px ${withAlpha(DEFAULT_ACCENT, 0.6)}, 0 0 26px ${withAlpha(color, 0.45)}`
    : '';

  const mode = modeMeta(state.prefs.playMode);
  setIcon(els.mode, mode.icon);
  els.mode.title = mode.title;
  els.mode.classList.toggle('is-active', state.prefs.playMode !== PLAY_MODES.sequence);

  els.rate.textContent = formatRate(state.prefs.playbackRate);
  els.rate.title = `当前 ${formatRate(state.prefs.playbackRate)}，点击切换倍速`;

  const effectiveVolume = state.prefs.muted ? 0 : state.prefs.volume;
  const volumeIcon = effectiveVolume === 0
    ? 'volume-mute'
    : effectiveVolume < 0.5
      ? 'volume-low'
      : 'volume';
  setIcon(els.mute, volumeIcon);
  els.mute.title = state.prefs.muted ? '取消静音 (M)' : '静音 (M)';
  els.volume.value = String(effectiveVolume);
  els.volume.style.setProperty('--volume', `${Math.round(effectiveVolume * 100)}%`);
  els.volumePercent.textContent = `${Math.round(effectiveVolume * 100)}%`;

  els.download.disabled = exporting;
  els.errorBar.hidden = !lastError;
  els.errorText.textContent = lastError;

  setProgressUi();
}

function modeMeta(mode) {
  switch (mode) {
    case PLAY_MODES.repeatOne:
      return { icon: 'repeat-1', title: '单曲循环（点击切换）' };
    case PLAY_MODES.shuffle:
      return { icon: 'shuffle', title: '随机播放（点击切换）' };
    default:
      return { icon: 'repeat', title: '顺序播放（点击切换）' };
  }
}

function formatRate(rate) {
  if (rate === 1) return '1.0x';
  return `${Number(rate.toFixed(2))}x`;
}

function setProgressUi() {
  if (!els.progress) return;
  const duration = Number.isFinite(audio.duration) ? audio.duration : 0;
  const displayTime = dragging ? dragTime : audio.currentTime;
  const percent = duration > 0 ? clamp((displayTime / duration) * 100, 0, 100) : 0;

  els.played.style.width = `${percent}%`;
  els.thumb.style.left = `clamp(7px, ${percent}%, calc(100% - 7px))`;
  els.timeCurrent.textContent = formatTime(displayTime);
  els.timeTotal.textContent = duration > 0 ? formatTime(duration) : '--:--';
  els.progress.classList.toggle('is-unready', duration <= 0);
  els.progress.setAttribute('aria-valuemin', '0');
  els.progress.setAttribute('aria-valuemax', String(Math.round(duration)));
  els.progress.setAttribute('aria-valuenow', String(Math.round(displayTime)));
}

function updateBuffered() {
  const duration = audio.duration;
  if (
    !Number.isFinite(duration) ||
    duration <= 0 ||
    audio.buffered.length === 0 ||
    !els.buffered
  ) {
    if (els.buffered) els.buffered.style.width = '0%';
    return;
  }
  const end = audio.buffered.end(audio.buffered.length - 1);
  els.buffered.style.width = `${clamp((end / duration) * 100, 0, 100)}%`;
}

function setError(message) {
  lastError = message;
  if (!els.errorBar) return;
  els.errorBar.hidden = !message;
  els.errorText.textContent = message;
}

function renderQueueBadge() {
  if (!els.queueCount) return;
  els.queueCount.textContent = String(state.queue.length);
  els.queueBtn.classList.toggle('is-open', drawerOpen);
  if (els.drawerCount) els.drawerCount.textContent = `${state.queue.length} 首`;
  if (els.clearQueue) els.clearQueue.hidden = state.queue.length === 0;
}

function renderDrawer() {
  if (!els.queueList) return;
  renderQueueBadge();
  if (!drawerOpen) return;

  els.queueList.replaceChildren();
  if (state.queue.length === 0) {
    const iconHost = el('span');
    setIcon(iconHost, 'music');
    els.queueList.append(
      el('div', { class: 'queue-empty' }, [
        iconHost,
        el('span', { text: '播放队列为空' }),
        el('small', { text: '在曲库列表中点击曲目即可加入队列' }),
      ]),
    );
    return;
  }

  state.queue.forEach((song, index) => {
    const category = state.library.categories.find((item) => item.id === song.category);
    const color = category?.color ?? DEFAULT_ACCENT;
    const isCurrent = state.currentKey === song.key;

    const left = el('div', { class: 'queue-item-left' });
    if (isCurrent && state.playing) {
      left.append(equalizer(color));
    } else {
      left.append(el('span', { class: 'queue-index', text: String(index + 1) }));
    }
    left.append(el('span', { class: 'queue-name', text: song.title, title: song.title }));

    const chip = el('span', { class: 'cat-chip', text: category?.nameZh ?? song.category });
    chip.style.color = color;
    chip.style.backgroundColor = withAlpha(color, 0.12);

    const remove = el('button', {
      class: 'item-remove-btn',
      attrs: { type: 'button', title: '从队列移除' },
    });
    setIcon(remove, 'x');
    remove.addEventListener('click', (event) => {
      event.stopPropagation();
      void removeFromQueue(index);
    });

    const item = el('div', {
      class: `queue-item${isCurrent ? ' is-current' : ''}`,
      dataset: { key: song.key },
    }, [left, el('div', { class: 'queue-item-right' }, [chip, remove])]);
    item.addEventListener('click', () => playSong(song));
    els.queueList.append(item);
  });
}