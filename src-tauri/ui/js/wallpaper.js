/**
 * 界面壁纸：默认样式 / 渐变蓝绿红 / 仓库内置两款（含 MP4 动态壁纸）/ 本地自定义文件。
 *
 * 壁纸不改动曲库数据，只把选择写进用户偏好；内置资源路径由 Rust 解析，
 * 前端统一通过 assetUrl 转成 WebView 可访问的 URL。
 * 动态壁纸用 `<video autoplay loop muted playsinline>` 铺满窗口，页面隐藏时暂停，
 * 回到前台自动继续，避免后台空耗 GPU。
 */

import { COMMANDS, EVENTS } from './constants.js';
import { applyStaticIcons, setIcon } from './icons.js';
import {
  on,
  setWallpaper,
  setWallpaperBlur,
  setWallpaperFit,
  setWallpaperOpacity,
  state,
} from './store.js';
import { assetUrl, getCurrentWindow, invoke, isTauri } from './tauri.js';
import { el, normalizeError, toast } from './ui.js';

const OPTIONS = [
  { id: 'default', label: '默认样式', hint: '当前配色', preview: 'default' },
  { id: 'gradient-blue', label: '渐变蓝', hint: '深邃静谧', preview: 'blue' },
  { id: 'gradient-green', label: '渐变绿', hint: '翡翠流光', preview: 'green' },
  { id: 'gradient-red', label: '渐变红', hint: '暖色余晖', preview: 'red' },
  { id: 'builtin-1', label: '幻彩流光', hint: 'MP4 动态', preview: 'builtin-1' },
  { id: 'builtin-2', label: '雪山云海', hint: 'JPG 静态', preview: 'builtin-2' },
];

const THEME_CLASSES = OPTIONS.map((option) => `wall-${option.id}`).concat('wall-custom');
const VIDEO_EXTENSION = /\.mp4$/i;

let shell = null;
let trigger = null;
let popover = null;
let mediaLayer = null;
let customButton = null;
let customHint = null;
let customPreview = null;
const builtins = new Map();

let currentId = 'default';
let currentPath = null;
let currentKind = 'image';
let currentOpacity = 0.35;
let currentBlur = 0;
let currentFit = 'cover';
let opacitySlider = null;
let opacityVal = null;
let blurSlider = null;
let blurVal = null;
const fitButtons = new Map();
/** 视频元素用于在标签页隐藏时暂停。 */
let activeVideo = null;

function applyOpacity(opacity) {
  const t = Math.max(0, Math.min(0.95, Number(opacity) || 0));
  currentOpacity = t;
  // t: 0 (实底) ~ 0.95 (超通透)
  // panelOpacity: 1.0 (实底) -> 0.08 (极薄通透)
  const panelOpacity = Math.max(0.04, 1 - t * 0.92).toFixed(3);
  // maskOpacity: 控制壁纸上的明暗遮罩。默认 t=0.35 时约 0.25，高透明度时趋近 0，壁纸鲜艳明朗
  const maskOpacity = Math.max(0, 0.40 - t * 0.44).toFixed(3);
  if (shell) {
    shell.style.setProperty('--panel-opacity', String(panelOpacity));
    shell.style.setProperty('--wall-mask-opacity', String(maskOpacity));
  }
  if (opacitySlider) {
    const percent = Math.round(t * 100);
    opacitySlider.value = String(percent);
    if (opacityVal) opacityVal.textContent = `${percent}%`;
  }
}

function applyBlur(blur) {
  const b = Math.max(0, Math.min(20, Math.round(Number(blur) || 0)));
  currentBlur = b;
  if (shell) {
    shell.style.setProperty('--panel-blur', `${b}px`);
    shell.style.setProperty('--table-blur', b > 0 ? `blur(${b}px) saturate(110%)` : 'none');
  }
  if (blurSlider) {
    blurSlider.value = String(b);
    if (blurVal) blurVal.textContent = `${b}px`;
  }
}

function applyFit(fit) {
  const f = fit === 'contain' ? 'contain' : 'cover';
  currentFit = f;
  if (shell) {
    shell.style.setProperty('--wallpaper-fit', f);
  }
  if (mediaLayer) {
    for (const el of mediaLayer.querySelectorAll('img, video')) {
      el.style.objectFit = f;
    }
  }
  for (const [mode, btn] of fitButtons.entries()) {
    const active = mode === f;
    btn.classList.toggle('is-active', active);
    btn.setAttribute('aria-pressed', String(active));
  }
}

