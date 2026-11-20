# 听风吟 · TFY BGM

专为视频剪辑打造的本地 BGM 音频素材库桌面客户端，基于 **Rust + Tauri 2** 构建。
分类扫描、收藏、检索、队列与全功能播放器全部在本地完成，不依赖浏览器与 Node 运行时。

> 本仓库已从 Vue 3 + Vite Web 应用迁移为 Rust + Tauri 桌面客户端：
> 旧的 `src/`、`public/`、`package.json`、`vite.config.ts` 等 Web 实现已移除，
> 前端改为 `src-tauri/ui/` 下的原生 HTML / CSS / ES Modules，业务逻辑集中在 Rust。

---

## 📦 仓库结构

| 内容 | 位置 | 说明 |
| :--- | :--- | :--- |
| 曲库音频 | `bgm/<分类>/*.mp3` | 文件名即曲名，随应用打包为资源 |
| 设备无关核心库 | `src-tauri/core/` | 目录扫描、导入/重命名/移动/软删除、分类元数据、偏好持久化、导出校验；可独立测试 |
| Tauri 应用层 | `src-tauri/src/` | 运行期状态、曲库根目录解析、命令与资源授权 |
| 原生前端 | `src-tauri/ui/` | 无构建步骤的 HTML / CSS / ES Modules |
| 应用配置 | `src-tauri/tauri.conf.json` | 窗口、CSP、资源打包、Asset Protocol |
| 权限 | `src-tauri/capabilities/default.json` | 仅授予 `core:default` |
| 图标 | `src-tauri/icons/` | 由 `app-icon.svg` 生成的各平台图标 |

```
bgm/<分类>/*.mp3 ──Rust 启动扫描──▶ LibrarySnapshot ──IPC──▶ 原生 UI 渲染
        └── Tauri 打包 ──▶ $RESOURCE/bgm/（安装后即可离线使用）
```

---

## 🗂 分类与目录规范

曲名直接取文件名（下划线 `_` 显示为空格），增删 / 替换 mp3 后重新扫描即可生效，
不再有 `songs.json` 或构建期虚拟清单。

| 中文分类 | 目录名 | 适用剪辑场景 |
| :--- | :--- | :--- |
| **欢快** | `cheerful` | 生活 Vlog、开箱解说、美食探店、快节奏卡点 |
| **独处** | `solitude` | 深夜独白、治愈钢琴、学习工作、慢生活 |
| **回忆** | `memories` | 怀旧故事、毕业季、岁月光影、情感纪实 |
| **史诗** | `epic` | 燃向混剪、大片战歌、企业宣传、运动高光 |
| **古风** | `gufeng` | 国风雅韵、汉服文旅、仙侠武侠 |
| **乡村** | `country` | 乡野烟火、田园慢生活、度假风景 |

注册分类与展示文案以 `categories.json` 为准：首次启动会按内置模板生成该文件，
之后既可在界面中维护，也可直接用编辑器修改后点击「重新扫描」生效。
放入未注册目录（即 `categories.json` 中不存在的目录）或 `bgm/` 根目录的 mp3 会被忽略，并在界面上给出告警。

### 分类维护

- **重命名**：右键点击任意分类标签 → 「重命名分类」，只修改显示名称，不会移动已导入的音频文件。
- **新增分类**：点击分类栏末尾的 `＋`，或右键分类 → 「新增分类」；可指定目录名（留空则使用分类名称），
  应用会同步在曲库根目录下创建同名文件夹，之后即可向该分类导入音频。
- **本地文件维护**：分类配置保存在应用配置目录的 `categories.json`（见下方「配置文件位置」），
  字段包括 `id`（目录名）、`nameZh`、`nameEn`、`labelEn`、`color`、`title`、`description`；
  缺失字段会自动补全，损坏或为空时回退为内置分类。修改文件后点击顶部的「重新扫描」即可刷新。
  每次新增 / 重命名分类以及重新扫描时，应用都会先重新读取该文件，手工编辑不会丢失。

### 曲库根目录解析顺序

1. 环境变量 `TFY_BGM_DIR`（适合开发与可移动硬盘）；
2. 界面上「更换目录」选择并持久化的文件夹；
3. **当前工作目录下的 `bgm/`**（内置默认，便于 portable 分发）；
4. 打包进应用的资源目录 `$RESOURCE/bgm`；
5. 开发模式下回退到仓库的 `bgm/` 目录。

界面底部状态卡可随时「重新扫描」「更换目录」「打开目录」；使用自定义目录时可「恢复内置」。

---

## ✨ 功能

