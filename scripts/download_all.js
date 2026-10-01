/**
 * BGM 曲库生成脚本
 * Copyright 2026 YuanJie
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * 清单来源：scripts/songs.md（版本库内唯一事实来源）
 * 产出：public/bgm/songs.json（元数据） + public/bgm/<分类>/<文件>.mp3（音频，默认被 git 忽略）
 * 音频不入库：使用 `pnpm bgm:publish` 生成 dist-bgm/ 后上传云盘分发。
 *
 * 用法：
 *   node scripts/download_all.js              # 校验清单 + 刷新元数据（已存在的音频不重复下载）
 *   node scripts/download_all.js --meta-only  # 只重写 songs.json，不访问网络
 *   node scripts/download_all.js --force      # 忽略本地缓存，重新检索下载
 */
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import https from 'node:https';
import http from 'node:http';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');

const LIST_PATH = path.join(__dirname, 'songs.md');
const ID_SNAPSHOT_PATH = path.join(__dirname, 'data', 'song-ids.json');
const BGM_BASE_DIR = path.join(rootDir, 'public', 'bgm');
const SONGS_JSON_PATH = path.join(BGM_BASE_DIR, 'songs.json');

const CATEGORY_MAP = {
  '欢快': { en: 'cheerful', labelZh: '欢快', labelEn: 'Cheerful / Upbeat', icon: 'Sparkles', color: '#f59e0b' },
  '独处': { en: 'solitude', labelZh: '独处', labelEn: 'Solitude / Chill', icon: 'Coffee', color: '#6366f1' },
  '回忆': { en: 'memories', labelZh: '回忆', labelEn: 'Memories / Nostalgia', icon: 'Clock', color: '#ec4899' },
  '史诗': { en: 'epic', labelZh: '史诗', labelEn: 'Epic / Grand', icon: 'Flame', color: '#ef4444' },
  '古风': { en: 'gufeng', labelZh: '古风', labelEn: 'Traditional / GuFeng', icon: 'Feather', color: '#10b981' },
  '乡村': { en: 'country', labelZh: '乡村', labelEn: 'Country / Rural', icon: 'Sun', color: '#84cc16' }
};

