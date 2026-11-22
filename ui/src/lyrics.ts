/**
 * 桌面歌词悬浮窗（独立 WebView 窗口，非主 Vue 应用）。
 *
 * 数据来源：Rust 广播的 `desktop-lyrics-data` 快照；窗口启动时主动拉取一次，
 * 避免错过创建前的最后一次广播。播放进度在本地按 100ms 平滑推进，
 * 后端每 1 秒校准一次 position，暂停 / 切歌 / 停止由快照状态直接驱动。
 *
 * 另外负责：定时巡检置顶状态；字体 / 字号 / 颜色 / 背景与窗口宽高的自定义及持久化
 * （含用户手动拖拽边缘后的尺寸记忆）；以及拦截拖动区域的双击，避免误触发窗口最大化。
 */

import {
  COMMANDS,
  DEFAULT_FONT_STACK,
  EVENTS,
  LYRICS_SIZE_LIMITS,
} from '@/lib/constants';
import { invoke, listen } from '@/lib/tauri';
import type {
  LyricLine,
  LyricsBootstrap,
  LyricsHover,
  LyricsHotZone,
  LyricsSnapshot,
  LyricsStyle,
  LyricsWindowSize,
  SystemFont,
} from '@/lib/types';

const EMPTY: LyricsSnapshot = {
  trackKey: null,
  title: '',
  playing: false,
  position: 0,
  rate: 1,
  lines: [],
  hasLyrics: false,
};

/** 显示样式保存在歌词窗口自己的 localStorage 中，重启应用后仍生效。 */
const STORAGE_KEY = 'tfy-bgm:desktop-lyrics-style';
/** 存储结构版本：布局 / 默认配色调整时 +1，旧存储会做一次兼容迁移。 */
const STYLE_VERSION = 4;
const FONT_DEFAULT_VALUE = '__default__';
const DEFAULT_STYLE: LyricsStyle = {
  fontFamily: '',
  fontSize: LYRICS_SIZE_LIMITS.fontSize.default,
  highlight: '#19d98b',
  background: 62,
  width: LYRICS_SIZE_LIMITS.width.default,
  height: LYRICS_SIZE_LIMITS.height.default,
  locked: false,
};
const COLOR_PRESETS = [
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
/** 扫描不到系统字体时的兜底选项。 */
const FALLBACK_FONTS = ['Microsoft YaHei', 'PingFang SC', 'Noto Sans CJK SC', 'Segoe UI'];

function element<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`缺少歌词窗口节点：${id}`);
  return node as unknown as T;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

const titleEl = element('lyrics-title');
const statusEl = element('lyrics-status');
const rootEl = element('lyrics');
const prevEl = element('lyrics-prev');
const currentEl = element('lyrics-current');
const nextEl = element('lyrics-next');
const closeBtn = element<HTMLButtonElement>('lyrics-close');
const lockBtn = element<HTMLButtonElement>('lyrics-lock');
const unlockBtn = element<HTMLButtonElement>('lyrics-unlock');
const sizeDownBtn = element<HTMLButtonElement>('lyrics-size-down');
const sizeUpBtn = element<HTMLButtonElement>('lyrics-size-up');
const sizeBadge = element('lyrics-size-badge');
const settingsBtn = element<HTMLButtonElement>('lyrics-settings');
const panelEl = element('lyrics-panel');
const panelCloseBtn = element<HTMLButtonElement>('lp-close');
const panelLockBtn = element<HTMLButtonElement>('lp-lock');
const fontSelect = element<HTMLSelectElement>('lp-font');
const fontState = element('lp-font-state');
const sizeInput = element<HTMLInputElement>('lp-size');
const sizeValue = element('lp-size-value');
const bgInput = element<HTMLInputElement>('lp-bg');
const bgValue = element('lp-bg-value');
const widthInput = element<HTMLInputElement>('lp-width');
const widthValue = element('lp-width-value');
const heightInput = element<HTMLInputElement>('lp-height');
const heightValue = element('lp-height-value');
const colorsEl = element('lp-colors');
const topState = element('lp-top-state');
const repinBtn = element<HTMLButtonElement>('lp-repin');
const resetBtn = element<HTMLButtonElement>('lp-reset');

let snapshot: LyricsSnapshot = EMPTY;
let basePosition = 0;
let baseStamp = performance.now();
let playing = false;
let rate = 1;
let fontsLoaded = false;

