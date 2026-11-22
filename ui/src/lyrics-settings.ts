/**
 * 桌面歌词显示设置悬浮窗（独立 WebView 窗口）。
 *
 * 由歌词窗口的「Aa」打开，贴在歌词条上方；歌词条自身尺寸不变。
 * 修改会写入共享的歌词样式并广播 `desktop-lyrics-style`，歌词窗口立即应用；
 * 锁定 / 恢复默认需要歌词热区与窗口位置，因此以 `desktop-lyrics-command` 交回歌词窗口执行。
 */

import { COMMANDS, EVENTS } from '@/lib/constants';
import {
  LYRICS_COLOR_PRESETS,
  LYRICS_FONT_DEFAULT,
  sanitizeLyricsStyle,
  readLyricsStyle,
  writeLyricsStyle,
} from '@/lib/lyrics-style';
import { emit, getCurrentWindow, invoke, listen } from '@/lib/tauri';
import type { LyricsCommand, LyricsStyle, LyricsWindowSize, SystemFont } from '@/lib/types';

/** 扫描不到系统字体时的兜底选项。 */
const FALLBACK_FONTS = ['Microsoft YaHei', 'PingFang SC', 'Noto Sans CJK SC', 'Segoe UI'];

function element<T extends HTMLElement = HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`缺少设置窗口节点：${id}`);
  return node as unknown as T;
}

const closeBtn = element<HTMLButtonElement>('close');
const sizeInput = element<HTMLInputElement>('size');
const sizeValue = element('size-value');
const fontSelect = element<HTMLSelectElement>('font');
const fontState = element('font-state');
const bgInput = element<HTMLInputElement>('bg');
const bgValue = element('bg-value');
const widthInput = element<HTMLInputElement>('width');
const widthValue = element('width-value');
const heightInput = element<HTMLInputElement>('height');
const heightValue = element('height-value');
const colorsEl = element('colors');
const lockBtn = element<HTMLButtonElement>('lock');
const topState = element('top-state');
const repinBtn = element<HTMLButtonElement>('repin');
const resetBtn = element<HTMLButtonElement>('reset');

let style: LyricsStyle = readLyricsStyle();
let fontsReady = false;

function applyControls(): void {
  sizeInput.value = String(style.fontSize);
  sizeValue.textContent = `${style.fontSize}px`;
  bgInput.value = String(style.background);
  bgValue.textContent = `${style.background}%`;
  widthInput.value = String(style.width);
  widthValue.textContent = `${style.width}px`;
  heightInput.value = String(style.height);
  heightValue.textContent = `${style.height}px`;
  if (fontsReady) fontSelect.value = style.fontFamily || LYRICS_FONT_DEFAULT;
  lockBtn.textContent = style.locked ? '解锁歌词' : '锁定歌词';
  for (const swatch of Array.from(colorsEl.children)) {
    const color = (swatch as HTMLElement).dataset.color ?? '';
    swatch.classList.toggle('is-active', color.toLowerCase() === style.highlight.toLowerCase());
  }
}

/** 写入共享样式并广播；歌词窗口收到后立即应用。 */
function persist(): void {
  writeLyricsStyle(style);
  void emit(EVENTS.desktopLyricsStyle, { ...style }).catch(() => undefined);
  applyControls();
}

function patchStyle(patch: Partial<LyricsStyle>): void {
  style = sanitizeLyricsStyle({ ...style, ...patch });
  persist();
}

function renderSwatches(): void {
  colorsEl.replaceChildren();
  for (const preset of LYRICS_COLOR_PRESETS) {
    const swatch = document.createElement('button');
    swatch.type = 'button';
    swatch.className = 'swatch';
    swatch.dataset.color = preset.value;
    swatch.style.background = preset.value;
    swatch.title = preset.label;
    swatch.setAttribute('aria-label', preset.label);
    swatch.addEventListener('click', () => patchStyle({ highlight: preset.value }));
    colorsEl.append(swatch);
  }
}

