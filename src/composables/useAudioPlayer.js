import { ref } from 'vue';
import { songKey, resolveSongUrl } from '../utils/songSchema';

// Global singleton state
const audio = new Audio();
const currentSong = ref(null);
const isPlaying = ref(false);
const isLoading = ref(false);
const currentTime = ref(0);
const duration = ref(0);
// Local storage volume
const loadVolume = () => {
  try {
    const raw = localStorage.getItem('tfy_bgm_volume');
    return raw !== null ? Math.max(0, Math.min(1, parseFloat(raw))) : 0.85;
  } catch (e) {
    return 0.85;
  }
};

const volume = ref(loadVolume());
const isMuted = ref(false);
const playbackRate = ref(1.0);
const playMode = ref('sequence'); // 'sequence' | 'repeat-one' | 'shuffle'
const queue = ref([]);
const queueIndex = ref(-1);
const isDraggingProgress = ref(false);
const bufferedPercent = ref(0);
// 当前曲目加载/播放失败提示（供底栏展示，避免静默卡死）
const playbackError = ref('');

// Local storage favorites（存稳定 key，不用位置型 id，避免重建曲库后收藏错位）
const FAVORITES_KEY = 'tfy_bgm_favorites';
const loadFavorites = () => {
  try {
    const raw = localStorage.getItem(FAVORITES_KEY);
    const list = raw ? JSON.parse(raw) : [];
    return Array.isArray(list) ? list : [];
  } catch (e) {
    return [];
  }
};
const favorites = ref(new Set(loadFavorites()));

const saveFavorites = () => {
  try {
    localStorage.setItem(FAVORITES_KEY, JSON.stringify(Array.from(favorites.value)));
  } catch (e) {}
};

const toggleFavorite = (key) => {
  if (!key) return;
  if (favorites.value.has(key)) {
    favorites.value.delete(key);
  } else {
    favorites.value.add(key);
  }
  saveFavorites();
  // Trigger reactivity
  favorites.value = new Set(favorites.value);
};

/**
 * 一次性升级：旧版本收藏存的是数值 id / 原始 url，这里按当前曲目表换算成稳定 key。
 */
const migrateFavorites = (songs) => {
  if (!Array.isArray(songs) || favorites.value.size === 0) return;
  const byId = new Map();
  const byUrl = new Map();
  for (const s of songs) {
    if (s?.id !== undefined) byId.set(String(s.id), songKey(s));
    if (s?.url) byUrl.set(s.url, songKey(s));
  }
  const migrated = new Set();
  let changed = false;
  for (const stored of favorites.value) {
    const mapped = byUrl.get(stored) ?? byId.get(String(stored));
    if (mapped) {
      migrated.add(mapped);
      if (mapped !== stored) changed = true;
    } else {
      migrated.add(stored); // 保留已下架曲目的收藏，回归时自动恢复
    }
  }
  if (changed) {
    favorites.value = migrated;
    saveFavorites();
  }
};

