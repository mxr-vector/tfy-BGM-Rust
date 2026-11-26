<script setup lang="ts">
import { computed } from 'vue';
import { guardLibraryPlayback } from '@/composables/useScheduler';
import { playerConfig } from '@/lib/config';
import { PLAY_MODES } from '@/lib/constants';
import { clamp, formatTime, withAlpha } from '@/lib/format';
import { audioState } from '@/stores/audio-engine';
import { toggleSongDetails } from '@/stores/song-details';
import {
  cyclePlayMode,
  formatRate,
  modeMeta,
  nextTrack,
  playSong,
  prevTrack,
  seekTo,
  setPlaybackRate,
  setVolume,
  toggleCurrentFavorite,
  toggleDrawer,
  toggleMute,
  togglePlay,
} from '@/stores/player';
import { categoryById, state, visibleSongs } from '@/stores/state';
import Icon from '@/components/Icon.vue';

const song = computed(() => state.currentSong);
const category = computed(() =>
  song.value ? categoryById(song.value.category) : null,
);
const color = computed(() => category.value?.color ?? '#19d98b');
const duration = computed(() => audioState.duration);
const percent = computed(() =>
  duration.value > 0
    ? clamp((audioState.currentTime / duration.value) * 100, 0, 100)
    : 0,
);
const effectiveVolume = computed(() =>
  state.prefs ? (state.prefs.muted ? 0 : state.prefs.volume) : 0,
);
const volumeIcon = computed(() => {
  if (effectiveVolume.value === 0) return 'volume-mute';
  return effectiveVolume.value < 0.5 ? 'volume-low' : 'volume';
});
const isFavorite = computed(() =>
  Boolean(song.value) && state.favorites.has(song.value!.key),
);
const rates = computed(() => playerConfig().rates);
const currentRate = computed(() => state.prefs?.playbackRate ?? 1);
const mode = computed(() => modeMeta(state.prefs?.playMode ?? PLAY_MODES.sequence));

function onPlayClick(): void {
  if (!song.value) {
    if (guardLibraryPlayback()) return;
    const list = visibleSongs.value;
    if (list.length > 0) playSong(list[0]!, list, true);
    return;
  }
  togglePlay();
}

function cycleRate(): void {
  const options = rates.value;
  if (options.length === 0) return;
  const index = options.findIndex((rate) => Math.abs(rate - currentRate.value) < 0.001);
  setPlaybackRate(options[(index + 1) % options.length]!);
}

function onSeekInput(event: Event): void {
  const value = Number((event.target as HTMLInputElement).value);
  if (Number.isFinite(value)) seekTo(value);
}

function onVolumeInput(event: Event): void {
  setVolume(Number((event.target as HTMLInputElement).value));
}
</script>

