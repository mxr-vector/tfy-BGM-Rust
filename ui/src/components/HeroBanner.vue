<script setup lang="ts">
import { computed } from 'vue';
import { formatCopy, heroCopy } from '@/lib/config';
import {
  DEFAULT_ACCENT,
  FAVORITES_ACCENT,
  HERO_KINDS,
} from '@/lib/constants';
import { withAlpha } from '@/lib/format';
import { playSong } from '@/stores/player';
import { categoryById, state, visibleSongs } from '@/stores/state';
import Icon from './Icon.vue';

const HERO_ART = `
<svg viewBox="0 0 660 160" preserveAspectRatio="xMaxYMid slice" aria-hidden="true">
  <defs>
    <radialGradient id="hero-moon" cx="50%" cy="50%" r="50%">
      <stop offset="0%" stop-color="#f2fff8" stop-opacity="0.92"/>
      <stop offset="38%" stop-color="#a7f3d0" stop-opacity="0.3"/>
      <stop offset="100%" stop-color="#34d97f" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="hero-fog" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0%" stop-color="#d1fae5" stop-opacity="0"/>
      <stop offset="50%" stop-color="#d1fae5" stop-opacity="0.14"/>
      <stop offset="100%" stop-color="#d1fae5" stop-opacity="0"/>
    </linearGradient>
    <filter id="hero-blur" x="-30%" y="-80%" width="160%" height="260%">
      <feGaussianBlur stdDeviation="6"/>
    </filter>
  </defs>

  <circle cx="556" cy="50" r="52" fill="url(#hero-moon)"/>
  <circle cx="556" cy="50" r="15" fill="#ecfdf5" opacity="0.18"/>

  <g fill="var(--hero-color, #19d98b)">
    <path opacity="0.08" d="M0 100 L26 92 L44 98 L66 70 L78 92 L96 84 L114 52 L128 76 L142 62 L156 88 L176 80 L198 96 L216 66 L230 88 L248 78 L268 96 L286 72 L300 90 L318 82 L338 58 L354 84 L372 74 L392 92 L410 84 L430 62 L444 86 L462 78 L486 96 L504 88 L528 68 L544 90 L562 80 L586 96 L606 88 L628 74 L646 90 L660 84 L660 160 L0 160 Z"/>
    <path opacity="0.14" d="M0 128 L30 116 L54 124 L76 104 L96 120 L118 110 L140 130 L166 114 L188 126 L210 106 L232 122 L256 112 L280 132 L304 118 L330 128 L352 108 L374 124 L398 114 L424 134 L448 120 L474 130 L500 110 L522 126 L546 116 L572 132 L596 122 L620 134 L642 124 L660 132 L660 160 L0 160 Z"/>
    <path opacity="0.24" d="M0 148 L48 144 L94 147 L138 140 L184 146 L230 141 L276 148 L322 143 L368 149 L414 145 L462 151 L512 146 L560 152 L612 148 L660 152 L660 160 L0 160 Z"/>
  </g>

  <g fill="#02120a" opacity="0.6">
    <path d="M392 150h2v-7h-2z"/><path d="M385 144l8-13 8 13z"/><path d="M387 136l6-10 6 10z"/>
    <path d="M418 151h2v-5h-2z"/><path d="M412 146l7-11 7 11z"/>
    <path d="M446 150h2v-6h-2z"/><path d="M439 144l8-12 8 12z"/><path d="M441 137l6-9 6 9z"/>
  </g>

  <g fill="url(#hero-fog)" filter="url(#hero-blur)">
    <rect x="250" y="112" width="410" height="22" rx="11"/>
    <rect x="330" y="136" width="330" height="16" rx="8"/>
  </g>

  <g fill="none" stroke="var(--hero-color, #19d98b)" stroke-width="1.2" stroke-linecap="round" opacity="0.45">
    <path d="M392 52q5-5 10 0q5-5 10 0"/>
    <path d="M424 38q4-4 8 0q4-4 8 0"/>
    <path d="M362 74q3-3 6 0q3-3 6 0"/>
  </g>
</svg>`;

const list = visibleSongs;

const info = computed(() => {
  if (state.favoritesOnly) {
    const copy = heroCopy(HERO_KINDS.favorites);
    return {
      title: copy.title,
      labelEn: copy.labelEn,
      color: FAVORITES_ACCENT,
      icon: 'heart',
      description: copy.description,
    };
  }

  if (state.category === 'all') {
    const copy = heroCopy(HERO_KINDS.all);
    return {
      title: copy.title,
      labelEn: copy.labelEn,
      color: DEFAULT_ACCENT,
      icon: 'music',
      description: formatCopy(copy.description, {
        categories: state.library.categories.map((category) => category.nameZh).join('、'),
      }),
    };
  }

  const category = categoryById(state.category);
  if (!category) {
    const copy = heroCopy(HERO_KINDS.all);
    return {
      title: copy.title,
      labelEn: copy.labelEn,
      color: DEFAULT_ACCENT,
      icon: 'music',
      description: formatCopy(copy.description, {
        categories: state.library.categories.map((item) => item.nameZh).join('、'),
      }),
    };
  }

  return {
    title: category.title,
    labelEn: category.labelEn,
    color: category.color,
    icon: category.icon || 'music',
    description: category.description,
  };
});

function playAll(): void {
  const songs = list.value;
  if (songs.length > 0) playSong(songs[0]!, songs, true);
}

function shuffle(): void {
  const songs = list.value;
  if (songs.length === 0) return;
  const index = Math.floor(Math.random() * songs.length);
  playSong(songs[index]!, songs, true);
}
</script>

<template>
  <section
    class="category-hero"
    id="hero"
    :style="{ '--hero-color': info.color }"
  >
    <div class="hero-art" aria-hidden="true" v-html="HERO_ART"></div>
    <div class="hero-content">
      <div class="hero-heading">
        <div class="hero-badges">
          <span
            class="hero-tag"
            :style="{
              '--tag-color': info.color,
              '--tag-bg': withAlpha(info.color, 0.13),
              '--tag-line': withAlpha(info.color, 0.24),
            }"
          >
            <Icon :name="info.icon" />{{ info.labelEn }}
          </span>
          <span class="hero-stat">
            {{ list.length }} 首 · {{ state.library.categories.length }} 种风格
          </span>
        </div>
        <h1 class="hero-title" :title="info.title">{{ info.title }}</h1>
      </div>
      <div class="hero-actions">
        <button
          type="button"
          class="primary-play-btn"
          data-hero-action="play-all"
          :disabled="list.length === 0"
          @click="playAll"
        >
          <Icon name="play-fill" /><span>全部播放 ({{ list.length }})</span>
        </button>
        <button
          type="button"
          class="secondary-btn"
          data-hero-action="shuffle"
          :disabled="list.length === 0"
          @click="shuffle"
        >
          <Icon name="shuffle" /><span>随机试听</span>
        </button>
      </div>
      <p class="hero-description" :title="info.description">{{ info.description }}</p>
    </div>
  </section>
</template>