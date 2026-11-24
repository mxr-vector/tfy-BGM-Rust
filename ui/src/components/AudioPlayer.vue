<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';
import {
  closePlayerPopover,
  playerPopover,
  togglePlayerPopover,
} from '@/composables/usePlayerPopover';
import { currentProgram, guardLibraryPlayback } from '@/composables/useScheduler';
import { playerConfig } from '@/lib/config';
import { DEFAULT_ACCENT, PLAY_MODES } from '@/lib/constants';
import { clamp, formatTime, withAlpha } from '@/lib/format';
import { audioState } from '@/stores/audio-engine';
import {
  desktopLyricsAvailable,
  desktopLyricsBusy,
  desktopLyricsEnabled,
  desktopLyricsHint,
  toggleDesktopLyrics,
} from '@/stores/desktop-lyrics';
import { songDetailsOpen, toggleSongDetails } from '@/stores/song-details';
import { loopPointFor, openLoopEditor } from '@/stores/loops';
import {
  cyclePlayMode,
  drawerOpen,
  exportSong,
  exporting,
  formatRate,
  modeMeta,
  nextTrack,
  playbackError,
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
import AppDropdown from './AppDropdown.vue';
import ExportSettingsPopover from './ExportSettingsPopover.vue';
import Icon from './Icon.vue';
import MarqueeText from './MarqueeText.vue';
import OutputPanel from './OutputPanel.vue';

const progressEl = ref<HTMLElement | null>(null);
const dragging = ref(false);
const dragTime = ref(0);
const hoverVisible = ref(false);
const hoverRatio = ref(0);

const song = computed(() => state.currentSong);
const category = computed(() =>
  song.value ? categoryById(song.value.category) : null,
);
const color = computed(() => category.value?.color ?? DEFAULT_ACCENT);
const programName = computed(() => {
  const entry = currentProgram.value;
  const current = song.value;
  if (!entry || !current) return '';
  return entry.songs.some((item) => item.key === current.key) ? entry.name : '';
});
const duration = computed(() => audioState.duration);
const displayTime = computed(() =>
  dragging.value ? dragTime.value : audioState.currentTime,
);
const percent = computed(() =>
  duration.value > 0 ? clamp((displayTime.value / duration.value) * 100, 0, 100) : 0,
);
const mode = computed(() => modeMeta(state.prefs?.playMode ?? PLAY_MODES.sequence));
const effectiveVolume = computed(() =>
  state.prefs ? (state.prefs.muted ? 0 : state.prefs.volume) : 0,
);
const volumeIcon = computed(() => {
  if (effectiveVolume.value === 0) return 'volume-mute';
  return effectiveVolume.value < 0.5 ? 'volume-low' : 'volume';
});
const isFavorite = computed(
  () => Boolean(song.value) && state.favorites.has(song.value!.key),
);
const loopPoint = computed(() => (song.value ? loopPointFor(song.value.key) : null));
const loopLabel = computed(() => {
  const point = loopPoint.value;
  if (!point) return '';
  return `循环 ${formatTime(point.start)}–${formatTime(point.end)}${point.enabled ? '' : '（已关闭）'}`;
});
const rates = computed(() => playerConfig().rates);
const currentRate = computed(() => state.prefs?.playbackRate ?? 1);
const rateOpen = computed(() => playerPopover.value === 'rate');

const rateMenu = ref<HTMLElement | null>(null);
const rateButton = ref<HTMLButtonElement | null>(null);
const coverStyle = computed(() =>
  song.value
    ? {
        '--cover-soft': withAlpha(color.value, 0.6),
        '--cover-line': withAlpha(color.value, 0.3),
      }
    : {},
);

function onPlayClick(): void {
  if (!song.value) {
    if (guardLibraryPlayback()) return;
    const list = visibleSongs.value;
    if (list.length > 0) playSong(list[0]!, list, true);
    return;
  }
  togglePlay();
}

function ratioFromEvent(event: PointerEvent): number {
  const rect = progressEl.value?.getBoundingClientRect();
  if (!rect || rect.width === 0) return 0;
  return clamp((event.clientX - rect.left) / rect.width, 0, 1);
}

function onPointerDown(event: PointerEvent): void {
  if (!Number.isFinite(duration.value) || duration.value <= 0) return;
  dragging.value = true;
  dragTime.value = ratioFromEvent(event) * duration.value;
  progressEl.value?.setPointerCapture(event.pointerId);
  hoverVisible.value = false;
}

function onPointerMove(event: PointerEvent): void {
  if (!Number.isFinite(duration.value) || duration.value <= 0) return;
  const ratio = ratioFromEvent(event);
  if (dragging.value) {
    dragTime.value = ratio * duration.value;
  } else {
    hoverVisible.value = true;
    hoverRatio.value = ratio;
  }
}

function finishDrag(event: PointerEvent): void {
  if (!dragging.value) return;
  if (progressEl.value?.hasPointerCapture(event.pointerId)) {
    progressEl.value.releasePointerCapture(event.pointerId);
  }
  dragging.value = false;
  seekTo(dragTime.value);
}

function onVolumeInput(event: Event): void {
  setVolume(Number((event.target as HTMLInputElement).value));
}

function toggleRateMenu(): void {
  togglePlayerPopover('rate');
  if (!rateOpen.value) return;
  void nextTick(() => {
    const active = rateMenu.value?.querySelector<HTMLButtonElement>('.rate-item.is-active');
    (active ?? rateMenu.value?.querySelector<HTMLButtonElement>('.rate-item'))?.focus();
  });
}

function closeRateMenu(focusTrigger = false): void {
  if (!rateOpen.value) return;
  closePlayerPopover('rate');
  if (focusTrigger) rateButton.value?.focus();
}

function chooseRate(rate: number): void {
  setPlaybackRate(rate);
  closeRateMenu(true);
}

function onRateMenuKeydown(event: KeyboardEvent): void {
  const items = Array.from(
    rateMenu.value?.querySelectorAll<HTMLButtonElement>('.rate-item') ?? [],
  );
  const index = items.indexOf(document.activeElement as HTMLButtonElement);
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    items[(index + 1) % items.length]?.focus();
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    items[(index - 1 + items.length) % items.length]?.focus();
  } else if (event.key === 'Escape') {
    event.preventDefault();
    event.stopPropagation();
    closeRateMenu(true);
  }
}

