/**
 * 分类元数据：曲库目录名 ↔ 展示信息（中文名 / 英文名 / 主题色）的唯一事实来源。
 * 构建期（vite.config.js 扫描 public/bgm 目录）与运行时（侧边栏、分类横幅、曲目着色）共用，
 * 避免同一份分类信息在多处漂移。
 *
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 */

export const CATEGORIES = [
  { id: 'cheerful', nameZh: '欢快', nameEn: 'Cheerful', labelEn: 'Cheerful / Upbeat', color: '#f59e0b' },
  { id: 'solitude', nameZh: '独处', nameEn: 'Solitude', labelEn: 'Solitude / Chill', color: '#6366f1' },
  { id: 'memories', nameZh: '回忆', nameEn: 'Memories', labelEn: 'Memories / Nostalgia', color: '#ec4899' },
  { id: 'epic', nameZh: '史诗', nameEn: 'Epic', labelEn: 'Epic / Grand', color: '#ef4444' },
  { id: 'gufeng', nameZh: '古风', nameEn: 'GuFeng', labelEn: 'Traditional / GuFeng', color: '#10b981' },
  { id: 'country', nameZh: '乡村', nameEn: 'Country', labelEn: 'Country / Rural', color: '#84cc16' },
];

export const CATEGORY_MAP = Object.fromEntries(CATEGORIES.map(c => [c.id, c]));