function kindOf(path) {
  return VIDEO_EXTENSION.test(path ?? '') ? 'video' : 'image';
}

function fileName(path) {
  return String(path ?? '').split(/[\\/]/).pop() || '自定义壁纸';
}

function isMediaId(id) {
  return id === 'custom' || id.startsWith('builtin-');
}

function sourceFor(id, path, kind) {
  if (id === 'custom') return path ? { path, kind } : null;
  const builtin = builtins.get(id);
  return builtin ? { path: builtin.path, kind: builtin.kind } : null;
}

function withBadge(host, iconName) {
  const badge = el('span', { class: 'wallpaper-badge', attrs: { 'aria-hidden': 'true' } });
  setIcon(badge, iconName);
  host.append(badge);
  return host;
}

function buildPopover() {
  const grid = el('div', { class: 'wallpaper-grid' });
  for (const option of OPTIONS) {
    const preview = el('span', {
      class: `wallpaper-preview preview-${option.preview}`,
      dataset: { preview: option.preview },
      attrs: { 'aria-hidden': 'true' },
    });
    const button = el(
      'button',
      {
        type: 'button',
        class: 'wallpaper-option',
        dataset: { wallpaper: option.id },
        attrs: {
          'aria-label': `使用${option.label}壁纸`,
          'aria-pressed': 'false',
        },
      },
      [
        preview,
        el('span', { class: 'wallpaper-meta' }, [
          el('span', { class: 'wallpaper-name', text: option.label }),
          el('span', { class: 'wallpaper-desc', text: option.hint }),
        ]),
        el('span', { class: 'wallpaper-check', dataset: { icon: 'check' } }),
      ],
    );
    button.addEventListener('click', () => {
      setWallpaper(option.id, null, {
        opacity: currentOpacity,
        blur: currentBlur,
        fit: currentFit,
      });
      closePopover();
    });
    grid.append(button);
  }

  // ------------------ 画面展示方式（铺满裁剪 / 完整展示） ------------------
  const fitCoverBtn = el('button', {
    type: 'button',
    class: 'wallpaper-fit-btn is-active',
    text: '铺满裁剪',
    attrs: { 'aria-label': '铺满裁剪（填满窗口，裁剪溢出边缘）', 'aria-pressed': 'true' },
  });
  const fitContainBtn = el('button', {
    type: 'button',
    class: 'wallpaper-fit-btn',
    text: '完整展示',
    attrs: { 'aria-label': '完整展示（保留完整画面，不裁切任何内容）', 'aria-pressed': 'false' },
  });
  fitButtons.set('cover', fitCoverBtn);
  fitButtons.set('contain', fitContainBtn);

  fitCoverBtn.addEventListener('click', () => {
    applyFit('cover');
    setWallpaperFit('cover');
  });
  fitContainBtn.addEventListener('click', () => {
    applyFit('contain');
    setWallpaperFit('contain');
  });

  const fitControl = el('div', { class: 'wallpaper-control-row' }, [
    el('span', { class: 'wallpaper-control-label', text: '画面展示' }),
    el('div', { class: 'wallpaper-fit-tabs' }, [fitCoverBtn, fitContainBtn]),
  ]);

  // ------------------ 界面透明度滑块 ------------------
  opacityVal = el('span', {
    class: 'wallpaper-control-val',
    id: 'wallpaper-opacity-val',
    text: `${Math.round(currentOpacity * 100)}%`,
  });
  opacitySlider = el('input', {
    class: 'wallpaper-slider',
    id: 'wallpaper-opacity-slider',
    attrs: {
      type: 'range',
      min: '0',
      max: '90',
      step: '5',
      value: String(Math.round(currentOpacity * 100)),
      'aria-label': '调整界面透明度',
    },
  });
  opacitySlider.addEventListener('input', () => {
    const val = Number(opacitySlider.value);
    opacityVal.textContent = `${val}%`;
    applyOpacity(val / 100);
  });
  opacitySlider.addEventListener('change', () => {
    const val = Number(opacitySlider.value);
    setWallpaperOpacity(val / 100);
  });
  const opacityControl = el('div', { class: 'wallpaper-control-group' }, [
    el('div', { class: 'wallpaper-control-head' }, [
      el('span', { class: 'wallpaper-control-label', text: '界面透明度' }),
      opacityVal,
    ]),
    el('div', { class: 'wallpaper-slider-track' }, [
      el('span', { class: 'slider-hint', text: '实底' }),
      opacitySlider,
      el('span', { class: 'slider-hint', text: '通透' }),
    ]),
  ]);

  // ------------------ 内容区模糊度滑块 ------------------
  blurVal = el('span', {
    class: 'wallpaper-control-val',
    id: 'wallpaper-blur-val',
    text: `${currentBlur}px`,
  });
  blurSlider = el('input', {
    class: 'wallpaper-slider',
    id: 'wallpaper-blur-slider',
    attrs: {
      type: 'range',
      min: '0',
      max: '16',
      step: '1',
      value: String(currentBlur),
      'aria-label': '调整内容区毛玻璃模糊度',
    },
  });
  blurSlider.addEventListener('input', () => {
    const b = Number(blurSlider.value);
    blurVal.textContent = `${b}px`;
    applyBlur(b);
  });
  blurSlider.addEventListener('change', () => {
    const b = Number(blurSlider.value);
    setWallpaperBlur(b);
  });
  const blurControl = el('div', { class: 'wallpaper-control-group' }, [
    el('div', { class: 'wallpaper-control-head' }, [
      el('span', { class: 'wallpaper-control-label', text: '内容区模糊' }),
      blurVal,
    ]),
    el('div', { class: 'wallpaper-slider-track' }, [
      el('span', { class: 'slider-hint', text: '清晰 (0px)' }),
      blurSlider,
      el('span', { class: 'slider-hint', text: '柔化' }),
    ]),
  ]);

  customPreview = el('span', {
    class: 'wallpaper-preview is-custom',
    dataset: { icon: 'image' },
    attrs: { 'aria-hidden': 'true' },
  });
  customHint = el('span', {
    class: 'wallpaper-desc',
    id: 'wallpaper-custom-hint',
    text: 'JPG / PNG / WebP / MP4',
  });
  customButton = el(
    'button',
    {
      type: 'button',
      class: 'wallpaper-option wallpaper-option-custom',
      id: 'wallpaper-custom-btn',
      attrs: { 'aria-label': '选择本地自定义壁纸', 'aria-pressed': 'false' },
    },
    [
      customPreview,
      el('span', { class: 'wallpaper-meta' }, [
        el('span', { class: 'wallpaper-name', text: '自定义壁纸' }),
        customHint,
      ]),
      el('span', { class: 'wallpaper-check', dataset: { icon: 'check' } }),
    ],
  );
  customButton.addEventListener('click', () => {
    void chooseCustom();
  });

  const panel = el(
    'div',
    {
      class: 'wallpaper-popover',
      id: 'wallpaper-popover',
      attrs: { role: 'dialog', 'aria-label': '界面壁纸', hidden: '' },
    },
    [
      el('div', { class: 'wallpaper-pop-head' }, [
        el('div', { class: 'wallpaper-pop-title' }, [
          el('strong', { text: '界面壁纸' }),
          el('span', { text: '选择后自动保存，支持动态壁纸' }),
        ]),
        el('span', { class: 'wallpaper-pop-icon', dataset: { icon: 'palette' } }),
      ]),
      grid,
      el('div', { class: 'wallpaper-pop-foot' }, [
        customButton,
        fitControl,
        opacityControl,
        blurControl,
      ]),
    ],
  );
  applyStaticIcons(panel);
  return panel;
}

