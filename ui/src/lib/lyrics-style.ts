/**
 * 桌面歌词显示样式：字体、字号、配色、窗口尺寸与锁定状态。
 *
 * 样式保存在与歌词窗口同源的 localStorage 中，独立设置悬浮窗与歌词窗口的
 * 拖拽调整共用同一份数据；重启应用后仍生效。
 */

import { LYRICS_SIZE_LIMITS } from './constants';
import type { LyricsStyle } from './types';

export const LYRICS_STYLE_STORAGE_KEY = 'tfy-bgm:desktop-lyrics-style';
/**
 * 存储结构版本：布局 / 默认配色调整时 +1，旧存储会做一次兼容迁移。
 * 新增「带默认值兜底」的字段（如 leadSeconds）不提升版本：sanitize 会自动补齐，
 * 否则旧存储会走迁移分支，把用户已保存的窗口高度与自定义高亮色一并重置。
 */
export const LYRICS_STYLE_VERSION = 4;
/** 字体下拉里「系统默认」的选项值；样式里用空串表示。 */
export const LYRICS_FONT_DEFAULT = '__default__';

/** 已唱标记提前量的上限（秒）；sanitize 与设置窗共用，避免两处各写一份范围。 */
export const LYRICS_LEAD_MAX = 2;
/** 已唱标记提前量的可选档位（秒）：0 表示严格按原始时间轴渲染。 */
export const LYRICS_LEAD_OPTIONS = [0, 1, LYRICS_LEAD_MAX] as const;

export const DEFAULT_LYRICS_STYLE: LyricsStyle = {
  fontFamily: '',
  fontSize: LYRICS_SIZE_LIMITS.fontSize.default,
  highlight: '#19d98b',
  background: 62,
  width: LYRICS_SIZE_LIMITS.width.default,
  height: LYRICS_SIZE_LIMITS.height.default,
  locked: false,
  leadSeconds: 1,
};

export const LYRICS_COLOR_PRESETS: readonly { label: string; value: string }[] = [
  { label: '薄荷绿', value: '#19d98b' },
  { label: '纯白', value: '#e6f4ee' },
  { label: '暖黄', value: '#efb32e' },
  { label: '樱粉', value: '#ee6f9f' },
  { label: '天蓝', value: '#5fa8e8' },
];

/** 旧版预设颜色 → 加深一个色度后的新预设，迁移时不改动用户自定义颜色。 */
const LEGACY_COLOR_MAP: Readonly<Record<string, string>> = {
  '#6ff3bc': '#19d98b',
  '#ffffff': '#e6f4ee',
  '#ffd479': '#efb32e',
  '#ff9ec4': '#ee6f9f',
  '#8ecbff': '#5fa8e8',
};

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function finite(value: unknown, fallback: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : fallback;
}

/** 把任意来源（存储 / 事件负载）钳制为合法样式，避免脏数据写进窗口。 */
export function sanitizeLyricsStyle(input: Partial<LyricsStyle> | null | undefined): LyricsStyle {
  const source = input ?? {};
  return {
    fontFamily: typeof source.fontFamily === 'string' ? source.fontFamily : '',
    fontSize: clamp(
      Math.round(finite(source.fontSize, DEFAULT_LYRICS_STYLE.fontSize)),
      LYRICS_SIZE_LIMITS.fontSize.min,
      LYRICS_SIZE_LIMITS.fontSize.max,
    ),
    highlight:
      typeof source.highlight === 'string' && /^#[0-9a-f]{6}$/i.test(source.highlight)
        ? source.highlight.toLowerCase()
        : DEFAULT_LYRICS_STYLE.highlight,
    background: clamp(Math.round(finite(source.background, DEFAULT_LYRICS_STYLE.background)), 0, 90),
    width: clamp(
      Math.round(finite(source.width, DEFAULT_LYRICS_STYLE.width)),
      LYRICS_SIZE_LIMITS.width.min,
      LYRICS_SIZE_LIMITS.width.max,
    ),
    height: clamp(
      Math.round(finite(source.height, DEFAULT_LYRICS_STYLE.height)),
      LYRICS_SIZE_LIMITS.height.min,
      LYRICS_SIZE_LIMITS.height.max,
    ),
    locked: source.locked === true,
    leadSeconds: clamp(
      Math.round(finite(source.leadSeconds, DEFAULT_LYRICS_STYLE.leadSeconds)),
      0,
      LYRICS_LEAD_MAX,
    ),
  };
}

export function readLyricsStyle(): LyricsStyle {
  try {
    const raw = localStorage.getItem(LYRICS_STYLE_STORAGE_KEY);
    if (!raw) return { ...DEFAULT_LYRICS_STYLE };
    const parsed = JSON.parse(raw) as Partial<LyricsStyle> & { version?: number };
    const style = sanitizeLyricsStyle(parsed);
    // 旧版的默认高度偏大（也曾被最小化事件写成 110）；升级时把高度恢复为新默认，
    // 字体 / 字号 / 背景等个性化设置保持不变；预设高亮色统一加深一个色度。
    if (parsed.version !== LYRICS_STYLE_VERSION) {
      style.height = DEFAULT_LYRICS_STYLE.height;
      style.highlight = LEGACY_COLOR_MAP[style.highlight] ?? style.highlight;
      writeLyricsStyle(style);
    }
    return style;
  } catch {
    return { ...DEFAULT_LYRICS_STYLE };
  }
}

export function writeLyricsStyle(style: LyricsStyle): void {
  try {
    localStorage.setItem(
      LYRICS_STYLE_STORAGE_KEY,
      JSON.stringify({ ...style, version: LYRICS_STYLE_VERSION }),
    );
  } catch {
    // 隐私模式 / 存储被禁用时仅本次会话生效。
  }
}