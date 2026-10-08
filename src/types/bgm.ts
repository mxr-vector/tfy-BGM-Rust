/**
 * 曲库共享类型：构建期扫描产物（清单条目）与前端运行时曲目模型。
 * node 侧（scripts/scan_bgm.ts、vite.config.ts）与前端共用同一份定义，
 * 清单结构一旦调整，编译期即可发现所有消费方，不会拖到运行时才炸。
 *
 * Copyright 2026 YuanJie
 * SPDX-License-Identifier: Apache-2.0
 */

/** 已注册分类目录名，与 utils/categories.ts 的 CATEGORIES 一一对应 */
export type CategoryId = 'cheerful' | 'solitude' | 'memories' | 'epic' | 'gufeng' | 'country';

/** 分类元数据（目录名 ↔ 展示信息），前端与构建期共用 */
export interface Category {
  id: CategoryId;
  /** 中文分类名 */
  nameZh: string;
  /** 英文分类名（标题、侧边栏副标题） */
  nameEn: string;
  /** 横幅 / 标签用的大写英文标语 */
  labelEn: string;
  /** 主题色（十六进制，供 CSS 变量与内联样式使用） */
  color: string;
}

/**
 * 构建期扫描 public/bgm 目录生成的清单条目：只有「曲名（=文件名）+ 分类 + 站内路径」，
 * 不携带时长 / 作者等易与实际音频失配的元数据。
 */
export interface BgmManifestEntry {
  /** 稳定标识：`分类/文件名主干`，收藏与队列据此保持不漂移 */
  key: string;
  /** 展示曲名：文件名主干，下划线还原为空格 */
  title: string;
  category: CategoryId;
  /** 未编码的站内原始路径，如 `/bgm/cheerful/kiss_the_rain.mp3` */
  url: string;
}

/** 运行时曲目：清单条目 + categories.ts 派生的展示信息（中文名 / 英文标语 / 主题色） */
export interface Song extends BgmManifestEntry {
  categoryZh: string;
  categoryEn: string;
  color: string;
}

/** 当前选中的筛选目标：全部分类 / 某个具体分类 */
export type ActiveCategory = CategoryId | 'all';

/** 列表排序方式 */
export type SortOrder = 'default' | 'name';

/** 侧边栏 / 快捷胶囊用的分类项：分类元数据 + 该分类下的曲目数 */
export interface CategoryWithCount extends Category {
  count: number;
}