function clearMedia() {
  if (activeVideo) {
    activeVideo.pause();
    activeVideo = null;
  }
  mediaLayer.replaceChildren();
  mediaLayer.classList.remove('has-media');
}

function renderMedia(source) {
  clearMedia();
  if (!source) return;

  mediaLayer.classList.add('has-media');

  if (source.kind === 'video') {
    const video = el('video', {
      class: 'wallpaper-video',
      attrs: {
        autoplay: '',
        loop: '',
        muted: '',
        playsinline: '',
        preload: 'auto',
        'disablepictureinpicture': '',
        'aria-hidden': 'true',
      },
    });
    video.muted = true;
    video.defaultMuted = true;
    video.addEventListener('loadeddata', () => video.classList.add('is-ready'));
    video.addEventListener('error', () => {
      clearMedia();
      toast('动态壁纸加载失败，已切换回默认样式', 'error');
      setWallpaper('default', null);
    });
    video.addEventListener('playing', () => video.classList.add('is-ready'));
    video.style.objectFit = currentFit;
    video.src = assetUrl(source.path);
    mediaLayer.append(video);
    activeVideo = video;
    if (!document.hidden) {
      void video.play().catch(() => {
        // WebView 可能阻止自动播放；静音视频通常可播，失败时保留静态首帧。
        video.classList.add('is-ready');
      });
    }
    return;
  }

  const image = el('img', {
    class: 'wallpaper-image',
    attrs: { alt: '', 'aria-hidden': 'true' },
  });
  image.addEventListener('load', () => image.classList.add('is-ready'));
  image.addEventListener('error', () => {
    clearMedia();
    toast('图片壁纸加载失败，已切换回默认样式', 'error');
    setWallpaper('default', null);
  });
  image.style.objectFit = currentFit;
  image.src = assetUrl(source.path);
  mediaLayer.append(image);
  if (image.complete && image.naturalWidth > 0) image.classList.add('is-ready');
}