1. **本地曲库与分类检索**
   - 内置 6 个分类（可右键重命名、用 `＋` 新增自定义分类）、收藏夹、实时搜索（曲名 / 中英文分类名 / 目录名）。
   - 默认推荐排序与按曲名排序（`Intl.Collator` 中文排序）。
   - 默认排序下支持拖动行调整顺序（仅内存，不写盘）。
2. **曲库文件管理（CRUD）**
   - **分类**：右键分类标签重命名，`＋` 或右键菜单新增分类；分类配置落盘为本地 `categories.json`。
   - **新增**：顶部「导入音乐」选择多个 mp3，复制到指定分类目录；同名自动追加序号，不覆盖已有文件。
   - **编辑**：行内编辑曲名与分类；曲名变化会重命名文件，分类变化会移动文件，收藏 key 自动跟随。
   - **删除**：单首或批量删除为软删除，文件移动到曲库根目录的 `.tfy-trash/<批次>/`，可手动恢复。
   - **批量**：表头全选/单选/Shift 连选，批量导出、批量移动分类、批量删除。
3. **音频导出**
   - 每首曲目与播放器均可导出 mp3，弹出系统原生保存对话框。
   - 多选后可批量导出到指定文件夹，同名自动追加序号。
   - 复制后校验落盘字节数与源文件一致，失败会清理不完整文件。
4. **全功能播放器**
   - 播放 / 暂停、上一首 / 下一首、顺序 / 单曲 / 随机。
   - 0.75x / 1.0x / 1.25x / 1.5x / 2.0x 倍速。
   - 高精度进度条：悬停时间预览，按住拖动 Seek；缓冲进度显示。
   - 音量滑杆、静音、播放队列抽屉、当前曲目收藏。
   - 音频经 Asset Protocol 读取后以 Blob 播放，兼容 Linux WebKitGTK 的媒体管线。
   - 加载失败自动跳过（最多连续 3 首）并提示原因，失败时会停下来而不会无限循环切歌。
5. **收藏与偏好持久化**
   - 收藏、音量、静音、倍速、播放模式、排序由 Rust 写入应用配置目录的 `prefs.json`。
   - 收藏使用稳定键 `分类/文件名主干`；重命名 / 移动分类时收藏自动迁移，替换同名文件不会丢失收藏。
6. **桌面体验**
   - 无 Node / npm 运行时依赖，离线可用；音频由 Tauri Asset Protocol 从本地曲库读取后播放。
   - 中文本地字体栈，不请求任何远程字体或第三方资源。

### 键盘快捷键

| 按键 | 功能 |
| :--- | :--- |
| `空格` | 播放 / 暂停（未播放时从当前列表第一首开始） |
| `←` / `→` | 后退 / 前进 5 秒 |
| `↑` / `↓` | 音量 +10% / -10% |
| `M` | 静音 / 恢复 |
| `/` | 聚焦搜索框 |
| `Esc` | 关闭队列抽屉；清除批量选择；搜索框内清空搜索 |

---

## 🚀 开发与构建

### 环境要求

- Rust **1.90+**（`rustup` 安装稳定版即可）。
- Tauri 2 桌面依赖：
  - **Fedora**
    ```bash
    sudo dnf install webkit2gtk4.1-devel gtk3-devel libsoup3-devel \
      javascriptcoregtk4.1-devel openssl-devel librsvg2-devel \
      libappindicator-gtk3-devel gcc gcc-c++ pkgconf-pkg-config
    ```
  - **Debian / Ubuntu**
    ```bash
    sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
      libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
    ```
  - **macOS**：Xcode Command Line Tools；**Windows**：MSVC 生成工具 + WebView2 Runtime。
- 音频由系统 WebView 解码，Linux 上如遇无声 / 无法拖动进度，请补装 GStreamer 插件：
  `sudo dnf install gstreamer1-plugins-good gstreamer1-plugins-ugly`（Debian/Ubuntu 对应 `gstreamer1.0-plugins-good` / `-ugly`）。

### 启动开发版

任选一种方式（项目本身不需要 Node 依赖）：

```bash
# 方式一：Rust 原生 CLI（推荐）
cargo install tauri-cli --version "^2" --locked   # 首次安装
cd src-tauri
cargo tauri dev

# 方式二：临时使用官方 CLI，不在项目中留下 node_modules
pnpm dlx @tauri-apps/cli@^2 dev
```

前端是 `frontendDist` 目录中的静态文件，由 Tauri 内置服务器直接读取：
修改 `src-tauri/ui/` 后刷新窗口（`Ctrl+R`）即可，无需前端构建；Rust 改动会自动重编译。

