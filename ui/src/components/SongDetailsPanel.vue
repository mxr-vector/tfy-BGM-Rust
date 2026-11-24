<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { useWindowState } from '@/composables/useWindowState';
import { DEFAULT_ACCENT } from '@/lib/constants';
import { withAlpha } from '@/lib/format';
import { toggleDrawer } from '@/stores/player';
import { closeSongDetails, songDetailsOpen } from '@/stores/song-details';
import { categoryById, state } from '@/stores/state';
import Icon from './Icon.vue';
import SongDetailsContent from './SongDetailsContent.vue';
import SongDetailsPlayerBar from './SongDetailsPlayerBar.vue';
import WindowControls from './WindowControls.vue';

const { viewportHeight } = useWindowState();
const closeButton = ref<HTMLButtonElement | null>(null);
const song = computed(() => state.currentSong);
const category = computed(() =>
  song.value ? categoryById(song.value.category) : null,
);
const accent = computed(() => category.value?.color ?? DEFAULT_ACCENT);
const queuePosition = computed(() => {
  if (!song.value) return 0;
  if (state.queueIndex >= 0 && state.queue[state.queueIndex]?.key === song.value.key) {
    return state.queueIndex + 1;
  }
  const index = state.queue.findIndex((item) => item.key === song.value?.key);
  return index >= 0 ? index + 1 : 0;
});
const artistLabel = computed(() => {
  if (!song.value) return '';
  if (song.value.artist && song.value.album) return `${song.value.artist} / ${song.value.album}`;
  return song.value.artist ?? category.value?.labelEn ?? song.value.category;
});
const vinylStyle = computed(() => ({
  '--details-accent': accent.value,
  '--details-accent-soft': withAlpha(accent.value, 0.45),
  '--details-accent-faint': withAlpha(accent.value, 0.12),
  '--details-viewport-height': `${viewportHeight.value}px`,
}));

watch(songDetailsOpen, async (open) => {
  if (!open) return;
  await nextTick();
  closeButton.value?.focus();
});

watch(
  () => state.currentSong?.key,
  (key) => {
    if (!key) closeSongDetails();
  },
);
</script>

<template>
  <Teleport to="body">
    <div
      v-if="songDetailsOpen && song"
      class="song-details-layer"
      :style="vinylStyle"
      role="dialog"
      aria-modal="true"
      aria-label="歌曲详情"
    >
      <header class="song-details-topbar" data-tauri-drag-region="deep">
        <button
          ref="closeButton"
          type="button"
          class="details-collapse"
          data-tauri-drag-region="false"
          @click="closeSongDetails"
        >
          <span class="details-collapse-icon"><Icon name="win-restore" /></span>
          <span>收起播放页</span>
          <Icon name="chevron-down" />
        </button>
        <div class="details-topbar-actions" data-tauri-drag-region="false">
          <button type="button" class="details-queue" @click="toggleDrawer">
            <Icon name="list-music" />
            <span>播放队列</span>
            <strong>{{ queuePosition || '—' }} / {{ state.queue.length }}</strong>
          </button>
          <WindowControls id-prefix="details-win" />
        </div>
      </header>

      <main class="song-details-main">
        <section class="details-vinyl-stage" aria-label="当前曲目唱片">
          <div class="details-vinyl-wrap" :class="{ 'is-playing': state.playing }">
            <div class="details-vinyl-shadow"></div>
            <div class="details-vinyl-record">
              <div class="details-vinyl-grooves"></div>
              <div class="details-vinyl-label">
                <Icon :name="category?.icon || 'music'" />
                <span>{{ category?.nameZh ?? song.category }}</span>
              </div>
              <div class="details-vinyl-hole"></div>
            </div>
          </div>
          <div class="details-vinyl-caption">
            <span class="details-playing-indicator" :class="{ 'is-playing': state.playing }">
              <i></i><i></i><i></i><i></i>
            </span>
            <span>{{ state.playing ? '正在播放' : '已暂停' }}</span>
            <span class="details-caption-divider"></span>
            <span>{{ category?.labelEn ?? song.category }}</span>
          </div>
        </section>

        <section class="details-info-stage">
          <header class="details-track-heading">
            <span class="details-kicker">NOW PLAYING</span>
            <h1 :title="song.title">{{ song.title }}</h1>
            <p :title="artistLabel">{{ artistLabel }}</p>
          </header>
          <SongDetailsContent :key="song.key" :song="song" @close="closeSongDetails" />
        </section>
      </main>

      <SongDetailsPlayerBar />
    </div>
  </Teleport>
</template>

