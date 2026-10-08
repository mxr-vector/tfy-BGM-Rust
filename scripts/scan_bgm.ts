/**
 * 曲库目录扫描器：直接读取 public/bgm/<分类>/ 下的 .mp3 文件名生成曲目清单。
 * 曲库不再有索引文件（songs.json）—— 文件名即曲名，替换 / 增删 mp3 后清单自动同步，
 * 不会出现「时长、作者与实际音频不匹配」的脏元数据。
 *
 * 消费方：
 *   - vite.config.ts：构建 / dev 期把扫描结果注入虚拟模块 `virtual:bgm-manifest`
 *   - scripts/publish_bgm.ts：按扫描结果打包云盘发布内容
 *
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 */
import fs from 'node:fs';
import path from 'node:path';
import { CATEGORIES } from '../src/utils/categories.ts';
import type { BgmManifestEntry } from '../src/types/bgm.ts';

/**
 * @param bgmDir public/bgm 绝对路径
 * @returns key 为「分类/文件名主干」（不受改名外的元数据影响，收藏/队列据此保持稳定）；
 *   展示用的分类中文名 / 英文名 / 主题色由前端运行时从 categories.ts 派生，不进清单。
 */
export function scanBgmSongs(bgmDir: string): BgmManifestEntry[] {
  const songs: BgmManifestEntry[] = [];
  for (const cat of CATEGORIES) {
    const dir = path.join(bgmDir, cat.id);
    if (!fs.existsSync(dir)) continue;
    for (const file of fs.readdirSync(dir).sort((a, b) => a.localeCompare(b, 'zh'))) {
      if (!file.toLowerCase().endsWith('.mp3')) continue;
      const stem = file.replace(/\.mp3$/i, '');
      songs.push({
        key: `${cat.id}/${stem}`,
        // 下划线视为空格分隔（kiss_the_rain -> kiss the rain），文件名主干仍用原始 stem 做 key
        title: stem.replace(/_/g, ' '),
        category: cat.id,
        // 未编码站内原始路径，前端渲染前逐段 encodeURIComponent
        url: `/bgm/${cat.id}/${file}`,
      });
    }
  }
  return songs;
}

/**
 * 扫描结果体检：目录不存在 / 扫出 0 首 / mp3 落在未注册目录或 bgm 根目录时输出告警，
 * 避免「放进去的音频无声消失」的新手故障只能靠播放失败才发现。构建日志可见，不阻断构建。
 */
export function warnBgmScanIssues(bgmDir: string, songs: BgmManifestEntry[]): string[] {
  const warnings: string[] = [];
  const rel = (p: string) => p.split(path.sep).join('/');
  if (!fs.existsSync(bgmDir)) {
    warnings.push(`${rel(bgmDir)} 目录不存在，曲库清单为空`);
    return warnings;
  }
  if (songs.length === 0) {
    warnings.push(`${rel(bgmDir)} 下未扫描到任何 mp3，页面曲库将为空（音频需随应用部署或配置 VITE_BGM_BASE）`);
  }
  const known = new Set<string>(CATEGORIES.map(c => c.id));
  for (const name of fs.readdirSync(bgmDir)) {
    const full = path.join(bgmDir, name);
    if (!fs.statSync(full).isDirectory()) {
      if (name.toLowerCase().endsWith('.mp3')) {
        warnings.push(`${rel(full)} 不在任何分类目录中，已忽略（请移入 public/bgm/<分类>/）`);
      }
      continue;
    }
    if (known.has(name)) continue;
    const stray = fs.readdirSync(full).filter(f => f.toLowerCase().endsWith('.mp3'));
    if (stray.length > 0) {
      warnings.push(`目录 ${rel(full)}/ 不在已注册分类（${[...known].join('/')}）中，其 ${stray.length} 个 mp3 已被忽略`);
    }
  }
  return warnings;
}