function setTopState(ok: boolean | null): void {
  if (ok === null) {
    topState.textContent = '置顶状态未知';
    topState.classList.add('is-bad');
    return;
  }
  topState.textContent = ok ? '已置顶 · 始终在最前' : '置顶失效，请重试';
  topState.classList.toggle('is-bad', !ok);
}

async function refreshTopState(): Promise<void> {
  try {
    setTopState(await invoke<boolean>(COMMANDS.keepDesktopLyricsOnTop));
  } catch {
    setTopState(null);
  }
}

/** 后台扫描系统字体；当前字体被卸载时回退默认并同步到歌词窗口。 */
async function loadFonts(): Promise<void> {
  fontSelect.disabled = true;
  fontState.textContent = '正在读取系统字体…';
  const fragment = document.createDocumentFragment();
  const addOption = (value: string, label: string): void => {
    const option = document.createElement('option');
    option.value = value;
    option.textContent = label;
    fragment.append(option);
  };
  addOption(LYRICS_FONT_DEFAULT, '系统默认（自动匹配中文字体）');

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
  const seen = new Set<string>([LYRICS_FONT_DEFAULT]);
  for (const family of families) {
    const value = family.value.trim();
    if (!value || seen.has(value)) continue;
    seen.add(value);
    addOption(value, family.label || value);
  }
  fontSelect.append(fragment);
  fontSelect.disabled = false;
  fontsReady = true;

  const wanted = style.fontFamily || LYRICS_FONT_DEFAULT;
  if (!seen.has(wanted)) {
    style.fontFamily = '';
    writeLyricsStyle(style);
    void emit(EVENTS.desktopLyricsStyle, { ...style }).catch(() => undefined);
    fontState.textContent = '原字体不可用，已回退系统默认';
  } else {
    fontState.textContent = scanned ? `共 ${seen.size - 1} 款字体` : '使用内置字体清单';
  }
  applyControls();
}

function sendCommand(command: LyricsCommand): void {
  void emit(EVENTS.desktopLyricsCommand, command).catch(() => undefined);
}

function closeWindow(): void {
  const current = getCurrentWindow();
  if (!current) return;
  void current.close().catch(() => undefined);
}

function installControls(): void {
  closeBtn.addEventListener('click', closeWindow);
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      closeWindow();
    }
  });

  sizeInput.addEventListener('input', () => {
    patchStyle({ fontSize: Number(sizeInput.value) });
  });
  bgInput.addEventListener('input', () => {
    patchStyle({ background: Number(bgInput.value) });
  });
  widthInput.addEventListener('input', () => {
    patchStyle({ width: Number(widthInput.value) });
  });
  heightInput.addEventListener('input', () => {
    patchStyle({ height: Number(heightInput.value) });
  });
  fontSelect.addEventListener('change', () => {
    const family = fontSelect.value === LYRICS_FONT_DEFAULT ? '' : fontSelect.value;
    patchStyle({ fontFamily: family });
    fontState.textContent = family
      ? `已选择 ${fontSelect.selectedOptions[0]?.textContent ?? family}`
      : '系统默认（自动匹配中文字体）';
  });

  // 锁定 / 恢复默认交给歌词窗口执行：锁定需要歌词热区，恢复默认要还原窗口位置。
  lockBtn.addEventListener('click', () => sendCommand({ action: 'lock', locked: !style.locked }));
  resetBtn.addEventListener('click', () => sendCommand({ action: 'reset' }));
  repinBtn.addEventListener('click', () => void refreshTopState());
}

function init(): void {
  renderSwatches();
  applyControls();
  installControls();
  void loadFonts();
  void refreshTopState();
  void listen<LyricsStyle>(EVENTS.desktopLyricsStyle, (payload) => {
    style = sanitizeLyricsStyle(payload);
    applyControls();
  });
  // 用户拖拽歌词条边缘时同步宽高数值；只刷新显示，不回写样式。
  void listen<LyricsWindowSize>(EVENTS.desktopLyricsSize, ({ width, height }) => {
    style.width = Math.round(width);
    style.height = Math.round(height);
    widthInput.value = String(style.width);
    widthValue.textContent = `${style.width}px`;
    heightInput.value = String(style.height);
    heightValue.textContent = `${style.height}px`;
  });
}

init();