<style scoped>
.song-details-layer {
  --details-accent: #19d98b;
  position: fixed;
  inset: 0;
  z-index: 1080;
  display: flex;
  flex-direction: column;
  min-width: 0;
  color: var(--text);
  background:
    radial-gradient(80% 70% at 12% 48%, var(--details-accent-faint), transparent 58%),
    linear-gradient(90deg, rgba(4, 10, 8, 0.6), rgba(4, 10, 8, 0.9) 58%, rgba(4, 10, 8, 0.96));
  backdrop-filter: blur(9px) saturate(115%);
  -webkit-backdrop-filter: blur(9px) saturate(115%);
  animation: detailsFadeIn 180ms ease;
}

.song-details-topbar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex: none;
  gap: 20px;
  padding: 22px 30px 8px;
}

.song-details-topbar button {
  display: inline-flex;
  align-items: center;
  gap: 9px;
  color: rgba(255, 255, 255, 0.68);
  font-size: 11.5px;
}

.song-details-topbar button:hover {
  color: #fff;
}

.details-collapse {
  height: 35px;
  padding: 0 12px 0 7px;
  border: 1px solid rgba(255, 255, 255, 0.11);
  border-radius: 10px;
  background: rgba(255, 255, 255, 0.055);
}

.details-collapse-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 23px;
  height: 23px;
  border-radius: 7px;
  background: rgba(255, 255, 255, 0.07);
}

.details-collapse [data-icon] {
  width: 14px;
  height: 14px;
}

.details-collapse > [data-icon]:last-child {
  width: 12px;
  height: 12px;
  margin-left: 2px;
  opacity: 0.55;
}

.details-queue {
  height: 34px;
  padding: 0 4px;
}

.details-queue [data-icon] {
  width: 15px;
  height: 15px;
}

.details-queue strong {
  margin-left: 5px;
  color: rgba(255, 255, 255, 0.42);
  font-size: 10.5px;
  font-weight: 500;
  font-variant-numeric: tabular-nums;
}

.details-topbar-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}

.details-topbar-actions .window-controls {
  margin-left: 3px;
}

.song-details-main {
  display: grid;
  grid-template-columns: minmax(300px, 0.9fr) minmax(440px, 1.1fr);
  align-items: center;
  gap: clamp(34px, 5vw, 78px);
  flex: 1;
  min-height: 0;
  width: min(1240px, calc(100% - 100px));
  margin: 0 auto;
  padding: 18px 0 22px;
}

.details-vinyl-stage {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  min-width: 0;
}

.details-vinyl-wrap {
  --details-vinyl-limit: max(160px, calc(var(--details-viewport-height, 100dvh) - 250px));
  position: relative;
  width: min(35vw, 420px, var(--details-vinyl-limit));
  max-width: 100%;
  aspect-ratio: 1;
}

.details-vinyl-shadow {
  position: absolute;
  inset: 12% -7% -6% 13%;
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.72);
  filter: blur(25px);
  transform: translateY(18px);
}

