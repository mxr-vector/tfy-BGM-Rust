/**
 * 应用级配置：Rust 侧从 `config.json` 读取后随 bootstrap 下发。
 * 这里保留一份内置默认值，字段缺失或类型不符时逐项回退。
 */

import { shallowRef } from 'vue';
import type { AppConfig, HeroCopy } from './types';
import type { HeroKind } from './constants';

type PartialRecord = Record<string, unknown>;

const DEFAULT_CONFIG: AppConfig = {
  brand: {
    title: '听风吟 · BGM',
    subtitle: '剪辑音频素材库',
    windowTitle: '听风吟 · TFY BGM',
  },
  player: {
    rates: [0.75, 1, 1.25, 1.5, 2],
    volumeStep: 0.1,
    seekStepSeconds: 5,
    restartThresholdSeconds: 3,
    maxAutoSkips: 3,
    loadTimeoutMs: 20_000,
  },
  ui: {
    all: {
      title: '全部精选视频剪辑 BGM',
      labelEn: 'ALL BGM SOUNDTRACKS',
      description:
        '汇集{categories}等分类的优质背景音乐，覆盖 Vlog、宣传片、故事片、卡点等全场景剪辑需求。',
      resultsTitle: '全部音乐素材',
    },
    favorites: {
      title: '我的收藏夹 · Favorite Tracks',
      labelEn: 'FAVORITES',
      description: '你标记喜欢的配乐都在这里，方便在剪辑过程中随时试听与导出使用。',
      resultsTitle: '我的收藏夹',
    },
  },
};

export const config = shallowRef<AppConfig>(cloneConfig(DEFAULT_CONFIG));

export function applyConfig(incoming: unknown): AppConfig {
  config.value = mergeConfig(DEFAULT_CONFIG, incoming);
  applyDocumentCopy();
  return config.value;
}

export function playerConfig() {
  return config.value.player;
}

/** 取「全部」或「收藏」页的文案。 */
export function heroCopy(kind: HeroKind): HeroCopy {
  return config.value.ui[kind] ?? config.value.ui.all;
}

/** 把 `{name}` 占位符替换为给定值，未知占位符保持原样。 */
export function formatCopy(template: string, values: Record<string, string> = {}): string {
  return String(template ?? '').replace(/\{(\w+)\}/g, (match, key: string) =>
    Object.prototype.hasOwnProperty.call(values, key) ? String(values[key]) : match,
  );
}

export function applyDocumentCopy(): void {
  document.title = config.value.brand.title;
}

function cloneConfig(source: AppConfig): AppConfig {
  return {
    brand: { ...source.brand },
    player: { ...source.player, rates: [...source.player.rates] },
    ui: {
      all: { ...source.ui.all },
      favorites: { ...source.ui.favorites },
    },
  };
}

function asRecord(value: unknown): PartialRecord {
  return value !== null && typeof value === 'object' ? (value as PartialRecord) : {};
}

function mergeConfig(defaults: AppConfig, incoming: unknown): AppConfig {
  const root = asRecord(incoming);
  const brand = asRecord(root.brand);
  const player = asRecord(root.player);
  const ui = asRecord(root.ui);

  return {
    brand: {
      title: textOr(brand.title, defaults.brand.title),
      subtitle: textOr(brand.subtitle, defaults.brand.subtitle),
      windowTitle: textOr(brand.windowTitle, defaults.brand.windowTitle),
    },
    player: {
      rates: ratesOr(player.rates, defaults.player.rates),
      volumeStep: positiveOr(player.volumeStep, defaults.player.volumeStep),
      seekStepSeconds: positiveOr(player.seekStepSeconds, defaults.player.seekStepSeconds),
      restartThresholdSeconds: positiveOr(
        player.restartThresholdSeconds,
        defaults.player.restartThresholdSeconds,
      ),
      maxAutoSkips: skipCountOr(player.maxAutoSkips, defaults.player.maxAutoSkips),
      loadTimeoutMs: positiveOr(player.loadTimeoutMs, defaults.player.loadTimeoutMs),
    },
    ui: {
      all: heroOr(ui.all, defaults.ui.all),
      favorites: heroOr(ui.favorites, defaults.ui.favorites),
    },
  };
}

function heroOr(incoming: unknown, fallback: HeroCopy): HeroCopy {
  const source = asRecord(incoming);
  return {
    title: textOr(source.title, fallback.title),
    labelEn: textOr(source.labelEn, fallback.labelEn),
    description: textOr(source.description, fallback.description),
    resultsTitle: textOr(source.resultsTitle, fallback.resultsTitle),
  };
}

function textOr(value: unknown, fallback: string): string {
  if (typeof value !== 'string') return fallback;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : fallback;
}

function positiveOr(value: unknown, fallback: number): number {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : fallback;
}

function skipCountOr(value: unknown, fallback: number): number {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : fallback;
}

function ratesOr(value: unknown, fallback: number[]): number[] {
  if (!Array.isArray(value)) return [...fallback];
  const rates = value
    .map((rate) => Number(rate))
    .filter((rate) => Number.isFinite(rate) && rate > 0);
  return rates.length > 0 ? rates : [...fallback];
}