<template>
  <footer class="details-player-bar">
    <div class="details-player-track">
      <div
        class="details-player-cover"
        :style="{
          '--details-cover-color': color,
          '--details-cover-soft': withAlpha(color, 0.42),
        }"
      >
        <Icon :name="category?.icon || 'music'" />
      </div>
      <div class="details-player-copy">
        <strong :title="song?.title">{{ song?.title ?? '未在播放' }}</strong>
        <span>{{ song?.artist || category?.nameZh || '从曲库中选择一首配乐' }}</span>
      </div>
    </div>

    <div class="details-player-center">
      <div class="details-transport">
        <button
          type="button"
          class="details-mode-btn"
          :class="{
            'is-active': (state.prefs?.playMode ?? PLAY_MODES.sequence) !== PLAY_MODES.sequence,
          }"
          :title="mode.title"
          :aria-label="mode.title"
          @click="cyclePlayMode"
        >
          <Icon :name="mode.icon" />
        </button>
        <button type="button" title="上一首" @click="prevTrack()">
          <Icon name="skip-back" />
        </button>
        <button
          type="button"
          class="details-play-btn"
          :title="song ? (state.playing ? '暂停 (空格)' : '播放 (空格)') : '播放当前列表第一首'"
          @click="onPlayClick"
        >
          <Icon :name="state.playing ? 'pause-fill' : 'play-fill'" />
        </button>
        <button type="button" title="下一首" @click="nextTrack()">
          <Icon name="skip-forward" />
        </button>
        <button
          type="button"
          class="details-rate-btn"
          :title="`播放速度：${formatRate(currentRate)}，点击切换`"
          @click="cycleRate"
        >
          {{ formatRate(currentRate) }}
        </button>
      </div>
      <div class="details-progress">
        <span>{{ formatTime(audioState.currentTime) }}</span>
        <input
          type="range"
          min="0"
          :max="duration > 0 ? duration : 1"
          step="0.01"
          :value="audioState.currentTime"
          :disabled="duration <= 0"
          :style="{ '--details-progress': `${percent}%` }"
          aria-label="播放进度"
          @input="onSeekInput"
        />
        <span>{{ duration > 0 ? formatTime(duration) : '--:--' }}</span>
      </div>
    </div>

    <div class="details-player-actions">
      <div class="details-volume">
        <button
          type="button"
          :title="state.prefs?.muted ? '取消静音 (M)' : '静音 (M)'"
          @click="toggleMute"
        >
          <Icon :name="volumeIcon" />
        </button>
        <input
          type="range"
          min="0"
          max="1"
          step="0.01"
          :value="effectiveVolume"
          :style="{ '--details-volume': `${Math.round(effectiveVolume * 100)}%` }"
          aria-label="音量"
          @input="onVolumeInput"
        />
      </div>
      <button
        type="button"
        class="is-active"
        title="收起歌曲详情"
        aria-label="收起歌曲详情"
        aria-pressed="true"
        @click="toggleSongDetails()"
      >
        <Icon name="music" />
      </button>
      <button
        type="button"
        :class="{ 'is-favorite': isFavorite }"
        title="收藏当前曲目"
        @click="toggleCurrentFavorite"
      >
        <Icon name="heart" />
      </button>
      <button
        type="button"
        :class="{ 'is-active': state.queue.length > 0 }"
        title="播放队列"
        @click="toggleDrawer"
      >
        <Icon name="list-music" />
      </button>
    </div>
  </footer>
</template>

<style scoped>
.details-player-bar {
  display: grid;
  grid-template-columns: minmax(220px, 1fr) minmax(320px, 1.35fr) minmax(220px, 1fr);
  align-items: center;
  gap: 28px;
  min-height: 92px;
  padding: 14px 30px;
  border-top: 1px solid rgba(255, 255, 255, 0.08);
  background: rgba(7, 14, 12, 0.94);
  box-shadow: 0 -18px 50px -30px rgba(0, 0, 0, 0.9);
  backdrop-filter: blur(24px) saturate(125%);
  -webkit-backdrop-filter: blur(24px) saturate(125%);
}

.details-player-track {
  display: flex;
  align-items: center;
  gap: 12px;
  min-width: 0;
}