function setText(node: HTMLElement, text: string): void {
  if (node.textContent !== text) node.textContent = text;
}

/** 本地按墙钟时间 × 播放倍速推进，避免依赖高频 IPC；后端每秒校准。 */
function positionNow(): number {
  return playing
    ? basePosition + ((performance.now() - baseStamp) / 1000) * rate
    : basePosition;
}

function activeIndex(position: number): number {
  const lines = snapshot.lines;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]!;
    if (position >= line.start && position < line.end) return index;
  }
  return -1;
}

function lineProgress(line: LyricLine, position: number): number {
  const span = line.end - line.start;
  if (!(span > 0)) return position >= line.end ? 1 : 0;
  return Math.min(1, Math.max(0, (position - line.start) / span));
}

const unitsCache = new Map<string, string[]>();

/** 按单词切分（中文为词），用于逐词计算「已唱 / 未唱」。 */
function splitUnits(text: string): string[] {
  const cached = unitsCache.get(text);
  if (cached) return cached;
  let units: string[] = [];
  try {
    const Segmenter = (Intl as unknown as {
      Segmenter?: new (
        locales?: string,
        options?: { granularity: 'word' | 'grapheme' },
      ) => { segment: (input: string) => Iterable<{ segment: string }> };
    }).Segmenter;
    if (Segmenter) {
      const segmenter = new Segmenter('zh-Hans', { granularity: 'word' });
      units = Array.from(segmenter.segment(text), (part) => part.segment);
    }
  } catch {
    units = [];
  }
  if (units.length === 0) units = Array.from(text);
  // 歌词长度有限；缓存避免每 100ms 重复分词，超长列表直接淘汰旧条目。
  if (unitsCache.size > 256) unitsCache.clear();
  unitsCache.set(text, units);
  return units;
}

/** 当前行 DOM 签名：文本不变时只更新每个词的渐进颜色，不重建 span。 */
let currentLineSignature = '';
let currentLineText = '';
let currentWordSpans: HTMLSpanElement[] = [];

const UNSUNG_TEXT = '#d7e9e1';
const UNSUNG_RGB: readonly [number, number, number] = [0xd7, 0xe9, 0xe1];
let highlightCacheHex = '';
let highlightCacheRgb: [number, number, number] = [0, 0, 0];

function highlightRgb(): [number, number, number] {
  if (highlightCacheHex === style.highlight) return highlightCacheRgb;
  const value = style.highlight.replace('#', '');
  highlightCacheRgb = [
    Number.parseInt(value.slice(0, 2), 16),
    Number.parseInt(value.slice(2, 4), 16),
    Number.parseInt(value.slice(4, 6), 16),
  ];
  highlightCacheHex = style.highlight;
  return highlightCacheRgb;
}

/** 未唱色 → 高亮色之间按进度混合，形成逐词连续推进而不是整词跳变。 */
function wordColor(fraction: number): string {
  if (fraction <= 0) return UNSUNG_TEXT;
  if (fraction >= 1) return style.highlight;
  const [red, green, blue] = highlightRgb();
  const mix = (from: number, to: number): number =>
    Math.round(from + (to - from) * fraction);
  return `rgb(${mix(UNSUNG_RGB[0], red)}, ${mix(UNSUNG_RGB[1], green)}, ${mix(UNSUNG_RGB[2], blue)})`;
}

function resetCurrentLine(text: string, signature: string): void {
  if (currentLineSignature === signature) return;
  currentLineSignature = signature;
  currentLineText = '';
  currentWordSpans = [];
  currentEl.textContent = text;
}

