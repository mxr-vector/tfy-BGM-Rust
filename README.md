# TFY · 视频剪辑 BGM 音频素材库与试听平台

专为视频剪辑量身定制的高颜值背景音乐库与试听平台，采用 **Vue 3 + TypeScript + Vite** 构建，拥有现代化暗黑磨砂玻璃（Glassmorphism）UI、沉浸式交互与全功能音频播放器。

---

## 📦 仓库结构与素材分发

曲库以 **`public/bgm/` 目录本身为唯一事实来源**：文件名即曲名，不再有索引文件与元数据脚本。
构建 / dev 期由 `vite.config.ts` 直接扫描各分类目录下的 `.mp3` 生成曲目清单（`virtual:bgm-manifest`），
因此手动替换 / 增删音频后，页面展示的曲名即刻同步，不会出现时长、作者与实际音频不匹配的情况。

| 内容 | 位置 | 是否进 git |
| :--- | :--- | :--- |
| 曲库共享类型（清单条目 / 曲目 / 分类，前后端与构建脚本共用） | `src/types/bgm.ts` | ✅ |
| 分类元数据（目录名 ↔ 中文名 / 英文名 / 主题色，前端唯一事实来源） | `src/utils/categories.ts` | ✅ |
| 目录扫描器（构建期 + 发布脚本共用，含扫描结果告警） | `scripts/scan_bgm.ts` | ✅ |
| 曲目运行时工具（地址解析 / 下载） | `src/utils/songSchema.ts` | ✅ |
| 分类目录占位 | `public/bgm/<分类>/.gitkeep` | ✅ |
| 音频本体 | `public/bgm/<分类>/*.mp3` | ❌ 体积大，按需本地/云盘分发 |
| 云盘发布产物 | `dist-bgm/` | ❌ |

```
public/bgm/<分类>/*.mp3 ──vite 构建期扫描──▶ virtual:bgm-manifest ──▶ 前端渲染（仅曲名 + 分类）
                       └────────pnpm bgm:publish────▶ dist-bgm/（打包上传云盘）
```

---

## 📁 分类与英文目录规范

所有 BGM 按类别放入对应标准英文文件夹（曲目数量以 `public/bgm/<分类>/` 目录下的实际 mp3 为准）：

| 中文分类 | 英文目录名 | 适用剪辑场景 | 目录路径 |
| :--- | :--- | :--- | :--- |
| **欢快** | `cheerful` | 生活日常 Vlog、开箱解说、趣味搞笑、快节奏卡点 | `public/bgm/cheerful/` |
| **独处** | `solitude` | 深夜独白、治愈流行、静心学习/工作、自律慢生活 | `public/bgm/solitude/` |
| **回忆** | `memories` | 怀旧温情、青春毕业季、岁月光影、情感纪实 | `public/bgm/memories/` |
| **史诗** | `epic` | 燃向混剪、大片战歌、高光集锦、企业宣传片 | `public/bgm/epic/` |
| **古风** | `gufeng` | 国风雅韵、汉服走秀、仙剑经典、唯美武侠 | `public/bgm/gufeng/` |
| **乡村** | `country` | 乡野烟火、农家田园、悠闲度假风景 | `public/bgm/country/` |

---

## 🎵 曲库清单

以下为各分类目录下的实际曲目（共 92 首，以 `public/bgm/<分类>/` 目录为准；页面展示时下划线 `_` 显示为空格）：

### 欢快 · cheerful（20 首）
Starlight、buttercup、monkeys spring monkeys、monsieur melody、spring in my step、stranger、summer（菊次郎的夏天）、上春山、亲爱的共产主义请赐我前行的力量、可爱翻倍 吨吨吨、咏春、天下第一好、来杯好茶摇一摇、步步高、猪突猛进、竹林小曲（燕云）、答案、象王行、踏遍青山、风月吻眉梢

### 独处 · solitude（19 首）
flower dance、kiss the rain、little story、merry christmas mr.lawrence、oh you beat、on more time、river flow in you、the truth that you leave、windy hill、一个人过冬天、人生路漫漫、幻昼、悬溺、所念皆星河、爱人错过、爱的供养（伴奏）、稳稳的幸福（钢琴）、萤火虫之舞、虚拟

### 回忆 · memories（11 首）
asphyxia、你听啊秋末的落叶、告白之夜、城南花已开、山上的山花开呀、思绪回到那年秋、时间煮雨、樱花草、穿越时空的思念、阳光灿烂的日子（萨克斯）、风居住的街道

### 史诗 · epic（28 首）
EI Dorado、Empire of Angels、breath and life、conquest of paradise、go time、he's a pirate、heart of courage、intro、last reunion、pacific rim、requiem for a tower、rise、star sky、sunburst、三界四洲、天地缓缓、好一朵茉莉花、归零、惊涛日落（红警）、望月婵媛吟唱、水龙吟、浮光、相许、知我、空山野马、精卫、难解、bloody mary

### 古风 · gufeng（13 首）
九州同、云水禅心、偏爱、双面燕洵、忘记时间、明月引、此生不换、灼雪、莫失莫忘、醉忆仙乡、雪见-落入凡尘、青花瓷（纯音乐）、风云决

### 乡村 · country（1 首）
莫忙

---

## ✨ 核心特性

1. **现代化 UI 审美**
   - 沉浸式暗色调 + 磨砂玻璃（Glassmorphism）质感。
   - 各分类专属动态色彩流光（琥珀金、深邃紫、玫瑰粉、烈焰红、翡翠绿、青柠绿）。
   - 正在播放时音频跳动可视化动效（Equalizer）与黑胶唱片旋转动效。
2. **多维筛选与极速检索**
   - 分类标签一键切换、收藏夹过滤。
   - 实时响应式搜索（快捷键 `/` 快速聚焦搜索框）。
   - 高密度列表视图，支持拖拽排序。
   - 排序方式（默认推荐、按曲名 A-Z）。
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

### 2. 构建静态包（免服务器，双击即用）
```bash
pnpm build
```
产物 `dist/index.html` 为**单个自包含文件**（JS/CSS 全部内联），配合同目录的 `dist/bgm/` 音频，
**直接双击 `dist/index.html` 即可在浏览器中打开使用，无需 nginx 等任何静态服务器**
（基于 `vite-plugin-singlefile` + 相对 `base`，兼容 `file://` 协议）。

> 说明：`file://` 下浏览器会拦截外部 ES 模块脚本，故构建期将脚本内联进 `index.html`；
> 音频 `<audio>` 走相对路径加载本地文件，不受该限制。收藏 / 音量等仍用 `localStorage` 持久化。

如需本地起服务预览，也可用 `pnpm preview`。

### 3. 曲库维护

直接向对应分类目录 `public/bgm/<分类>/` 放入 / 替换 / 删除 `.mp3` 文件即可，**文件名就是页面上展示的曲名**（下划线 `_` 显示为空格）；
`pnpm dev` 会监听 `public/bgm` 目录，改动即时刷新，生产构建则在打包时重新扫描。
音频放入未注册目录（非 `categories.ts` 中登记的 6 个分类）或 `public/bgm` 根目录时不会进曲库，构建日志会给出告警。

```bash
# 重新扫描目录并打包云盘发布内容 -> dist-bgm/（可选 -- --clean / -- --zip）
pnpm bgm:publish
```

收藏（favorite）/ 播放队列以稳定的「分类/曲名」key 保存，替换同名文件不会错位；只要文件名不变，收藏与队列仍然有效。

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