const SPECIFIC_ALIASES = {
  '于人于物 细水长流 弥足珍贵': ['弥足珍贵', '细水长流 弥足珍贵', '细水长流'],
  'on more time': ['One More Time', 'One More Time 纯音乐', 'on more time'],
  'oh you beat': ['Oh You Beat', 'You Beat', 'beat 纯音乐'],
  '稳稳的幸福（钢琴）': ['稳稳的幸福 钢琴', '稳稳的幸福 伴奏', '稳稳的幸福'],
  '人生路慢慢': ['人生路慢慢', '人生路慢漫', '慢慢人生路'],
  '爱的供养（伴奏）': ['爱的供养 伴奏', '爱的供养 钢琴', '爱的供养'],
  'windy hill': ['Windy Hill', 'Windy Hill 纯音乐', 'Windy Hill bgm'],
  'flower dance': ['Flower Dance', 'Flower Dance 钢琴', 'Flower Dance bgm'],
  'river flow in you': ['River Flows in You', 'River Flows In You 纯音乐', 'River Flows In You 钢琴'],
  'kiss the rain': ['Kiss the Rain', '雨的印记', 'Kiss the Rain 钢琴'],
  'the truth that you leave': ['The Truth That You Leave', 'The Truth That You Leave 钢琴'],
  '所念皆星河': ['所念皆星河', '所念皆星河 伴奏', '所念皆星河 钢琴'],
  '萤火虫之歌': ['萤火虫之歌', '萤火虫之歌 纯音乐', '萤火之歌'],
  '一个人过冬': ['一个人过冬', '一个人过冬 纯音乐', '一个人过冬 伴奏'],
  'merry christmas mr.lawrence': ['Merry Christmas Mr. Lawrence', '坂本龙一 圣诞快乐劳伦斯先生', '圣诞快乐劳伦斯先生 钢琴'],
  '思绪回到那年秋': ['思绪回到那年秋', '思绪回到那年秋 纯音乐', '思绪回到那年秋 伴奏'],
  '你听啊秋末的落叶': ['你听啊秋末的落叶', '你听啊秋末的落叶 伴奏', '你听啊秋末的落叶 纯音乐'],
  '山上的山花开呀': ['山上的山花开呀', '山上的山花开', '山花开呀'],
  '樱花草': ['樱花草 纯音乐', '樱花草 伴奏', '樱花草 钢琴', '樱花草'],
  '阳光灿烂的日子（萨克斯）': ['阳光灿烂的日子 萨克斯', '阳光灿烂的日子 纯音乐', '阳光灿烂的日子'],
  '时间煮雨': ['时间煮雨 伴奏', '时间煮雨 钢琴', '时间煮雨 纯音乐', '时间煮雨'],
  '告白之夜': ['告白之夜', '告白の夜', '告白之夜 小提琴', '告白之夜 纯音乐'],
  'asphyxia': ['asphyxia 伴奏', 'asphyxia 纯音乐', 'asphyxia 钢琴', 'asphyxia'],
  '风居住的街道': ['风居住的街道', '风居住的街道 二胡', '风居住的街道 纯音乐'],
  '穿越时空的思念': ['穿越时空的思念', '穿越时空的思念 纯音乐', '穿越时空的思念 钢琴'],
  '城南花已开': ['城南花已开', '城南花已开 伴奏', '城南花已开 纯音乐'],
  '空山野马': ['空山野马', '空山野马 纯音乐', '空山野马 伴奏'],
  '难解': ['难解', '难解 黑神话', '难解 伴奏', '难解 纯音乐'],
  '三界四洲': ['三界四洲', '黑神话 三界四洲', '三界四洲 纯音乐'],
  '望月婵媛吟唱': ['望月婵媛', '望月婵媛 吟唱', '望月 吟唱'],
  '知我': ['知我', '知我 伴奏', '黑神话 知我', '知我 纯音乐'],
  '惊涛日落（红警）': ['惊涛日落', '红警 惊涛日落', 'Grinder 红警'],
  '好一朵茉莉花': ['好一朵茉莉花 纯音乐', '好一朵茉莉花 琵琶', '好一朵茉莉花 二胡', '好一朵茉莉花 交响'],
  '归零': ['归零', '归零 伴奏', '归零 纯音乐'],
  '天地缓缓': ['天地缓缓 伴奏', '天地缓缓 纯音乐', '天地缓缓'],
  'last reunion': ['Last Reunion', 'The Last Reunion', 'Last Reunion Peter Roe'],
  'rise': ['Rise John Dreamer', 'Rise 战歌', 'Rise The Glitch Mob'],
  'bloody mary': ['Bloody Mary instrumental', 'Bloody Mary 纯音乐', 'Bloody Mary 伴奏'],
  'star sky': ['Star Sky Instrumental', 'Star Sky Two Steps From Hell', 'Star Sky 战歌'],
  'heart of courage': ['Heart of Courage Two Steps From Hell', 'Heart of Courage 战歌'],
  'Empire of Angels': ['Empire of Angels Thomas Bergersen', 'Empire of Angels Two Steps From Hell'],
  'EI Dorado': ['El Dorado Two Steps From Hell', 'El Dorado 纯音乐', 'El Dorado'],
  'breath and life': ['Breath and Life Audiomachine', 'Breath and Life 纯音乐'],
  'go time': ['Go Time Mark Petrie', 'Go Time 战歌'],
  'sunburst': ['Sunburst Tobu', 'Sunburst 纯音乐'],
  'conquest of paradise': ['Conquest of Paradise Vangelis', 'Conquest of Paradise 纯音乐'],
  'he\'s a pirate': ['He\'s a Pirate 加勒比海盗', '加勒比海盗 主题曲', 'He\'s a Pirate 纯音乐'],
  'pacific rim': ['Pacific Rim 环太平洋', '环太平洋 主题曲', 'Pacific Rim 纯音乐'],
  'requiem for a tower': ['Requiem for a Tower', 'Requiem for a Tower Clint Mansell', 'Requiem for a Dream'],
  '水龙吟': ['水龙吟 原神', '水龙吟 Samudrartha', '水龙吟'],
  '精卫': ['精卫 伴奏', '精卫 纯音乐', '精卫 海来阿木'],
  '相许': ['相许 伴奏', '相许 纯音乐', '相许'],
  'intro': ['The XX Intro', 'Intro 纯音乐', 'Intro The xx'],
  'monkeys spring monkeys': ['Monkeys Spinning Monkeys Kevin MacLeod', 'Monkeys Spinning Monkeys bgm'],
  'monsieur melody': ['Monsieur Melody', 'Monsieur Melody bgm', 'Monsieur Melody 原版'],
  '偏爱': ['张芸京 偏爱', '偏爱 张芸京', '偏爱 仙剑奇侠传'],
  '此生不换': ['青鸟飞鱼 此生不换', '此生不换 青鸟飞鱼', '此生不换 TV Size'],
  '忘记时间': ['胡歌 忘记时间', '忘记时间 胡歌', '忘记时间 仙剑'],
  '莫失莫忘': ['麦振鸿 莫失莫忘', '莫失莫忘 麦振鸿', '莫失莫忘 仙剑奇侠传'],
  '雪见-落入凡尘': ['麦振鸿 雪见 落入凡尘', '雪见·落入凡尘 麦振鸿', '雪见 落入凡尘'],
  '悬溺': ['葛东琪 悬溺', '悬溺 葛东琪', '悬溺 原版'],
  '爱人错过': ['告五人 爱人错过', '爱人错过 告五人', '爱人错过 原版']
};