// Format seconds into mm:ss
export function formatTime(seconds) {
  if (isNaN(seconds) || seconds < 0) return '00:00';
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

// Update buffered percentage
function updateBuffered() {
  if (audio.buffered.length > 0) {
    const dur = audio.duration || duration.value || 0;
    if (dur > 0) {
      const end = audio.buffered.end(audio.buffered.length - 1);
      bufferedPercent.value = Math.min(100, Math.max(0, (end / dur) * 100));
    }
  }
}

// Audio element event listeners
audio.preload = 'metadata';

audio.addEventListener('loadstart', () => {
  isLoading.value = true;
  bufferedPercent.value = 0;
  playbackError.value = '';
});

audio.addEventListener('progress', () => {
  updateBuffered();
});

audio.addEventListener('canplay', () => {
  isLoading.value = false;
  duration.value = audio.duration || (currentSong.value?.duration) || 0;
  updateBuffered();
});

audio.addEventListener('timeupdate', () => {
  if (!isDraggingProgress.value) {
    currentTime.value = audio.currentTime;
  }
  if (!duration.value && audio.duration) {
    duration.value = audio.duration;
  }
});

audio.addEventListener('play', () => {
  isPlaying.value = true;
  isLoading.value = false;
  consecutiveFailures = 0;
});

audio.addEventListener('pause', () => {
  isPlaying.value = false;
});

// 自动跳过加载失败的曲目，但有连续上限，避免曲库缺文件时死循环跳歌
const MAX_AUTO_SKIPS = 3;
let consecutiveFailures = 0;
let lastFailureAt = 0;

audio.addEventListener('ended', () => {
  handleTrackEnded();
});

audio.addEventListener('error', (err) => {
  console.warn('Audio playback error:', err);
  isLoading.value = false;
  isPlaying.value = false;

  const title = currentSong.value?.title || '该素材';
  const now = Date.now();
  if (now - lastFailureAt > 1500) consecutiveFailures = 0;
  lastFailureAt = now;
  consecutiveFailures++;

  if (queue.value.length > 1 && consecutiveFailures <= MAX_AUTO_SKIPS) {
    playbackError.value = `《${title}》加载失败，已自动跳到下一首`;
    nextTrack(true);
  } else {
    playbackError.value = `《${title}》加载失败：音频文件可能未随应用一起部署（请确认 mp3 已放入 public/bgm 对应目录，或配置 VITE_BGM_BASE 指向云盘）`;
  }
});

function handleTrackEnded() {
  if (playMode.value === 'repeat-one') {
    audio.currentTime = 0;
    currentTime.value = 0;
    audio.play().catch(console.error);
    return;
  }
  nextTrack(true);
}

function playSong(song, newQueue = null, forcePlay = false) {
  if (!song) return;

  const key = songKey(song);

  if (newQueue && Array.isArray(newQueue)) {
    queue.value = [...newQueue];
    const idx = queue.value.findIndex(s => songKey(s) === key);
    queueIndex.value = idx !== -1 ? idx : 0;
  } else if (!queue.value.some(s => songKey(s) === key)) {
    queue.value.push(song);
    queueIndex.value = queue.value.length - 1;
  } else {
    queueIndex.value = queue.value.findIndex(s => songKey(s) === key);
  }

  // If already playing the same song
  if (currentSong.value && songKey(currentSong.value) === key) {
    if (forcePlay) {
      audio.currentTime = 0;
      currentTime.value = 0;
      audio.play().then(() => {
        isPlaying.value = true;
      }).catch(console.warn);
      return;
    }
    togglePlay();
    return;
  }

  currentSong.value = song;
  audio.src = resolveSongUrl(song);
  audio.playbackRate = playbackRate.value;
  audio.volume = isMuted.value ? 0 : volume.value;
  currentTime.value = 0;
  duration.value = 0;
  bufferedPercent.value = 0;

  audio.play()
    .then(() => {
      isPlaying.value = true;
    })
    .catch((e) => {
      console.warn('Auto-play prevented or network issue:', e);
      isPlaying.value = false;
    });
}

function togglePlay() {
  if (!currentSong.value && queue.value.length > 0) {
    playSong(queue.value[0], null, true);
    return;
  }
  if (!currentSong.value) return;

  if (isPlaying.value) {
    audio.pause();
  } else {
    audio.play().catch(console.error);
  }
}

function seek(time) {
  if (isNaN(time)) return;
  const target = Math.max(0, Math.min(time, duration.value || audio.duration || 1000));
  currentTime.value = target;
  audio.currentTime = target;
}

function seekPercentage(ratio) {
  const target = ratio * (duration.value || 1);
  seek(target);
}

function setVolume(v) {
  const val = Math.max(0, Math.min(1, v));
  volume.value = val;
  try {
    localStorage.setItem('tfy_bgm_volume', String(val));
  } catch (e) {}
  if (val > 0) isMuted.value = false;
  audio.volume = isMuted.value ? 0 : val;
}

function toggleMute() {
  isMuted.value = !isMuted.value;
  if (!isMuted.value && volume.value === 0) {
    setVolume(0.5);
  } else {
    audio.volume = isMuted.value ? 0 : volume.value;
  }
}

function setPlaybackRate(rate) {
  playbackRate.value = rate;
  audio.playbackRate = rate;
}

function togglePlayMode() {
  const modes = ['sequence', 'repeat-one', 'shuffle'];
  const nextIdx = (modes.indexOf(playMode.value) + 1) % modes.length;
  playMode.value = modes[nextIdx];
}

function nextTrack(forcePlay = true) {
  if (queue.value.length === 0) return;

  if (queue.value.length === 1) {
    audio.currentTime = 0;
    currentTime.value = 0;
    if (forcePlay) {
      audio.play().catch(console.warn);
    }
    return;
  }
  
  if (playMode.value === 'shuffle') {
    let nextIdx;
    do {
      nextIdx = Math.floor(Math.random() * queue.value.length);
    } while (nextIdx === queueIndex.value);
    queueIndex.value = nextIdx;
    playSong(queue.value[nextIdx], null, forcePlay);
    return;
  }

  let nextIdx = queueIndex.value + 1;
  if (nextIdx >= queue.value.length) {
    nextIdx = 0; // Loop back
  }
  queueIndex.value = nextIdx;
  playSong(queue.value[nextIdx], null, forcePlay);
}

function prevTrack(forcePlay = true) {
  if (queue.value.length === 0) return;
  
  // If playing more than 3s, replay current track
  if (audio.currentTime > 3) {
    seek(0);
    if (!isPlaying.value && forcePlay) {
      audio.play().catch(console.warn);
    }
    return;
  }

  if (queue.value.length === 1) {
    seek(0);
    if (!isPlaying.value && forcePlay) {
      audio.play().catch(console.warn);
    }
    return;
  }

  let prevIdx = queueIndex.value - 1;
  if (prevIdx < 0) {
    prevIdx = queue.value.length - 1;
  }
  queueIndex.value = prevIdx;
  playSong(queue.value[prevIdx], null, forcePlay);
}

function removeFromQueue(index) {
  if (index < 0 || index >= queue.value.length) return;
  
  const isCurrent = queueIndex.value === index;
  queue.value.splice(index, 1);

  if (queue.value.length === 0) {
    audio.pause();
    currentSong.value = null;
    queueIndex.value = -1;
    currentTime.value = 0;
    duration.value = 0;
    return;
  }

  if (isCurrent) {
    const nextIdx = index < queue.value.length ? index : 0;
    queueIndex.value = nextIdx;
    playSong(queue.value[nextIdx], null, isPlaying.value);
  } else if (queueIndex.value > index) {
    queueIndex.value--;
  }
}

function clearQueue() {
  audio.pause();
  queue.value = [];
  queueIndex.value = -1;
  currentSong.value = null;
  currentTime.value = 0;
  duration.value = 0;
  isPlaying.value = false;
}

function addToQueue(song) {
  if (!song) return;
  const key = songKey(song);
  if (!queue.value.some(s => songKey(s) === key)) {
    queue.value.push(song);
  }
}

export function useAudioPlayer() {
  return {
    audio,
    currentSong,
    isPlaying,
    isLoading,
    currentTime,
    duration,
    bufferedPercent,
    volume,
    isMuted,
    playbackRate,
    playMode,
    queue,
    queueIndex,
    isDraggingProgress,
    playbackError,
    favorites,
    toggleFavorite,
    migrateFavorites,
    songKey,
    playSong,
    togglePlay,
    seek,
    seekPercentage,
    setVolume,
    toggleMute,
    setPlaybackRate,
    togglePlayMode,
    nextTrack,
    prevTrack,
    removeFromQueue,
    clearQueue,
    addToQueue,
    formatTime
  };
}