function renderCurrentLine(line: LyricLine, progress: number, force: boolean): void {
  const units = splitUnits(line.text);
  if (force || currentLineText !== line.text || currentWordSpans.length !== units.length) {
    currentLineText = line.text;
    const fragment = document.createDocumentFragment();
    for (const unit of units) {
      const span = document.createElement('span');
      span.className = 'lyrics-word is-unsung';
      span.textContent = unit;
      span.dataset.sung = '0';
      span.dataset.color = UNSUNG_TEXT;
      span.style.color = UNSUNG_TEXT;
      fragment.append(span);
    }
    currentEl.replaceChildren(fragment);
    currentWordSpans = Array.from(currentEl.children) as HTMLSpanElement[];
  }
  // 每个词拥有等分的演唱时间窗；当前词按窗内进度插值，形成渐进过渡。
  const total = units.length;
  for (let index = 0; index < total; index += 1) {
    const span = currentWordSpans[index]!;
    const fraction = Math.min(1, Math.max(0, progress * total - index));
    const color = wordColor(fraction);
    if (span.dataset.color !== color) {
      span.style.color = color;
      span.dataset.color = color;
    }
    const sung = fraction >= 1;
    if (span.dataset.sung !== (sung ? '1' : '0')) {
      span.dataset.sung = sung ? '1' : '0';
      span.classList.toggle('is-sung', sung);
      span.classList.toggle('is-unsung', !sung);
    }
  }
  currentLineSignature = `${line.start}|${line.text}`;
}

/** 文本变化后重新计算锁定热区；逐词推进不会触发。 */
let lastZoneSignature = '';

function render(force = false): void {
  if (!snapshot.trackKey) {
    // 停止播放：清空歌词，保留明确的空闲提示。
    setText(titleEl, '未在播放');
    setText(statusEl, '');
    setText(prevEl, '');
    resetCurrentLine('等待播放曲目…', 'idle');
    setText(nextEl, '');
    trackZoneChange();
    return;
  }

  setText(titleEl, snapshot.title || '当前曲目');
  setText(statusEl, playing ? '播放中' : '已暂停');

  if (!snapshot.hasLyrics || snapshot.lines.length === 0) {
    setText(prevEl, '');
    resetCurrentLine('当前曲目暂无字幕', 'empty');
    setText(nextEl, '请在左侧「字幕生成」中生成字幕');
    trackZoneChange();
    return;
  }

  const lines = snapshot.lines;
  const position = positionNow();
  const index = activeIndex(position);
  setText(prevEl, index > 0 ? lines[index - 1]!.text : '');
  setText(nextEl, index >= 0 && index + 1 < lines.length ? lines[index + 1]!.text : '');
  if (index >= 0) {
    const line = lines[index]!;
    renderCurrentLine(line, lineProgress(line, position), force);
  } else {
    resetCurrentLine('♪', 'gap');
  }
  trackZoneChange();
}

function trackZoneChange(): void {
  const signature = `${prevEl.textContent}|${currentEl.textContent}|${nextEl.textContent}`;
  if (signature === lastZoneSignature) return;
  lastZoneSignature = signature;
  scheduleHotZone();
}

function apply(next: LyricsSnapshot): void {
  snapshot = next;
  basePosition = next.position;
  baseStamp = performance.now();
  playing = next.playing;
  rate = Number.isFinite(next.rate) && next.rate > 0 ? next.rate : 1;
  render(true);
}

// ---------------- 显示样式 ----------------

function persistStyle(next: LyricsStyle): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...next, version: STYLE_VERSION }));
  } catch {
    // 隐私模式 / 存储被禁用时仅本次会话生效。
  }
}

function readStyle(): LyricsStyle {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...DEFAULT_STYLE };
    const parsed = JSON.parse(raw) as Partial<LyricsStyle> & { version?: number };
    const style: LyricsStyle = {
      fontFamily: typeof parsed.fontFamily === 'string' ? parsed.fontFamily : '',
      fontSize:
        typeof parsed.fontSize === 'number' && Number.isFinite(parsed.fontSize)
          ? clamp(
              Math.round(parsed.fontSize),
              LYRICS_SIZE_LIMITS.fontSize.min,
              LYRICS_SIZE_LIMITS.fontSize.max,
            )
          : DEFAULT_STYLE.fontSize,
      highlight:
        typeof parsed.highlight === 'string' && /^#[0-9a-f]{6}$/i.test(parsed.highlight)
          ? parsed.highlight
          : DEFAULT_STYLE.highlight,
      background:
        typeof parsed.background === 'number' && Number.isFinite(parsed.background)
          ? clamp(Math.round(parsed.background), 0, 90)
          : DEFAULT_STYLE.background,
      width:
        typeof parsed.width === 'number' && Number.isFinite(parsed.width)
          ? clamp(
              Math.round(parsed.width),
              LYRICS_SIZE_LIMITS.width.min,
              LYRICS_SIZE_LIMITS.width.max,
            )
          : DEFAULT_STYLE.width,
      height:
        typeof parsed.height === 'number' && Number.isFinite(parsed.height)
          ? clamp(
              Math.round(parsed.height),
              LYRICS_SIZE_LIMITS.height.min,
              LYRICS_SIZE_LIMITS.height.max,
            )
          : DEFAULT_STYLE.height,
      locked: parsed.locked === true,
    };
    // 旧版的默认高度偏大（也曾被最小化事件写成 110）；升级时把高度恢复为新默认，
    // 字体 / 字号 / 背景等个性化设置保持不变；预设高亮色统一加深一个色度。
    if (parsed.version !== STYLE_VERSION) {
      style.height = DEFAULT_STYLE.height;
      style.highlight = LEGACY_COLOR_MAP[style.highlight.toLowerCase()] ?? style.highlight;
      persistStyle(style);
    }
    return style;
  } catch {
    return { ...DEFAULT_STYLE };
  }
}

