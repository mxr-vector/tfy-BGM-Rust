/**
 * songs.json 的唯一数据契约定义。
 * 生成侧（scripts/download_all.js、scripts/download_paladin_songs.js）与
 * 消费侧（src/App.vue 加载校验、各组件渲染）共用同一套规则，避免 schema 漂移。
 *
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 */

export const SONG_FIELDS = [
  'id',      // 数值 id：由 scripts/data/song-ids.json 按「分类+曲名」锁定，清单增删不再影响已有曲目
  'key',     // 稳定字符串键 `${category}/${文件名去扩展名}`，收藏 / 队列 / 播放态以此比对
  'title',
  'category',
  'categoryZh',
  'categoryEn',
  'color',
  'artist',
  'duration',
  'durationFormatted',
  'url',     // 未编码的站内原始路径，渲染前经 encodeBgmPath() 逐段转义
  'cover',
  'downloaded'
];

export const REQUIRED_FIELDS = ['id', 'key', 'title', 'category', 'url'];

/** 曲库文件在 public 下的相对位置 */
export const BGM_DIR = 'bgm';
export const SONGS_JSON_FILE = 'songs.json';

/**
 * 校验单条曲目元数据。
 * @returns {{ok: boolean, reason: string}}
 */
export function validateSong(song) {
  if (!song || typeof song !== 'object') return { ok: false, reason: '不是对象' };
  for (const field of REQUIRED_FIELDS) {
    if (song[field] === undefined || song[field] === null || song[field] === '') {
      return { ok: false, reason: `缺少必填字段 ${field}` };
    }
  }
  if (typeof song.url !== 'string' || !song.url.startsWith('/')) {
    return { ok: false, reason: 'url 必须是以 / 开头的站内路径' };
  }
  if (typeof song.duration !== 'number' || !Number.isFinite(song.duration) || song.duration < 1) {
    return { ok: false, reason: 'duration 必须是 >= 1 秒的正数' };
  }
  if (typeof song.downloaded !== 'boolean') {
    return { ok: false, reason: 'downloaded 必须是布尔值' };
  }
  return { ok: true, reason: '' };
}

/** 归一化 + 过滤，返回可安全渲染的曲目数组 */
export function normalizeSongs(list) {
  if (!Array.isArray(list)) return [];
  return list.filter(s => validateSong(s).ok);
}

/** 队列 / 收藏 / 播放态统一使用的稳定标识 */
export function songKey(song) {
  if (!song) return '';
  return song.key || song.url || String(song.id ?? '');
}

/** 音频文件尚未随应用一起分发时，可指向云盘直链 */
const BGM_BASE_KEY = 'tfy_bgm_base';

export function getBgmBase() {
  const envBase = import.meta.env?.VITE_BGM_BASE;
  if (typeof envBase === 'string' && envBase) return envBase;
  try {
    return localStorage.getItem(BGM_BASE_KEY) || '';
  } catch (e) {
    return '';
  }
}

export function setBgmBase(base) {
  try {
    if (base) localStorage.setItem(BGM_BASE_KEY, base);
    else localStorage.removeItem(BGM_BASE_KEY);
  } catch (e) {}
}

/**
 * 将站内原始路径逐段转义（仅用于展示 / 比对，不改变磁盘上的真实文件名）。
 */
export function encodeBgmPath(url) {
  const [beforeHash, hash = ''] = String(url).split('#');
  const [beforeQuery, query = ''] = beforeHash.split('?');
  const encoded = beforeQuery
    .split('/')
    .map(segment => (segment ? encodeURIComponent(segment) : segment))
    .join('/');
  const tail = (query ? `?${query}` : '') + (hash ? `#${hash}` : '');
  return encoded + tail;
}

/**
 * 将站内原始路径映射到实际可访问地址。
 * VITE_BGM_BASE / tfy_bgm_base 支持三种写法：
 *   1. 含 {path} 模板，如 https://cdn/pan/bgm/{file}（{path} 为 bgm/分类/文件.mp3）
 *   2. 以 public 结尾的网盘根，如 https://host/public（自动拼 /bgm/...）
 *   3. 留空，走应用自身的 BASE_URL（音频随 dist 一起部署）
 */
export function resolveSongUrl(song) {
  if (!song?.url) return '';
  if (/^https?:\/\//i.test(song.url)) return song.url;

  const clean = song.url.replace(/^\/+/, '');          // bgm/cheerful/xxx.mp3
  const encoded = clean.split('/').map(encodeURIComponent).join('/');
  const file = encoded.split('/').pop();
  const base = getBgmBase().replace(/\/+$/, '');

  if (!base) {
    const appBase = typeof import.meta.env?.BASE_URL === 'string' ? import.meta.env.BASE_URL : '/';
    return `${appBase.replace(/\/$/, '')}/${encoded}`;
  }
  if (base.includes('{path}')) return base.replace('{path}', encoded);
  if (base.includes('{file}')) return base.replace('{file}', file);
  if (/\/public$/i.test(base)) return `${base}/${encoded.replace(/^bgm\//, 'bgm/')}`;
  return `${base}/${encoded}`;
}

/** 判断解析后的地址是否与当前页面同源 */
export function isSameOriginUrl(url) {
  try {
    return new URL(url, window.location.href).origin === window.location.origin;
  } catch (e) {
    return true;
  }
}

function triggerAnchorDownload(href, filename) {
  const a = document.createElement('a');
  a.href = href;
  a.download = filename;
  a.rel = 'noopener';
  document.body.appendChild(a);
  a.click();
  a.remove();
}

/**
 * 下载曲目音频。
 * 同源地址直接用 <a download>；跨域（云盘直链）时 download 属性会被浏览器忽略，
 * 改为 fetch→blob 触发下载；若因 CORS/网络失败则回退到新标签打开，避免按钮变哑。
 */
export async function downloadSong(song, filename) {
  const url = resolveSongUrl(song);
  if (!url) return;
  const name = filename || `${song?.title || 'audio'}.mp3`;

  if (isSameOriginUrl(url)) {
    triggerAnchorDownload(url, name);
    return;
  }

  try {
    const res = await fetch(url);
    if (!res.ok) throw new Error(`status ${res.status}`);
    const blob = await res.blob();
    const objectUrl = URL.createObjectURL(blob);
    triggerAnchorDownload(objectUrl, name);
    setTimeout(() => URL.revokeObjectURL(objectUrl), 10000);
  } catch (e) {
    window.open(url, '_blank', 'noopener');
  }
}