.details-player-cover {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  flex: none;
  overflow: hidden;
  border-radius: 11px;
  color: #f4fff9;
  background:
    radial-gradient(120% 120% at 22% 14%, var(--details-cover-soft), transparent 62%),
    linear-gradient(145deg, color-mix(in srgb, var(--details-cover-color) 55%, #10231b), #07110e 78%);
  box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.09);
}

.details-player-cover [data-icon] {
  width: 20px;
  height: 20px;
}

.details-player-copy {
  display: flex;
  flex-direction: column;
  min-width: 0;
  gap: 3px;
}

.details-player-copy strong {
  overflow: hidden;
  color: var(--text);
  font-size: 14px;
  font-weight: 650;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.details-player-copy span {
  overflow: hidden;
  color: var(--text-3);
  font-size: 11.5px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.details-player-center {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 7px;
  min-width: 0;
}

.details-transport {
  display: flex;
  align-items: center;
  gap: 14px;
}

.details-transport button,
.details-player-actions button,
.details-volume button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 30px;
  height: 30px;
  flex: none;
  border-radius: 9px;
  color: var(--text-2);
}

.details-transport button:hover,
.details-player-actions button:hover,
.details-volume button:hover {
  color: var(--text);
  background: rgba(255, 255, 255, 0.06);
}

.details-transport [data-icon] {
  width: 16px;
  height: 16px;
}

.details-transport .details-play-btn {
  width: 40px;
  height: 40px;
  border-radius: 50%;
  color: #04150e;
  background: linear-gradient(180deg, #8ff5ce, #19d98b);
  box-shadow: 0 7px 20px -10px rgba(25, 217, 139, 0.8);
}

.details-transport .details-play-btn:hover {
  color: #04150e;
  background: linear-gradient(180deg, #a4ffdd, #25e79a);
}

.details-transport .details-mode-btn.is-active {
  color: var(--accent-bright);
  background: rgba(25, 217, 139, 0.12);
}

.details-transport .details-rate-btn {
  width: auto;
  min-width: 31px;
  padding: 0 6px;
  color: var(--text-3);
  font-size: 10.5px;
  font-weight: 650;
  font-variant-numeric: tabular-nums;
}

.details-progress {
  display: grid;
  grid-template-columns: 38px minmax(120px, 1fr) 38px;
  align-items: center;
  gap: 9px;
  width: 100%;
  color: var(--text-3);
  font-size: 10.5px;
  font-variant-numeric: tabular-nums;
}

.details-progress span:last-child {
  text-align: right;
}

.details-progress input,
.details-volume input {
  height: 3px;
  margin: 0;
  appearance: none;
  -webkit-appearance: none;
  border-radius: 999px;
  cursor: pointer;
  background:
    linear-gradient(90deg, #6ff3bc, #19d98b) no-repeat,
    rgba(255, 255, 255, 0.13);
  background-size: var(--details-progress, 0%) 100%, 100% 100%;
}

.details-progress input::-webkit-slider-thumb,
.details-volume input::-webkit-slider-thumb {
  width: 10px;
  height: 10px;
  appearance: none;
  -webkit-appearance: none;
  border: 0;
  border-radius: 50%;
  background: #effff7;
  box-shadow: 0 0 8px rgba(25, 217, 139, 0.5);
}

.details-progress input::-moz-range-thumb,
.details-volume input::-moz-range-thumb {
  width: 10px;
  height: 10px;
  border: 0;
  border-radius: 50%;
  background: #effff7;
}

.details-player-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 4px;
}

.details-player-actions [data-icon] {
  width: 16px;
  height: 16px;
}

.details-player-actions button.is-active {
  color: var(--accent-bright);
  background: rgba(25, 217, 139, 0.12);
}

.details-player-actions button.is-favorite {
  color: var(--favorite);
}

.details-player-actions button.is-favorite svg {
  fill: currentColor;
}

.details-volume {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-right: 6px;
}

.details-volume input {
  width: 76px;
  background-size: var(--details-volume, 0%) 100%, 100% 100%;
}

@media (max-width: 1050px) {
  .details-player-bar {
    grid-template-columns: minmax(170px, 1fr) minmax(280px, 1.4fr) auto;
    gap: 16px;
    padding-inline: 20px;
  }

  .details-volume input,
  .details-player-actions button:last-child {
    display: none;
  }
}

@media (max-width: 760px) {
  .details-player-bar {
    grid-template-columns: minmax(0, 1fr) auto;
    min-height: 82px;
    padding: 11px 14px;
  }

  .details-player-center {
    align-items: flex-end;
  }

  .details-progress {
    display: none;
  }

  .details-player-actions .details-volume,
  .details-player-actions button:nth-last-child(2) {
    display: none;
  }

  .details-player-cover {
    width: 42px;
    height: 42px;
  }
}
</style>