.details-vinyl-record {
  position: absolute;
  inset: 0;
  overflow: hidden;
  border: 1px solid rgba(255, 255, 255, 0.12);
  border-radius: 50%;
  background:
    radial-gradient(circle at 36% 28%, rgba(255, 255, 255, 0.13), transparent 22%),
    conic-gradient(from 35deg, #111714, #030504 18%, #151a17 31%, #030504 48%, #111512 65%, #030504 82%, #151a17);
  box-shadow:
    0 34px 70px -24px rgba(0, 0, 0, 0.95),
    inset 0 0 0 7px rgba(0, 0, 0, 0.38),
    inset 0 0 0 9px rgba(255, 255, 255, 0.025);
  animation: vinylSpin 24s linear infinite;
  animation-play-state: paused;
}

.details-vinyl-wrap.is-playing .details-vinyl-record {
  animation-play-state: running;
}

.details-vinyl-grooves {
  position: absolute;
  inset: 4%;
  border-radius: 50%;
  background: repeating-radial-gradient(
    circle,
    transparent 0,
    transparent 3px,
    rgba(255, 255, 255, 0.038) 3.5px,
    rgba(0, 0, 0, 0.16) 4px,
    transparent 4.8px
  );
  mask-image: radial-gradient(circle, transparent 0 31%, #000 32% 100%);
  -webkit-mask-image: radial-gradient(circle, transparent 0 31%, #000 32% 100%);
}

.details-vinyl-label {
  position: absolute;
  inset: 31%;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  border: 1px solid rgba(255, 255, 255, 0.18);
  border-radius: 50%;
  color: #07130e;
  background:
    radial-gradient(circle at 32% 25%, rgba(255, 255, 255, 0.48), transparent 36%),
    linear-gradient(145deg, var(--details-accent-soft), var(--details-accent));
  box-shadow: inset 0 0 24px rgba(0, 0, 0, 0.2);
}

.details-vinyl-label [data-icon] {
  width: 30px;
  height: 30px;
  opacity: 0.78;
}

.details-vinyl-label span {
  max-width: 70%;
  overflow: hidden;
  font-size: 9px;
  font-weight: 750;
  letter-spacing: 0.08em;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.details-vinyl-hole {
  position: absolute;
  top: 50%;
  left: 50%;
  width: 10px;
  height: 10px;
  border: 2px solid rgba(255, 255, 255, 0.3);
  border-radius: 50%;
  background: #050806;
  transform: translate(-50%, -50%);
}

.details-vinyl-caption {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 25px;
  color: rgba(255, 255, 255, 0.38);
  font-size: 10px;
}

.details-playing-indicator {
  display: flex;
  align-items: center;
  gap: 2px;
  height: 12px;
}

.details-playing-indicator i {
  width: 2px;
  height: 4px;
  border-radius: 2px;
  background: var(--details-accent);
}

.details-playing-indicator.is-playing i {
  animation: equalize 900ms ease-in-out infinite alternate;
}

.details-playing-indicator.is-playing i:nth-child(2) {
  animation-delay: -300ms;
}

.details-playing-indicator.is-playing i:nth-child(3) {
  animation-delay: -600ms;
}

.details-playing-indicator.is-playing i:nth-child(4) {
  animation-delay: -150ms;
}

.details-caption-divider {
  width: 1px;
  height: 9px;
  margin-inline: 3px;
  background: rgba(255, 255, 255, 0.14);
}

.details-info-stage {
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  height: min(720px, calc(var(--details-viewport-height, 100dvh) - 210px));
}

.details-track-heading {
  flex: none;
  margin-bottom: 24px;
}

.details-kicker {
  display: block;
  margin-bottom: 8px;
  color: var(--details-accent);
  font-size: 9px;
  font-weight: 750;
  letter-spacing: 0.22em;
}

.details-track-heading h1 {
  overflow: hidden;
  margin: 0;
  color: #f5fff9;
  font-size: clamp(34px, 4vw, 54px);
  font-weight: 760;
  letter-spacing: -0.045em;
  line-height: 1.08;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.details-track-heading p {
  overflow: hidden;
  margin: 10px 0 0;
  color: rgba(255, 255, 255, 0.46);
  font-size: 13px;
  text-overflow: ellipsis;
  white-space: nowrap;
}

@keyframes detailsFadeIn {
  from {
    opacity: 0;
  }
  to {
    opacity: 1;
  }
}

@keyframes vinylSpin {
  to {
    transform: rotate(360deg);
  }
}

@keyframes equalize {
  from {
    height: 3px;
  }
  to {
    height: 11px;
  }
}

@media (max-width: 1050px) {
  .song-details-main {
    grid-template-columns: minmax(240px, 0.75fr) minmax(390px, 1.25fr);
    gap: 30px;
    width: calc(100% - 56px);
  }

  .details-vinyl-wrap {
    width: min(32vw, 340px, var(--details-vinyl-limit));
  }

  .details-info-stage {
    height: min(620px, calc(var(--details-viewport-height, 100dvh) - 200px));
  }
}

@media (max-width: 820px) {
  .song-details-main {
    grid-template-columns: 210px minmax(0, 1fr);
    gap: 24px;
    width: calc(100% - 40px);
  }

  .details-vinyl-wrap {
    width: min(210px, var(--details-vinyl-limit));
  }

  .details-track-heading h1 {
    font-size: 34px;
  }
}

@media (max-width: 680px) {
  .song-details-topbar {
    padding: 14px 16px 6px;
  }

  .details-queue span {
    display: none;
  }

  .song-details-main {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 22px;
    width: calc(100% - 32px);
    padding-top: 8px;
    overflow-y: auto;
  }

  .details-vinyl-stage {
    flex: none;
  }

  .details-vinyl-wrap {
    width: min(54vw, 250px);
  }

  .details-vinyl-caption {
    margin-top: 14px;
  }

  .details-info-stage {
    flex: none;
    height: 590px;
  }

  .details-track-heading {
    margin-bottom: 18px;
  }
}

@media (prefers-reduced-motion: reduce) {
  .song-details-layer,
  .details-vinyl-record,
  .details-playing-indicator i {
    animation: none;
  }
}
</style>