/* ------------------------------------------------------------------ *
 * 清单解析
 * ------------------------------------------------------------------ */

function parseSongsMd() {
  if (!fs.existsSync(LIST_PATH)) {
    throw new Error(`曲库清单缺失: ${LIST_PATH}（scripts/songs.md 必须提交到版本库）`);
  }
  const lines = fs.readFileSync(LIST_PATH, 'utf-8').split('\n');

  const categories = [];
  let current = null;
  let inComment = false;

  for (const raw of lines) {
    const line = raw.trim();
    if (!line) continue;

    if (inComment) {                       // HTML 注释块内一律忽略（许可证头等）
      if (line.includes('-->')) inComment = false;
      continue;
    }
    if (line.startsWith('<!--')) {
      if (!line.includes('-->')) inComment = true;
      continue;
    }
    if (line.startsWith('>')) continue;    // 说明性引用

    // Markdown 标题形式：## 欢快
    const heading = line.match(/^#{1,6}\s+(.+)$/);
    if (heading) {
      const token = heading[1].trim();
      if (CATEGORY_MAP[token]) {
        const meta = CATEGORY_MAP[token];
        current = { nameZh: token, nameEn: meta.en, labelEn: meta.labelEn, color: meta.color, songs: [] };
        categories.push(current);
      } else {
        current = null;                    // 非分类标题（如文件主标题）
      }
      continue;
    }

    const title = /^[-*]\s+/.test(line) ? line.replace(/^[-*]\s+/, '').trim() : line;
    if (current && title) current.songs.push(title);
  }

  const empty = categories.filter(c => c.songs.length === 0);
  if (empty.length) {
    console.warn(`! 清单中以下分类没有任何曲目: ${empty.map(c => c.nameZh).join(', ')}`);
  }
  return categories;
}

/* ------------------------------------------------------------------ *
 * 文件名 / URL
 * ------------------------------------------------------------------ */

// 只替换为下划线，避免直接删除字符造成不同曲目文件名冲突
function sanitizeFilename(name) {
  return name
    .replace(/[\\/:*?"<>|#%&+\s]+/g, '_')
    .replace(/\.{2,}/g, '.')
    .slice(0, 80);
}

// songs.json 存未编码的原始路径（作为稳定标识的一部分），前端加载时逐段 encodeURIComponent
function webPath(catEn, filename) {
  return `/bgm/${catEn}/${filename}`;
}

/* ------------------------------------------------------------------ *
 * 稳定 id：按 `分类/曲名` 锁定，清单增删不再影响已有曲目 id
 * ------------------------------------------------------------------ */

function loadIdSnapshot() {
  try {
    return JSON.parse(fs.readFileSync(ID_SNAPSHOT_PATH, 'utf-8'));
  } catch (e) {
    console.warn(`! 无法读取 id 快照 (${ID_SNAPSHOT_PATH})，将重新分配 id`);
    return {};
  }
}

function makeIdAllocator(snapshot, existingSongs) {
  const used = new Set();
  const isPositiveInt = (n) => Number.isInteger(n) && n >= 1;
  for (const s of existingSongs) {
    if (isPositiveInt(s.id)) used.add(s.id);
  }
  for (const [k, v] of Object.entries(snapshot)) {
    if (isPositiveInt(v)) used.add(v);
    else delete snapshot[k];            // 修正旧版游标从 0 起步留下的非法 id（含 0）
  }

  let cursor = 1;                         // id 从 1 起，绝不分配 0
  const nextFree = () => {
    let n = cursor;
    while (used.has(n)) n++;
    cursor = n + 1;
    used.add(n);
    return n;
  };

  return (catEn, title) => {
    const key = `${catEn}/${title}`;
    if (isPositiveInt(snapshot[key])) {
      used.add(snapshot[key]);
      return snapshot[key];
    }
    const id = nextFree();
    snapshot[key] = id;
    return id;
  };
}

// 与前端 songKey() 保持一致的稳定字符串键
const stableKey = (catEn, filename) => `${catEn}/${filename.replace(/\.mp3$/i, '')}`;

/* ------------------------------------------------------------------ *
 * 时长解析：优先 ffprobe，回退到 MPEG 帧头估算
 * ------------------------------------------------------------------ */

const MPEG_BITRATES = {
  '1.0': [0, 32, 40, 48, 56, 64, 80, 90, 96, 112, 128, 160, 192, 224, 256, 0],   // MPEG1 Layer3
  '2.5': [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0],      // MPEG2/2.5 Layer3
};
const SAMPLE_RATES = {
  '1.0': [44100, 48000, 32000],
  '2.5': [22050, 24000, 16000],
  '3.0': [11025, 12000, 8000],
};

function probeDurationWithFfprobe(filePath) {
  try {
    const out = execFileSync(
      'ffprobe',
      ['-v', 'error', '-show_entries', 'format=duration', '-of', 'csv=p=0', filePath],
      { encoding: 'utf-8', timeout: 15000, stdio: ['ignore', 'pipe', 'ignore'] }
    );
    const sec = parseFloat(out.trim());
    return Number.isFinite(sec) && sec > 0 ? Math.round(sec) : null;
  } catch (e) {
    return null;
  }
}

function scanSyncWord(fd, size) {
  const buf = Buffer.alloc(4);
  let pos = 0;
  while (pos + 4 <= size) {
    fs.readSync(fd, buf, 0, 4, pos);
    if (buf[0] === 0xff && (buf[1] & 0xe0) === 0xe0) return pos;
    pos += buf[0] === 0xff ? 1 : 4;
  }
  return -1;
}

function parseFrame(buf, size) {
  const versionBits = (buf[1] >> 3) & 0x03;      // 0=MPEG2.5, 2=MPEG2, 3=MPEG1
  const layerBits = (buf[1] >> 1) & 0x03;        // 1=Layer3
  if (layerBits !== 1) return null;
  const versionKey = versionBits === 3 ? '1.0' : versionBits === 0 ? '3.0' : '2.5';
  const bitrateIdx = (buf[2] >> 4) & 0x0f;
  const srIdx = (buf[2] >> 2) & 0x03;
  const padding = (buf[2] >> 1) & 0x01;

  const bitrates = versionKey === '1.0' ? MPEG_BITRATES['1.0'] : MPEG_BITRATES['2.5'];
  const bitrate = bitrates[bitrateIdx];
  const sampleRate = SAMPLE_RATES[versionKey][srIdx];
  if (!bitrate || !sampleRate) return null;

  const samplesPerFrame = versionKey === '1.0' ? 1152 : 576;
  const frameLen = Math.floor((samplesPerFrame / 8 * bitrate * 1000) / sampleRate) + padding;
  if (frameLen <= 0 || frameLen > size) return null;

  return { frameLen, duration: samplesPerFrame / sampleRate };
}

function probeDurationByFrames(filePath) {
  let fd;
  try {
    fd = fs.openSync(filePath, 'r');
    const size = fs.fstatSync(fd).size;
    const audioStart = scanSyncWord(fd, size);
    if (audioStart < 0) return null;

    const buf = Buffer.alloc(4);
    let pos = audioStart;
    let frames = 0;
    let seconds = 0;
    let bytes = 0;
    while (pos + 4 <= size && frames < 20) {
      fs.readSync(fd, buf, 0, 4, pos);
      const frame = parseFrame(buf, size);
      if (!frame) break;
      seconds += frame.duration;
      bytes += frame.frameLen;
      frames++;
      pos += frame.frameLen;
    }
    if (frames === 0 || bytes === 0) return null;

    // 采样帧求得平均单帧时长与平均帧长，再用「剩余音频字节 / 平均帧长」估算总帧数，
    // 两者相乘即为总时长（CBR 近似）。切勿直接返回平均单帧时长。
    const avgFrameDuration = seconds / frames;
    const avgFrameLen = bytes / frames;
    const audioBytes = Math.max(0, size - audioStart);
    const estFrames = audioBytes / avgFrameLen;
    if (!(estFrames > 0) || !(avgFrameDuration > 0)) return null;
    const total = estFrames * avgFrameDuration;
    return Number.isFinite(total) && total > 0 ? Math.round(total) : null;
  } catch (e) {
    return null;
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}

function probeDuration(filePath, fallbackSec = 180) {
  return probeDurationWithFfprobe(filePath) ?? (probeDurationByFrames(filePath) ?? fallbackSec);
}

/* ------------------------------------------------------------------ *
 * 网络检索（仅 --force / 缺失文件时才会走到这里）
 * ------------------------------------------------------------------ */

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

function normalizeCover(url) {
  if (!url) return '';
  return String(url).replace('{size}', '400').replace(/^http:\/\//, 'https://');
}

async function searchSong(keyword) {
  const clean = keyword.replace(/（[^）]*）|\([^\)]*\)/g, ' ').replace(/\s+/g, ' ').trim();
  const querySet = new Set();

  if (SPECIFIC_ALIASES[keyword]) {
    for (const a of SPECIFIC_ALIASES[keyword]) querySet.add(a);
  }

  querySet.add(keyword);
  if (clean && clean !== keyword) {
    querySet.add(clean);
  }
  querySet.add(`${keyword} bgm`);
  querySet.add(`${clean || keyword} 纯音乐`);
  querySet.add(`${clean || keyword} 伴奏`);
  querySet.add(`${clean || keyword} 钢琴`);

  const queries = Array.from(querySet);

  for (const q of queries) {
    const ip = randomIP();
    const searchUrl = `http://mobilecdn.kugou.com/api/v3/search/song?format=json&keyword=${encodeURIComponent(q)}&page=1&pagesize=15&showtype=1`;
    try {
      const res = await fetch(searchUrl, {
        headers: {
          'User-Agent': 'Mozilla/5.0 (iPhone; CPU iPhone OS 14_0 like Mac OS X)',
          'X-Forwarded-For': ip,
          'Client-IP': ip
        },
        signal: AbortSignal.timeout(5000)
      });
      const data = await res.json();
      const songs = data?.data?.info || [];

      for (const song of songs) {
        const pIp = randomIP();
        const infoUrl = `https://m.kugou.com/app/i/getSongInfo.php?cmd=playInfo&hash=${song.hash}`;
        try {
          const infoRes = await fetch(infoUrl, {
            headers: {
              'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)',
              'X-Forwarded-For': pIp,
              'Client-IP': pIp
            },
            signal: AbortSignal.timeout(4000)
          });
          const infoData = await infoRes.json();
          const playUrl = infoData.url || (infoData.backup_url && infoData.backup_url[0]);
          if (playUrl) {
            return {
              title: song.songname || keyword,
              artist: song.singername || '未知艺术家',
              duration: song.duration || infoData.timeLength || 180,
              cover: normalizeCover(infoData.imgUrl || infoData.album_img || ''),
              playUrl
            };
          }
        } catch (e) {
          // continue checking next song
        }
      }
    } catch (e) {
      // try next query
    }
  }

  return null;
}

function downloadFile(url, destPath, redirectCount = 0) {
  const MAX_REDIRECTS = 5;
  return new Promise((resolve, reject) => {
    const tmpPath = `${destPath}.part`;
    const file = fs.createWriteStream(tmpPath);
    const client = url.startsWith('https') ? https : http;

    const req = client.get(url, { headers: { 'User-Agent': 'Mozilla/5.0' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        fs.unlink(tmpPath, () => {});
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
        fs.unlink(tmpPath, () => {});
        reject(new Error(`Failed with status code: ${res.statusCode}`));
        return;
      }
      res.pipe(file);
      file.on('finish', () => {
        file.close(() => {
          fs.rename(tmpPath, destPath, (err) => (err ? reject(err) : resolve()));
        });
      });
    });

    req.on('error', (err) => {
      file.close();
      fs.unlink(tmpPath, () => {});
      reject(err);
    });

    req.setTimeout(30000, () => {
      req.destroy(new Error('Download timeout'));
    });
  });
}

/* ------------------------------------------------------------------ *
 * 主流程
 * ------------------------------------------------------------------ */

function readExistingSongs() {
  try {
    const data = JSON.parse(fs.readFileSync(SONGS_JSON_PATH, 'utf-8'));
    return Array.isArray(data) ? data : [];
  } catch (e) {
    return [];
  }
}

async function run() {
  const args = process.argv.slice(2);
  const metaOnly = args.includes('--meta-only');
  const force = args.includes('--force');

  const categories = parseSongsMd();
  const total = categories.reduce((n, c) => n + c.songs.length, 0);
  console.log(`Parsed ${categories.length} categories, ${total} songs from ${path.relative(rootDir, LIST_PATH)}`);

  if (!fs.existsSync(BGM_BASE_DIR)) fs.mkdirSync(BGM_BASE_DIR, { recursive: true });

  const existing = readExistingSongs();
  const existingByStable = new Map();
  for (const s of existing) {
    if (s && s.category && s.url) existingByStable.set(`${s.category}::${s.url}`, s);
  }

  const snapshot = loadIdSnapshot();
  const allocateId = makeIdAllocator(snapshot, existing);

  const allSongsMetadata = [];
  const seenUrls = new Map();
  const usedKeys = new Set();
  let downloadedCount = 0;
  let refreshedCount = 0;
  let failedCount = 0;
  let missingCount = 0;

  for (const cat of categories) {
    const catDir = path.join(BGM_BASE_DIR, cat.nameEn);
    if (!fs.existsSync(catDir)) fs.mkdirSync(catDir, { recursive: true });
    console.log(`\n=== Processing Category [${cat.nameZh} / ${cat.nameEn}] (${cat.songs.length} songs) ===`);

    for (let i = 0; i < cat.songs.length; i++) {
      const songTitle = cat.songs[i];
      const filename = `${sanitizeFilename(songTitle)}.mp3`;
      const targetPath = path.join(catDir, filename);
      const relativeWebPath = webPath(cat.nameEn, filename);
      const stable = stableKey(cat.nameEn, filename);
      const id = allocateId(cat.nameEn, songTitle);

      if (seenUrls.has(relativeWebPath)) {
        console.warn(`! 文件名冲突: "${songTitle}" 与 "${seenUrls.get(relativeWebPath)}" 落到同一个文件 ${relativeWebPath}`);
      } else {
        seenUrls.set(relativeWebPath, songTitle);
      }
      usedKeys.add(`${cat.nameEn}/${songTitle}`);

      const prev = existingByStable.get(`${cat.nameEn}::${relativeWebPath}`);
      let metadata = {
        id,
        key: stable,
        title: songTitle,
        category: cat.nameEn,
        categoryZh: cat.nameZh,
        categoryEn: cat.labelEn,
        color: cat.color,
        artist: prev?.artist || '未知艺术家',
        duration: prev?.duration || 180,
        durationFormatted: prev?.durationFormatted || '03:00',
        url: relativeWebPath,
        cover: normalizeCover(prev?.cover || ''),
        downloaded: false
      };

      const stat = fs.existsSync(targetPath) ? fs.statSync(targetPath) : null;
      const cached = !force && stat && stat.size > 100000;

      if (cached) {
        // 命中缓存同样要刷新元数据，避免占位值（180s / Various Artists）被永久固化
        const duration = probeDuration(targetPath, metadata.duration);
        metadata.duration = duration;
        metadata.durationFormatted = formatDuration(duration);
        const id3Artist = readId3Artist(targetPath);
        metadata.artist = hasRealArtist(metadata.artist) ? metadata.artist : (id3Artist || prev?.artist || '未知艺术家');
        metadata.downloaded = true;
        metadata.fileSize = stat.size;
        if (duration > 1800 || stat.size > 25 * 1024 * 1024) {
          console.warn(`  ! 异常素材: ${songTitle} 时长 ${(duration / 60).toFixed(1)} 分钟 / ${(stat.size / 1024 / 1024).toFixed(1)} MB，请确认是否误下了整张专辑`);
        } else if (duration < 30) {
          console.warn(`  ! 疑似下载不完整: ${songTitle} 仅 ${duration} 秒 (${(stat.size / 1024).toFixed(0)} KB)，建议 --force 重新获取`);
        }
        if (!prev || prev.duration !== duration) refreshedCount++;
        console.log(`[${i + 1}/${cat.songs.length}] (Cached ${(stat.size / 1024 / 1024).toFixed(2)} MB) ${songTitle}`);
        allSongsMetadata.push(metadata);
        continue;
      }

      if (metaOnly) {
        console.log(`[${i + 1}/${cat.songs.length}] (Not on disk) ${songTitle} -> downloaded: false`);
        allSongsMetadata.push(metadata);
        missingCount++;
        continue;
      }

      console.log(`[${i + 1}/${cat.songs.length}] Searching: ${songTitle}...`);
      const searchRes = await searchSong(songTitle);

      if (searchRes && searchRes.playUrl) {
        metadata.artist = searchRes.artist;
        metadata.duration = searchRes.duration;
        metadata.durationFormatted = formatDuration(searchRes.duration);
        metadata.cover = searchRes.cover;

        try {
          console.log(`  -> Downloading for "${songTitle}" (${searchRes.artist})...`);
          await downloadFile(searchRes.playUrl, targetPath);
          const newStat = fs.statSync(targetPath);
          console.log(`  -> Success! Size: ${(newStat.size / 1024 / 1024).toFixed(2)} MB`);
          metadata.downloaded = true;
          metadata.fileSize = newStat.size;
          const probed = probeDuration(targetPath, metadata.duration);
          metadata.duration = probed;
          metadata.durationFormatted = formatDuration(probed);
          downloadedCount++;
        } catch (err) {
          console.error(`  -> Download failed: ${err.message}`);
          failedCount++;
        }
      } else {
        console.warn(`  -> Not found online: ${songTitle}`);
        failedCount++;
      }

      allSongsMetadata.push(metadata);
      await new Promise(r => setTimeout(r, 150));
    }
  }

  // 写入前按唯一数据契约校验，脏条目不落盘
  const { validateSong } = await import('../src/utils/songSchema.js');
  const validSongs = [];
  const seenKeys = new Set();
  const rejected = [];
  for (const s of allSongsMetadata) {
    const check = validateSong(s);
    if (!check.ok) {
      rejected.push(`${s?.title ?? '<无标题>'}: ${check.reason}`);
      continue;
    }
    if (seenKeys.has(s.key)) {
      rejected.push(`${s.title}: key 重复 ${s.key}`);
      continue;
    }
    seenKeys.add(s.key);
    validSongs.push(s);
  }
  if (rejected.length) {
    console.warn(`\n! 丢弃 ${rejected.length} 条不合格元数据:`);
    for (const r of rejected) console.warn(`    ${r}`);
  }

  // 剪除清单中已删除的条目，保持快照与清单同构
  const pruned = Object.keys(snapshot).filter(k => !usedKeys.has(k));
  for (const k of pruned) delete snapshot[k];
  if (pruned.length) console.log(`\n- id 快照剪除 ${pruned.length} 条已下架曲目`);

  fs.mkdirSync(path.dirname(SONGS_JSON_PATH), { recursive: true });
  fs.writeFileSync(SONGS_JSON_PATH, JSON.stringify(validSongs, null, 2) + '\n', 'utf-8');
  fs.mkdirSync(path.dirname(ID_SNAPSHOT_PATH), { recursive: true });
  fs.writeFileSync(ID_SNAPSHOT_PATH, JSON.stringify(snapshot, null, 2) + '\n', 'utf-8');

  const present = validSongs.filter(s => s.downloaded).length;
  console.log('\n======================================');
  console.log(`Done! Total songs: ${validSongs.length}`);
  console.log(`Files on disk: ${present}/${validSongs.length}; newly downloaded: ${downloadedCount}; metadata refreshed: ${refreshedCount}; missing (not on disk): ${missingCount}; problems: ${failedCount + rejected.length}`);
  console.log(`Metadata saved to ${path.relative(rootDir, SONGS_JSON_PATH)}`);
  console.log(`id snapshot saved to ${path.relative(rootDir, ID_SNAPSHOT_PATH)}`);
}

// 历史数据里 'Various Artists' 是下载失败时的占位值，不能当作真实艺术家
const PLACEHOLDER_ARTISTS = new Set(['', '未知艺术家', 'Various Artists']);
const hasRealArtist = (name) => !!name && !PLACEHOLDER_ARTISTS.has(name);

function decodeId3Text(payload) {
  const enc = payload[0];
  let text;
  if (enc === 1) {
    let start = 1;
    if (payload[start] === 0xff && payload[start + 1] === 0xfe) start = 3;
    text = payload.subarray(start).toString('utf16le');
  } else {
    text = payload.subarray(1).toString(enc === 2 ? 'utf8' : 'latin1');
  }
  return text.split('\0')[0].trim();
}

// 遍历 ID3v2 帧取 TPE1（艺术家）；标签体可能很大（内嵌封面），按头部尺寸整段读取
function readId3Artist(filePath) {
  let fd;
  try {
    fd = fs.openSync(filePath, 'r');
    const head = Buffer.alloc(10);
    if (fs.readSync(fd, head, 0, 10, 0) < 10) return null;
    if (head.toString('latin1', 0, 3) !== 'ID3') return null;

    const version = head[3];
    const size = ((head[6] & 0x7f) << 21) | ((head[7] & 0x7f) << 14) | ((head[8] & 0x7f) << 7) | (head[9] & 0x7f);
    const tag = Buffer.alloc(Math.min(size, 2 * 1024 * 1024));
    fs.readSync(fd, tag, 0, tag.length, 10);

    // tag 已从偏移 10（即首帧起始）读入，v2.3/v2.4 帧体均从 0 开始；
    // 仅当头部 flags 置了扩展头位（0x40）时才按其长度跳过。
    let offset = 0;
    if (head[5] & 0x40) {
      if (version === 4) {
        // v2.4 扩展头大小为 syncsafe，且包含自身 4 字节
        const extSize = ((tag[0] & 0x7f) << 21) | ((tag[1] & 0x7f) << 14) | ((tag[2] & 0x7f) << 7) | (tag[3] & 0x7f);
        offset = extSize;
      } else {
        // v2.3 扩展头大小为普通 UInt32，不含自身 4 字节
        offset = 4 + tag.readUInt32BE(0);
      }
    }
    while (offset + 10 <= tag.length) {
      const id = tag.toString('latin1', offset, offset + 4);
      if (!/^[A-Z0-9]{4}$/.test(id)) break;
      const frameSize = version === 4
        ? ((tag[offset + 4] & 0x7f) << 21) | ((tag[offset + 5] & 0x7f) << 14) | ((tag[offset + 6] & 0x7f) << 7) | (tag[offset + 7] & 0x7f)
        : tag.readUInt32BE(offset + 4);
      if (frameSize <= 0 || offset + 10 + frameSize > tag.length) break;
      if (id === 'TPE1') {
        const value = decodeId3Text(tag.subarray(offset + 10, offset + 10 + frameSize));
        if (value) return value;
      }
      offset += 10 + frameSize;
    }
    return null;
  } catch (e) {
    return null;
  } finally {
    if (fd !== undefined) fs.closeSync(fd);
  }
}

run().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