function syncOptions() {
  for (const button of popover.querySelectorAll('button[data-wallpaper]')) {
    const active = button.dataset.wallpaper === currentId;
    button.classList.toggle('is-active', active);
    button.setAttribute('aria-pressed', String(active));
  }
  if (customButton) {
    const active = currentId === 'custom';
    customButton.classList.toggle('is-active', active);
    customButton.setAttribute('aria-pressed', String(active));
  }
  trigger.classList.toggle('is-active', currentId !== 'default');
}

function updateCustomPreview() {
  if (!customPreview) return;
  const saved = state.prefs?.customWallpaper ?? null;
  customPreview.replaceChildren();
  customPreview.classList.remove('has-image', 'is-video');

  const paintIcon = (name) => {
    const icon = el('span', { class: 'preview-icon', dataset: { icon: name } });
    setIcon(icon, name);
    customPreview.append(icon);
  };

  if (!saved) {
    customHint.textContent = '支持 JPG / PNG / WebP / MP4';
    paintIcon('image');
    return;
  }
  const kind = kindOf(saved);
  customHint.textContent = `上次使用：${fileName(saved)}`;
  if (kind === 'video') {
    paintIcon('play-fill');
    customPreview.classList.add('is-video');
    return;
  }
  customPreview.classList.add('has-image');
  const img = el('img', { attrs: { alt: '', 'aria-hidden': 'true' } });
  img.addEventListener('error', () => {
    img.remove();
    paintIcon('image');
  });
  img.src = assetUrl(saved);
  customPreview.append(img);
}

function applySelection(id, path, kind, options = {}) {
  currentId = id;
  currentPath = path;
  currentKind = kind;
  const opacity =
    typeof options === 'object' && options !== null ? options.opacity : options;
  const blur =
    typeof options === 'object' && options !== null ? options.blur : null;
  const fit =
    typeof options === 'object' && options !== null ? options.fit : null;

  if (opacity !== null && opacity !== undefined) applyOpacity(opacity);
  if (blur !== null && blur !== undefined) applyBlur(blur);
  if (fit !== null && fit !== undefined) applyFit(fit);

  const source = isMediaId(id) ? sourceFor(id, path, kind) : null;

  shell.classList.remove(...THEME_CLASSES, 'has-wallpaper', 'has-video');
  shell.classList.add(`wall-${id}`);
  if (id !== 'default') shell.classList.add('has-wallpaper');
  // 动态壁纸时降低毛玻璃半径，避免每帧对 4K 视频做模糊拖慢渲染。
  if (source?.kind === 'video') shell.classList.add('has-video');

  renderMedia(source);
  syncOptions();
  updateCustomPreview();
}

function positionPopover() {
  const rect = trigger.getBoundingClientRect();
  const width = popover.offsetWidth;
  const left = Math.min(Math.max(12, rect.right - width), window.innerWidth - width - 12);
  popover.style.left = `${Math.max(12, left)}px`;
  popover.style.top = `${rect.bottom + 10}px`;
}

function openPopover() {
  if (!popover) return;
  popover.hidden = false;
  trigger.setAttribute('aria-expanded', 'true');
  positionPopover();
  // 打开时把焦点交给当前选中项，键盘可直接切换。
  const active =
    popover.querySelector('.wallpaper-option.is-active') ??
    popover.querySelector('.wallpaper-option');
  active?.focus({ preventScroll: true });
}

function closePopover() {
  if (!popover || popover.hidden) return;
  popover.hidden = true;
  trigger.setAttribute('aria-expanded', 'false');
  trigger.focus({ preventScroll: true });
}

async function chooseCustom() {
  if (!isTauri) {
    toast('请在桌面客户端中选择本地壁纸', 'error');
    closePopover();
    return;
  }
  try {
    const picked = await invoke(COMMANDS.chooseWallpaperFile);
    if (!picked) return;
    setWallpaper('custom', picked.path, {
      opacity: currentOpacity,
      blur: currentBlur,
      fit: currentFit,
    });
    closePopover();
    toast(`已应用壁纸：${fileName(picked.path)}`, 'success');
  } catch (error) {
    toast(`选择壁纸失败：${normalizeError(error)}`, 'error');
  }
}