let style = readStyle();

function saveStyle(): void {
  persistStyle(style);
}

/** 族名 → CSS font-family；始终保留默认字体栈兜底缺字。 */
function cssFont(family: string): string {
  if (!family || family === FONT_DEFAULT_VALUE) return DEFAULT_FONT_STACK;
  const safe = family.replace(/["\\]/g, '');
  return `"${safe}", ${DEFAULT_FONT_STACK}`;
}

function hexToRgba(hex: string, alpha: number): string {
  const value = hex.replace('#', '');
  const red = Number.parseInt(value.slice(0, 2), 16);
  const green = Number.parseInt(value.slice(2, 4), 16);
  const blue = Number.parseInt(value.slice(4, 6), 16);
  return `rgba(${red}, ${green}, ${blue}, ${alpha})`;
}

function renderSwatches(): void {
  colorsEl.replaceChildren();
  for (const preset of COLOR_PRESETS) {
    const swatch = document.createElement('button');
    swatch.type = 'button';
    swatch.className =
      'lp-swatch' +
      (preset.value.toLowerCase() === style.highlight.toLowerCase() ? ' is-active' : '');
    swatch.style.background = preset.value;
    swatch.title = preset.label;
    swatch.setAttribute('aria-label', preset.label);
    swatch.addEventListener('click', () => {
      style.highlight = preset.value;
      saveStyle();
      applyStyle();
    });
    colorsEl.append(swatch);
  }
}

/** 把当前样式写进 CSS 变量并同步控件显示。 */
function applyStyle(): void {
  const root = document.documentElement.style;
  root.setProperty('--lyrics-font', cssFont(style.fontFamily));
  root.setProperty('--lyrics-size', `${style.fontSize}px`);
  root.setProperty('--lyrics-side-size', `${Math.round(style.fontSize * 0.58)}px`);
  root.setProperty('--lyrics-highlight', style.highlight);
  root.setProperty('--lyrics-sung-line', hexToRgba(style.highlight, 0.55));
  root.setProperty('--lyrics-glow', hexToRgba(style.highlight, 0.45));
  const top = style.background / 100;
  root.setProperty('--lyrics-bg-top', `rgba(4, 14, 11, ${top.toFixed(2)})`);
  root.setProperty('--lyrics-bg-bottom', `rgba(4, 14, 11, ${(top * 0.55).toFixed(2)})`);

  sizeInput.value = String(style.fontSize);
  sizeValue.textContent = `${style.fontSize}px`;
  sizeBadge.textContent = String(style.fontSize);
  sizeBadge.title = `当前字体大小 ${style.fontSize}px`;
  sizeDownBtn.disabled = style.fontSize <= LYRICS_SIZE_LIMITS.fontSize.min;
  sizeUpBtn.disabled = style.fontSize >= LYRICS_SIZE_LIMITS.fontSize.max;
  bgInput.value = String(style.background);
  bgValue.textContent = `${style.background}%`;
  widthInput.value = String(style.width);
  widthValue.textContent = `${style.width}px`;
  heightInput.value = String(style.height);
  heightValue.textContent = `${style.height}px`;
  if (fontsLoaded) fontSelect.value = style.fontFamily || FONT_DEFAULT_VALUE;
  renderSwatches();
  // 字号 / 字体变化后重建当前行，已唱词比例与热区随新布局刷新。
  render(true);
  scheduleHotZone();
}

/** 后台扫描系统字体；面板打开时直接使用结果，避免空白 / 卡顿。 */
async function loadFonts(): Promise<void> {
  if (fontsLoaded) return;
  fontsLoaded = true;
  fontSelect.replaceChildren();
  fontSelect.disabled = true;
  fontState.textContent = '正在读取系统字体…';

  const fragment = document.createDocumentFragment();
  const addOption = (value: string, label: string): void => {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    fragment.append(option);
  };
  addOption(FONT_DEFAULT_VALUE, '系统默认（自动匹配中文字体）');

  let families: { value: string; label: string }[] = FALLBACK_FONTS.map((family) => ({
    value: family,
    label: family,
  }));
  let scanned = false;
  try {
    const fonts = await invoke<SystemFont[]>(COMMANDS.systemFonts);
    if (fonts.length > 0) {
      families = fonts.map((font) => ({ value: font.family, label: font.label || font.family }));
      scanned = true;
    }
  } catch {
    // 扫描失败时仍提供通用字体族。
  }
  // 后端已按「中文字体优先」排序，这里保持原顺序并按族名去重。
  const seen = new Set<string>([FONT_DEFAULT_VALUE]);
  for (const font of families) {
    const family = font.value.trim();
    if (!family || seen.has(family)) continue;
    seen.add(family);
    addOption(family, font.label || family);
  }
  fontSelect.append(fragment);
  fontSelect.disabled = false;

  // 配置里的字体本机已不存在时回退默认，避免出现「下拉空白」。
  const wanted = style.fontFamily || FONT_DEFAULT_VALUE;
  if (!seen.has(wanted)) {
    style.fontFamily = '';
    saveStyle();
    fontState.textContent = '原字体不可用，已回退系统默认';
    applyStyle();
  } else {
    fontState.textContent = scanned ? `共 ${seen.size - 1} 款字体` : '使用内置字体清单';
  }
  fontSelect.value = style.fontFamily || FONT_DEFAULT_VALUE;
  fontSelect.dataset.ready = '1';
}

function setPanel(open: boolean): void {
  if (style.locked) open = false;
  panelEl.hidden = !open;
  settingsBtn.classList.toggle('is-open', open);
  rootEl.classList.toggle('is-panel-open', open);
  if (open) {
    void loadFonts();
    scheduleHotZone();
  }
}

// ---------------- 锁定 / 鼠标穿透 ----------------

/** 锁定态歌词热区（窗口逻辑像素），交给 Rust 做光标命中检测。 */
let hotZone: LyricsHotZone = { x: 0, y: 0, width: 0, height: 0 };
let hotTimer = 0;
let lockPushTimer = 0;

function scheduleHotZone(): void {
  window.clearTimeout(hotTimer);
  hotTimer = window.setTimeout(measureHotZone, 120);
}

function measureHotZone(): void {
  const viewportWidth = window.innerWidth;
  const viewportHeight = window.innerHeight;
  let top = Number.POSITIVE_INFINITY;
  let bottom = Number.NEGATIVE_INFINITY;
  for (const node of [prevEl, currentEl, nextEl]) {
    const rect = node.getBoundingClientRect();
    if (rect.height <= 0) continue;
    top = Math.min(top, rect.top);
    bottom = Math.max(bottom, rect.bottom);
  }
  if (!Number.isFinite(top) || !Number.isFinite(bottom)) {
    top = viewportHeight * 0.28;
    bottom = viewportHeight * 0.72;
  }
  const padX = 12;
  const padY = 8;
  hotZone = {
    x: padX,
    y: Math.max(0, top - padY),
    width: Math.max(24, viewportWidth - padX * 2),
    height: Math.max(24, bottom - top + padY * 2),
  };
  pushLockState();
}

function pushLockState(immediate = false): void {
  window.clearTimeout(lockPushTimer);
  const send = (): void => {
    void invoke(COMMANDS.setDesktopLyricsLock, {
      locked: style.locked,
      hotZone,
    }).catch(() => {
      // 窗口可能正在关闭；下次打开会重新上报。
    });
  };
  if (immediate) send();
  else lockPushTimer = window.setTimeout(send, 40);
}

function applyLockState(): void {
  rootEl.classList.toggle('is-locked', style.locked);
  if (style.locked) {
    // 锁定后不再允许拖动，避免临时取消穿透时误拖窗口。
    rootEl.removeAttribute('data-tauri-drag-region');
  } else {
    rootEl.setAttribute('data-tauri-drag-region', 'deep');
  }
  unlockBtn.hidden = true;
  lockBtn.classList.toggle('is-open', style.locked);
  if (style.locked) setPanel(false);
}

function setLocked(locked: boolean): void {
  const changed = style.locked !== locked;
  style.locked = locked;
  if (changed) saveStyle();
  applyLockState();
  scheduleHotZone();
  // 锁定 / 解锁必须立即同步穿透状态，不能等防抖。
  pushLockState(true);
}

function showUnlock(hovered: boolean): void {
  unlockBtn.hidden = !(style.locked && hovered);
}

// ---------------- 窗口尺寸 ----------------

/** 把当前偏好尺寸应用到窗口；`place` 为 true 时重新贴到屏幕底部居中。 */
async function resizeWindow(place = false): Promise<void> {
  try {
    await invoke(COMMANDS.resizeDesktopLyrics, {
      width: style.width,
      height: style.height,
    });
    if (place) {
      await invoke(COMMANDS.placeDesktopLyrics, {
        width: style.width,
        height: style.height,
      });
    }
    scheduleHotZone();
  } catch {
    // 窗口可能正在关闭；下次开启时会重新应用已保存的尺寸。
  }
}

let sizeSaveTimer = 0;

/** 用户拖拽边缘或滑块调整后记住尺寸；只更新状态，不再回写窗口以免形成循环。 */
function rememberSize(next: LyricsWindowSize): void {
  // 最小化（Win+M / 显示桌面）时 Windows 广播的是 0×0，不能当成用户拖拽结果；
  // 否则会被钳到最小值并持久化，下次打开歌词窗就只剩一条细带。
  if (next.width <= 0 || next.height <= 0) return;
  if (!Number.isFinite(next.width) || !Number.isFinite(next.height)) return;
  style.width = clamp(
    Math.round(next.width),
    LYRICS_SIZE_LIMITS.width.min,
    LYRICS_SIZE_LIMITS.width.max,
  );
  style.height = clamp(
    Math.round(next.height),
    LYRICS_SIZE_LIMITS.height.min,
    LYRICS_SIZE_LIMITS.height.max,
  );
  // 只同步尺寸控件，避免拖拽期间高频重建色块等 DOM。
  widthInput.value = String(style.width);
  widthValue.textContent = `${style.width}px`;
  heightInput.value = String(style.height);
  heightValue.textContent = `${style.height}px`;
  // 拖拽会连续触发 Resized，写 localStorage 做 200ms 防抖。
  window.clearTimeout(sizeSaveTimer);
  sizeSaveTimer = window.setTimeout(saveStyle, 200);
  scheduleHotZone();
}

// ---------------- 始终置顶 ----------------

/** 重新声明 Z 序；Rust 侧在窗口失焦时也会主动调用一次。 */
async function enforceTop(): Promise<void> {
  try {
    const ok = await invoke<boolean>(COMMANDS.keepDesktopLyricsOnTop);
    setText(topState, ok ? '已置顶 · 始终在最前' : '置顶失效，请重试');
    topState.classList.toggle('is-bad', !ok);
  } catch {
    setText(topState, '置顶状态未知');
    topState.classList.add('is-bad');
  }
}

// ---------------- 初始化 ----------------

const FONT_SIZE_STEP = 2;

/** 工具栏 A− / A+：不打开设置面板也能直接调字号，范围与滑杆一致。 */
function stepFontSize(delta: number): void {
  const next = clamp(
    style.fontSize + delta,
    LYRICS_SIZE_LIMITS.fontSize.min,
    LYRICS_SIZE_LIMITS.fontSize.max,
  );
  if (next === style.fontSize) return;
  style.fontSize = next;
  saveStyle();
  applyStyle();
}

function installControls(): void {
  closeBtn.addEventListener('click', () => {
    void invoke(COMMANDS.setDesktopLyricsEnabled, { enabled: false });
  });
  settingsBtn.addEventListener('click', () => setPanel(panelEl.hidden));
  panelCloseBtn.addEventListener('click', () => setPanel(false));
  lockBtn.addEventListener('click', () => setLocked(true));
  panelLockBtn.addEventListener('click', () => setLocked(true));
  unlockBtn.addEventListener('click', () => setLocked(false));
  sizeDownBtn.addEventListener('click', () => stepFontSize(-FONT_SIZE_STEP));
  sizeUpBtn.addEventListener('click', () => stepFontSize(FONT_SIZE_STEP));

  // 穿透未生效的平台（或轮询事件偶尔丢失）用 DOM 悬停兜底显示解锁入口。
  for (const node of [prevEl, currentEl, nextEl]) {
    node.addEventListener('mouseenter', () => {
      if (style.locked) showUnlock(true);
    });
  }
  rootEl.addEventListener('mouseleave', () => {
    if (style.locked) showUnlock(false);
  });

  fontSelect.addEventListener('change', () => {
    style.fontFamily = fontSelect.value === FONT_DEFAULT_VALUE ? '' : fontSelect.value;
    saveStyle();
    applyStyle();
    if (style.fontFamily) fontState.textContent = `已选择 ${fontSelect.selectedOptions[0]?.textContent ?? style.fontFamily}`;
  });
  sizeInput.addEventListener('input', () => {
    style.fontSize = clamp(
      Number(sizeInput.value) || DEFAULT_STYLE.fontSize,
      LYRICS_SIZE_LIMITS.fontSize.min,
      LYRICS_SIZE_LIMITS.fontSize.max,
    );
    saveStyle();
    applyStyle();
  });
  bgInput.addEventListener('input', () => {
    style.background = clamp(Number(bgInput.value) || 0, 0, 90);
    saveStyle();
    applyStyle();
  });
  widthInput.addEventListener('input', () => {
    style.width = clamp(
      Number(widthInput.value) || DEFAULT_STYLE.width,
      LYRICS_SIZE_LIMITS.width.min,
      LYRICS_SIZE_LIMITS.width.max,
    );
    saveStyle();
    applyStyle();
    void resizeWindow();
  });
  heightInput.addEventListener('input', () => {
    style.height = clamp(
      Number(heightInput.value) || DEFAULT_STYLE.height,
      LYRICS_SIZE_LIMITS.height.min,
      LYRICS_SIZE_LIMITS.height.max,
    );
    saveStyle();
    applyStyle();
    void resizeWindow();
  });
  repinBtn.addEventListener('click', () => void enforceTop());
  resetBtn.addEventListener('click', () => {
    style = { ...DEFAULT_STYLE };
    saveStyle();
    applyStyle();
    applyLockState();
    pushLockState(true);
    // 「恢复默认」同时还原尺寸与底部居中位置。
    void resizeWindow(true);
  });

  // Tauri 的拖动脚本在 document 上监听 mousedown，双击拖拽区域会触发最大化。
  // 窗口已禁用最大化，这里在 window 捕获阶段再截断双击，避免无谓 IPC 与文本选中。
  window.addEventListener(
    'mousedown',
    (event) => {
      if (event.button === 0 && event.detail >= 2) event.stopPropagation();
    },
    true,
  );
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) void enforceTop();
  });
}

