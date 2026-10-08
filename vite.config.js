import vue from '@vitejs/plugin-vue'
import { defineConfig, normalizePath } from 'vite'
import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { scanBgmSongs, warnBgmScanIssues } from './scripts/scan_bgm.js'

// 曲库清单虚拟模块：构建 / dev 期直接扫描 public/bgm 目录下的 mp3 文件名生成，
// 前端 import 后即为最新曲目列表，无需索引文件（songs.json）。
const VIRTUAL_ID = 'virtual:bgm-manifest'
const RESOLVED_ID = `\0${VIRTUAL_ID}`

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '.')
// chokidar 回调路径为正斜杠，前缀比较需统一格式（Windows 下 path.join 会生成本地分隔符）
const bgmDir = normalizePath(path.join(projectRoot, 'public', 'bgm'))

function bgmManifestPlugin() {
  return {
    name: 'bgm-manifest',
    resolveId(id) {
      if (id === VIRTUAL_ID) return RESOLVED_ID
    },
    load(id) {
      if (id !== RESOLVED_ID) return
      const songs = scanBgmSongs(bgmDir)
      for (const w of warnBgmScanIssues(bgmDir, songs)) {
        this.warn(`[bgm-manifest] ${w}`)
      }
      return `export default ${JSON.stringify(songs)}`
    },
    configureServer(server) {
      // public 目录默认不被监听，显式加入后 mp3 增删即时刷新清单
      if (fs.existsSync(bgmDir)) server.watcher.add(bgmDir)
      server.watcher.on('all', (_event, file) => {
        if (!normalizePath(file).startsWith(bgmDir)) return
        const mod = server.moduleGraph.getModuleById(RESOLVED_ID)
        if (mod) server.moduleGraph.invalidateModule(mod)
      })
    },
  }
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [bgmManifestPlugin(), vue()],
})