function paintBuiltinPreview(item) {
  const preview = popover.querySelector(
    `[data-wallpaper="${item.id}"] .wallpaper-preview`,
  );
  if (!preview) return;
  if (!item.thumb) {
    // 资源里没有缩略图（例如精简安装包）时用类型图标兜底。
    const icon = el('span', { class: 'preview-icon', dataset: { icon: item.kind === 'video' ? 'film' : 'image' } });
    setIcon(icon, item.kind === 'video' ? 'film' : 'image');
    preview.replaceChildren(icon);
  } else {
    preview.classList.add('has-image');
    preview.replaceChildren(el('img', { attrs: { src: assetUrl(item.thumb), alt: '' } }));
  }
  if (item.kind === 'video') {
    preview.classList.add('is-video');
    withBadge(preview, 'play-fill');
  }
}

async function loadBuiltins() {
  if (!isTauri) return;
  try {
    const items = await invoke(COMMANDS.builtinWallpapers);
    const found = new Set();
    for (const item of items) {
      builtins.set(item.id, item);
      found.add(item.id);
      paintBuiltinPreview(item);
    }
    // 资源缺失（例如精简安装包）时隐藏对应选项，避免出现点了没反应的卡片。
    for (const option of OPTIONS) {
      if (!option.id.startsWith('builtin-') || found.has(option.id)) continue;
      popover.querySelector(`[data-wallpaper="${option.id}"]`)?.remove();
      if (currentId === option.id) applySelection('default', null, 'image');
    }
    // 首屏可能先于资源解析完成，拿到路径后补上真正的图片 / 视频。
    if (currentId.startsWith('builtin-') || currentId === 'custom') {
      applySelection(currentId, currentPath, currentKind);
    }
  } catch (error) {
    toast(`内置壁纸加载失败：${normalizeError(error)}`, 'error');
  }
}

/** 曲库 bootstrap 完成后调用，把持久化的壁纸选择应用到界面。 */
export function syncWallpaperFromPrefs() {
  if (!shell || !state.prefs) return;
  const id = state.prefs.wallpaper || 'default';
  const path = state.prefs.customWallpaper ?? null;
  const opacity = state.prefs.wallpaperOpacity ?? 0.35;
  const blur = state.prefs.wallpaperBlur ?? 0;
  const fit = state.prefs.wallpaperFit ?? 'cover';
  applyOpacity(opacity);
  applyBlur(blur);
  applyFit(fit);
  applySelection(id, path, kindOf(path), { opacity, blur, fit });
}

export function initWallpaper() {
  shell = document.querySelector('.app-shell');
  trigger = document.getElementById('btn-wallpaper');
  const topbar = document.querySelector('.topbar');
  if (!shell || !trigger || !topbar) return;

  mediaLayer = el('div', { class: 'wallpaper-media', attrs: { 'aria-hidden': 'true' } });
  shell.prepend(mediaLayer);

  popover = buildPopover();
  document.body.append(popover);

  trigger.addEventListener('click', (event) => {
    event.stopPropagation();
    if (popover.hidden) openPopover();
    else closePopover();
  });
  popover.addEventListener('click', (event) => event.stopPropagation());
  document.addEventListener('click', closePopover);
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && !popover.hidden) {
      event.stopPropagation();
      closePopover();
    }
  });
  window.addEventListener('resize', () => {
    if (!popover.hidden) positionPopover();
  });

  // 动态壁纸：标签页不可见 / 窗口失焦时暂停，回到前台恢复播放。
  document.addEventListener('visibilitychange', () => {
    if (!activeVideo) return;
    if (document.hidden) activeVideo.pause();
    else void activeVideo.play().catch(() => {});
  });
  getCurrentWindow()?.onFocusChanged(({ payload: focused }) => {
    if (!activeVideo) return;
    if (focused) void activeVideo.play().catch(() => {});
    else activeVideo.pause();
  });

  on(EVENTS.wallpaper, ({ id, path, opacity, blur, fit }) =>
    applySelection(id, path ?? null, kindOf(path), { opacity, blur, fit })
  );

  applyOpacity(currentOpacity);
  applyBlur(currentBlur);
  applyFit(currentFit);
  applySelection('default', null, 'image', {
    opacity: currentOpacity,
    blur: currentBlur,
    fit: currentFit,
  });
  void loadBuiltins();
}
