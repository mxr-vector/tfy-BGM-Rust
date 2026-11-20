/**
 * 应用级配置的读取与回退。
 *
 * 配置由 Rust 侧从 `config.json` 读取后随 bootstrap 下发；这里保存一份内置默认值，
 * 用于首次渲染与配置缺失/字段不全时逐项回退。界面文案统一从 `heroCopy` 取，
 * 播放器行为参数统一从 `playerConfig` 取，避免在业务模块里散落字面量。
 */

const DEFAULT_CONFIG = {
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
    loadTimeoutMs: 20000,
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

let current = mergeConfig(DEFAULT_CONFIG, null);

/** 应用 Rust 下发的配置；缺字段时逐项回退内置默认值。 */
export function applyConfig(incoming) {
  current = mergeConfig(DEFAULT_CONFIG, incoming);
  applyDocumentCopy();
  return current;
}

export function appConfig() {
  return current;
}

export function playerConfig() {
  return current.player;
}

/** 取「全部」或「收藏」页的文案。 */
export function heroCopy(kind) {
  return current.ui[kind] ?? current.ui.all;
}

/** 把 `{name}` 占位符替换为给定值，未知占位符保持原样。 */
export function formatCopy(template, values = {}) {
  return String(template ?? '').replace(/\{(\w+)\}/g, (match, key) =>
    Object.prototype.hasOwnProperty.call(values, key) ? String(values[key]) : match,
  );
}

/** 把配置里的品牌文案写到静态骨架（文档标题、顶部标题栏）。 */
export function applyDocumentCopy() {
  document.title = current.brand.title;
  const title = document.getElementById('brand-title');
  if (title) title.textContent = current.brand.title;
  const subtitle = document.getElementById('brand-subtitle');
  if (subtitle) subtitle.textContent = current.brand.subtitle;
}

function mergeConfig(defaults, incoming) {
  const brand = incoming?.brand ?? {};
  const player = incoming?.player ?? {};
  const ui = incoming?.ui ?? {};

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

function heroOr(incoming, fallback) {
  const source = incoming ?? {};
  return {
    title: textOr(source.title, fallback.title),
    labelEn: textOr(source.labelEn, fallback.labelEn),
    description: textOr(source.description, fallback.description),
    resultsTitle: textOr(source.resultsTitle, fallback.resultsTitle),
  };
}

function textOr(value, fallback) {
  if (typeof value !== 'string') return fallback;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : fallback;
}

function positiveOr(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? number : fallback;
}

function skipCountOr(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : fallback;
}

function ratesOr(value, fallback) {
  if (!Array.isArray(value)) return [...fallback];
  const rates = value
    .map((rate) => Number(rate))
    .filter((rate) => Number.isFinite(rate) && rate > 0);
  return rates.length > 0 ? rates : [...fallback];
}
