/**
 * 界面字体清单。
 *
 * 字体选项不再硬编码：Rust 端扫描系统已安装的字体文件、解析真实字体族名，
 * 前端只做会话内缓存，并把族名拼成写进 `--font-sans` 的字体栈。
 */

import { shallowRef } from 'vue';
import { COMMANDS, DEFAULT_FONT_STACK, LEGACY_UI_FONT_FAMILIES } from './constants';
import { invoke } from './tauri';
import type { SystemFont } from './types';

/** 字体下拉框选项。 */
export interface FontOption {
  /** `system` 或字体族名。 */
  id: string;
  label: string;
  /** 预览用的字体栈：只保留族名，避免兜底字体掩盖各字体的差异。 */
  previewStack: string;
  cjk: boolean;
}

/** 带安装状态的选项；字体被卸载后仍要能显示当前值。 */
export type ResolvedFontOption = FontOption & { installed: boolean };

/** 「系统默认」固定排第一，不依赖扫描结果。 */
const SYSTEM_OPTION: FontOption = {
  id: 'system',
  label: '系统默认',
  previewStack: DEFAULT_FONT_STACK,
  cjk: true,
};

/** 族名写进 CSS 字符串前去掉可能截断声明的字符。 */
function cleanFamily(family: string): string {
  return family.replace(/["\\;{}]/g, '').trim();
}

/** 旧版本字体 id → 族名；普通族名原样返回。 */
function canonicalFamily(fontId: string): string {
  return LEGACY_UI_FONT_FAMILIES[fontId] ?? fontId;
}

/** 生成 `--font-sans`：选中字体在前，内置中文字体栈兜底，缺字时不会掉到系统默认。 */
export function fontStack(fontId: string | undefined): string {
  const family = cleanFamily(canonicalFamily(fontId ?? 'system'));
  if (!family || family.toLowerCase() === 'system') return DEFAULT_FONT_STACK;
  return `"${family}", ${DEFAULT_FONT_STACK}`;
}

// 用 shallowRef 保存，字体扫描完成后依赖它的 computed（如当前选择的展示名）会自动更新。
const cache = shallowRef<SystemFont[]>([]);
let pending: Promise<SystemFont[]> | null = null;

/** 已缓存的系统字体（尚未加载时为空数组）。 */
export function cachedFonts(): SystemFont[] {
  return cache.value;
}

/** 读取系统字体；会话内缓存，`force` 用于安装新字体后重新扫描。 */
export function loadSystemFonts(force = false): Promise<SystemFont[]> {
  if (!force && cache.value.length > 0) return Promise.resolve(cache.value);
  if (!force && pending) return pending;
  const request = invoke<SystemFont[]>(COMMANDS.systemFonts).then((list) => {
    cache.value = Array.isArray(list) ? list.filter((font) => Boolean(font?.family)) : [];
    return cache.value;
  });
  pending = request;
  void request
    .catch(() => undefined)
    .then(() => {
      if (pending === request) pending = null;
    });
  return request;
}

/** 下拉框选项：系统默认 + 扫描到的系统字体。 */
export function fontOptions(): FontOption[] {
  return [
    SYSTEM_OPTION,
    ...cache.value.map((font) => {
      const family = cleanFamily(font.family);
      return {
        id: font.family,
        label: font.label || font.family,
        previewStack: family ? `"${family}"` : DEFAULT_FONT_STACK,
        cjk: font.cjk,
      };
    }),
  ];
}

/** 当前选择对应的选项；字体已被卸载时返回占位项，由界面提示「未安装」。 */
export function resolveFontOption(fontId: string | undefined): ResolvedFontOption {
  const raw = fontId ?? 'system';
  if (raw.toLowerCase() === 'system') return { ...SYSTEM_OPTION, installed: true };

  const family = cleanFamily(canonicalFamily(raw));
  const found = cache.value.find((font) => font.family.toLowerCase() === family.toLowerCase());
  return {
    id: found?.family ?? family,
    label: found?.label ?? family,
    previewStack: family ? `"${family}"` : DEFAULT_FONT_STACK,
    cjk: found?.cjk ?? false,
    installed: Boolean(found),
  };
}
