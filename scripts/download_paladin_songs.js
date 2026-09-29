import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import https from 'node:https';
import http from 'node:http';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');

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
  const gufengDir = path.join(rootDir, 'public', 'bgm', 'gufeng');
  if (!fs.existsSync(gufengDir)) {
    fs.mkdirSync(gufengDir, { recursive: true });
  }

  const results = [];

  for (const song of SONGS_TO_DOWNLOAD) {
    console.log(`\nFetching play info for [${song.title}] by ${song.artist}...`);
    const info = await getSongInfo(song.hash);
    const playUrl = info.url || (info.backup_url && info.backup_url[0]);
    if (!playUrl) {
      throw new Error(`Could not get play URL for ${song.title}`);
    }

    const destPath = path.join(gufengDir, song.filename);
    console.log(`Downloading to ${destPath}...`);
    await downloadFile(playUrl, destPath);

    const stat = fs.statSync(destPath);
    console.log(`Downloaded ${song.title}: ${(stat.size / 1024 / 1024).toFixed(2)} MB`);

    const duration = info.timeLength || 180;
    const cover = (info.imgUrl ? info.imgUrl.replace('{size}', '400') : (info.album_img ? info.album_img.replace('{size}', '400') : '')).replace(/^http:\/\//, 'https://');

    results.push({
      title: song.title,
      category: 'gufeng',
      categoryZh: '古风',
      categoryEn: 'Traditional / GuFeng',
      color: '#10b981',
      artist: song.artist,
      duration: duration,
      durationFormatted: formatDuration(duration),
      url: `/bgm/gufeng/${song.filename}`,
      cover: cover,
      downloaded: true,
      fileSize: stat.size
    });

    // Small delay to be polite
    await new Promise(r => setTimeout(r, 200));
  }

  console.log('\nAll 5 songs downloaded successfully!');
  console.log(JSON.stringify(results, null, 2));
}

run().catch(console.error);
