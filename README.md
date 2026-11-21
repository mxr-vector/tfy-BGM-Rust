# 听风吟 · TFY BGM（GPUI 分支）

> **分支定位**：本仓库是 `tfy-bgm` 的 **GPUI 分支**。
> 功能范围跟随主线（Tauri 版），但客户端与界面**用 Rust 实现**：
> 基于 [GPUI 0.2.2](https://gpui.rs)（Zed 的 GPU UI），不使用 WebView、Node 运行时或前端构建链。
>
> 主线实现在 Tauri 侧；本分支只做同等功能范围的原生 Rust 实现，视觉与交互对照主线对齐。
> 两者共用同一套曲库核心逻辑（`core/`，来自主线的 `bgm-core`）。

本地 BGM 曲库的 macOS 桌面客户端。曲库扫描、导入、改名、移动、软删除和偏好设置由 `core/` 提供；
播放用 rodio 0.23 解码本地 mp3，全部资源都在本地，不依赖网络服务。

## 与主线的一致性

- **功能跟随主线**：主线新增/调整的功能，本分支需要对齐实现，保持一致的用户可见行为。
- **实现语言不同**：主线是 Rust + Tauri 2 + 原生 HTML/CSS；本分支是纯 Rust + GPUI，没有 WebView。
- **核心逻辑共享**：曲库与配置逻辑来自主线的 `bgm-core`，以 `core/` 形式内置在本仓库，
  从主线更新时同步该目录（当前同步到主线 `bgm-tauri2` 分支 `63a0598`）。
- **已知差异**：
  - rodio 的倍速播放会同时改变音高；主线 Tauri 版基于浏览器 `Audio`，默认保音高。
    这是播放后端差异，不是功能缺失。A/B 循环不受影响：本分支把整曲解码成 PCM，
    循环区间在采样级无缝回绕，与主线 Web Audio 的 `loopStart/loopEnd` 精度一致。
  - mp4 动态壁纸与毛玻璃模糊暂不支持（GPUI 0.2.2 无视频解码与背景模糊），
    壁纸仅支持内置静态图、渐变与 jpg/png/webp 自定义图片，透明度调节可用。
  - 批量多选与诊断面板尚未实现（主线有 BatchBar 与 DiagnosticsPanel）。

## 界面

视觉与交互对照主线 `bgm-tauri2` 分支（当前基线 `63a0598`）实现：

- 原生透明标题栏，红黄绿交通灯与品牌、搜索、导入、排序共用顶栏；
- 紧凑分类横幅、风格筛选、分类封面与双行曲目信息（带 ID3 艺人副标题）；
- “全部播放 / 随机试听”、收藏和“更多”浮动操作菜单（含循环点编辑）；
- 始终显示的悬浮播放器：播放状态、循环区间高亮、超长标题跑马灯；
- **节目单页**：按时间段排程节目与歌曲，自动对齐、时段内拦截手动点播；
- **界面壁纸**：默认配色、渐变、内置静态图与自定义图片，可调透明度与填充方式；
- **导出设置**：响度归一化开关与场景预设（需要系统安装 ffmpeg）。

全部图标为编译期内嵌的 Lucide SVG，不使用 WebView、emoji 或外部前端资源。
默认窗口 1240×820，最小 1020×680。预览图：[1240×820](preview-1240x820.png) · [1020×680](preview-1020x680.png)。

## 仓库结构

| 内容 | 位置 | 说明 |
| :--- | :--- | :--- |
| 应用入口 | `src/main.rs` | GPUI `Application` 启动、窗口选项、全局快捷键绑定 |
| 键盘动作 | `src/actions.rs` | `gpui::actions!` 声明，按上下文区分曲库/输入框/对话框 |
| 界面视图 | `src/view.rs` | `Workspace` 的 `Render` 实现：面板、菜单、节目单页、壁纸层与播放器 |
| 曲库状态 | `src/library.rs` | `Library`、筛选、曲库根目录解析、节目单/循环点持久化与导出设置 |
| 文本输入 | `src/text_input.rs` | 自绘 `TextField`，支持 IME、选区与剪贴板 |
| 音频播放 | `src/audio.rs` | rodio 输出设备；整曲 PCM 解码，支持采样级 A/B 循环、倍速与音量 |
| 文件监听 | `src/watch.rs` | notify 监听曲库目录，700ms 防抖 + 30s 兜底触发增量扫描 |
| 图标资源 | `src/icons.rs` | 内嵌 Lucide SVG 路径与着色 |
| 资源注册 | `src/assets.rs` | GPUI `AssetSource`，提供 `icons/*.svg` 与内置壁纸 `bg/2.jpg` |
| 曲库核心 | `core/` | 从主线 `bgm-core` 同步：扫描/导入/配置/偏好/节目单/循环/响度导出 |

## 运行

需要 Rust 1.90+（edition 2024）：

```bash
cd ~/Dev/Rust/tfy-bgm-gpui
cargo run
```

曲库查找顺序（与主线 `library_root.rs` 一致）：

1. 环境变量 `TFY_BGM_DIR`
2. 上次在界面里选过的目录
3. 开发模式回退到主线仓库的 `../tfy-bgm/bgm`
4. 外置曲库：程序同级 `bgm/` → 当前目录 `bgm/` → 配置目录 `bgm/`，都没有时自动创建

配置写在 `~/Library/Application Support/tfy-bgm-gpui/`（`prefs.json`、`categories.json`、`config.json`、`schedule.json`、`loops.json`），与 Tauri 版的 `com.yuanjie.tfybgm` 分开，不会互相覆盖。

## 快捷键

| 按键 | 作用 |
| :--- | :--- |
| 空格 | 播放 / 暂停 |
| ← / → | 后退 / 前进 |
| ↑ / ↓ | 音量 |
| M | 静音 |
| / | 聚焦搜索 |
| Esc | 关闭对话框，或清空搜索 |
| ⌘Q | 退出 |

## 许可

代码使用 Apache-2.0。`bgm/` 中的 mp3 版权归原权利人，不在许可范围内，本客户端不会把它们打进包。
浏览器打开预览图即可查看界面效果。