### 打包

```bash
cd src-tauri
cargo tauri build
```

产物位于 `src-tauri/target/release/bundle/`（平台对应 deb/rpm/AppImage、dmg、msi/nsis）。
音频目录通过 `bundle.resources` 打进安装包，因此安装后离线即用；由于曲库约 350 MB，
安装包会相应较大。若希望安装包保持精简，可删除 `tauri.conf.json` 中的 `bundle.resources`，
改用「更换目录」或 `TFY_BGM_DIR` 指向外部曲库。

---

## 🧪 测试与检查

```bash
cd src-tauri

cargo test -p bgm-core                 # 扫描 / 偏好 / 导出单元测试
cargo clippy -p bgm-core --all-targets -- -D warnings
cargo fmt --all -- --check
cargo check --workspace                # 需要上面的 WebKitGTK 等系统依赖
```

`bgm-core` 不依赖 Tauri，可在无图形依赖的环境（如 CI 容器）里独立测试业务逻辑。

---

## 🔐 架构与安全

- **状态与命令**：`AppState` 持有偏好（`parking_lot::RwLock`）与最近扫描结果；
  前端只能通过命令读取快照、保存偏好、切换 / 扫描曲库、导入 / 编辑 / 移动 / 删除曲目与导出音频。
- **偏好校验**：前端提交的偏好会经过 `Prefs::normalize`（音量 clamp、倍速白名单、
  收藏去重与长度限制）；曲库目录只能由专门命令修改，普通偏好保存不能覆盖。
- **分类配置**：新增 / 重命名分类由 Rust 校验名称与目录名必须是单个普通路径分量，
  同名分类与重复目录名会被拒绝；配置写入应用配置目录的 `categories.json`，手工编辑后重新扫描生效。
- **最小权限**：`capabilities/default.json` 仅启用 `core:default`。
  原生文件夹选择、保存对话框与「打开目录」全部在 Rust 侧调用插件，不向 WebView 暴露插件权限。
- **资源授权**：Asset Protocol 仅授权当前曲库根目录（递归），可播放路径限定在扫描结果内。
- **CSP**：默认仅允许本地脚本 / 样式，`media-src` 允许 `asset:` 与 `http://asset.localhost`，
  不加载远程脚本、字体或图片。
- **写操作安全**：前端只能提交曲库快照中的 key 与分类 id；实际路径由 Rust 用
  「曲库根目录 + 注册分类 + 文件名」重新推导，并校验文件名必须是单一普通路径分量，
  拒绝路径穿越；重命名冲突直接报错，不覆盖已有音频。
- **导出安全**：导出目标由系统保存对话框确定；复制后做大小校验，源路径由曲库快照推导而非前端路径。
- **删除可恢复**：删除采用软删除，文件移动到 `<曲库根>/.tfy-trash/<批次>/`，
  扫描器忽略该隐藏目录；批量删除前会弹出系统确认对话框。

### 偏好与分类配置位置

由 Tauri `app_config_dir` 决定，两个文件并列存放（`prefs.json` 偏好、`categories.json` 分类）：

- Linux：`~/.config/com.yuanjie.tfybgm/`
- macOS：`~/Library/Application Support/com.yuanjie.tfybgm/`
- Windows：`%APPDATA%\com.yuanjie.tfybgm\`

偏好文件损坏或缺失时回退为默认值；分类文件缺失时生成内置分类，损坏或为空时回退为内置分类，均不会阻止应用启动。

---

## 🔁 从旧 Web 版迁移

- 曲库目录由 `public/bgm/` 迁移为仓库根目录的 `bgm/`，音频随桌面应用打包或指向任意本地目录。
- 收藏、音量、倍速等状态从浏览器 `localStorage` 迁移到应用配置目录的 `prefs.json`；
  旧浏览器里的收藏不会自动导入，可在客户端中重新收藏。
- 旧的云盘地址配置（`VITE_BGM_BASE`）与静态站点发布方式已移除，桌面端直接读取本地文件。
- 旧版 Vue/Vite 源码、`package.json`、`index.html` 等已从仓库删除，客户端不含 Node 构建链。

---

## 📄 许可与免责声明

- 代码与文档采用 **Apache License 2.0** 授权（见 `LICENSE`，Copyright 2026 YuanJie）。
- **音频素材版权归原权利人所有**。本仓库不附带任何音频授权，
  `bgm/` 下的 mp3 不属于 Apache-2.0 授权范围。
- 素材仅供个人学习与剪辑练手；商业发布前请自行向版权方取得授权。