</script>

<template>
  <footer
    class="audio-player"
    id="player"
    :class="{ 'is-playing': Boolean(song) && state.playing }"
  >
    <div class="player-wrapper">
      <div
        ref="progressEl"
        class="progress-bar"
        :class="{ 'is-dragging': dragging, 'is-unready': duration <= 0 }"
        id="progress"
        role="slider"
        aria-label="播放进度"
        tabindex="0"
        aria-valuemin="0"
        :aria-valuemax="Math.round(duration)"
        :aria-valuenow="Math.round(displayTime)"
        @pointerdown="onPointerDown"
        @pointermove="onPointerMove"
        @pointerup="finishDrag"
        @pointercancel="finishDrag"
        @pointerleave="hoverVisible = false"
      >
        <div class="progress-track">
          <div
            class="progress-buffered"
            id="progress-buffered"
            :style="{ width: `${audioState.bufferedPercent}%` }"
          ></div>
          <div
            class="progress-played"
            id="progress-played"
            :style="{ width: `${percent}%` }"
          ></div>
        </div>
        <div
          class="progress-thumb"
          id="progress-thumb"
          :style="{ left: `clamp(7px, ${percent}%, calc(100% - 7px))` }"
        ></div>
        <div
          v-if="hoverVisible"
          class="hover-tooltip"
          id="hover-tooltip"
          :style="{ left: `${hoverRatio * 100}%` }"
        >
          {{ formatTime(hoverRatio * duration) }}
        </div>
      </div>

      <div class="player-content">
        <div class="track-info">
          <div class="track-cover" id="now-cover" :style="coverStyle">
            <div
              class="track-cover-hit"
              :class="{ 'is-clickable': Boolean(song), 'is-open': songDetailsOpen }"
              role="button"
              :tabindex="song ? 0 : -1"
              :title="song ? (songDetailsOpen ? '收起歌曲详情' : '展开歌曲详情') : ''"
              :aria-label="song ? (songDetailsOpen ? '收起歌曲详情' : '展开歌曲详情') : ''"
              :aria-expanded="songDetailsOpen"
              @click="toggleSongDetails()"
              @keydown.enter.prevent="toggleSongDetails()"
              @keydown.space.prevent="toggleSongDetails()"
            >
              <span class="cover-icon" id="now-cover-icon">
                <Icon :name="song ? (category?.icon || 'music') : 'disc'" />
              </span>
            </div>
          </div>
          <div class="track-meta">
            <div class="title-row">
              <MarqueeText
                class="track-title"
                id="now-title"
                :text="song?.title ?? '未在播放'"
                :title="song?.title ?? ''"
                :paused="!state.playing"
              />
              <span
                v-if="song"
                class="category-badge"
                id="now-category"
                :style="{
                  '--tag-color': color,
                  '--tag-bg': withAlpha(color, 0.13),
                  '--tag-line': withAlpha(color, 0.32),
                }"
              >
                {{ category?.nameZh ?? song.category }}
              </span>
              <span
                v-if="programName"
                class="program-badge"
                title="当前节目"
              >
                <Icon name="clock" />
                <MarqueeText :text="programName" :paused="!state.playing" />
              </span>
              <button
                v-if="loopPoint"
                type="button"
                class="loop-badge"
                :class="{ 'is-on': loopPoint.enabled }"
                :title="`${loopLabel}，点击编辑`"
                @click.stop="song && openLoopEditor(song)"
              >
                <Icon name="repeat-1" />
                <span>{{ loopLabel }}</span>
              </button>
            </div>
            <span class="track-sub" id="now-sub" :title="song?.artist ?? song?.fileName ?? ''">
              {{ song ? (song.artist ? `${song.artist}${song.album ? ` · ${song.album}` : ''}` : (category?.labelEn ?? song.category)) : '从曲库中选择一首配乐' }}
            </span>
          </div>
          <button
            v-if="song"
            type="button"
            class="action-btn now-fav"
            id="btn-now-favorite"
            :class="{ 'is-favorite': isFavorite }"
            title="收藏当前曲目"
            @click="toggleCurrentFavorite"
          >
            <Icon name="heart" />
          </button>
        </div>

        <div class="controls-section">
          <div class="buttons-row">
            <button
              type="button"
              class="control-btn"
              :class="{ 'is-active': (state.prefs?.playMode ?? 'sequence') !== PLAY_MODES.sequence }"
              id="btn-mode"
              :title="mode.title"
              @click="cyclePlayMode"
            >
              <Icon :name="mode.icon" />
            </button>
            <button type="button" class="control-btn" id="btn-prev" title="上一首" @click="prevTrack()">
              <Icon name="skip-back" />
            </button>
            <button
              type="button"
              class="play-main-btn"
              id="btn-play"
              :title="song ? (state.playing ? '暂停 (空格)' : '播放 (空格)') : '播放当前列表第一首 (空格)'"
              @click="onPlayClick"
            >
              <Icon :name="state.playing ? 'pause-fill' : 'play-fill'" />
            </button>
            <button type="button" class="control-btn" id="btn-next" title="下一首" @click="nextTrack()">
              <Icon name="skip-forward" />
            </button>
            <div class="rate-wrapper">
              <button
                ref="rateButton"
                type="button"
                class="speed-btn"
                id="btn-rate"
                :class="{ 'is-open': rateOpen }"
                :title="`播放速度：${formatRate(currentRate)}，点击选择`"
                aria-haspopup="listbox"
                :aria-expanded="rateOpen"
                :aria-controls="rateOpen ? 'rate-menu' : undefined"
                @click.stop="toggleRateMenu"
              >
                <span>{{ formatRate(currentRate) }}</span>
                <Icon name="chevron-down" class="speed-caret" />
              </button>
              <AppDropdown
                :open="rateOpen"
                :anchor="rateButton"
                side="top"
                align="center"
                :width="120"
                :max-height="280"
                label="播放速度"
                @close="closeRateMenu"
              >
                <div
                  id="rate-menu"
                  ref="rateMenu"
                  class="app-menu-list"
                  role="listbox"
                  aria-label="播放速度"
                  @keydown="onRateMenuKeydown"
                >
                  <button
                    v-for="rate in rates"
                    :key="rate"
                    type="button"
                    class="app-menu-option rate-item"
                    :class="{ 'is-active': Math.abs(rate - currentRate) < 0.001 }"
                    role="option"
                    :aria-selected="Math.abs(rate - currentRate) < 0.001"
                    @click="chooseRate(rate)"
                  >
                    <span class="app-menu-option-label">{{ formatRate(rate) }}</span>
                    <Icon name="check" class="app-menu-check" />
                  </button>
                </div>
              </AppDropdown>
            </div>
          </div>
          <div class="time-display">
            <span class="time-current" id="time-current">{{ formatTime(displayTime) }}</span>
            <span class="time-separator">/</span>
            <span class="time-total" id="time-total">
              {{ duration > 0 ? formatTime(duration) : '--:--' }}
            </span>
          </div>
        </div>

        <div class="utility-section">
          <div class="volume-container">
            <button
              type="button"
              class="action-btn"
              id="btn-mute"
              :title="state.prefs?.muted ? '取消静音 (M)' : '静音 (M)'"
              @click="toggleMute"
            >
              <Icon :name="volumeIcon" />
            </button>
            <input
              type="range"
              class="volume-range"
              id="volume-range"
              min="0"
              max="1"
              step="0.01"
              :value="effectiveVolume"
              :style="{ '--volume': `${Math.round(effectiveVolume * 100)}%` }"
              aria-label="音量"
              @input="onVolumeInput"
            />
            <span class="volume-percent" id="volume-percent">
              {{ Math.round(effectiveVolume * 100) }}%
            </span>
          </div>
          <OutputPanel />
          <ExportSettingsPopover />
          <button
            type="button"
            class="action-btn"
            :class="{ 'is-active': desktopLyricsEnabled }"
            id="btn-desktop-lyrics"
            :title="desktopLyricsHint"
            :aria-label="desktopLyricsHint"
            :aria-pressed="desktopLyricsEnabled"
            :disabled="!desktopLyricsAvailable || desktopLyricsBusy"
            @click="toggleDesktopLyrics"
          >
            <Icon name="lyrics" />
          </button>
          <button
            type="button"
            class="action-btn"
            id="btn-download"
            title="导出当前音频（归一化时输出 mp3，否则保持原格式）"
            :disabled="exporting || !song"
            @click="song && exportSong(song.key)"
          >
            <Icon name="download" />
          </button>
          <button
            type="button"
            class="action-btn queue-btn"
            :class="{ 'is-open': drawerOpen }"
            id="btn-queue"
            title="播放队列"
            @click="toggleDrawer"
          >
            <Icon name="list-music" />
            <span class="queue-count" id="queue-count">{{ state.queue.length }}</span>
          </button>
        </div>
      </div>

      <div v-if="playbackError" class="playback-error" id="playback-error">
        <Icon name="alert" />
        <span id="playback-error-text">{{ playbackError }}</span>
      </div>
    </div>
  </footer>
</template>