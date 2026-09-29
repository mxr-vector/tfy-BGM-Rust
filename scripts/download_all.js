import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import https from 'node:https';
import http from 'node:http';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');

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

function randomIP() {
  return [
    Math.floor(Math.random() * 150 + 50),
    Math.floor(Math.random() * 250 + 1),
    Math.floor(Math.random() * 250 + 1),
    Math.floor(Math.random() * 250 + 1)
  ].join('.');
}

function parseIndexMd() {
  const mdPath = path.join(rootDir, 'data', 'index.md');
  const content = fs.readFileSync(mdPath, 'utf-8');
  const lines = content.split('\n').map(l => l.trim());
  
  const categories = [];
  let currentCategory = null;

  for (const line of lines) {
    if (!line) continue;
    if (CATEGORY_MAP[line]) {
      currentCategory = {
        nameZh: line,
        nameEn: CATEGORY_MAP[line].en,
        labelEn: CATEGORY_MAP[line].labelEn,
        color: CATEGORY_MAP[line].color,
        songs: []
      };
      categories.push(currentCategory);
    } else if (currentCategory) {
      currentCategory.songs.push(line);
    }
  }

  return categories;
}

function sanitizeFilename(name) {
  return name
    .replace(/[\\\/\:\*\?\"\<\>\|]/g, '')
    .replace(/\s+/g, '_')
    .slice(0, 80);
}

function formatDuration(sec) {
  if (!sec || isNaN(sec)) return '03:00';
  const m = Math.floor(sec / 60);
  const s = Math.floor(sec % 60);
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
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
              artist: song.singername || '网络歌手',
              duration: song.duration || infoData.timeLength || 180,
              cover: (infoData.imgUrl ? infoData.imgUrl.replace('{size}', '400') : (infoData.album_img ? infoData.album_img.replace('{size}', '400') : '')).replace(/^http:\/\//, 'https://'),
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

function downloadFile(url, destPath) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(destPath);
    const client = url.startsWith('https') ? https : http;
    
    const req = client.get(url, { headers: { 'User-Agent': 'Mozilla/5.0' } }, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        fs.unlink(destPath, () => {});
        downloadFile(res.headers.location, destPath).then(resolve).catch(reject);
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

async function run() {
  const categories = parseIndexMd();
  console.log(`Parsed ${categories.length} categories.`);

  const bgmBaseDir = path.join(rootDir, 'public', 'bgm');
  if (!fs.existsSync(bgmBaseDir)) fs.mkdirSync(bgmBaseDir, { recursive: true });

  const allSongsMetadata = [];
  let songId = 1;

  for (const cat of categories) {
    const catDir = path.join(bgmBaseDir, cat.nameEn);
    if (!fs.existsSync(catDir)) fs.mkdirSync(catDir, { recursive: true });
    console.log(`\n=== Processing Category [${cat.nameZh} / ${cat.nameEn}] (${cat.songs.length} songs) ===`);

    for (let i = 0; i < cat.songs.length; i++) {
      const songTitle = cat.songs[i];
      const safeName = sanitizeFilename(songTitle);
      const filename = `${safeName}.mp3`;
      const targetPath = path.join(catDir, filename);
      const relativeWebPath = `/bgm/${cat.nameEn}/${filename}`;

      let metadata = {
        id: songId++,
        title: songTitle,
        category: cat.nameEn,
        categoryZh: cat.nameZh,
        categoryEn: cat.labelEn,
        color: cat.color,
        artist: 'Various Artists',
        duration: 180,
        durationFormatted: '03:00',
        url: relativeWebPath,
        cover: '',
        downloaded: false
      };

      // Check if file already exists with valid size
      if (fs.existsSync(targetPath) && fs.statSync(targetPath).size > 100000) {
        const stat = fs.statSync(targetPath);
        console.log(`[${i + 1}/${cat.songs.length}] (Cached: ${(stat.size / 1024 / 1024).toFixed(2)} MB) ${songTitle}`);
        metadata.downloaded = true;
        metadata.fileSize = stat.size;
        allSongsMetadata.push(metadata);
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
          const stat = fs.statSync(targetPath);
          console.log(`  -> Success! Size: ${(stat.size / 1024 / 1024).toFixed(2)} MB`);
          metadata.downloaded = true;
          metadata.fileSize = stat.size;
        } catch (err) {
          console.error(`  -> Download failed: ${err.message}`);
        }
      } else {
        console.warn(`  -> Not found online: ${songTitle}`);
      }

      allSongsMetadata.push(metadata);
      await new Promise(r => setTimeout(r, 150));
    }
  }

  // Save metadata
  const jsonPathPublic = path.join(bgmBaseDir, 'songs.json');
  fs.writeFileSync(jsonPathPublic, JSON.stringify(allSongsMetadata, null, 2), 'utf-8');

  console.log('\n======================================');
  console.log(`Done! Total songs: ${allSongsMetadata.length}`);
  console.log(`Downloaded: ${allSongsMetadata.filter(s => s.downloaded).length}/${allSongsMetadata.length}`);
  console.log(`Metadata saved to ${jsonPathPublic}`);
}

run().catch(console.error);
