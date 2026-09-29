import { ref, reactive, computed, watch } from 'vue';

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

// Local storage favorites
const loadFavorites = () => {
  try {
    const raw = localStorage.getItem('tfy_bgm_favorites');
    return raw ? JSON.parse(raw) : [];
  } catch (e) {
    return [];
  }
};
const favorites = ref(new Set(loadFavorites()));

const saveFavorites = () => {
  try {
    localStorage.setItem('tfy_bgm_favorites', JSON.stringify(Array.from(favorites.value)));
  } catch (e) {}
};

const toggleFavorite = (id) => {
  if (favorites.value.has(id)) {
    favorites.value.delete(id);
  } else {
    favorites.value.add(id);
  }
  saveFavorites();
  // Trigger reactivity
  favorites.value = new Set(favorites.value);
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
});

audio.addEventListener('pause', () => {
  isPlaying.value = false;
});

audio.addEventListener('ended', () => {
  handleTrackEnded();
});

audio.addEventListener('error', (err) => {
  console.warn('Audio playback error:', err);
  isLoading.value = false;
  isPlaying.value = false;
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

  if (newQueue && Array.isArray(newQueue)) {
    queue.value = [...newQueue];
    const idx = queue.value.findIndex(s => s.id === song.id);
    queueIndex.value = idx !== -1 ? idx : 0;
  } else if (!queue.value.some(s => s.id === song.id)) {
    queue.value.push(song);
    queueIndex.value = queue.value.length - 1;
  } else {
    queueIndex.value = queue.value.findIndex(s => s.id === song.id);
  }

  // If already playing the same song
  if (currentSong.value?.id === song.id) {
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
  audio.src = song.url;
  audio.playbackRate = playbackRate.value;
  audio.volume = isMuted.value ? 0 : volume.value;
  currentTime.value = 0;
  duration.value = song.duration || 0;
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
  if (!queue.value.some(s => s.id === song.id)) {
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
    favorites,
    toggleFavorite,
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
