/**
 * 仙剑专题曲目定点下载脚本（按酷狗 hash 直取）。
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 *
 * 产出的条目严格遵守 src/utils/songSchema.js 定义的单一数据契约：
 *   node scripts/download_paladin_songs.js           # 只下载并打印元数据
 *   node scripts/download_paladin_songs.js --merge   # 同时写入 songs.json、songs.md、id 快照
 * 注：新增曲目必须同步到 scripts/songs.md，否则下次 download_all.js 重跑会把它当已下架曲目清掉。
 */
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import https from 'node:https';
import http from 'node:http';
import { validateSong } from '../src/utils/songSchema.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');

const SONGS_MD_PATH = path.join(__dirname, 'songs.md');
const ID_SNAPSHOT_PATH = path.join(__dirname, 'data', 'song-ids.json');
const SONGS_JSON_PATH = path.join(rootDir, 'public', 'bgm', 'songs.json');

const SONGS_TO_DOWNLOAD = [
  {
    title: '偏爱',
    artist: '张芸京',
    filename: '偏爱.mp3',
    hash: '625ace44b53039174078a823088b395e'
  },
  {
    title: '此生不换',
    artist: '青鸟飞鱼',
    filename: '此生不换.mp3',
    hash: '8b75b96fb4a8d896f1801425c7904a79'
  },
  {
    title: '忘记时间',
    artist: '胡歌',
    filename: '忘记时间.mp3',
    hash: '4bc3e4a9da56f7e308efc14befd522df'
  },
  {
    title: '莫失莫忘',
    artist: '麦振鸿',
    filename: '莫失莫忘.mp3',
    hash: '89c94895bf893a66aeaceffdf4b45eea'
  },
  {
    title: '雪见-落入凡尘',
    artist: '麦振鸿',
    filename: '雪见-落入凡尘.mp3',
    hash: '8cadcd668ba293695c07a18ddcd9f43e'
  }
];

function randomIP() {
  return [
    Math.floor(Math.random() * 150 + 50),
    Math.floor(Math.random() * 250 + 1),
    Math.floor(Math.random() * 250 + 1),
    Math.floor(Math.random() * 250 + 1)
  ].join('.');
}

function formatDuration(sec) {
  if (!sec || isNaN(sec)) return '03:00';
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
}

async function getSongInfo(hash) {
  const ip = randomIP();
  const infoUrl = `https://m.kugou.com/app/i/getSongInfo.php?cmd=playInfo&hash=${hash}`;
  const res = await fetch(infoUrl, {
    headers: {
      'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
      'X-Forwarded-For': ip,
      'Client-IP': ip
    }
  });
  return await res.json();
}

function normalizeCover(url) {
  if (!url) return '';
  return String(url).replace('{size}', '400').replace(/^http:\/\//, 'https://');
}

function downloadFile(url, destPath, redirectCount = 0) {
  const MAX_REDIRECTS = 5;
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(destPath);
    const client = url.startsWith('https') ? https : http;

    const req = client.get(url, { headers: { 'User-Agent': 'Mozilla/5.0' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        fs.unlink(destPath, () => {});
        if (redirectCount >= MAX_REDIRECTS) {
          reject(new Error(`Too many redirects (>${MAX_REDIRECTS})`));
          return;
        }
        // Location 可能是相对路径，基于当前 url 归一化为绝对地址后再递归
        let nextUrl;
        try {
          nextUrl = new URL(res.headers.location, url).href;
        } catch (e) {
          reject(new Error(`Invalid redirect location: ${res.headers.location}`));
          return;
        }
        downloadFile(nextUrl, destPath, redirectCount + 1).then(resolve).catch(reject);
        return;
      }
      if (res.statusCode !== 200) {
        file.close();
        fs.unlink(destPath, () => {});
        reject(new Error(`Failed with status code: ${res.statusCode}`));
        return;
      }
      res.pipe(file);
      file.on('finish', () => {
        file.close(() => resolve());
      });
    });

    req.on('error', (err) => {
      file.close();
      fs.unlink(destPath, () => {});
      reject(err);
    });

    req.setTimeout(30000, () => {
      req.destroy(new Error('Download timeout'));
    });
  });
}

function loadIdSnapshot() {
  try {
    return JSON.parse(fs.readFileSync(ID_SNAPSHOT_PATH, 'utf-8'));
  } catch (e) {
    return {};
  }
}

// 与 download_all.js 同构：按「分类/曲名」锁定数值 id，新条目取未使用的最小正整数
function allocateId(snapshot, title) {
  const key = `gufeng/${title}`;
  if (typeof snapshot[key] === 'number') return snapshot[key];
  const used = new Set(Object.values(snapshot).filter(v => typeof v === 'number'));
  let id = 1;
  while (used.has(id)) id++;
  snapshot[key] = id;
  return id;
}