async function init(): Promise<void> {
  installControls();
  applyStyle();
  applyLockState();
  // 先按偏好尺寸调整并贴到底部居中，再开始拉取数据；窗口在页面加载完成后才显示，
  // 因此用户看到的就是最终尺寸与位置。
  void resizeWindow(true);
  // 字体扫描放到后台，面板首次打开时列表已就绪；失败仍会回退到内置字体清单。
  window.setTimeout(() => void loadFonts(), 350);
  await listen<LyricsSnapshot>(EVENTS.desktopLyricsData, apply);
  await listen<LyricsWindowSize>(EVENTS.desktopLyricsSize, rememberSize);
  await listen<LyricsHover>(EVENTS.desktopLyricsHover, ({ hovered }) => showUnlock(hovered));
  try {
    const boot = await invoke<LyricsBootstrap>(COMMANDS.desktopLyricsSnapshot);
    apply(boot.snapshot);
  } catch {
    render();
  }
  scheduleHotZone();
  pushLockState(true);
  // 本地刷新当前行与逐词渐进进度；文本未变化时只更新颜色，不重建 DOM。
  window.setInterval(render, 50);
  void enforceTop();
  // 全屏应用 / 虚拟桌面切换可能把 topmost 降级，定时重新声明。
  window.setInterval(() => void enforceTop(), 3_000);
  render();
}

void init();
