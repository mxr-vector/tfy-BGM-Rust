# TFY · 视频剪辑 BGM 音频素材库与试听平台

专为视频剪辑量身定制的高颜值背景音乐库与试听平台，采用 **Vue 3 + Vite** 构建，拥有现代化暗黑磨砂玻璃（Glassmorphism）UI、沉浸式交互与全功能音频播放器。

---

## 📦 仓库结构与素材分发

曲库只有**一份事实来源**，音频本体不进 git：

| 内容 | 位置 | 是否进 git |
| :--- | :--- | :--- |
| 曲库清单（人工维护，一行一曲） | `scripts/songs.md` | ✅ |
| 稳定 id 快照（按「分类+曲名」锁定） | `scripts/data/song-ids.json` | ✅ |
| 数据契约定义（前后端共用） | `src/utils/songSchema.js` | ✅ |
| 曲库元数据（脚本生成） | `public/bgm/songs.json` | ✅ |
| 音频本体（~330 MB） | `public/bgm/<分类>/*.mp3` | ❌ 云盘分发 |
| 云盘发布产物 | `dist-bgm/` | ❌ |

```
scripts/songs.md ──node scripts/download_all.js──▶ public/bgm/songs.json + public/bgm/<分类>/*.mp3
                                   └─node scripts/publish_bgm.js─▶ dist-bgm/（上传云盘）
```

---

## 📁 分类与英文目录规范

所有 BGM 已按类别创建对应标准英文文件夹（下表数量仅供参考，以 `scripts/songs.md` 为准）：

| 中文分类 | 英文目录名 | 曲目数量 | 适用剪辑场景 | 目录路径 |
| :--- | :--- | :--- | :--- | :--- |
| **欢快** | `cheerful` | 20 首 | 生活日常 Vlog、开箱解说、趣味搞笑、快节奏卡点 | `public/bgm/cheerful/` |
| **独处** | `solitude` | 19 首 | 深夜独白、治愈流行、静心学习/工作、自律慢生活 | `public/bgm/solitude/` |
| **回忆** | `memories` | 11 首 | 怀旧温情、青春毕业季、岁月光影、情感纪实 | `public/bgm/memories/` |
| **史诗** | `epic` | 28 首 | 燃向混剪、大片战歌、高光集锦、企业宣传片 | `public/bgm/epic/` |
| **古风** | `gufeng` | 13 首 | 国风雅韵、汉服走秀、仙剑经典、唯美武侠 | `public/bgm/gufeng/` |
| **乡村** | `country` | 1 首 | 乡野烟火、农家田园、悠闲度假风景 | `public/bgm/country/` |

---

## ✨ 核心特性

1. **现代化 UI 审美**
   - 沉浸式暗色调 + 磨砂玻璃（Glassmorphism）质感。
   - 各分类专属动态色彩流光（琥珀金、深邃紫、玫瑰粉、烈焰红、翡翠绿、青柠绿）。
   - 正在播放时音频跳动可视化动效（Equalizer）与黑胶唱片旋转动效。
2. **多维筛选与极速检索**
   - 分类标签一键切换、收藏夹过滤。
   - 实时响应式搜索（快捷键 `/` 快速聚焦搜索框）。
   - 支持网格卡片（Grid）与高密度列表（List）双视图无缝切换。
   - 多种排序方式（默认推荐、按名称 A-Z、时长升降序、文件大小）。
3. **全功能可拖拽底部音频播放器**
   - **高精度时间轴拖拽**：鼠标悬停实时时间预览、支持随意点击与滑动拖拽精准跳转进度（Seek）。
   - **全键盘快捷键控制**：
     - `空格 (Space)`：播放 / 暂停
     - `←` / `→`：快退 5 秒 / 快进 5 秒
     - `↑` / `↓`：音量调节 (+10% / -10%)
     - `M`：一键静音 / 恢复音量
     - `/`：快速聚焦搜索框
   - **播放模式切换**：支持顺序循环、单曲循环、随机播放。
   - **多倍速播放**：0.75x、1.0x、1.25x、1.5x、2.0x 随意切换。
   - **实时播放队列抽屉**：右侧滑入队列面板，支持快速切歌。
   - **一键下载**：每首歌曲及播放器右侧均有一键下载本地 MP3 按钮。

---

## 🚀 启动与使用指南

### 1. 启动本地开发服务
```bash
pnpm dev
```
启动后在浏览器中打开 `http://localhost:5173/` 即可尽情试听。

### 2. 构建生产环境包
```bash
pnpm build
pnpm preview
```

### 3. 曲库维护
```bash
# 校验清单 + 刷新元数据（时长用 ffprobe 实测、艺术家从 ID3 回填，不重复下载）
pnpm bgm:check

# 增量下载缺失音频（清单来自 scripts/songs.md）
pnpm bgm:download

# 强制重新检索下载
node scripts/download_all.js --force

# 重新打包云盘发布内容 -> dist-bgm/
pnpm bgm:publish
```

新增曲目：在 `scripts/songs.md` 对应分类下加一行曲名，再跑 `pnpm bgm:download`。
曲目 id 由 `scripts/data/song-ids.json` 按「分类+曲名」锁定，增删条目不会打乱已有 id（收藏不再错位）。

### 4. 音频地址（云盘分发）

音频本体不进 git，三种部署方式：

1. **音频随应用一起上线**：把 `dist-bgm/public/bgm` 拷进 `dist/`，无需额外配置（走 Vite `BASE_URL`）。
2. **云盘目录直链**：`VITE_BGM_BASE=https://host/public pnpm build`，只部署 `dist/` 里的应用本体。
3. **单文件模板**：`VITE_BGM_BASE=https://host/bgm/{file}`（支持 `{path}` / `{file}` 占位符）。

本地临时调试也可以直接改运行时配置：
```js
localStorage.setItem('tfy_bgm_base', 'https://host/public')
```

---

## 📄 许可与免责声明

- 代码与文档采用 **Apache License 2.0** 授权（见 `LICENSE`，Copyright 2026 YuanJie）。
- **音频素材与封面图片的版权归原权利人所有**。本仓库仅包含清单、生成脚本与应用代码，**不附带任何音频授权**，
  也不得将 `public/bgm/` 下的音频视为 Apache-2.0 授权范围的一部分。
- 素材仅供个人学习与剪辑练手；商业发布前请自行向版权方取得授权。
