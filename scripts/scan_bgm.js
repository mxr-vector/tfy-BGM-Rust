/**
 * 曲库目录扫描器：直接读取 public/bgm/<分类>/ 下的 .mp3 文件名生成曲目清单。
 * 曲库不再有索引文件（songs.json）—— 文件名即曲名，替换 / 增删 mp3 后清单自动同步，
 * 不会出现「时长、作者与实际音频不匹配」的脏元数据。
 *
 * 消费方：
 *   - vite.config.js：构建 / dev 期把扫描结果注入虚拟模块 `virtual:bgm-manifest`
 *   - scripts/publish_bgm.js：按扫描结果打包云盘发布内容
 *
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 */
import fs from 'node:fs';
import path from 'node:path';
import { CATEGORIES } from '../src/utils/categories.js';

/**
 * @param {string} bgmDir public/bgm 绝对路径
 * @returns {Array<{key:string,title:string,category:string,categoryZh:string,categoryEn:string,color:string,url:string}>}
 */
export function scanBgmSongs(bgmDir) {
  const songs = [];
  for (const cat of CATEGORIES) {
    const dir = path.join(bgmDir, cat.id);
    if (!fs.existsSync(dir)) continue;
    for (const file of fs.readdirSync(dir).sort((a, b) => a.localeCompare(b, 'zh'))) {
      if (!file.toLowerCase().endsWith('.mp3')) continue;
      const title = file.replace(/\.mp3$/i, '');
      songs.push({
        key: `${cat.id}/${title}`,
        title,
        category: cat.id,
        categoryZh: cat.nameZh,
        categoryEn: cat.labelEn,
        color: cat.color,
        // 未编码站内原始路径，前端渲染前逐段 encodeURIComponent
        url: `/bgm/${cat.id}/${file}`,
      });
    }
  }
  return songs;
}
