/// <reference types="vite/client" />

/**
 * 构建期虚拟模块：vite.config.ts 扫描 public/bgm 目录后注入的曲库清单。
 * 前端 import 到的即 `BgmManifestEntry[]`，与扫描器返回结构由类型系统锁死。
 */
declare module 'virtual:bgm-manifest' {
  import type { BgmManifestEntry } from './types/bgm.ts';
  const manifest: BgmManifestEntry[];
  export default manifest;
}

/** 环境变量：音频分发地址（见 .env.example / README「音频地址」） */
interface ImportMetaEnv {
  readonly VITE_BGM_BASE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