// 把新曲名补写进清单，否则下次 download_all.js 会把它当已下架曲目清掉
function ensureInSongsMd(titles) {
  if (!fs.existsSync(SONGS_MD_PATH)) {
    throw new Error(`缺少 ${path.relative(rootDir, SONGS_MD_PATH)}，无法安全合并新曲目`);
  }
  let content = fs.readFileSync(SONGS_MD_PATH, 'utf-8');
  const lines = content.split('\n');
  const headingIdx = lines.findIndex(l => l.trim() === '## 古风');
  if (headingIdx === -1) throw new Error('songs.md 中没有 `## 古风` 分节');

  let end = headingIdx + 1;
  while (end < lines.length && !/^#{1,6}\s/.test(lines[end])) end++;
  const section = lines.slice(headingIdx + 1, end).map(l => l.trim()).filter(Boolean);
  const additions = titles.filter(t => !section.includes(t));
  if (!additions.length) return;

  let insertAt = headingIdx + 1;
  while (insertAt < end && lines[insertAt].trim() !== '') insertAt++;
  lines.splice(insertAt, 0, ...additions);
  fs.writeFileSync(SONGS_MD_PATH, lines.join('\n'), 'utf-8');
  console.log(`+ songs.md 补充 ${additions.length} 条: ${additions.join(', ')}`);
}

function mergeIntoSongsJson(results) {
  let songs = [];
  try {
    songs = JSON.parse(fs.readFileSync(SONGS_JSON_PATH, 'utf-8'));
  } catch (e) {
    console.warn('! songs.json 不存在或解析失败，本次合并将新建清单');
  }
  if (!Array.isArray(songs)) songs = [];

  for (const item of results) {
    const check = validateSong(item);
    if (!check.ok) throw new Error(`条目不符合数据契约，拒绝合并: ${item.title} -> ${check.reason}`);
  }

  const byUrl = new Map(songs.map(s => [s.url, s]));
  let added = 0;
  for (const item of results) {
    const prev = byUrl.get(item.url);
    if (prev) {
      // 契约字段直接对齐；其余字段只填空，不用占位值（cover: '' / duration: 180）盖掉真实数据
      for (const [field, value] of Object.entries(item)) {
        if (field === 'id' || field === 'key') { prev[field] = value; continue; }
        const meaningful = value !== '' && value !== undefined && !(field === 'duration' && value === 180);
        if (meaningful && (prev[field] === '' || prev[field] === undefined)) prev[field] = value;
      }
    } else {
      songs.push(item);
      added++;
    }
  }
  fs.writeFileSync(SONGS_JSON_PATH, JSON.stringify(songs, null, 2) + '\n', 'utf-8');
  console.log(`+ songs.json 新增 ${added} 条，更新 ${results.length - added} 条`);
}

async function run() {
  const merge = process.argv.slice(2).includes('--merge');
  const gufengDir = path.join(rootDir, 'public', 'bgm', 'gufeng');
  if (!fs.existsSync(gufengDir)) {
    fs.mkdirSync(gufengDir, { recursive: true });
  }

  const snapshot = loadIdSnapshot();
  const results = [];

  for (const song of SONGS_TO_DOWNLOAD) {
    const destPath = path.join(gufengDir, song.filename);
    const duration = info_duration(song);
    let cover = '';

    if (fs.existsSync(destPath) && fs.statSync(destPath).size > 100000) {
      console.log(`(Cached) ${song.title}`);
    } else {
      console.log(`\nFetching play info for [${song.title}] by ${song.artist}...`);
      const info = await getSongInfo(song.hash);
      const playUrl = info.url || (info.backup_url && info.backup_url[0]);
      if (!playUrl) {
        throw new Error(`Could not get play URL for ${song.title}`);
      }
      cover = normalizeCover(info.imgUrl || info.album_img || '');

      console.log(`Downloading to ${destPath}...`);
      await downloadFile(playUrl, destPath);
      const stat = fs.statSync(destPath);
      console.log(`Downloaded ${song.title}: ${(stat.size / 1024 / 1024).toFixed(2)} MB`);
    }

    const stat = fs.statSync(destPath);
    results.push({
      id: allocateId(snapshot, song.title),
      key: `gufeng/${song.filename.replace(/\.mp3$/i, '')}`,
      title: song.title,
      category: 'gufeng',
      categoryZh: '古风',
      categoryEn: 'Traditional / GuFeng',
      color: '#10b981',
      artist: song.artist,
      duration: duration,
      durationFormatted: formatDuration(duration),
      url: `/bgm/gufeng/${song.filename}`,
      cover: '',
      downloaded: true,
      fileSize: stat.size
    });

    // Small delay to be polite
    await new Promise(r => setTimeout(r, 200));
  }

  console.log('\nAll songs ready!');

  if (merge) {
    ensureInSongsMd(results.map(s => s.title));
    mergeIntoSongsJson(results);
    fs.mkdirSync(path.dirname(ID_SNAPSHOT_PATH), { recursive: true });
    fs.writeFileSync(ID_SNAPSHOT_PATH, JSON.stringify(snapshot, null, 2) + '\n', 'utf-8');
    console.log(`+ id 快照已更新: ${path.relative(rootDir, ID_SNAPSHOT_PATH)}`);
  } else {
    console.log('提示：加 --merge 可直接写入 songs.json / songs.md（不再人工粘贴合并）');
    console.log(JSON.stringify(results, null, 2));
  }
}

// 缓存命中时拿不到接口时长，用已记录的时长占位（后续 download_all.js 会用 ffprobe 校正）
function info_duration(song) {
  return song.duration && song.duration > 0 ? song.duration : 180;
}

run().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
