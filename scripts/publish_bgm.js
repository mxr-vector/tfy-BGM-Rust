/**
 * 云盘发布脚本：把体积较大的音频素材从 git 仓库中剥离，按 songs.json 的引用清单
 * 打包到 dist-bgm/public/，连同校验和清单一起上传云盘即可。
 *
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 *
 * 用法：
 *   node scripts/publish_bgm.js            # 生成 dist-bgm/
 *   node scripts/publish_bgm.js --clean    # 生成后删除旧产物再打包
 *   node scripts/publish_bgm.js --zip      # 额外打包成 dist-bgm.zip
 *
 * 上传后告诉前端音频地址：
 *   构建期 VITE_BGM_BASE=https://host/public pnpm build
 *   或运行时 localStorage.setItem('tfy_bgm_base', 'https://host/public')
 */
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const rootDir = path.resolve(__dirname, '..');

const SRC_DIR = path.join(rootDir, 'public', 'bgm');
const OUT_DIR = path.join(rootDir, 'dist-bgm', 'public', 'bgm');
const MANIFEST_PATH = path.join(rootDir, 'dist-bgm', 'bgm-manifest.json');

function humanSize(bytes) {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
}

function sha256(filePath) {
  return crypto.createHash('sha256').update(fs.readFileSync(filePath)).digest('hex');
}

function rmrf(target) {
  if (fs.existsSync(target)) fs.rmSync(target, { recursive: true, force: true });
}

function zipDir(cwd, outName) {
  try {
    if (process.platform === 'win32') {
      console.warn('! --zip 在 Windows 下需自行打包，已跳过');
      return false;
    }
    fs.rmSync(path.join(cwd, `${outName}.zip`), { force: true });
    execFileSync('zip', ['-qr', `${outName}.zip`, outName], { cwd });
    return true;
  } catch (e) {
    console.warn(`! 打包 zip 失败（未安装 zip 命令？）: ${e.message}`);
    return false;
  }
}

function run() {
  const args = process.argv.slice(2);
  const doZip = args.includes('--zip');

  const songsPath = path.join(SRC_DIR, 'songs.json');
  if (!fs.existsSync(songsPath)) {
    throw new Error(`缺少 ${path.relative(rootDir, songsPath)}，请先运行 node scripts/download_all.js`);
  }
  const songs = JSON.parse(fs.readFileSync(songsPath, 'utf-8'));
  if (!Array.isArray(songs) || songs.length === 0) {
    throw new Error('songs.json 不是非空数组，拒绝发布');
  }

  if (args.includes('--clean')) rmrf(path.join(rootDir, 'dist-bgm'));
  rmrf(OUT_DIR);
  fs.mkdirSync(OUT_DIR, { recursive: true });

  const files = [];
  const missing = [];

  for (const song of songs) {
    if (!song?.url) continue;
    const rel = song.url.replace(/^\/+/, '').replace(/^bgm\//, '');   // 分类/文件.mp3
    const src = path.join(SRC_DIR, rel);
    if (!fs.existsSync(src)) {
      missing.push(song.url);
      continue;
    }
    const dest = path.join(OUT_DIR, rel);
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.copyFileSync(src, dest);
    const stat = fs.statSync(dest);
    files.push({ path: `bgm/${rel}`, size: stat.size, sha256: sha256(dest) });
  }

  fs.copyFileSync(songsPath, path.join(OUT_DIR, 'songs.json'));

  const totalBytes = files.reduce((n, f) => n + f.size, 0);
  const manifest = {
    generatedAt: new Date().toISOString(),
    songCount: songs.length,
    fileCount: files.length,
    totalBytes,
    totalSize: humanSize(totalBytes),
    missing,
    files
  };
  fs.writeFileSync(MANIFEST_PATH, JSON.stringify(manifest, null, 2) + '\n', 'utf-8');

  console.log('======================================');
  console.log(`发布目录: ${path.relative(rootDir, path.join(rootDir, 'dist-bgm'))}/`);
  console.log(`音频 ${files.length}/${songs.length} 个，共 ${humanSize(totalBytes)}`);
  if (missing.length) {
    console.warn(`! 清单引用但磁盘缺失 ${missing.length} 个:`);
    for (const m of missing) console.warn(`    ${m}`);
  }
  console.log(`校验清单: ${path.relative(rootDir, MANIFEST_PATH)}`);
  console.log('上传云盘后，用 VITE_BGM_BASE（或 localStorage tfy_bgm_base）指向网盘中的 public 目录即可。');

  if (doZip) {
    if (zipDir(path.join(rootDir, 'dist-bgm'), 'bgm')) {
      console.log(`已打包: ${path.relative(rootDir, path.join(rootDir, 'dist-bgm', 'bgm.zip'))}`);
    }
  }
}

run();
