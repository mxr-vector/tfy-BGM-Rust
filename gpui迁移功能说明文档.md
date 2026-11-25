# 已实现功能文档 · 主线 `bgm-tauri2` 功能基线

> **用途**：作为 `bgm-gpui`（纯 Rust + GPUI）分支逐项对齐功能的**唯一功能事实来源**。**基准是当前分支 `bgm-tauri2`：gpui 只做对齐，不缩减功能、不改数据契约、不改对外接口**（详见 §0.0）。
> **代码基线**：`bgm-tauri2` @ `80faf7d`（2026-11-25，`feat(tts_script): 重构纯文本解析实现段落合并与智能分段`）。
> **对照分支**：`origin/bgm-gpui` @ `59175f0`（2026-11-20，`feat: 以 GPUI 实现重建 bgm-gpui 分支`）。
> **核对方式**：对照上一版代码基线 `e59ee18` 与当前工作树，以 Rust 实现、Tauri 注册表、HTTP 路由及前端调用为依据。统计为源代码计数。
> **本版重点**：补齐音频索引、公开歌单迁移、语音合成，并更新歌曲详情、平台能力和开放接口契约。

---

## 0. 文档说明与范围

### 0.0 基准原则（先读）

**本分支（`bgm-tauri2`）是唯一功能基准；`bgm-gpui` 只做对齐，不做功能取舍。**

| # | 原则 | 具体含义 |
| :--- | :--- | :--- |
| 1 | **功能范围不缩减** | 本文 §1 的 21 个功能域，gpui 需全部具备；不得以「平台限制」为由长期缺失，只能有「能力缺失时降级 + 界面明示」这一种处理方式，且降级分支必须与主线一致 |
| 2 | **数据契约不改写** | 5 个配置文件的字段名、类型、默认值、收敛规则、稳定 key 语义、`KeyChange` 迁移契约，一律以 §4 / §5.2 为准；gpui 不得新增/删减字段或改变序列化形态 |
| 3 | **对外契约不变** | 83 个命令的参数与返回、14 个后端事件的名称与载荷（另有 2 个歌词窗口内部事件）、33 个 HTTP 方法+路径、标准 SSE（id/event/data）、状态码语义、快照字段，一律以附录 A/B/C 为准，外部脚本在两个分支间必须可直接复用 |
| 4 | **历史差异必须消除** | gpui 当前「仅 mp3」「`Prefs` 8 字段」「`LibrarySource` 缺 `External`」「均匀随机」等属于**待消除的落后状态**，不是可保留的分支特性。判定标准见 §6.1 |
| 5 | **听感与交互结果对齐** | 交叉淡入淡出、A/B 循环、智能随机权重、响度归一化目标、字幕切分与时间轴、歌词逐词推进，均以主线实测行为为准；实现手段可不同，用户可感知**结果**需一致 |
| 6 | **UI 布局与视觉样式不作要求** | 面板划分、栅格、间距、配色、圆角、阴影、动效、组件形态、图标风格均可自由设计；只要求**功能可达**（每个能力有入口）、**状态可辨**（播放/加载/禁用/错误可区分）、**反馈语义一致**（提示含义、拦截原因、失败原因不弱化）。不得以「样式不同」为由判定未对齐，也不得以「样式对齐」代替功能实现 |
| 7 | **允许不同的只有实现层** | 允许差异：WebView → 原生窗口、Web Audio → 原生混音、`setSinkId` → 系统音频 API、Tauri 事件总线 → 通道。除此之外的行为差异都视为未对齐 |

> 阅读顺序建议：先读本节，再看目标功能在 §2 的元信息表（尤其「gpui 实现建议」列），最后用 §6.6 的任务清单推进、§8.3 验收。
>
> **不在本文范围**：界面布局、视觉风格、动效与组件拆分。本文只描述「有什么功能、怎么算对」，不描述「长什么样」。

### 0.1 读者与使用方式

| 读者 | 建议阅读路径 |
| :--- | :--- |
| GPUI 分支开发者 | §1 总览 → 目标功能的 §2 小节 → §3 跨模块流程 → §6 实现建议 → §8.3 验收门槛 |
| 主线维护者 | §2 模块详情（改动时同步更新）→ §4 配置表 → §5 状态所有权 |
| 测试 / QA | §8 验收清单 + §7 已知边界 |
| 交接 / 评审 | §1、§5、§6、§7 |

### 0.2 状态定义（本文统一口径）

| 标记 | 含义 | 判定标准 |
| :--- | :--- | :--- |
| **已实现** | 功能完整可用，主路径与降级路径都有代码 | 能在标注位置找到入口、实现、错误处理 |
| **部分实现** | 主路径可用，但依赖平台 / 外部工具，缺失时降级 | 存在显式能力探测与降级分支 |
| **未实现** | 代码中不存在 | 无入口、无模块 |
| **已废弃** | 旧实现被新实现取代，仅保留兼容/清理逻辑 | 存在迁移、回退、清理代码 |
| **待确认** | 代码与文档/预期不一致，或需实机验证 | 已在 §7.3 编号列出 |

### 0.3 规模基线（实测，非估算）

| 维度 | 数量 | 统计口径 |
| :--- | :--- | :--- |
| 功能域 | **21** | §1 的 F01–F21 |
| `bgm-core` 模块 | **16** | `src-tauri/core/src/*.rs` |
| Tauri 应用层模块 | **22** | `src-tauri/src/lib.rs` 顶级 `mod` 声明，含平台与歌单子模块入口 |
| 注册的 Tauri 命令 | **83** | `lib.rs` 的 `generate_handler!`（附录 A） |
| Rust → 前端事件 | **14** | Rust → 前端的业务事件（含音频索引与 TTS）（附录 B） |
| 窗口内前端事件 | **2** | 歌词窗 ↔ 设置窗（`desktop-lyrics-style` / `desktop-lyrics-command`） |
| HTTP 路由声明 | **30** | `api.rs::router()` 的 `.route(...)` |
| HTTP 方法+路径组合 | **33** | 包含 `/v1/outputs` 和 `/v1/subtitles/track` 的多方法声明 |
| 通用 action（HTTP） | **14** | `ui/src/stores/api.ts::runAction` |
| 配置文件 | **5** | `prefs.json` / `config.json` / `categories.json` / `schedule.json` / `loops.json` |
| 派生数据目录 | **2** | `subtitles/`、`separations/`（另有 `audio-index.json`、应用缓存下的 `tts/`、曲库内的 `.tfy-trash/`） |
| `bgm-core` 单元测试 | **108** | `grep -c '#\[test\]'`（含 `#[ignore]`） |
| 应用层单元测试 | **110** | 同上（`api` / `state` / `models` / `subtitles` / `separation` 等） |

### 0.4 代码分层

```
┌──────────────────────────────────────────────────────────────┐
│ ui/  Vite + Vue 3 + TS（WebView 内）                          │
│  stores/  业务状态与动作（player / queue / outputs / schedule │
│           / api / subtitles / desktop-lyrics / models / ...） │
│  lib/     纯函数（audio-analysis / subtitle-edit / lyrics-    │
│           style / config / constants / types）                │
│  components/ 31 个 .vue                                       │
└───────────────────────────┬──────────────────────────────────┘
                            │ invoke(command) / emit(event)
┌───────────────────────────▼──────────────────────────────────┐
│ src-tauri/src  应用层（Tauri 2，依赖平台与 WebView）           │
│  state.rs    运行时状态 + 写操作编排 + watch 增量             │
│  commands.rs 83 个命令（参数校验 + 调 core + 序列化）          │
│  api.rs      本机 HTTP + SSE + 命令桥                          │
│  lyrics.rs   桌面歌词窗口（多窗口 / 置顶 / 锁定）              │
│  models.rs   ASR（下载 / 加载 / 分片识别）                     │
│  separation.rs 人声分离（C API 绑定 + 子进程 worker）          │
│  subtitles.rs 字幕生成 / 编辑 / 迁移                          │
│  desktop.rs fonts.rs wallpapers.rs library_root.rs            │
│  library_watch.rs settings.rs                                 │
└───────────────────────────┬──────────────────────────────────┘
                            │ 纯 Rust 调用
┌───────────────────────────▼──────────────────────────────────┐
│ src-tauri/core  crate `bgm-core`（无 Tauri 依赖，可独立测试）   │
│  library / ops / tags / categories / prefs / config /         │
│  schedule / loops / export / loudness / paths / persist /     │
│  limits                                                       │
└──────────────────────────────────────────────────────────────┘
```

**分层契约**：`commands.rs` 只做「校验参数 → 调 `AppState` / core → 序列化」，业务规则一律下沉到 `bgm-core`。这条约束是 GPUI 迁移的最大资产：`core/` 可以整体搬运，应用层与前端需要重写。

---

## 1. 功能总览表

### 1.1 21 个功能域

| 编号 | 功能域 | 状态 | 实现层 | 核心落点 | gpui 现状 | 优先级 |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| F01 | 曲库扫描与分类 | 已实现 | core | `library.rs`、`categories.rs` | 有（旧版：**仅 mp3**、无标签、`LibrarySource` 少 `external`） | P0 |
| F02 | 曲库文件操作 | 已实现 | core + app | `ops.rs`、`state.rs` | 有（命名/软删除齐全；**导入仅 mp3、导出恒 `.mp3`**） | P0 |
| F03 | 标签与时长 | 已实现 | core | `tags.rs` | **无** | P0 |
| F04 | 曲库实时监听 | 已实现 | app | `library_watch.rs`、`state.rs` | **无** | P1 |
| F05 | 播放引擎与传输控制 | 已实现 | ui | `audio-engine.ts`、`player.ts` | 极简 rodio（无淡入淡出/循环） | P1 |
| F06 | 播放队列与智能随机 | 已实现 | ui | `queue.ts` | 简单随机 | P1 |
| F07 | 多输出设备路由 | 部分实现 | ui | `outputs.ts`、`audio-engine.ts` | **无** | P2 |
| F08 | 节目单调度 | 已实现 | core + ui | `schedule.rs`、`useScheduler.ts` | **无** | P1 |
| F09 | 导出与响度归一化 | 已实现 | core | `export.rs`、`loudness.rs` | 有（仅 mp3 纯复制，无归一化） | P1 |
| F10 | 歌曲详情与可视化诊断 | 已实现 | ui + core | `SongDetailsContent.vue`、`audio-analysis.ts`、`audio_index.rs` | **无** | P2 |
| F11 | 偏好与收藏 | 已实现 | core + ui | `prefs.rs`、`prefs.ts` | 有（旧版 8 字段） | P0 |
| F12 | 界面壁纸 | 已实现 | app + ui | `wallpapers.rs`、`useWallpaper.ts` | **无** | P2 |
| F13 | 系统设置与桌面集成 | 部分实现 | app + ui | `desktop.rs`、`fonts.rs`、`useSystemIntegration.ts` | **无** | P2 |
| F14 | 本机 HTTP 开放接口 | 已实现 | app + ui | `api.rs`、`api.ts` | **无** | P2 |
| F15 | ASR 模型管理 | 已实现 | app | `models.rs` | **无** | P1 |
| F16 | 人声分离引擎 | 已实现 | app | `separation.rs` | **无** | P1 |
| F17 | 字幕生成与编辑 | 已实现 | app + ui | `subtitles.rs`、`subtitles.ts` | **无** | P1 |
| F18 | 桌面歌词窗口 | 已实现 | app + ui | `lyrics.rs`、`lyrics.ts` | **无** | P2 |
| F19 | 音频索引、以声搜声与查重 | 已实现 | core + app + ui | `audio_index.rs`、`state.rs`、`audio-tools/` | **无** | P1 |
| F20 | 公开歌单迁移 | 已实现 | app + ui | `playlist_migration/`、`usePlaylistMigration.ts` | **无** | P2 |
| F21 | TTS 文本转语音 | 已实现 | app + ui | `tts.rs`、`tts_models.rs`、`tts_script.rs` | **无** | P1 |

> 优先级定义：**P0** = `core/` 直接替换即可获得，风险最低；**P1** = 用户高频可见，需要 UI 与后端配合；**P2** = 平台相关或可后置。

### 1.2 状态统计

| 状态 | 数量 | 功能域 |
| :--- | :--- | :--- |
| 已实现 | 19 | F01–F06、F08–F12、F14–F21（F18 见 Wayland 降级） |
| 部分实现 | 2 | F07（仅 Chromium 系 WebView）、F13（平台特有能力为 WebView 近似） |
| 未实现 | 0 | — |
| 待确认 | 0 个功能域 | 实机验证见 §7.3，包含新增工作台验证项 |

### 1.3 已废弃的旧实现（不作为功能域）

| 旧实现 | 现状 | 取代者 | 代码位置 |
| :--- | :--- | :--- | :--- |
| 单人声分离引擎（人声与伴奏由同一模型出） | 已废弃 | 双引擎：人声可选模型 + 伴奏固定 UVR Inst_HQ_3 | `separation.rs::execute_job` |
| 伴奏文件命名 `instrumental.wav` | 已废弃 | `accompaniment.wav`，旧文件在分离成功后清理 | `separation.rs:1353` |
| 字体 id（`harmony` / `misans` / `pingfang` / `yahei`） | 已废弃 | 直接存系统字体族名，旧值仅读取时映射 | `ui/src/lib/constants.ts::LEGACY_UI_FONT_FAMILIES` |
| `prefs` v1「默认套用导出场景」 | 已废弃 | 空场景 + 默认预设（归一化仍开启） | `prefs.rs::load_with_rates` |
| 字幕「ffmpeg 缺失时直接识别源音频」降级 | 已废弃 | 强制人声分离 + ffmpeg 归一化，缺失即报错 | `subtitles.rs::normalize_to_wav` |
| 桌面歌词内嵌设置面板 | 已废弃 | 独立悬浮窗 `lyrics-settings.html` | `lyrics.rs::toggle_settings` |

---

## 2. 模块级功能详情

> 每项统一包含 13 个字段。代码位置写作 `文件:符号`，保留旧模块的 `e59ee18` 历史行号作为定位提示。当前实现优先按符号检索。

### F01 曲库扫描与分类

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 曲库扫描与分类（`scan_library_cached` / `Categories`） |
| **状态** | 已实现 |
| **判断依据** | `library.rs:146` 完整实现扫描、去重、告警；`categories.rs:88/130/209/250` 实现加载、归一化、新增、改名；7 + 6 项单元测试覆盖 |
| **功能描述** | 递归扫描 `bgm/<分类>/` 下的受支持音频，产出「曲目列表 + 分类计数 + 告警 + 扫描时间」快照。文件名即曲名，不维护索引文件。分类文案与主题色由 `categories.json` 决定，首次启动按编译期内嵌模板生成。 |
| **入口/触发** | ① 启动 `AppState::initialize`；② 命令 `rescan_library`；③ 写操作后 `refresh_library`；④ watch 事件 `handle_watch_event`；⑤ HTTP `POST /v1/library/rescan` |
| **关键代码位置** | `core/src/library.rs:22` `AUDIO_EXTENSIONS`、`:31` `audio_extension`、`:137` `scan_library`、`:146` `scan_library_cached`、`:196` `scan_category`、`:307` `collect_stray_warnings`；`core/src/categories.rs:30` `Category`、`:76` `defaults`、`:130` `normalize`、`:209` `add`、`:250` `update`；`core/assets/categories.default.json`；`core/src/limits.rs` |
| **输入** | `root: &Path`、`source: LibrarySource`、`categories: &[Category]`、`cache: &MetaCache`（可选） |
| **输出** | `LibrarySnapshot { root, source, songs[], categories[], warnings[], scannedAtMs }`；`Song { key, title, fileName, category, path, artist?, album?, genre?, durationMs? }` |
| **核心流程** | ① 若 root 不是目录 → 全部告警 + 分类计数补 0；② 按 `categories` 声明顺序遍历每个 `root/<id>`；③ 单目录内过滤扩展名 → 读 `fs::metadata` → 取 stem → 组 key；④ 超出 512 字节的 key 跳过并告警；⑤ 同 stem 多格式按优先级保留一条（其余告警）；⑥ 目录内按 `title` 小写 + `fileName` 排序；⑦ 汇总分类计数；⑧ 扫描根目录游离文件/未注册目录并告警；⑨ 全库为空时补一条提示 |
| **依赖项** | `tags::MetaCache`（读标签）、`categories::Category`、`limits::{MAX_KEY_LEN, MAX_NAME_CHARS}`；不依赖 Tauri |
| **配置项** | 无（分类本身即配置）；根目录来源见 §4.10 |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | **直接复用** `library.rs` + `categories.rs`（零 Tauri 依赖）。需保留 `LibrarySource` 四值语义（`bundled/external/custom/env`，serde 小写），否则界面「曲库来源」文案会错。`MetaCache` 必须在应用生命周期内复用同一实例，否则每次扫描都会重新读标签（这是 GPUI 侧最容易踩的性能坑）。 |
| **待确认** | 无 |

**扫描规则与边界（F01）**

| 规则 | 具体行为 | 依据 |
| :--- | :--- | :--- |
| 扩展名 | `flac / wav / m4a / mp3 / ogg`（小写常量），比较时大小写不敏感；按字节从末尾找 `.`，`.mp3` 这类无主干的文件名不算 | `library.rs:31-42` |
| 格式优先级 | 数组顺序即优先级：`flac > wav > m4a > mp3 > ogg` | `library.rs:22` |
| 稳定 key | `"{category.id}/{stem}"`，`stem` 为文件名主干（**保留原始大小写与下划线**） | `library.rs:259` |
| 曲名 | `stem.replace('_', " ")`；**不使用 ID3 title** | `library.rs:268` |
| key 上限 | `key.len() > 512`（**字节数**，非字符数）→ 跳过 + 告警 | `library.rs:260` |
| 排序 | 分类内 `title.to_lowercase()` 升序，相同再按 `file_name`；分类之间按 `categories.json` 顺序，不排序 | `library.rs:161-166` |
| 非 UTF-8 文件名 | 跳过 + 告警「跳过文件名不是有效 UTF-8 的文件」 | `library.rs:226` |
| 符号链接 | 未特殊处理（`fs::metadata` 跟随链接）；目录内软链会按真实文件处理 | `library.rs:237` |
| 未注册目录 | 只统计其中音频数量，>0 才告警；以 `.` 开头的目录静默忽略 | `library.rs:320` |
| 根目录音频 | 逐个告警「不在任何分类目录中，已被忽略」 | `library.rs:329` |
| 空曲库 | 追加一条「未发现任何受支持的音频」提示 | `library.rs:171` |
| 分类校验 | id 必须是单个普通路径分量：非空、非 `.`/`..`、不以 `.` 开头、不含控制字符与路径/通配保留字符（含竖线）、≤40 字符 | `categories.rs:339` |
| 分类归一化 | 空 `nameZh` → 用 id；空 `nameEn` → 用 `nameZh`；空 `labelEn` → `nameZh.to_uppercase()`；非法颜色 → 调色板轮转；空 icon → 按 id 映射（6 个内置）或 `music`；空 title → `"{nameZh}音乐"`；空 description → 固定文案；**id 重复（忽略大小写）丢弃后者** | `categories.rs:130-198` |
| 新增分类 | 名称必填且不允许重名（忽略大小写）；id 留空则用名称作目录名；id 重名拒绝 | `categories.rs:209` |
| 改名分类 | **不改 id、不移动文件**；`title` 中若含旧名则同步替换；颜色非法报错 | `categories.rs:250-290` |

### F02 曲库文件操作

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 导入 / 改名 / 移动 / 批量移动 / 软删除 / 批量导出 |
| **状态** | 已实现 |
| **判断依据** | `ops.rs:101/209/239/311/365` 五个入口全部实现，返回统一 `BatchReport`；11 项单元测试覆盖同名序号、格式保留、冲突报错、回收站、导出命名 |
| **功能描述** | 对曲库文件系统的**全部写操作**。路径一律由「根目录 + 注册分类 + 文件名」推导，**不信任前端传入的绝对路径**。写入前先做安全校验，失败逐条记录不中断整批。 |
| **入口/触发** | 命令 `import_tracks`、`update_track`、`move_tracks`、`delete_tracks`（含系统确认框）、`export_tracks`；分类 `create_category` / `update_category` |
| **关键代码位置** | `core/src/ops.rs:101` `import_files`、`:139` `import_one`、`:189` `rename_song`、`:199` `move_song`、`:209` `move_songs`、`:239` `update_song`、`:311` `trash_songs`、`:365` `export_songs`、`:443` `validate_title`、`:467` `song_file_path`、`:507` `unique_path`、`:533` `unique_trash_dir`；`app/src/state.rs:606` `import_tracks`、`:625` `update_track`、`:658` `move_tracks`、`:680` `delete_tracks`、`:694` `export_tracks`、`:723` `finish_mutation` |
| **输入** | 导入：`category: String` + 文件选择器返回的 `Vec<PathBuf>`；改名：`key, title, category`；移动：`keys: Vec<String>, category`；删除：`keys`；导出：`keys` + 目录选择器 |
| **输出** | `LibraryMutation { library, changes: KeyChange[], removed: string[], report: BatchReport }`；`BatchReport { total, succeeded, skipped, failed, messages[], failures?[], notice? }` |
| **核心流程** | **导入**：校验分类 → `create_dir_all` → 逐文件：校验存在/是文件/是音频 → 若源已在目标目录则跳过 → 取 stem（trim，截 120）→ 校验 key ≤512 → `unique_path` → `copy_validated`。<br>**改名**：定位源文件 → `validate_title` → 目标精确路径已存在且非同一文件 → `TargetExists` 报错（不覆盖）→ `fs::rename` → 返回 `KeyChange`。<br>**移动**：源 stem 不变 → 目标用 `unique_path`（重名追加 ` (2)`）→ rename。<br>**软删除**：生成批次目录 `.tfy-trash/<epoch_ms>` → 逐条 rename 为 `<category>__<safe title>.<ext>` → 收集 `removed`。 |
| **依赖项** | `export::{copy_validated, safe_file_name}`、`library::{audio_extension, DEFAULT_AUDIO_EXTENSION}`、`limits::{MAX_KEY_LEN, MAX_STEM_CHARS}`、`paths::TRASH_DIR_NAME` |
| **配置项** | 无 |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | **直接复用 `ops.rs`**。`KeyChange` 是这一域的核心契约：GPUI 的 `Library::Mutation` 必须保证「key 变更后收藏 / 循环点 / 当前曲目 / 队列 / 字幕记录不丢」。注意 `unique_path` 在生成文件名时会把 stem 截到 **100 字符**（比 `MAX_STEM_CHARS=120` 更短），GPUI 若自行实现命名规则必须与之一致，否则两个分支对同一批文件会生成不同文件名。 |
| **待确认** | 无 |

**文件操作规则（F02）**

| 操作 | 命名规则 | 冲突处理 | 失败语义 |
| :--- | :--- | :--- | :--- |
| 导入 | 复制到 `root/<category>/`，保留源扩展名；stem trim 后截 120 字符，空则 `audio`；最终名字再截 100 | **追加 ` (2)`、` (3)`…**，不覆盖；同主干不同扩展名也算冲突 | 单条失败计入 `failed` + `failures[]`，其余继续 |
| 改名 | 目标名 = 新曲名 + 源扩展名 | 目标已存在且非同一文件 → **报错中止该条** | `TargetExists` |
| 移动 | 保留源 stem + 扩展名 | `unique_path` 追加 ` (2)` | 单条失败不中断 |
| 软删除 | `<root>/.tfy-trash/<epoch_ms>/<category>__<safe title>.<ext>` | 批次内重名追加 ` (2)`；批次目录重名追加 `-2` | 文件已不存在 → `skipped` |
| 批量导出 | `<dest>/<safe_file_name(title)>.<ext>` | 追加 ` (2)`；`stem_taken` **忽略大小写**比较 | 目录不存在 → 整批失败 |
| 曲名校验 | trim 后非空、不以 `.` 开头、无控制字符与路径/通配保留字符（含竖线）、≤120 字符 | — | `EmptyTitle` / `InvalidTitle` / `TitleTooLong` |
| 路径安全 | `song_file_path` 要求 `file_name` 是**单个 Normal 分量**（`components().count()==1`），拒绝 `../x` 与绝对路径 | — | `InvalidFile` |

### F03 标签与时长

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 音频标签解析与时长读取（`lofty` + `MetaCache`） |
| **状态** | 已实现 |
| **判断依据** | `tags.rs:85` `read_tags`、`:49` `MetaCache::get`；4 项单元测试含「缺失文件返回空」「垃圾字节不 panic」「WAV 时长 ±100ms」「缓存命中」 |
| **功能描述** | 扫描时读取标题/艺人/专辑/流派与时长，随 `Song` 返回；标签缺失字段省略（`skip_serializing_if`），不影响曲目可用性。 |
| **入口/触发** | `scan_category` 内对每个文件调用 `cache.get(&path, &meta)` |
| **关键代码位置** | `core/src/tags.rs:22` `MAX_CACHE_ENTRIES=20_000`、`:24` `MAX_TAG_CHARS=200`、`:27` `TrackTags`、`:38` `MetaCache`、`:85` `read_tags`、`:115` `clean` |
| **输入** | `path: &Path` + `fs::Metadata`（用于缓存失效判断） |
| **输出** | `TrackTags { title?, artist?, album?, genre?, duration_ms? }` |
| **核心流程** | ① 取 `len` 与 `modified` → 毫秒时间戳；② 缓存命中（路径 + 大小 + 修改时间全等）直接返回；③ 未命中 → `Probe::open` → `ParseOptions::new().read_cover_art(false)` → `read()`；④ 时长取 `tagged.properties().duration()`，`>0` 且 ≤`u32::MAX` 才写入；⑤ 标签取 `primary_tag()`，否则 `first_tag()`；⑥ 每个字段 trim、去空、截 200 字符；⑦ 写回缓存；⑧ 任何错误 → 返回 `Default`（全空），**不产生告警** |
| **依赖项** | `lofty`（`config` / `file` / `probe` / `tag` 模块）；`std::sync::Mutex` |
| **配置项** | 无 |
| **异常/边界** | ① 缓存超 20 000 条时**整体清空**（非 LRU），下一次全库重读；② 缓存键不含 inode / hash，仅大小 + mtime，mtime 精度不足的文件系统上「同大小同秒替换」可能误命中【已知局限】；③ 显式关闭封面解析以避免大曲库 IO 放大；④ 解析失败静默降级 |
| **gpui 实现建议** | **直接复用**。GPUI 若已有自己的标签读取，必须保证缓存失效条件一致（路径 + 长度 + 修改毫秒），否则 watch 触发后会重复解析全库，在 500+ 曲目时表现为「每次文件变动界面卡一下」。注意 `Song.title` **不是** ID3 title，GPUI 不要「顺手修正」成标签标题，否则 key/显示会与主线分叉。 |
| **待确认** | `lofty` 对 `m4a` 的 `ilst` 与 `ogg` 的 Vorbis Comment 覆盖范围依赖 crate 版本，升级时需回归（当前 `lofty` 版本见 `core/Cargo.toml`）|

### F04 曲库实时监听

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 系统级文件监听 + 防抖 + 兜底扫描 + 增量 diff |
| **状态** | 已实现 |
| **判断依据** | `library_watch.rs` 完整实现 `notify` 监听、700ms 防抖、30s 兜底；`state.rs:540` `handle_watch_event` 实现增量与迁移；`state.rs:905` `diff_songs` 有 2 项单测 |
| **功能描述** | 曲库目录发生增删改时界面自动刷新，无需手动「重新扫描」。事件经防抖合并后重新扫描并与上一份快照比对，推送 `LibraryChange`。 |
| **入口/触发** | 启动 `setup` 中 `state.start_watching`；切换根目录 / 重扫时 `install_root` 重建；兜底定时器 |
| **关键代码位置** | `app/src/library_watch.rs:21` `DEBOUNCE=700ms`、`:22` `FALLBACK_INTERVAL=30s`、`:24` `LibraryWatcher`、`:110` `is_relevant`；`app/src/state.rs:31` `LIBRARY_CHANGED_EVENT`、`:540` `handle_watch_event`、`:884` `same_content`、`:905` `diff_songs`、`:876` `key_migrations` |
| **输入** | `notify` 事件（路径 + 类型） |
| **输出** | `LibraryChange { library, added[], removed[], moved[] }`，经 `library-changed` 事件推送 |
| **核心流程** | ① `notify::recommended_watcher` 递归监听 root；② 回调内 `is_relevant` 过滤（只看目录、无扩展名路径、音频扩展名；只接受 Create、Remove、Modify(Name)、Modify(Data)、Modify(Metadata)、Any）；③ 通过 `mpsc` 发信号；④ 监听线程 `recv_timeout(400ms)`；收到信号后**持续 drain 到安静 700ms**；⑤ 调 `handle_watch_event`；⑥ 内容未变 → 返回 `None`，不推送；⑦ 有变化 → diff + 迁移收藏/循环点/字幕 + 更新 runtime + emit；⑧ 30s 无事件也主动扫一次 |
| **依赖项** | `notify`、`crossbeam` 语义的 `std::sync::mpsc`、`AppState`、`subtitles::migrate_keys` |
| **配置项** | 无（700ms / 30s 为常量） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | GPUI 无 Tauri 事件总线，建议「监听线程 → 通道 → UI 主循环 `cx.notify()`」。**必须保留防抖与兜底**：`notify` 在批量拷贝时会连发数百个事件，去掉防抖会导致反复全库扫描；网络盘 / 休眠恢复会丢事件，去掉 30s 兜底会「文件变了但界面不动」。`ops_lock` 的 `try_lock` 语义也要保留：写操作进行中直接放弃本次扫描，否则会把「文件已移动、key 还没迁移」的中间态当成删除，误删收藏。 |
| **待确认** | 无 |

**监听与增量规则（F04）**

| 场景 | 表现 | 依据 |
| :--- | :--- | :--- |
| 同目录改名 | 增量里是 **`added` 一条 + `removed` 一条**，不是 `moved` | `state.rs:999` 单测 |
| 跨分类移动 | 识别为 `moved`（同 `fileName` 且同 `title`，且新 key 不在旧快照中） | `state.rs:921-936` |
| 移动识别条件 | 必须 `file_name` 与 `title` **同时相同**；仅同名不同目录不算 | 同上 |
| 事件乱序 | `applyExternalChange` 丢弃 `scannedAtMs` 更旧的事件，避免界面回退 | `library.ts:87` |
| 写操作并发 | `handle_watch_event` 用 `try_lock`，写操作持锁期间直接返回 `None` | `state.rs:541` |
| 监听启动失败 | 只 `eprintln!`，不阻断启动；`install_root` 会补一条「文件监听不可用」告警 | `lib.rs:53`、`state.rs:852` |
| 线程回收 | `Drop` 只置 `stop` 标志，**不 join**（避免与写操作锁互锁） | `library_watch.rs:94` |

### F05 播放引擎与传输控制

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | Web Audio 整曲解码播放、交叉淡入淡出、A/B 循环、倍速、失败跳过 |
| **状态** | 已实现（强依赖 Web Audio，GPUI 需重写） |
| **判断依据** | `audio-engine.ts:461` `loadSource`、`:592` `crossfadeTo`、`:579` `spawnSource`、`:356` `startPlayback`、`:392` `pausePlayback`、`:335` `setLoopRegion`；`player.ts:147` `playSong`、`:279` `handleEnded`、`:292` `failCurrent` |
| **功能描述** | 不用 `<audio>`，而是把整首音频解码成 `AudioBuffer` 播放，以换取采样级循环、实时频谱与整曲分析能力。切歌时保留新旧两个声部做交叉淡入淡出。 |
| **入口/触发** | 播放器按钮、列表双击、快捷键 `空格`、`playSong` / `nextTrack` / `prevTrack`、节目单 `applyScheduleEntry`、HTTP `play/next/previous/toggle`、启动自动播放 |
| **关键代码位置** | `ui/src/stores/audio-engine.ts`（748 行，全部）；`ui/src/stores/player.ts`（454 行）；`ui/src/lib/config.ts::playerConfig` |
| **输入** | `path`（本地绝对路径）、`LoadOptions { autoplay, restoreTime, loop, crossfadeSeconds, fadeInSeconds, onAutoplayBlocked }`、偏好（音量/静音/倍速） |
| **输出** | `audioState { duration, currentTime, bufferedPercent, loadedPath }`；引擎事件 `play` / `pause` / `ended` |
| **核心流程** | ① `loadSource` 自增 `loadToken`；② 若正在播放且 `crossfade > 0.05` → `keepPlaying`，否则停旧源、清 buffer；③ `fetch(assetUrl(path))`（`loadTimeoutMs` 超时）→ `blob.arrayBuffer()` → `decodeAudioData`；④ token 变化则返回 `superseded`；⑤ `keepPlaying` → `crossfadeTo`；否则按 `autoplay` 决定 `startPlayback(fadeIn)`；⑥ `buildVoice` 创建 `BufferSource → Gain → Analyser/K-weighting`；有循环区间则设 `loop/loopStart/loopEnd`；⑦ `requestAnimationFrame` 时钟推进 `audioState.currentTime`；⑧ `ended` → 交给 `handleEnded` |
| **依赖项** | Web Audio API（`AudioContext` / `AudioBufferSourceNode` / `GainNode` / `AnalyserNode` / `MediaStreamAudioDestinationNode`）、`assetUrl`（Tauri asset 协议） |
| **配置项** | `config.player.{loadTimeoutMs, rates, restartThresholdSeconds, maxAutoSkips, seekStepSeconds, volumeStep}`；`prefs.{volume, muted, playbackRate, crossfadeSeconds, playMode}` |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | 这是迁移成本最高的模块之一。**不要试图逐行移植**，按下表「语义 → Rust 方案」重写：<br>• 整曲解码 → `symphonia` 解到 `Vec<Vec<f32>>`（或分块缓存），内存约 23MB/分钟（48k 立体声 float32），需设上限（主线是 30 分钟）；<br>• 采样级循环 → 自定义 `rodio::Source`，在 `next()` 里对 sample index 取模，或直接把 PCM 交给 `cpal` 输出回调自行索引；<br>• 交叉淡入淡出 → 自建双 voice 混音器，共享同一个输出时钟（`cpal` 的 sample counter），用线性包络；<br>• 80ms 传输淡入淡出 → 同一包络，只是时长更短；<br>• 倍速 → `cpal` 侧重采样；**保音高需要额外的 phase vocoder / WSOLA**，主线依赖浏览器 `playbackRate` 自动保音高，GPUI 若不做会与主线听感不同（gpui README 已声明这是已知差异）；<br>• `ended` → 混音器读到 EOF 时发事件；<br>• `superseded` 语义 → 用单调递增的 `generation: u64`，解码线程完成后比对 generation 再提交。 |
| **待确认** | ① GPUI 侧是否需要保音高倍速（决定是否引入 `rubato` / `signalsmith-stretch`）——产品决策项；② `rodio 0.23` 默认特性实际覆盖的容器（mp3/wav/flac/ogg/m4a）需构建后实测 |

**播放规则与边界（F05）**

| 规则 | 具体行为 | 依据 |
| :--- | :--- | :--- |
| 整曲解码上限 | `durationMs > 30min` 直接跳过并提示「整曲解码内存占用过高」 | `player.ts:57,205` |
| 传输淡入淡出 | 播放、暂停与恢复使用 80ms 线性包络，暂停的声部在包络时长再加 80ms 后停止。TTS 录参考音时另用 `suspendPlayback` 停止各声部，并禁止录音期间重新发声 | `audio-engine.ts:72,392` |
| 交叉淡入淡出 | `0–12s`，`prefs` 默认 **2s**；旧声部线性淡出、新声部同时淡入；连续快速切歌时先停掉上一轮 `outgoing`，避免三层叠音 | `audio-engine.ts:592-625` |
| 自然播完 | 无旧声部可交叉，用**相同时长**给新曲补淡入 | `player.ts:281` |
| A/B 循环（引擎） | 区间跨度 `> 0.05s` 才生效；位置按 `region.start + ((pos-start) % span)` 回绕 | `audio-engine.ts:323-328,429-435` |
| A/B 循环（存储） | 跨度 `< 0.2s` 拒绝；端点钳制 `0–86400`；非有限数丢弃 | `loops.rs:19-20,45-52` |
| 循环区间裁剪 | 终点按可信时长裁剪（优先当前 buffer，其次 `song.durationMs`），`< 0.1s` 视为无效 | `player.ts:116-136` |
| 上一首 | 当前播放 `> restartThresholdSeconds`（默认 3s）→ 先回开头；否则切上一首 | `player.ts:260` |
| 失败跳过 | 队列 >1 且 `failureStreak <= maxAutoSkips`（默认 3）→ toast + 下一首；否则写 `playbackError` 并停 | `player.ts:292-305` |
| 倍速变更 | 先用旧倍速结算 `offset`，再改 `playbackRate`，避免进度跳变 | `audio-engine.ts:310-320` |
| 自动播放被拦截 | `onAutoplayBlocked` 回调把 `state.playing` 改回 `false` 并 emit `pause` | `audio-engine.ts:373-389` |
| 曲目路径变化 | 同一 key 换了路径（改名/外部移动）→ `crossfade: 0` 硬切重载并按原进度恢复 | `player.ts:99-104` |
| 曲目消失 | `currentSong` 变 null → 若正在加载也 `unloadSource()`，作废未完成请求 | `player.ts:88-91` |

### F06 播放队列与智能随机

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 队列维护 + 顺序/单曲/智能随机 |
| **状态** | 已实现 |
| **判断依据** | `queue.ts:62` `smartNextIndex`、`:75` `smartPick`、`:141` `recentPenalty`、`:162` `detachQueueItem`；`player.ts:243` `nextTrack` |
| **功能描述** | 队列只在内存中（不持久化），指针随播放移动。`shuffle` 不是均匀随机，而是按「BGM 可听性」加权：避免同专辑/同艺人连播、近期播过降权、长短曲搭配。 |
| **入口/触发** | 播放任意曲目（`syncQueueForPlayback`）、`nextTrack`、队列抽屉点击/移除/清空、随机试听按钮（`smartPickSong`） |
| **关键代码位置** | `ui/src/stores/queue.ts`（183 行）；`ui/src/stores/player.ts:243-289`；`ui/src/stores/state.ts:94` `visibleSongs` |
| **输入** | `queue: Song[]`、`queueIndex`、`state.favorites`、`history` |
| **输出** | 新 `queueIndex`；或 `-1` 表示无可用候选 |
| **核心流程** | ① `recordPlay` 记录 key + 时间戳（上限 48 条）；② `smartPick` 对每个候选算权重；③ 若存在「非当前艺人/专辑」的候选，则把同艺人/同专辑权重直接置 0；④ 按权重轮盘赌；⑤ `nextSequentialIndex` / `prevSequentialIndex` 取模循环 |
| **依赖项** | `state.favorites`、`Song.{artist, album, genre, category, durationMs}`（来自 F03） |
| **配置项** | `prefs.playMode`（`sequence` / `repeat-one` / `shuffle`） |
| **异常/边界** | 候选总权重为 0 → 返回 `-1`，播放器不动；队列长度 1 时 `nextTrack` 等价于回到开头；队列空时所有操作直接 return |
| **gpui 实现建议** | **可直接移植为纯 Rust 函数**（不依赖 DOM）。建议把 `smartPick` 放到 `bgm-core` 的新模块（如 `queue.rs`），这样两个分支共享同一套权重，避免「随机听感不一致」这类难复现的问题。`history` 需常驻内存，跟随进程生命周期。 |
| **待确认** | 无 |

**智能随机权重（F06，精确值）**

| 条件 | 乘数 | 依据 |
| :--- | :--- | :--- |
| 收藏 | ×1.9 | `queue.ts:90` |
| 同分类 | ×1.18 | `queue.ts:91` |
| 异分类 | ×0.95 | `queue.ts:92` |
| 同流派（双方都有值且相等） | ×1.08 | `queue.ts:93` |
| 近期播过 | `× max(0.05, 1 - penalty)`，penalty ∈ [0,1] | `queue.ts:95-96` |
| 当前 ≥180s，候选 <90s | ×0.22 | `queue.ts:99` |
| 当前 <60s，候选 <60s | ×0.4 | `queue.ts:100` |
| 当前 ≥300s，候选 ≥300s | ×0.75 | `queue.ts:101` |
| 同专辑 | ×0.03 | `queue.ts:108` |
| 同艺人（非专辑） | ×0.05 | `queue.ts:109` |
| 存在替代曲目时 | 同专辑/同艺人 **权重置 0**（直接排除，不是降权） | `queue.ts:114-128` |
| 近期惩罚窗口 | 45 分钟 / 48 条；`penalty = (1 - age/45) * (0.45 + 0.55 * 位置比例)` | `queue.ts:6-7,141-153` |

> 旧文档把「长曲接短曲」写成「≥180s 后接 <90s ×0.22」是对的，但把「短曲接短曲」写成 `<90s` 应为 **`<60s`**，把「长曲接长曲」写成「长曲」应为 **`≥300s`**。已在本表修正。

### F07 多输出设备路由

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 同一路混音同时输出到多个物理设备，每路独立音量/静音 |
| **状态** | **部分实现**（仅 Chromium 系 WebView） |
| **判断依据** | `outputs.ts:224` `ensureSink` 依赖 `AudioContext.setSinkId` 与 `MediaStreamAudioDestinationNode`；`runApply` 只在错误信息含「不支持」时降级 `supported=false`；Linux WebKitGTK / macOS WKWebView 无 `setSinkId` |
| **功能描述** | `AudioContext.destination` 只能指向一个设备，因此为每个额外设备建立独立 `AudioContext`，`setSinkId` 指定硬件，再用 `MediaStream` 把主混音桥接过去——所有设备共享主引擎渲染时钟，长时间播放不会漂移。 |
| **入口/触发** | 播放器「播放输出」面板、设置「播放 → 输出设备」、HTTP `GET /v1/outputs`、`POST /v1/outputs`、`outputs.*` action |
| **关键代码位置** | `ui/src/stores/outputs.ts:36` `MAX_OUTPUT_ROUTES=8`、`:109` `refreshDevices`、`:153` `requestDeviceAccess`、`:176` `applyOutputRoutes`、`:224` `ensureSink`、`:293` `setOutputRoute`；`ui/src/stores/audio-engine.ts:261` `getMixStream`、`:268` `setDefaultOutput`；`app/src/lib.rs:39` 麦克风权限放行 |
| **输入** | `prefs.outputRoutes: OutputRoute[]`；设备枚举结果 |
| **输出** | 各路 `AudioContext` + `GainNode`；`outputsState { supported, labelsAvailable, devices, routed, deviceError, routeError, busy }` |
| **核心流程** | ① `installOutputs` 监听 `prefs.outputRoutes`（deep, immediate）+ `devicechange`；② `refreshDevices` 枚举 `audiooutput`，`default` 恒在首位；③ `runApply` 串行队列执行：先设默认输出增益，再逐个 `ensureSink`，失败则 `closeSink` 并记错误；④ `ensureSink`：新建 `AudioContext` → `setSinkId(deviceId)` → `createMediaStreamSource(getMixStream())` → `Gain` → `destination`；⑤ 播放事件触发 `resumeOutputs`（浏览器要求手势后才能启动音频） |
| **依赖项** | Web Audio `setSinkId`、`MediaStreamAudioDestinationNode`、`navigator.mediaDevices`、Tauri 麦克风权限放行 |
| **配置项** | `prefs.outputRoutes`（≤8，按 deviceId 去重，`default` 恒首位）；每路 `{ deviceId, label, volume, muted, enabled }` |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | 用系统音频 API 重写：`cpal` 枚举设备 + 为每个启用设备建立独立输出流，把同一个 mix bus 的样本广播到各流（`ringbuf` / `crossbeam` 分发），每流独立增益。**不要**为每路建独立解码器——主线共享一个渲染时钟正是为了避免漂移。`OutputRoute` 的字段与 8 路上限**必须保持一致**，否则 `prefs.json` 在两个分支间互不兼容。Windows 需注意 WASAPI 共享模式下的采样率协商；Linux 需处理 PulseAudio / PipeWire 设备 id 不稳定（主线用浏览器 deviceId，GPUI 要用稳定的设备标识，可能需要 `device.name + index` 组合）。 |
| **待确认** | ① 主线在 macOS / Linux 上的多输出是否**完全不可用**（而非部分可用）需实机验证；② GPUI 侧设备 id 的稳定性方案尚未确定 |

**输出路由规则（F07）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 上限 | 8 路；超限时 `setOutputRoute` 返回 `false`，HTTP 层转 **409** | `outputs.ts:304`、`api.ts:206` |
| `default` 语义 | 固定代表系统默认输出，恒排第一行；`prefs.normalize` 保证它一定存在 | `prefs.ts:347-355`、`outputs.ts:85` |
| 默认启用策略 | 路由列表为空 → `default.enabled=true`；已有其它设备 → 新插入的 `default.enabled=false`（避免无意外放） | `prefs.rs:345-354` |
| 音量语义 | 每路音量是**叠加在主音量之上的独立增益**；静音只影响该路 | `audio-engine.ts:132-135`、`setDefaultOutput` |
| 设备标签 | 未授权时 `enumerateDevices` 只给空 label，用「输出设备 N」占位；`requestDeviceAccess` 申请一次麦克风权限后立即 `stop()` 所有 track（不录音） | `outputs.ts:120-137,153-173` |
| 已拔出设备 | 路由仍保留，`outputRows` 标记 `missing: true`，界面显示但不可用 | `outputs.ts:73-82` |
| 能力降级 | 只有错误信息含「不支持」才把 `supported=false`；枚举失败只记 `deviceError`，不降级 | `outputs.ts:140,219` |

### F08 节目单调度

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 每日时段排程 + 歌曲级时间轴 + 到点接管播放 |
| **状态** | 已实现 |
| **判断依据** | `schedule.rs` 模型/校验/重叠检测齐全（9 项单测）；`useScheduler.ts` 实现 1s tick、2 分钟回看、90s 休眠对齐；`schedule.ts::applyScheduleEntry` 负责应用 |
| **功能描述** | 把一天切成互不重叠的时段，每档节目有名称、起止时间、到点音量、启停状态与**按时间排布的歌曲列表**；到点自动接管播放，时段内拦截手动点播。 |
| **入口/触发** | 侧栏「节目单」页；`installScheduler()` 在 `App.vue` 顶层调用（1s interval）；保存/启用节目后 `syncToCurrentProgram()` |
| **关键代码位置** | `core/src/schedule.rs:16` `SCHEDULE_VERSION=2`、`:17-18` 上限、`:105` `normalize`、`:125` `overlap_error`、`:154` `ScheduleEntry::normalize`、`:222` `normalize_start`；`ui/src/composables/useScheduler.ts:22` `CATCHUP_WINDOW_MS`、`:56` `tick`、`:123` `currentProgram`、`:142` `guardLibraryPlayback`、`:156` `syncToCurrentProgram`、`:199` `installScheduler`、`:214` `handleScheduleEnded`；`ui/src/stores/schedule.ts:11` `persistSchedule`、`:51` `applyScheduleEntry` |
| **输入** | `Schedule { version, entries[] }`；系统本地时间 |
| **输出** | 应用到播放器的队列与音量；`activeScheduleId`、`schedulerNow`、`currentProgram`、`nextProgram` |
| **核心流程** | ① 保存：前端 `normalize` → 后端 `normalize` → `overlap_error` 拒绝 → 落盘；② 调度 tick：收集「开始时间 ≤ now 且 ≥ now-2min」且未触发过的候选 → 取**最晚**一条应用，其余标记已触发；③ 无候选且（间隔 >90s 或从未激活）→ 对齐到当前节目里最晚一首已开始的歌；④ 应用：先设音量 → 解析队列（跳过已不在曲库的 key）→ 定位目标歌 → `playSong(queue)`；⑤ 歌曲播完：`endedHandler` 先于常规逻辑执行，只循环当前到点的歌 |
| **依赖项** | `player.ts`（`setVolume` / `playSong` / `setEndedHandler`）、`state.schedule`、`songByKey` |
| **配置项** | `schedule.json`（§4.5） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | 需要一个 **1 秒节拍源**（`cx.spawn` + `Timer`，或独立线程 + 通道）。必须把「2 分钟回看」与「>90s 休眠对齐」两条逻辑一起移植——只判断「当前这一秒」在窗口最小化 / 系统休眠后会漏播整段节目。`setEndedHandler` 这种「钩子覆盖默认行为」的设计在 Rust 里建议改为显式枚举：`EndedAction::{ ScheduleReplay(key), RepeatOne, Next }`，由播放器统一决策，避免回调链难追踪。`triggered` 集合需按「日期 + 节目 + 歌曲」做 key，并在跨天时清理（主线用 `dayKey` 拼进 key，超过 4000 条整体清空）。 |
| **待确认** | 无 |

**节目单规则（F08）**

| 规则 | 具体行为 | 依据 |
| :--- | :--- | :--- |
| 时间语义 | 本地时间，`[start, end)`，**不跨天**；`23:59` 起始视为无效（当天留不出 1 分钟） | `schedule.rs:159` |
| 重叠 | 相邻（`A.end == B.start`）合法；真正重叠时保存**拒绝**并给出具体节目名；读取历史文件时自动丢弃后者 | `schedule.rs:125,138` |
| 上限 | 64 档节目 / 500 首歌曲；超出截断 | `schedule.rs:17-18,199` |
| 缺省值 | 节目 `09:00–10:00`；`end` 缺失或 ≤start → `start+60min`（不超过 23:59） | `schedule.rs:48-59,163-167` |
| 名称/ID | 名称为空 → `节目 N`；id 为空 → `slot-N`；重复 → 追加 `-2`、`-3` | `schedule.rs:170-218` |
| 歌曲时间 | 起点夹到 `[start, end-1]`，终点夹到 `[song_start+1, end]`；同节目内 key 去重 | `schedule.rs:185-200` |
| 音量 | `null` = 保持当前；有值 `0–1` 钳制；非有限数丢弃 | `schedule.rs:202-205` |
| tick 节流 | 1 秒一次；候选窗口 2 分钟；同窗口多条只应用最晚一条 | `useScheduler.ts:56-88` |
| 休眠对齐 | 两次 tick 间隔 >90s，或从未激活 → 取当前节目里最晚已开始的歌 | `useScheduler.ts:89-103` |
| 播完行为 | 时段内只循环「当前已到点」的歌，等下一首起始时间再切；不串到其它时段 | `useScheduler.ts:214-220` |
| 手动点播拦截 | 时段内 `guardLibraryPlayback()` 返回 true，界面 toast；HTTP `play` 返回 **409** | `useScheduler.ts:142`、`api.ts:125` |
| 立即对齐 | 保存/启用后调 `syncToCurrentProgram()`；时段内还没有歌到点则从第一首开始 | `useScheduler.ts:156-171` |

### F09 导出与响度归一化

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 单曲/批量导出 + ffmpeg 两遍 loudnorm |
| **状态** | 已实现（归一化需 ffmpeg；缺失时安全降级） |
| **判断依据** | `export.rs:51` `export_audio`、`:61` `export_with_ffmpeg`、`:174` `copy_validated`、`:198` `safe_file_name`；`loudness.rs:40` 预设、`:90` `ffmpeg_path`、`:97` `normalize_file`、`:157` `measure`；8 + 5 项单测 |
| **功能描述** | 复制或转码到用户选择的位置。开启归一化且探测到 ffmpeg 时，按目标响度用**两遍 loudnorm** 输出 mp3；否则原格式复制并说明原因。 |
| **入口/触发** | 播放器「导出音频」（`export_track`，单曲 + 保存对话框）；批量「导出选中」（`export_tracks`，目录对话框）；`export_info` 查询能力 |
| **关键代码位置** | `core/src/export.rs`（369 行）、`core/src/loudness.rs`（391 行）、`core/src/ops.rs:365` `export_songs`、`app/src/commands.rs:37` `export_extension`、`:851` `export_info`、`:998` `export_track`、`:971` `export_tracks` |
| **输入** | `source: &Path`、`destination: &Path`、`ExportSettings { normalize: bool, preset: ExportPreset }` |
| **输出** | `ExportOutcome { bytes, normalized, warning }`；批量 `BatchReport` + `notice` |
| **核心流程** | ① 校验源存在、非同一文件；② `normalize == false` → 同格式复制（带提示）；③ 无 ffmpeg → 同格式复制（带提示）；④ 目标扩展名不是 mp3 → 同格式复制（带提示），若源格式也不同 → 报错；⑤ 两遍 loudnorm：`measure()` 解析 stderr JSON → 拼 `linear=true` filter → 编码 mp3 320k；⑥ 输出为空 / 失败 → 删目标，回退复制（仅当扩展名一致）；⑦ 批量由 `ops.rs` 只探测一次 ffmpeg，逐首复用，同名追加序号 |
| **依赖项** | `ffmpeg`（外部可执行文件）、`serde_json`（解析 loudnorm 输出） |
| **配置项** | `prefs.loudnessNormalization`（默认 true）、`prefs.exportScene`（`""`/`video-editing`/`podcast`/`focus`/`broadcast`） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | `export.rs` + `loudness.rs` **零 Tauri 依赖，可直接搬运**。唯一需要替换的是「保存/选择目录对话框」：GPUI 用 `rfd`（`AsyncFileDialog`）或平台原生 API。**不要改写决策树**（哪一步回退、哪一步报错），这套顺序保证了「绝不把错误字节写进目标文件」。Windows 上要保留 `CREATE_NO_WINDOW`，否则绿色版会闪控制台窗口。`ffmpeg_path()` 会 spawn 3 次探测进程，单曲导出在打开对话框时就会调用一次（`commands.rs:37`），GPUI 建议缓存探测结果。 |
| **待确认** | 无 |

**导出预设（F09）**

| id | 名称 | 目标 LUFS | 真峰值 dBTP | LRA |
| :--- | :--- | :--- | :--- | :--- |
| `video-editing`（默认） | 视频剪辑 | -14.0 | -1.0 | 11.0 |
| `podcast` | 播客/访谈 | -16.0 | -1.5 | 11.0 |
| `focus` | 专注/冥想 | -20.0 | -2.0 | 7.0 |
| `broadcast` | 广播/影视 | -23.0 | -1.0 | 7.0 |

**导出规则（F09）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 默认 | 归一化**开启**；`exportScene=""` 表示「未显式选场景」，回退默认预设（**不是关闭归一化**） | `export.rs:32-40` |
| ffmpeg 查找 | `TFY_FFMPEG`（可指目录）→ exe 同级 → `PATH`；每个候选执行 `-version` 判活 | `loudness.rs:224-259` |
| 两遍 loudnorm | 第一遍 `print_format=json` 只测量，第二遍 `linear=true` + 实测值做线性增益 | `loudness.rs:103-137` |
| 编码参数 | `libmp3lame 320k`、`-id3v2_version 3`、`-map 0:a:0 -map 0:v? -c:v copy`（保留封面） | 同上 |
| 静音素材 | 测得 `-inf` → `LoudnormError::Silent` → 回退复制（扩展名一致时）或报错 | `loudness.rs:180-184,198-213` |
| 复制校验 | `copied != metadata.len()` → 删目标 + `SizeMismatch` | `export.rs:186-193` |
| 同文件保护 | 源与目标 canonical 相同 → `SameFile` | `export.rs:74,182` |
| 文件名安全 | 控制字符与路径/通配保留字符（含竖线）→ `_`；去首尾空白与 `.`；空 → `audio`；≤120 字符 | `export.rs:198-216` |
| 批量提示 | 全部成功归一化 → 「已导出 N 首（响度归一化 · 目标 X LUFS）」；部分 → 注明数量；0 首 → 普通提示 + 首条 warning | `ops.rs:414-439` |

### F10 歌曲详情与可视化诊断

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 整曲波形 + 实时频谱 + 瞬时 LUFS/峰值 + 整曲分析结论 + 循环区间标记 |
| **状态** | 已实现 |
| **判断依据** | `audio-analysis.ts:88` `analyseBuffer`、`:357` `computePeaks`（374 行纯数值）；`audio-engine.ts:146-179` K-weighting 节点；`DiagnosticsPanel.vue`（600 行） |
| **功能描述** | 点击播放器封面展开歌曲详情，在「歌词」「详情」间切换，或从工具栏打开音频诊断。歌词区可预览 SRT/LRC，并进入生成、编辑和导出。详情区展示元数据、循环状态及可用的 F19 缓存分析。所有实时诊断指标以**源文件**为基准（归一化前），导出目标响度仅作对照显示，不参与测量。 |
| **入口/触发** | 点击播放器封面。详情页可先展示曲库元数据，实时诊断需当前 `AudioBuffer` |
| **关键代码位置** | `ui/src/lib/audio-analysis.ts`（全部）；`ui/src/components/DiagnosticsPanel.vue`；`ui/src/stores/audio-engine.ts:280` `getAnalyserHandle`、`:286` `getAudioBuffer` |
| **输入** | `AudioBuffer`（已解码，不重新读文件） |
| **输出** | `AudioAnalysis { duration, sampleRate, channels, integratedLufs, rmsDb, peakDb, clippedSamples, clipRuns, pops, maxDelta, silence[], minCorrelation, averageCorrelation, outOfPhase, issues[], verdict }` |
| **核心流程** | ① 逐声道逐采样扫描：峰值、削波、跳变（爆音）、RMS 块能量、K 加权能量；② 400ms 窗 / 100ms 步进合成响度块；③ BS.1770 双门限积分；④ 静音段（-50dBFS、≥0.5s）；⑤ 立体声按 1s 块算相关系数；⑥ 汇总 issues 与 severity（≥2 bad / 1 warning / 0 good） |
| **依赖项** | Web Audio `AnalyserNode`（实时）、`AudioBuffer`（离线）；`save_loop_point` / `remove_loop_point`（循环标记） |
| **配置项** | 无 |
| **异常/边界** | 单声道无相位指标；尾部不足一窗忽略；`duration=0` 时多数指标为 null；实时频谱 72 段、`-90 ~ -10 dBFS` 映射 |
| **gpui 实现建议** | 移植成本最高（约 370 行数值代码 + 600 行渲染）。**建议分两步**：① 先把 `audio-analysis.ts` 的算法 1:1 翻成 Rust 纯函数（输入 `&[Vec<f32>]` + sample_rate，输出同一结构体），并用手工构造的样本（正弦、削波、静音、反相）做单测——这部分与 UI 无关，可以在 `bgm-core` 里完成；② 再做 GPUI 波形渲染（`Path` / 逐列矩形）。实时频谱需要 `rustfft` + 窗函数，且必须与播放时钟对齐；`AnalyserNode` 的 `smoothingTimeConstant=0.72` 需要在 Rust 侧手工实现同样的指数平滑，否则观感不同。**K 加权参数**（1681.974Hz / +3.9998dB / Q0.707175，38.135Hz / Q0.500327）必须逐字保留。 |
| **待确认** | 无 |

**判定阈值（F10）**

| 条件 | 等级 | 文案要点 |
| :--- | :--- | :--- |
| 削波采样 >0（≥0.999 满幅） | 严重（bad） | 采样数 + 峰值 dBFS |
| 左右相关 < -0.25 | 严重 | 最低相关值 |
| 静音总时长 >5s | 严重 | 段数 + 总秒数 |
| 有静音但 ≤5s | 提醒 | 同上 |
| 综合响度 > -9 或 < -24 LUFS | 提醒 | 偏离 -24~-9 区间 |
| 爆音 >40 次 | 提醒 | 次数 |
| 无上述问题 | good | 「可直接使用」 |

### F11 偏好与收藏

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | `prefs.json` 全量偏好 + 收藏集合 |
| **状态** | 已实现 |
| **判断依据** | `prefs.rs:18` `PREFS_VERSION=4`、`:273` `normalize`、`:437` `load`、`:481` `save`；27 项单测覆盖钳制、迁移、壁纸、模型、歌词字段 |
| **功能描述** | 音量、静音、倍速、播放模式、排序、淡入淡出、输出路由、导出开关与场景、壁纸、字体与系统开关、关闭行为、收藏、模型与歌词状态，全部持久化并在重启后恢复。 |
| **入口/触发** | 前端 `persistPrefs()`（280ms 防抖）/ `persistPrefs(true)`（立即）；`flushPrefsNow()` 在退出/隐藏前调用；后端 `update_prefs` 维护 6 个权威字段 |
| **关键代码位置** | `core/src/prefs.rs`（1030 行）；`ui/src/stores/prefs.ts:22` `persistPrefs`、`:34` `flushPrefs`、`:61` `toggleFavorite`；`app/src/state.rs:196` `save_prefs`、`:374` `update_prefs` |
| **输入** | 整包 `Prefs`（前端提交） |
| **输出** | 归一化后的 `Prefs`（以后端返回值为准，前端覆盖本地） |
| **核心流程** | ① 前端改内存 → `persistPrefs` 防抖 280ms；② `save_prefs` 用 `config.player.rates` 归一化；③ **强制保留** 后端权威字段（见下）；④ 落盘（临时文件 + rename）；⑤ 返回新 `Prefs`，前端以 sequence 号丢弃过期响应 |
| **依赖项** | `loudness::find_preset`（场景校验）、`paths::strip_verbatim_prefix`、`persist::write_json` |
| **配置项** | 全部字段见 §4.2 |
| **异常/边界** | 损坏文件 → 默认值 + 1 条告警，不阻断启动；未知字段忽略；`save_prefs` 不保留 `separation_model`（由前端提交值为准）；`library_dir` / `models_dir` 只能通过专用命令改 |
| **gpui 实现建议** | **直接复用 `prefs.rs`，但必须补齐 35 个字段**（gpui 当前只有 8 个）。容器级 `#[serde(default)]` + 未知字段忽略意味着**双向覆盖会丢配置**：GPUI 若用旧结构体读入再写回，会把 `crossfadeSeconds`、`outputRoutes`、`wallpaper*`、`smtc` 等全部抹掉。迁移期建议先只读不写，或直接把 `prefs.rs` 整体替换。`--` 另一个坑：`normalize` 里的 `f32::NAN` 分支，GPUI 若用 `serde_json` 读 `null` 会反序列化失败而不是走默认值，需要 `Option` + 默认。 |
| **待确认** | 无 |

**后端权威字段（`save_prefs` 会用内存值覆盖前端提交值）**

| 字段 | 原因 | 依据 |
| :--- | :--- | :--- |
| `libraryDir` | 只能通过 `choose_library_dir` / `reset_library_dir` 修改 | `state.rs:202` |
| `modelsDir` | 只能通过 `choose_models_dir` / `reset_models_dir` 修改 | `state.rs:203` |
| `lastAsrModel` | 由模型加载/释放命令维护 | `state.rs:204` |
| `desktopLyricsEnabled` | 由歌词开关命令维护 | `state.rs:205` |
| `asrDownloadUrl`、`ttsDownloadUrl` | 通过 `save_model_download_source` 更新 | `state.rs::save_prefs` |

> `separationModel` **不在**该列表：它由前端在生成前写入（`subtitles.ts:432`），后端只做长度与去空归一化。这是「前端权威」字段，与上面 6 个相反。

### F12 界面壁纸

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 默认样式 / 三种渐变 / 两款内置资源 / 自定义图片或 MP4 + 透明度 / 模糊 / 填充 |
| **状态** | 已实现 |
| **判断依据** | `wallpapers.rs` 实现资源定位、白名单授权、自定义校验；`useWallpaper.ts` 计算 `panelOpacity` / `maskOpacity` / `shellClass`；`WallpaperLayer.vue` 处理视频暂停与错误回退 |
| **功能描述** | 壁纸层在面板下方，面板保持半透明毛玻璃；动态壁纸静音循环，窗口失焦或页面隐藏时暂停。 |
| **入口/触发** | 顶栏壁纸按钮 → `WallpaperPopover`；`builtin_wallpapers` 启动时 `loadBuiltins()`；`choose_wallpaper_file` 选本地文件 |
| **关键代码位置** | `app/src/wallpapers.rs:17` `BUILTIN_WALLPAPERS`、`:62` `allow_wallpaper_file`、`:91` `builtin_catalog`、`:117` `prime_custom`；`ui/src/composables/useWallpaper.ts:32` `wallpaperView`；`ui/src/components/WallpaperLayer.vue`；`app/src/state.rs:125` 启动预授权 |
| **输入** | `prefs.{wallpaper, customWallpaper, wallpaperOpacity, wallpaperBlur, wallpaperFit}` |
| **输出** | CSS 变量 `--panel-opacity` / `--wall-mask-opacity` / `--panel-blur` / `--table-blur` / `--wallpaper-fit` |
| **核心流程** | ① 启动 `prime_custom` 把上次的自定义文件加入 asset 白名单；② `builtin_catalog` 逐个 `resolve_asset`（resource_dir → 仓库兜底），缺失的条目**不返回**，前端据此隐藏；③ 前端 `wallpaperView` 计算有效 source 与透明度；④ 视频 `focused && !hidden` 才 play，否则 pause；⑤ 加载失败 → toast + `setWallpaper('default')` |
| **依赖项** | Tauri asset 协议（`convertFileSrc` 等价物 `assetUrl`）、`bundle.resources` 打包 `assets/bg/*` |
| **配置项** | 合法 id：`default` / `gradient-blue` / `gradient-green` / `gradient-red` / `builtin-1` / `builtin-2` / `custom`；自定义扩展名 `jpg/jpeg/png/webp/mp4` |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | GPUI 没有 `<video>`，视频壁纸成本最高。建议分阶段：① 静态图 / 渐变先做；② 视频用首帧降级（启动时 ffmpeg 抽一帧缓存为图片）；③ 真要动态，再评估 `ffmpeg-next` 解码 + 纹理上传（需自行管理解码线程与帧同步，CPU 占用不低）。数据字段与 id 集合**必须与主线一致**，否则 `prefs.json` 互不兼容。**功能与样式的分界**：「透明度 / 模糊 / 填充方式可调且持久化」「视频失焦暂停」「资源缺失时隐藏选项」是功能，必须实现；具体面板材质、模糊实现方式、渐变配色属于样式，可自由设计（§0.0 原则 6）。 |
| **待确认** | GPUI 是否需要动态壁纸（产品取舍）；若需要，帧率与 CPU 预算未定 |

**壁纸规则（F12）**

| 规则 | 具体行为 | 依据 |
| :--- | :--- | :--- |
| id 收敛 | 未知 id → `default`；`custom` 但无合法路径 → `default` | `prefs.rs:363-370` |
| 自定义路径保留 | 切回内置壁纸后 `customWallpaper` 仍保留上次路径 | `prefs.rs:777` 单测 |
| 扩展名校验 | 仅 `jpg/jpeg/png/webp/mp4`；不检查文件是否存在（运行时判断） | `prefs.rs:63-72` |
| 透明度 | 存储 0–1；**界面实际钳到 0.95**（留一点实体感） | `useWallpaper.ts:36-40` |
| 面板不透明度 | `max(0.04, 1 - opacity * 0.92)`（保证文字可读） | `useWallpaper.ts:56` |
| 遮罩 | `max(0, 0.4 - opacity * 0.44)` | `useWallpaper.ts:57` |
| 模糊 | 0–20 整数；`--table-blur` 仅在 >0 时生成 `blur() saturate(110%)` | `prefs.rs:378`、`App.vue:48` |
| 内置缺失 | 资源文件找不到 → 该选项从列表消失（`availableWallpaperOptions`） | `wallpapers.rs:94-100` |
| 视频暂停 | 窗口失焦或 `document.hidden` → `pause()`；恢复时 `play().catch()` | `WallpaperLayer.vue:17-28` |

> Linux MP4 通过 `platform/linux/media.rs` 的独立本机 Range 媒体服务播放。主窗用 `wallpaper_media_url` 获取可播放地址。其他平台沿用 asset 协议。视频失焦或页面不可见时暂停。

### F13 系统设置与桌面集成

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 四组设置（常规 / 播放 / 模型 / 第三方接入）+ 字体、自启、托盘、GPU、防休眠、媒体控件、总线处理 |
| **状态** | **部分实现**（SMTC / MPRIS2 / 空间音效 / 音频增强 / 防休眠均为 WebView 层近似） |
| **判断依据** | `desktop.rs` 托盘/自启/GPU 已实现；`fonts.rs` 系统字体扫描已实现；`useSystemIntegration.ts` 的 Wake Lock / Media Session / Panner / Compressor 全部依赖浏览器 API；`apply_gpu_acceleration` 在 macOS 为空实现 |
| **功能描述** | 「基本系统配置」弹窗，四组配置项；其中需要落到操作系统的项（自启、托盘、GPU）由 Rust 执行，其余为偏好或 WebView 行为。 |
| **入口/触发** | 顶栏齿轮 → `openSettings(tab)`；`apply_desktop_settings`、`hide_to_tray`、`quit_app`、`system_fonts` |
| **关键代码位置** | `app/src/desktop.rs:25` `DesktopState`、`:70` `install`、`:86` `apply`、`:126` `build_tray`、`:164` `attach_window_events`、`:205` `hide_to_tray`、`:229` `set_autostart`、`:321` `apply_gpu_acceleration`；`app/src/fonts.rs:36` `list`、`:41` `scan`、`:95` `describe`、`:186` `font_files`、`:266` `registry_files`；`ui/src/composables/useSystemIntegration.ts`；`ui/src/stores/settings.ts`；`ui/src/components/SettingsDialog.vue`（1096 行）、`FontSelect.vue`（347 行） |
| **输入** | `prefs.{uiFont, autostart, minimizeToTray, gpuAcceleration, autoplayOnStart, preventSleep, smtc, spatialAudio, audioEnhancement, mpris2, closeAction}`；`api.token` / `api.cors` |
| **输出** | 系统副作用（注册表 / autostart / plist / 托盘 / WebView 参数 / Wake Lock / Media Session）；`SystemFont[]` |
| **核心流程** | **字体**：遍历平台字体目录（深度 4）→ 解析 ttc/otc 每个 face → 取 name 表族名（优先英文做 CSS、优先中文做展示）→ 按 CJK 优先排序 → 进程内 `OnceLock` 缓存。<br>**自启**：Windows 写 `HKCU\...\Run`；Linux 写 `~/.config/autostart/tfy-bgm.desktop`；macOS 写 `~/Library/LaunchAgents/com.yuanjie.tfybgm.plist`。<br>**托盘**：`TrayIconBuilder` + 菜单（显示/退出）+ 左键恢复；关闭开关即 `tray = None`。<br>**关闭行为**：`ask` 弹 `ClosePrompt` → `quit_app` 或 `hide_to_tray`。 |
| **依赖项** | `tauri::tray`、`windows-registry`（仅 Windows）、`ttf-parser`、WebView 的 `wakeLock` / `mediaSession` |
| **配置项** | 见 §4.2；平台项只在对应 `platform` 下显示 |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | 逐项映射：<br>• **字体**：`fonts.rs` 只依赖 `ttf-parser` + `std`，**可整体复用**（去掉 `tauri` 无关，实际已无）；<br>• **自启**：逻辑可复用，Windows 保留 `windows-registry`，Linux/macOS 直接写文件；<br>• **托盘**：`tray-icon` crate + `muda` 菜单；<br>• **GPU 加速**：GPUI 本身就是 GPU 渲染，该开关语义需重新定义（建议：保留字段但标注「GPUI 不适用」，或映射到 `gpui` 的软件渲染回退）；<br>• **防休眠**：Windows `SetThreadExecutionState`、Linux `zbus` → `org.freedesktop.ScreenSaver.Inhibit`、macOS `IOPMAssertionCreateWithName`；<br>• **媒体控件**：Windows `windows` crate 的 `SystemMediaTransportControls`、Linux `zbus` 实现 `org.mpris.MediaPlayer2`、macOS `objc2` + `MPNowPlayingInfoCenter`；<br>• **空间音效 / 音频增强**：主线是 Web Audio 近似（HRTF 声像 + 总线压缩），GPUI 若不做，应把开关保留为字段但在界面标注「当前实现不支持」，**不要显示为已生效**。 |
| **待确认** | ① GPUI 是否实现原生 SMTC / MPRIS2（成本较高，建议后置）；② 「GPU 加速」开关在 GPUI 下的产品语义 |

**桌面集成规则（F13）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 字体族名 | CSS 用英文名，展示用中文名；有 Typographic Family 时优先；族名以 `@` 开头（竖排）丢弃；>96 字符丢弃 | `fonts.rs:95-141` |
| 字体排序 | CJK 支持优先 → 展示名含中文优先 → 名称升序 | `fonts.rs:79-84` |
| 字体缓存 | 进程内 `OnceLock`，运行期新装字体重启后可见 | `fonts.rs:36-39` |
| 自启失败 | `apply` 回滚托盘状态并返回错误，前端撤销开关 | `desktop.rs:86-105` |
| 托盘不可用 | 强制把 `minimize_to_tray` 置 false，避免「窗口隐藏后无入口恢复」 | `desktop.rs:73-78` |
| 关闭到托盘 | `hide_to_tray` 会**同时开启** `minimize_to_tray` 并创建托盘 | `desktop.rs:205-216` |
| 关闭最小化托盘 | 若 `closeAction == tray` → 回退 `ask`（前端 `setMinimizeToTray`） | `settings.ts:114-119` |
| GPU 加速 | 仅在窗口创建前生效；Windows 追加 `--disable-gpu` 到 `additionalBrowserArgs`（保留 wry 默认参数）；Linux 设 `WEBKIT_DISABLE_COMPOSITING_MODE=1`；**macOS 无操作** | `desktop.rs:321-354` |
| 防休眠 | 播放中且开关开启才持锁；页面隐藏后系统自动释放，回到前台重新申请 | `useSystemIntegration.ts:50-75,200-203` |
| 媒体控件 | 需维持一个 **10 秒静音 WAV 循环**（Chromium 只认「正在播放的媒体元素」）；暂停时暂停该元素 | `useSystemIntegration.ts:78-134` |
| 空间音效 | HRTF，`positionZ=-1`、`rolloff=0.15`、`distanceModel=inverse`；仅 Windows | `audio-engine.ts:232-242` |
| 音频增强 | 总线压缩：threshold -18dB、knee 12、ratio 2.2、attack 10ms、release 250ms；仅 Windows | `audio-engine.ts:221-229` |

> 当前平台实现位于 `src-tauri/src/platform/`。自启、GPU 启动参数、字体源、配置目录、路径比较和媒体服务由平台模块分派。MPRIS2 设置仅在 Linux Wayland 会话提供。

### F14 本机 HTTP 开放接口

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | `127.0.0.1:8737` JSON API + SSE 事件流 |
| **状态** | 已实现 |
| **判断依据** | `api.rs` 实现 axum 路由、鉴权、CORS、SSE、命令桥；`api.ts` 实现 14 个 action 与快照；`api.rs` 测试覆盖来源、状态码、目录与 SSE 参数 |
| **功能描述** | 供 OBS、Stream Deck、直播脚本接入。Rust 只监听回环地址，播放控制由前端执行（命令经事件转发 + 6 秒超时）。 |
| **入口/触发** | 启动 `bridge.serve()`；`config.api.enabled` 为 true 时监听 |
| **关键代码位置** | `app/src/api.rs:41` `API_COMMAND_EVENT`、`:44` `COMMAND_TIMEOUT=6s`、`:118` `endpoints`、`:272` `serve`、`:340` `command`、`:377` `authorize`、`:408` `router`、`:476` `cors`、`:518` `origin_allowed`；`ui/src/stores/api.ts:67` `installApiBridge`、`:113` `runAction`、`:232` `snapshot`；`app/src/lib.rs:70-75` 启动顺序 |
| **输入** | HTTP 请求（方法 + 路径 + JSON body）；`Authorization: Bearer` / `X-TFY-Token` / `?token=` |
| **输出** | `{ok:true,data}` / `{ok:false,error}`；SSE `{type:"state", state:{...}}` |
| **核心流程** | ① 启动时读 `api.enabled` / `port` → `TcpListener::bind`（非阻塞）→ 交给 tokio；② 每个请求先过 CORS 中间件（无 `Origin` 直接放行）；③ 需要鉴权的 handler 调 `authorize`（`/v1/health` 除外）；④ 控制类 handler → `bridge.command(action, params)` → 生成 id → `emit("api-command")` → 等 oneshot；⑤ 前端 `runAction` 执行 → `api_reply(id, ok, data/error, code)`；⑥ 超时 6s → 504；前端未就绪 → 503；⑦ 前端每秒 `api_publish_state` 推送快照，同时广播给 SSE |
| **依赖项** | `axum 0.8`、`tokio`、`tokio-stream`、`reqwest`（无关）、前端播放引擎 |
| **配置项** | `config.api.{enabled, port, token, cors}`（§4.3） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | GPUI 没有 WebView 桥，**命令可直接在 Rust 内执行**，`api-command` / `api_reply` 这层可以省掉（改为在 axum handler 里直接调播放引擎的命令通道）。**但必须保留同一份路径、请求体、状态码与快照字段**，否则外部脚本会在两个分支间失效。建议把「快照结构」提取成 `bgm-core` 里的 `#[derive(Serialize)] struct ApiSnapshot`，两个分支共用，避免字段漂移。注意 `serve()` 用 `std::net::TcpListener` 绑定后再 `from_std`，这是为了「端口占用错误同步返回」；GPUI 若直接 `tokio::spawn` 绑定，需要额外把错误回传。 |
| **待确认** | 无 |

播放控制走 6 秒前端命令桥。字幕读写、分离结果、模型状态和任务查询由 Rust 直接处理。全局与逐任务 SSE 的行为见附录 C。

**HTTP 状态码语义（F14）**

| 码 | 触发条件 | 依据 |
| :--- | :--- | :--- |
| 202 | 字幕生成、分离或 ASR 加载已入队 | `api.rs`、`tasks.rs` |
| 400 | 参数缺失/非法（`key` / `seconds` / `volume` / `deviceId` / 无更新字段） | `api.rs:645,679,691,703,719` |
| 401 | token 缺失或错误 | `api.rs:395` |
| 403 | 浏览器 `Origin` 不被允许 | `api.rs:485` |
| 404 | 曲目不存在 / 未知 action | `api.ts:124,213` |
| 409 | 节目单时段内点播 / 输出路数已满 / 同类型同曲目任务在途 / 资源忙 | `api.ts:126,207` |
| 500 | 前端未预期异常 / 无法把命令发给界面 | `api.ts:107`、`api.rs:356` |
| 503 | 界面未就绪 / 设备授权失败 / 通道关闭 | `api.rs:342,365`、`api.ts:188` |
| 504 | 命令执行超时（6s） | `api.rs:369` |

### F15 ASR 模型管理

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 模型目录 / 扫描 / 下载 / 加载 / 释放 / WAV 识别 / 启动自动恢复 |
| **状态** | 已实现（仅 Qwen3-ASR 0.6B-int8 四件套） |
| **判断依据** | `models.rs:438` `load`、`:538` `transcribe_segments`、`:595` `download`、`:720` `download_files`；`commands.rs:366` `spawn_restore_last_model`、`:419` `auto_load_after_download` |
| **功能描述** | 「设置 → 模型」分组：显示目录、下载地址、扫到的 `.onnx` 列表、默认模型状态；下载固定 9 个文件；加载进 sherpa-onnx；可对 WAV 做识别。 |
| **入口/触发** | `models_info`、`choose_models_dir`、`reset_models_dir`、`open_models_dir`、`download_asr_model`、`cancel_asr_download`、`load_asr_model`、`unload_asr_model`、`transcribe_wav` |
| **关键代码位置** | `app/src/models.rs:28` `DEFAULT_MODEL_NAME`、`:60` `COMPONENTS`、`:77` `QWEN3_FILES`、`:256` `scan`、`:334` `default_model`、`:354` `ModelRuntime`、`:628` `create_recognizer`、`:653` `DownloadSource`、`:720` `download_files`、`:1058` `resample_to_asr`、`:1114` `missing_components`；`ui/src/stores/models.ts` |
| **输入** | `root: &Path`、`dir: &Path`（模型目录）、`url: String`（下载地址） |
| **输出** | `ModelsInfo { root, isDefaultRoot, defaultDownloadUrl, downloadUrl, defaultModel, models[], loaded, loadStatus, downloading, warnings[] }`；`ModelLoadStatus { phase, path, error }` |
| **核心流程** | **下载**：解析地址（ModelScope 仓库入口 / 直连 base）→ 逐个文件：已完整（大小等于清单）则跳过 → `.part` 流式写入 → 每 240ms 推一次进度 → 校验 `Content-Length` → rename → 完成后校验四件套 → 自动加载。<br>**加载**：CAS 抢 `loading` 标志 → canonical 越界校验 → 校验四件套 → `OfflineRecognizer::create` → 存 `Arc<Loaded>` → 写 `status`。<br>**识别**：取 `Arc<Loaded>`（锁外解码）→ `Wave::read` → 重采样到 16k → 解码 → 清洗文本。 |
| **依赖项** | `sherpa-onnx`（`OfflineRecognizer` + `OfflineQwen3ASRModelConfig`）、`reqwest`（blocking + rustls）、`dirs`、`bzip2`/`tar`（分离模型解压） |
| **配置项** | `prefs.{modelsDir, asrDownloadUrl, lastAsrModel}` |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | `models.rs` 的**业务逻辑几乎全部可复用**，只需替换两处：① `AppHandle` → 事件回调（`impl Fn(Progress)`）；② `app.path().app_config_dir()` 无关（模型目录来自 `dirs`）。`sherpa-onnx` crate 是纯 Rust 绑定，GPUI 可直接用。**注意**：`Qwen3` 的 `tokenizer` 目录必须完整（9 个文件里 6 个在 `tokenizer/` 下），只校验四件套的「目录存在」不等于可加载，真实加载失败在 `recognizer.create` 才暴露——建议 GPUI 保持同样的「先校验存在、再交给引擎报错」的分层。 |
| **待确认** | 无 |

**ASR 规则（F15）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 默认目录 | `dirs::download_dir()/bgm-models`，无下载目录 → `~/Downloads/bgm-models` | `models.rs:231-236` |
| 扫描 | 深度 ≤4，跳过符号链接、隐藏目录、`separation/`；同目录 `.onnx` 归为一个模型；根目录单文件 → `id = "file:<name>"`、`loadable=false` | `models.rs:256-331,1131-1165` |
| 可加载判定 | `conv_frontend.onnx` + `encoder.int8.onnx` + `decoder.int8.onnx` + `tokenizer/` 目录全部存在 | `models.rs:1114` |
| 下载清单 | 固定 9 个文件（3 个 onnx + 6 个 tokenizer 文件），总大小写死在 `QWEN3_FILES` | `models.rs:77-123` |
| 完整性 | 优先 `Content-Length`；缺失时回退清单字节数；`received == 0` 或 `received < expected` → 失败并删 `.part` | `models.rs:796,856-868` |
| 取消 | `cancelled` 原子标志，逐块检查；取消后删 `.part` | `models.rs:812-855` |
| 并发 | 同一时刻只允许 1 个下载、1 个加载（CAS） | `models.rs:439,598` |
| 下载后自动加载 | 已加载同一路径 → 直接广播 success；已有加载任务 → 不抢占；否则加载并推送状态 | `commands.rs:419-474` |
| 启动恢复 | 读 `prefs.lastAsrModel` → 拼路径 → 越界/不完整则跳过 → 已有加载任务则跳过 → 后台加载并推送状态 | `commands.rs:366-416` |
| 释放 | `unload()` + `lastAsrModel = None`（下次启动不再自动恢复） | `commands.rs:529-543` |
| 线程数 | `min(available_parallelism, 4)` | `models.rs:1196` |
| 采样率 | 恒定 16k；非 16k 输入先做带限重采样（8 taps 窗） | `models.rs:1058-1096` |
| 分片 | 目标 10s，切点在 ±2.5s 内找 20ms 能量最低帧；单片 ≥3s；窗口钳到 2–60s | `models.rs:50-57,565` |
| 文本清洗 | 去控制标记（`strip_meta_instructions`）+ 折叠空白 | `models.rs:911-957` |

### F16 人声分离引擎

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 原始音频 → 44.1kHz 双声道 → 双引擎分离 → `vocals.wav` + `accompaniment.wav` |
| **状态** | 已实现（依赖随包模型或用户自备模型 + ffmpeg） |
| **判断依据** | `separation.rs` 1616 行完整实现：模型目录、预检、解码、worker 子进程、并行/串行、WAV 写出、记录、迁移、清理 |
| **功能描述** | 字幕生成的**强制前置**。人声由用户选择的模型生成，伴奏固定由 `UVR-MDX-NET-Inst_HQ_3` 生成；两者都落盘并校验后才允许进入识别。 |
| **入口/触发** | `separation_models`（列模型）、`generate_subtitles`（内部调 `separate`）、`reveal_subtitle_audio`、`delete_subtitle`（清理） |
| **关键代码位置** | `app/src/separation.rs:29` `CUSTOM_DIR_NAME`、`:31-43` 常量、`:48` `WORKER_ARG`、`:56` `StemTarget`、`:405` `custom_specs`、`:480` `specs`、`:531` `catalog`、`:603` `extract_archive`、`:846` `scan_metadata`、`:914` `preflight_custom`、`:958` `decode_input`、`:1071` `separation_dir`、`:1088` `migrate`、`:1198` `separate`、`:1400` `worker_main`、`:1500` `execute_job`、`:1609` `cleanup` |
| **输入** | `song: &Song`、`model_id: String`、`models_root: &Path`、`target_override: Option<&str>` |
| **输出** | `SeparationInfo { modelId, modelName, engine, target, accompanimentModelName, vocalsPath, accompanimentPath, accompanimentKind, sampleRate }` |
| **核心流程** | ① 解析模型 spec（随包 spleeter 首次解压 `.tar.bz2`；自定义 UVR 需 `preflight_custom`）；② 禁止用伴奏模型当人声模型；③ 解码源音频为 44.1k 双声道 float32（ffmpeg → 临时 `.f32`）；④ 写 `job.json` 到临时目录；⑤ 以 `--separation-worker <job>` 启动**自身可执行文件**子进程；⑥ worker 内：常规时长并行跑两个人声/伴奏引擎（线程按核心均分），>10 分钟串行；⑦ 按 `stem_index` 取正确声道 → 写 16bit WAV；⑧ 主进程校验 `.part` 大小 >44 字节 → rename 为正式文件 → 写 `separation.json` |
| **依赖项** | `sherpa-onnx-sys`（直接绑定 `OfflineSourceSeparation` C API）、`ffmpeg`、`bzip2` + `tar`、`tauri::path::BaseDirectory`（随包资源定位） |
| **配置项** | 模型目录（`prefs.modelsDir`）；自定义模型放 `<models>/separation/`；`prefs.separationModel`（上次选择，前端维护） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | **整体可复用**，包括 C API 绑定与 worker 子进程机制。需要替换的只有 `bundled_dir`（`app.path().resolve(..., BaseDirectory::Resource)`）→ gpui 侧改为「exe 同级 / 资源目录」的路径解析。**worker 机制务必保留**：sherpa-onnx 对不兼容模型会 `_Exit(-1)`，进程内无法捕获；隔离到子进程后主进程仍能给出可读错误。`manifest` 里的随包模型（`UVR-*-*.onnx`、`.tar.bz2`）需要 GPUI 的打包流程一并带上，否则「随包模型」条目会显示不可用。 |
| **待确认** | ① GPUI 打包是否随附 3 个分离模型（体积较大）；② 自定义模型 `target_known=false` 时的用户确认 UI 在 GPUI 如何呈现 |

**分离规则（F16）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 模型列表 | 人声列表只保留 `target == Vocals` 的条目；伴奏固定 `uvr-inst-hq-3`，不出现在人声列表 | `separation.rs:531-544` |
| 自定义 Spleeter | `separation/<dir>/` 内同时有 `vocals*.onnx` 与 `accompaniment*.onnx` | `separation.rs:403-444` |
| 自定义 UVR | `separation/` 根目录的 `.onnx`；文件名含 `Inst` 判为乐器模型 | `separation.rs:392-403` |
| 角色确认 | 自定义 UVR `target_known=false`，前端必须让用户确认主输出；Spleeter 固定人声 | `separation.rs:465,1238-1240` |
| 预检 | 自定义模型校验 `model_type`（`spleeter`/`UVR`）、`stems==2`、UVR 的 9 项必需元数据 | `separation.rs:830-955` |
| 输入格式 | ffmpeg 解码为 44.1k / 2ch / f32；任何源容器都支持（受 ffmpeg 支持范围限制） | `separation.rs:958-995` |
| 并行阈值 | `samples > 10min * 44100` → 串行；否则两个引擎并行 | `separation.rs:1544-1560` |
| 线程分配 | `(cores / engines).clamp(1, 4)` | `separation.rs:825-827` |
| 重生成保护 | 先写 `.part`，成功后才 rename 覆盖正式文件；失败清理 `.part`，保留上一份交付物 | `separation.rs:1248-1351` |
| 交付物目录 | `<config>/separations/<可读前缀>-<FNV1a 哈希>/{vocals.wav, accompaniment.wav, separation.json, subtitles.srt, subtitles.lrc}` | `separation.rs:1071`、`subtitles.rs:588-592` |
| key 迁移 | 改名/移动时 `separation::migrate` 搬目录并改写记录内路径；失败回滚 | `separation.rs:1088`、`subtitles.rs:329-342` |
| 删除 | `delete_subtitle` → `separation::cleanup` 整目录删除 | `subtitles.rs:258` |

### F17 字幕生成与编辑

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 分离 → 归一化 → 分片 ASR → SRT/LRC 落盘 → 记录检索 → 分段/原文编辑 → 导出 |
| **状态** | 已实现 |
| **判断依据** | `subtitles.rs` 47KB 完整实现生成、持久化、迁移、解析、校验、写回；16 项单测；`SubtitlesPage.vue` 1538 行 |
| **功能描述** | 为曲库曲目生成字幕并持久化，供预览、导出与桌面歌词使用。生成前必须由用户确认分离模型，未确认直接拒绝，**不会回退到原始混合音频**。 |
| **入口/触发** | 曲库右键/「更多」→「字幕生成」；侧栏「字幕生成」页搜索加入；`generate_subtitles`、`export_subtitle`、`subtitle_for_track`、`list_subtitles`、`delete_subtitle`、`update_subtitle`、`update_subtitle_text` |
| **关键代码位置** | `app/src/subtitles.rs:34` `SUBTITLE_PROGRESS_EVENT`、`:93` `StoredSubtitle`、`:171` `subtitle_stem`、`:195` `load_stored`、`:208` `list_stored`、`:241` `delete_stored`、`:315` `migrate_keys`、`:462` `normalize_to_wav`、`:503` `generate`、`:707` `normalize_segments`、`:747` `update_stored`、`:792` `parse_srt`、`:872` `parse_lrc`；`ui/src/stores/subtitles.ts`（741 行）；`ui/src/lib/subtitle-edit.ts`（267 行）；`ui/src/components/SubtitlePage.vue` |
| **输入** | `key: String`、`separation_model: String`、`separation_target: Option<String>`；编辑时为 `segments` 或 `format + text` |
| **输出** | `SubtitleResult { key, title, sourcePath, sourceFormat, separation, duration, segments[], srt, lrc, savedPath, srtPath, lrcPath }`；`StoredSubtitle`（§4.7） |
| **核心流程** | ① 校验源文件存在、格式受支持、模型已加载、分离模型非空；② `separate` → 校验两个人声/伴奏文件 >44 字节；③ `normalize_to_wav(vocals)` → 16k 单声道（**只对人声**）；④ `transcribe_segments` → 分片识别 + 每片进度；⑤ 删除临时 WAV；⑥ 格式化 SRT/LRC → 写入分离目录 `subtitles.srt/.lrc`；⑦ `save_stored` 写 `subtitles/<stem>.json`；⑧ 推 `done` 事件 |
| **依赖项** | F15（模型）、F16（分离）、`ffmpeg`、`bgm_core::export::safe_file_name` |
| **配置项** | `prefs.subtitleFormat`（预览偏好）、`prefs.separationModel`（上次模型） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | `subtitles.rs` 几乎可整体复用：只需把 `AppHandle` 换成 `(dirs, event_sink)`，把 `app.path().app_config_dir()` 换成 `dirs::config_dir().join(identifier)`。**key → 文件名的哈希必须完全一致**（FNV-1a 64 + 可读前缀 48 字符），否则两个分支会写出两套文件、互相读不到。编辑校验规则（5000 条 / 2000 字 / 时间合法性 / 换行折叠）与解析器（SRT 缺序号、`MM:SS,mmm` 简写、LRC 白名单元数据、末句按 `durationMs` 推导）也要逐条保留——这些是「不静默丢内容」的保证。 |
| **待确认** | 无 |

**字幕规则（F17）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 生成前置 | 未加载 ASR 模型 / 未选分离模型 / 源文件不存在 / 格式不支持 → 直接报错 | `subtitles.rs:511-527` |
| 只识别人声 | 归一化与识别只读 `separation.vocals_path`；伴奏与原始混合音频不得参与 | `subtitles.rs:556-567` |
| 临时文件 | 归一化 WAV 在识别结束后删除；生成中断残留由 `delete_stored` 清理 | `subtitles.rs:582,251-256` |
| 记录路径 | `<config>/subtitles/<prefix>-<hash16>.json`；`wav/` 子目录存临时归一化音频 | `subtitles.rs:160-190` |
| 记录字段 | `key/title/segments/createdAtMs/updatedAtMs?/fileName?/durationMs?/srt/lrc/separationModelName?/accompanimentModelName?/vocalsPath?/accompanimentPath?/accompanimentKind?/srtPath?/lrcPath?` | `subtitles.rs:93-130` |
| 读取重建 | `load_stored` / `list_stored` 总是用当前规则重建 `srt` / `lrc`，避免旧文件字段缺失 | `subtitles.rs:199-200,227-228` |
| 记录列表 | 按 `createdAtMs` 倒序；**曲目已移除的记录仍保留**（孤立记录可预览/导出/删除） | `subtitles.rs:204-237` |
| 迁移 | 只处理 `from != to`；先搬分离目录再改 JSON；JSON 失败回滚目录 | `subtitles.rs:315-345` |
| 编辑校验 | 非空、≤5000 条、时间非负、`end > start`、文本折叠换行、≤2000 字（**超限报错不截断**）、按起点排序 | `subtitles.rs:707-741` |
| 编辑落盘 | 记录必须已存在；`createdAtMs` 不变；写 `updatedAtMs`；临时文件 + rename | `subtitles.rs:747-790` |
| SRT 解析 | 兼容缺序号、`MM:SS,mmm` 简写、多行文本；含 `-->` 但解析失败 → **报行号** | `subtitles.rs:792-870` |
| LRC 解析 | 只忽略白名单元数据标签（`ar/al/ti/au/by/offset/re/ve/length/tool/kana/language`）；`[offset:]` 不参与计时；一行多时间戳展开；末句结束 = 下一条起点 → 记录时长 → 曲库时长 → `start+5s` | `subtitles.rs:872-990` |
| 导出 | 默认文件名 `safe_file_name(title).ext`；默认目录为系统下载目录；内容取 `stored.srt/lrc` | `commands.rs:623-666` |

### F18 桌面歌词窗口

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 独立透明置顶歌词窗 + 主窗播放状态同步 + 样式/锁定/尺寸设置 |
| **状态** | 已实现 |
| **判断依据** | `lyrics.rs` 27KB 实现窗口生命周期、置顶巡检、锁定穿透、热区轮询、设置悬浮窗；`lyrics.ts` 585 行实现渲染；`lyrics-style.ts` 样式持久化 |
| **功能描述** | 模型加载成功后才可开启。开启后创建独立透明置顶窗口，按播放进度高亮当前字幕行，显示上一行/下一行；暂停冻结、切歌切换、无字幕提示。 |
| **入口/触发** | 播放器「桌面歌词」开关 → `set_desktop_lyrics_enabled`；`sync_desktop_lyrics`（每秒 + 跳变时）；`toggle_desktop_lyrics_settings`；窗口自身 3 秒巡检 `keep_desktop_lyrics_on_top` |
| **关键代码位置** | `app/src/lyrics.rs:25-53` 常量、`:58` `LyricLine`、`:67` `LyricsSnapshot`、`:166` `LyricsState`、`:237` `set_enabled`、`:272` `toggle_settings`、`:379` `set_lock`、`:449` `start_lock_monitor`、`:526` `sync_snapshot`、`:541` `enforce_top_most`、`:580` `resize`、`:603` `place_at_bottom`、`:633` `ensure_window`；`ui/src/stores/desktop-lyrics.ts`；`ui/src/lyrics.ts`（585 行）；`ui/src/lyrics-settings.ts`（259 行）；`ui/src/lib/lyrics-style.ts` |
| **输入** | `enabled: bool`；`LyricsSnapshot { trackKey, title, playing, position, rate, lines[], hasLyrics }` |
| **输出** | 窗口创建/关闭；事件 `desktop-lyrics-control/data/size/hover/settings-state`；`LyricsBootstrap { enabled, snapshot }` |
| **核心流程** | ① 前端 `installDesktopLyrics` 拉 bootstrap + 订阅 control；② 开关 → `set_enabled`（异步命令，`spawn_blocking`）→ `ensure_window` 建窗（透明、置顶、无边框、跳过任务栏）；③ 主窗每秒 `sync_desktop_lyrics`（切歌/播放态/倍速变化立即，进度跳变 >0.25s 立即补发）；④ Rust 存快照并广播 `desktop-lyrics-data`；⑤ 歌词窗按 `position + rate` 本地推进渲染（50ms 一帧）；⑥ 锁定 → `set_ignore_cursor_events(true)` + 热区轮询，鼠标进入热区才显示解锁入口 |
| **依赖项** | Tauri 多窗口（`WebviewWindowBuilder`）、`capabilities/default.json` 的 windows 列表、字幕记录（F17） |
| **配置项** | `prefs.desktopLyricsEnabled`；样式存歌词窗口 `localStorage`（`tfy-bgm:desktop-lyrics-style`，version 4） |
| **异常/边界** | 见下表 |
| **gpui 实现建议** | GPUI 支持多窗口，但**透明 + 始终置顶 + 鼠标穿透**需要平台特定处理（Windows 用 `WS_EX_LAYERED` + `WS_EX_TRANSPARENT` + `SetWindowPos(HWND_TOPMOST)`；Linux X11/Wayland 差异大；macOS `NSWindow.level = .screenSaver` + `ignoresMouseEvents`）。建议：① 先做「不透明置顶窗」验证渲染与同步；② 再做透明与穿透；③ 锁定态的鼠标热区轮询在 GPUI 里可用 `cx.on_mouse_event` 或平台光标查询，不需要像主线那样起线程。**同步频率**（每秒快照 + 本地按 rate 推进）必须保留，否则跨窗口会有可见滞后。 |
| **待确认** | ① GPUI 在 Wayland 下能否稳定实现「始终置顶 + 穿透」（协议限制）；② 多显示器 DPI 缩放下的尺寸/位置钳制策略 |

**歌词规则（F18）**

| 规则 | 行为 | 依据 |
| :--- | :--- | :--- |
| 开关前置 | `modelsInfo.loaded` 为空时开关禁用并提示 | `desktop-lyrics.ts:28,116` |
| 窗口尺寸 | 默认 760×120；范围 420–1600 × 120–900；底部留白 32px | `lyrics.rs:46-53` |
| 设置窗 | 380×460，贴歌词条上方 8px，关闭歌词窗时一并关闭 | `lyrics.rs:39-43,265` |
| 置顶 | 创建后二次确认；窗口失焦重新声明；窗口自身每 3 秒巡检 | `lyrics.rs:541`、`lyrics.ts:581` |
| 锁定 | 锁定后 `set_ignore_cursor_events(true)`；热区由歌词窗上报；鼠标进入才发 `hover=true` | `lyrics.rs:379-524` |
| 已唱提前量 | `leadSeconds ∈ {0,1,2}`，默认 1；渲染时间轴整体前移 | `lyrics-style.ts:21-24,34` |
| 样式版本 | `LYRICS_STYLE_VERSION=4`；版本不匹配只重置高度与预设高亮色，**保留字体/字号/背景** | `lyrics-style.ts:17,96-113` |
| 关闭窗口 | 用户点 × / Alt+F4 → `CloseRequested`：置 `enabled=false`、广播、关闭设置窗，**并落盘 `desktopLyricsEnabled=false`**；程序退出导致的关闭（`quitting`）不落盘 | `lyrics.rs:688-699` |
| 窗口销毁 | `Destroyed`：只复位内存状态与广播，不落盘（用于程序主动关闭的收尾） | `lyrics.rs:700-708` |
| 死锁防护 | `set_desktop_lyrics_enabled` 必须是 `async` + `spawn_blocking`（`WebviewWindowBuilder::build` 会阻塞等待主线程） | `commands.rs:741-763` 注释 |

---

> 原生 Wayland 使用普通透明歌词窗口。位置由合成器控制，跳过贴底和悬浮设置窗的主动定位。锁定保持窗口可交互，页面悬停提供解锁入口。置顶取决于合成器，KDE Wayland 可能忽略置顶请求。XWayland 走普通窗口路径。

### F19 本地音频索引、以声搜声与查重

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 本地音频索引、指纹匹配与重复项管理 |
| **状态** | 已实现，分析依赖 ffmpeg |
| **判断依据** | `audio_index.rs` 实现分析、指纹匹配与增量计划，`state.rs` 编排后台同步 |
| **功能描述** | 为当前曲库建立本地索引，供歌曲详情、以声搜声和重复项管理共用 |
| **入口/触发** | 侧栏「以声搜声」和「音频指纹去重」页面中建立或关闭索引，启用后随启动和曲库变化后台同步 |
| **关键代码位置** | `src-tauri/core/src/audio_index.rs`、`src-tauri/src/state.rs::queue_audio_index_update`、`ui/src/composables/useAudioIndex.ts`、`ui/src/components/audio-tools/` |
| **输入** | 曲库根目录、音频文件、查询文件或麦克风录音 |
| **输出** | `AudioIndexStatus`、`AudioIndexInfo`、`DuplicateReport`、`SoundSearchResult` 与软删除结果 `LibraryMutation` |
| **核心流程** | 首次启用需确认。按曲库版本生成增量计划，锁外分析，提交前校验版本，发布 `Arc` 快照。同一后台工作器合并新的同步请求。分析使用 11 025 Hz PCM，生成波形和指纹。查重阈值 0.85，查询阈值 0.35 |
| **依赖项** | ffmpeg、桌面 IPC、文件选择器，麦克风路径使用 `MediaRecorder` |
| **配置项** | `<config>/audio-index.json`，版本 2，按曲库根目录判断索引是否可用 |
| **异常/边界** | 录音 IPC 接受 ≤2 MiB 原始字节，查询上限默认 10、夹到 1–50。关闭需确认并删除索引。查重只将明确选中的合法索引路径移到回收站，也能处理普通曲库列表未展示的同主干副本 |
| **GPUI 实现建议** | 复用 core 的分析和匹配，以原生录音与文件对话框接入，保留增量计划、过期工作丢弃、确认与回收站语义 |
| **待确认** | 目标平台的录音设备和实际编码格式需实机验证 |

### F20 公开歌单迁移

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 将公开歌单转换为可复制或保存的曲目文本 |
| **状态** | 已实现，依赖第三方公开数据 |
| **判断依据** | `playlist_migration` 实现来源识别、请求、分页及结果校验，前端实现多来源与文本预览 |
| **功能描述** | 读取网易云音乐、QQ 音乐、汽水音乐的公开分享链接，保留平台曲目顺序、曲名与艺人 |
| **入口/触发** | 侧栏「歌单迁移」→ 添加分享链接 → 预览、复制或保存 TXT |
| **关键代码位置** | `src-tauri/src/playlist_migration/{mod,source}.rs`、`src-tauri/src/commands.rs::parse_playlist_migration_source`、`ui/src/composables/usePlaylistMigration.ts` |
| **输入** | `url: string`，平台允许的 HTTPS 歌单链接 |
| **输出** | `Playlist { platform, title, tracks: [{ title, artists }], expectedTotal, warning }`，平台值为 `netease`、`qq`、`qishui` |
| **核心流程** | 识别来源 → 校验短链跳转 → 拉取公开数据及必要分页 → 输出可用曲目与完整性提示 → 复制或将预览原样写为 UTF-8 TXT |
| **依赖项** | `reqwest`、上游公开接口或分享页、剪贴板与保存对话框 |
| **配置项** | 无持久配置。通过桌面 IPC 调用 |
| **异常/边界** | 同时最多读取 4 个来源，单请求 10 秒、单来源 45 秒。短链最多跳转 4 次且仍须符合平台规则。部分成功时返回已读曲目与警告，无可用曲目且发生错误时返回失败 |
| **GPUI 实现建议** | 复用来源白名单、重定向校验、响应大小限制及分页逻辑，保留部分成功状态与预览文本原样导出 |
| **待确认** | 上游平台改版后需回归真实分享链接 |

### F21 TTS 文本转语音

| 项目 | 内容 |
| :--- | :--- |
| **功能名称** | 本地脚本配音、参考音色与 WAV 导出 |
| **状态** | 已实现 |
| **判断依据** | `tts.rs` 管理模型与合成，`tts_script.rs` 解析脚本，`speech_audio.rs` 处理参考音频 |
| **功能描述** | Kokoro 选择模型内置说话人，ZipVoice 选择内置或用户参考录音并填写对应逐字稿 |
| **入口/触发** | 侧栏「文生语音」工作台与「设置 → 模型」中的 TTS 模型管理 |
| **关键代码位置** | `src-tauri/src/{tts,tts_models,tts_script,speech_audio}.rs`、`ui/src/components/tts/`、`ui/src/stores/tts.ts` |
| **输入** | TXT/SRT/LRC 或编辑后的 `TtsSegment { startMs, endMs, text }[]`、模型、音色、可选参考音频与文字 |
| **输出** | `SynthesisResult { name, path, sampleRate, duration }`，保留模型原生采样率的 WAV，可预听并另存 |
| **核心流程** | 纯文本合并段落并按句界与长度分段，长段优先在标点等边界切分，分段上限 180 字符。带时间轴脚本按校验后的时隙排布。安装和合成按 UUID 报进度与取消，合成时按需加载所选引擎 |
| **依赖项** | `sherpa-onnx`、模型资源、ffmpeg（参考音频处理） |
| **配置项** | `prefs.ttsDownloadUrl`，模型位于 `<models>/tts/`。开发资源 `models/voices/`，包内 `voices/`，可由同目录 `prompts.json` 匹配逐字稿 |
| **异常/边界** | ZipVoice 参考音频归一化到 16 kHz，要求 3–10 秒与对应文本。自定义参考原始上传上限 25 MiB。合成最长 30 分钟，结果存应用缓存，清理限额为 8 项及 512 MiB |
| **GPUI 实现建议** | 复用脚本、参考音频与模型逻辑，用原生线程、进度通道、播放器和保存对话框替换 Tauri glue。保留合成取消、原生采样率与阶段错误 |
| **待确认** | 各目标平台随包参考音色及模型文件需实机验收 |

## 3. 跨模块核心流程

> 本节描述「一个动作跨了几个模块、按什么顺序、失败点在哪」。单看功能点容易漏掉这些耦合。

### 3.1 启动流程（Rust + 前端）

Linux 先初始化壁纸媒体服务，再恢复偏好。应用注册 `AppState` 后为已启用的音频索引排入后台同步。ASR 继续按 `lastAsrModel` 后台恢复，TTS 在手动加载或合成时创建引擎。入口见 `src-tauri/src/lib.rs::run`。

```
main()
 └─ separation_worker_main()  ← 若 argv 含 --separation-worker，执行分离并退出
 └─ run()
     ├─ settings::startup_gpu_acceleration()   ← 建窗前读 prefs.gpuAcceleration
     ├─ desktop::apply_gpu_acceleration(&mut context, enabled)
     ├─ Builder: 权限回调（麦克风/自动播放 → Allow）
     ├─ setup()
     │   ├─ AppState::initialize()
     │   │   ├─ Settings::load()  （config → prefs → categories → schedule → loops）
     │   │   ├─ wallpapers::prime_custom()      ← 首屏壁纸白名单
     │   │   ├─ library_root::resolve() + ensure_category_dirs()
     │   │   ├─ scan_library_cached()
     │   │   ├─ allow_audio_scope(root)         ← asset 协议授权
     │   │   └─ models::ensure_root()
     │   ├─ set_title(config.brand.windowTitle)
     │   ├─ start_watching()                    ← 失败只记日志
     │   ├─ manage(AppState) / manage(LyricsState) / manage(DesktopState)
     │   ├─ desktop::install(autostart, minimizeToTray)
     │   ├─ ApiBridge::new() → manage → serve()
     │   └─ commands::spawn_restore_last_model()
     └─ run(context)
```

前端 `App.vue`：

```
setup 顶层：installScheduler() / applyDocumentCopy() / loadBuiltins()
           installOutputs() / installSystemIntegration() / installApiBridge()
onMounted：bootstrap() → invoke(bootstrap) → applyBootstrap()
           installDesktopLyrics() / installSubtitles()
           listen(library-changed)
           appWindow.onFocusChanged()
```

**关键顺序约束**

| 约束 | 原因 |
| :--- | :--- |
| `AppState` 必须在 `ApiBridge::serve()` 之前 `manage` | 首批 HTTP 请求可能立刻抵达，找不到状态会 503 |
| `DesktopState` 必须在 `desktop::install()` 之前 `manage` | 窗口事件回调里读 `minimize_to_tray` |
| GPU 开关必须在 `Builder::run` 之前应用 | WebView 渲染后端只能建窗前决定 |
| 前端 `installApiBridge` 早于首帧播放 | 否则 HTTP 命令会因 `ready=false` 全部 503 |
| 前端 `installOutputs` 早于用户点击播放 | 否则首次播放时路由尚未建立，只有系统默认出声 |

### 3.2 曲库写操作与 key 迁移（最关键的跨模块契约）

```
前端 actions.ts                      state.rs                        core/ops.rs
  invoke(update_track) ─────────────▶ update_track()
                                       ├─ ops::update_song() ───────▶ 校验 + rename
                                       ├─ finish_mutation(changes,…)
                                       │   ├─ sync_track_metadata()
                                       │   │   ├─ prefs.favorites 重映射 + 去删除
                                       │   │   └─ loops.apply_key_changes()
                                       │   ├─ refresh_library()
                                       │   └─ subtitles::migrate_keys()
                                       │        ├─ separation::migrate()  （搬目录）
                                       │        └─ migrate_one()           （改 JSON）
                                       └─ 返回 LibraryMutation
  applyMutation(result) ◀─────────────┘
    ├─ queue / currentKey / favorites / selected 重映射
    ├─ loopPoints 重映射
    ├─ replaceLibrary(snapshot)
    └─ applySubtitleKeyChanges(remap)
```

**必须同时迁移的 5 处引用**

| 引用 | 存储位置 | 迁移函数 |
| :--- | :--- | :--- |
| 收藏 | `prefs.json.favorites` | `state.rs::sync_track_metadata` |
| 循环点 | `loops.json.points` | `loops.rs::apply_key_changes` |
| 当前曲目 / 队列指针 | 前端内存 | `library.ts::applyMutation` |
| 字幕记录（含分离目录与路径） | `subtitles/*.json` + `separations/*` | `subtitles.rs::migrate_keys` |
| 前端字幕缓存 / 歌词缓存 | 前端内存 | `applySubtitleKeyChanges` / `invalidateDesktopLyrics` |

**失败语义**：任何一步迁移失败只产生 warning 并并入 `library.warnings`，**不阻断**主操作。收藏迁移失败会保留旧 key（可能变成孤立收藏），循环点迁移失败同理——这是有意的「宁可留脏数据也不丢用户配置」。

### 3.3 外部文件变化（watch → UI）

```
notify 事件 ──▶ is_relevant 过滤 ──▶ mpsc ──▶ 700ms 防抖
                                              │
                                    AppState::handle_watch_event()
                                      ├─ ops_lock.try_lock() 失败 → 放弃
                                      ├─ scan_library_cached(shared MetaCache)
                                      ├─ same_content? → None（不推送）
                                      ├─ diff_songs() → added/removed/moved
                                      ├─ sync_track_metadata(moved, removed)
                                      ├─ subtitles::migrate_keys(moved)
                                      └─ runtime = next; emit library-changed
                                              │
前端 applyExternalChange()
  ├─ scannedAtMs 更旧 → 丢弃
  ├─ 重映射 queue/currentKey/favorites/selected/loopPoints
  ├─ replaceLibrary()
  ├─ applySubtitleKeyChanges()
  └─ toast「曲库已自动更新：新增 X，移除 Y，移动 Z」
```

**为什么需要 `scannedAtMs` 比较**：用户点「重命名」后，命令返回值与 watch 事件可能几乎同时到达；事件里的快照可能比命令返回值更旧。只按到达顺序应用会让界面「回退」到旧列表。前端用时间戳丢弃旧事件，后端用 `try_lock` 丢弃写操作期间的扫描，两者共同保证最终一致。

### 3.4 切歌（含交叉淡入淡出与失败跳过）

```
用户点击 / nextTrack / 节目单 / HTTP play
  └─ playSong(song, queue, forcePlay, autoplay, fadeIn)
      ├─ syncQueueForPlayback()            ← 队列与指针
      ├─ 同一 key 且已有 buffer → toggle / rewind
      ├─ state.currentKey / currentSong 先更新（UI 立刻显示新曲）
      ├─ recordPlay(key)                   ← 智能随机历史
      └─ loadSongSource()
          ├─ durationMs > 30min → 失败跳过
          ├─ loadSource(path, {crossfade, fadeIn, loop})
          │   ├─ keepPlaying? → 旧声部继续响，解码完成后 crossfadeTo
          │   └─ 否则停旧源 → 解码 → startPlayback(fadeIn)
          └─ failed → unloadSource() + failCurrent()
                        ├─ queue>1 且 streak ≤ maxAutoSkips → toast + nextTrack
                        └─ 否则 playbackError 并停
```

**自然播完**：`ended` → `handleEnded` → ① 节目单 handler 优先（只循环当前到点歌）→ ② `repeat-one` 回开头 + 淡入 → ③ `nextTrack(true, crossfade)`（新曲补淡入）。

**竞态防护**

| 机制 | 作用 |
| :--- | :--- |
| `loadToken` | 后一次 `loadSource` 让前一次返回 `superseded`，避免旧解码结果覆盖新曲 |
| `sourceToken` | 作废被取代的 `onended` 回调，避免误触发「播完切歌」 |
| `outgoing` 单槽 | 连续快速切歌时先停上一轮淡出声部，避免三层叠音 |
| `failureStreak` | 成功 `play` 时清零；连续失败达上限停住，避免无限切歌 |
| `publishedPosition/At/Rate` | 桌面歌词判断「进度是跳变还是正常推进」 |

### 3.5 节目单到点接管

```
installScheduler() → setEndedHandler(handleScheduleEnded) + tick() + setInterval(1s)
tick():
  ├─ 收集候选人（enabled 且 at ∈ [now-2min, now] 且未触发过）
  ├─ 有候选：按 at 倒序取最晚一条 → trigger(entry, song)
  │            └─ applyScheduleEntry(volume → queue → playSong)
  ├─ 无候选且 (gap>90s 或 从未激活)：对齐当前节目里最晚已开始歌
  └─ 清理：无当前节目 → activeScheduleId=null；triggered>4000 → clear
```

**与播放器的接口**：`setEndedHandler` 是一个「返回 bool 的覆盖钩子」——节目单时段内返回 true，播放器不再自动下一首。这是主线里唯一一处「业务层反向控制播放器」的设计，GPUI 迁移时建议改成显式枚举（见 F08 建议）。

### 3.6 导出（单曲 / 批量）

```
export_track(key):
  ├─ state.song(key) → 不存在报错
  ├─ export_extension(state, fileName)   ← 探测 ffmpeg（每次开对话框都探一次）
  ├─ 保存对话框（默认目录 = 系统下载）
  └─ export_audio(source, target, settings)
      └─ export_with_ffmpeg()
          ├─ normalize=false → copy_same_format(warning="未开启…")
          ├─ ffmpeg=None     → copy_same_format(warning="未检测到…")
          ├─ target 非 mp3   → 同格式则 copy(warning) / 异格式则 FormatMismatch
          └─ normalize_file() 两遍 loudnorm
              ├─ 成功 → ExportOutcome{normalized:true}
              └─ 失败 → 删目标 → 同格式 copy(warning) / 异格式报错

export_tracks(keys):
  └─ 目录对话框 → state.export_tracks → ops::export_songs
      ├─ ffmpeg 只探测一次（同时作为 ext 判定依据）
      ├─ 逐首 unique_path + export_with_ffmpeg
      └─ 汇总 notice（全归一化 / 部分 / 未归一化 + 首条 warning）
```

### 3.7 字幕生成（强制链路）

```
generate_subtitles(key, separation_model, separation_target)
  ├─ state.song(key) → Song
  ├─ spawn_blocking(subtitles::generate)
  │   ├─ 校验：源存在 / 扩展名 / model.loaded / separation_model 非空
  │   ├─ emit(separating)
  │   ├─ separation::separate()
  │   │   ├─ resolve_spec（spleeter 首次解压 / 自定义预检）
  │   │   ├─ resolve_accompaniment（固定 UVR Inst_HQ_3）
  │   │   ├─ decode_input() → 44.1k/2ch/f32
  │   │   ├─ 写 job.json → 启动 worker 子进程
  │   │   ├─ worker: 并行/串行双引擎 → 写 .part
  │   │   ├─ 校验 >44 字节 → rename → separation.json
  │   │   └─ 返回 SeparationInfo
  │   ├─ 二次校验 vocals/accompaniment 存在且 >44 字节
  │   ├─ emit(normalizing) → normalize_to_wav(vocals) → 16k mono
  │   ├─ emit(transcribing) → transcribe_segments() → 每片 emit
  │   ├─ 删除临时 WAV
  │   ├─ format_srt / format_lrc → 写 <separations>/subtitles.{srt,lrc}
  │   └─ save_stored() → <config>/subtitles/<hash>.json
  └─ 失败 → emit(error)，命令返回 Err
```

**不可跳过的约束**：① 必须先分离再识别；② 只识别人声轨；③ 分离交付物必须真实落盘。这三点在 `generate` 里有显式校验，GPUI 若重写流程必须保留同样的「硬失败」语义，否则会把「伴奏当人声」识别出一堆噪声文本。

### 3.8 桌面歌词同步

```
主窗（desktop-lyrics.ts）                        Rust（lyrics.rs）              歌词窗（lyrics.ts）
  watch(playing, currentKey, rate) ─┐
  watch(currentTime 跳变 >0.25s) ───┼─ publish() ──▶ sync_desktop_lyrics()
  setInterval(1s) ──────────────────┘                 ├─ state.set_snapshot()
                                                      └─ emit(desktop-lyrics-data) ──▶ 渲染（50ms/帧）
  开关 ──▶ set_desktop_lyrics_enabled() ──▶ set_enabled() ──▶ ensure_window()
                                                          └─ emit(control) ────────▶ 同步开关
  设置窗 ──▶ toggle_settings() ──▶ 380×460 悬浮窗 ──▶ localStorage 样式 ──▶ emit(style) ──▶ 歌词窗重算
```

**性能要点**：主窗每秒推一次完整 `segments`（可能上千条）。主线接受这个开销（字幕通常 <300 条），GPUI 若遇到长字幕，建议改为「切歌时推全量 + 进度只推 position」。

### 3.9 HTTP 控制回路

下图为播放控制等前端 action 的路径。字幕、分离和 ASR 加载由 Rust 在校验后入串行队列并返回 `202`，客户端用任务查询或 SSE 获取进度及结果。

```
外部脚本 ──HTTP──▶ axum handler
                    ├─ CORS 中间件（无 Origin 放行）
                    ├─ authorize（token）
                    └─ call(action, params)
                        ├─ ready? 否 → 503
                        ├─ id = next_id++
                        ├─ pending.insert(id, oneshot)
                        ├─ emit("api-command", {id, action, params})
                        └─ timeout(6s, rx)
                              ├─ Ok(Ok(v)) → 200 {ok,data}
                              ├─ Ok(Err(f)) → f.status
                              └─ Err(_) → 504

前端 api.ts
  listen("api-command") → runAction(action, params)
    ├─ 成功 → api_reply(id, ok=true, data)
    └─ 失败 → api_reply(id, ok=false, error, code)
  setInterval(1s) → api_publish_state(snapshot)
                     └─ bridge.publish() → 缓存 + 广播 SSE
```

### 3.10 ASR 模型：下载 → 自动加载 → 启动恢复

```
download_asr_model()
  ├─ ModelRuntime::download()（CAS 抢 downloading）
  │   ├─ DownloadSource::parse(url)
  │   ├─ 逐文件：is_complete? 跳过 : 流式写 .part + 每 240ms 进度
  │   ├─ 校验 Content-Length / 非空 → rename
  │   └─ 校验四件套
  ├─ auto_load_after_download()
  │   ├─ 已加载同一路径 → 广播 success
  │   ├─ 已有加载任务 → 直接返回（不抢占）
  │   └─ 否则 load() + emit(model-load-status)
  └─ 若 is_loaded_path(dir) → remember_loaded_model(relative_id)

下次启动
  setup() → spawn_restore_last_model()
    ├─ prefs.lastAsrModel 为空 → 返回
    ├─ 路径越界 / 不完整 → 返回（静默）
    ├─ 已有加载任务 → 返回
    └─ 后台 load() + 推送状态
```

---

## 4. 配置与环境依赖

### 4.1 文件位置

应用配置目录（Tauri `app_config_dir()`）：

| 平台 | 路径 |
| :--- | :--- |
| Windows | `%APPDATA%\com.yuanjie.tfybgm\` |
| macOS | `~/Library/Application Support/com.yuanjie.tfybgm/` |
| Linux | `~/.config/com.yuanjie.tfybgm/` |

写入统一走 `persist::write_json`：先写 `<path>.tmp`，再 `rename`；Windows 上 rename 失败时退化为直接覆盖并删除临时文件。

**注意**：`config.json` 与 `categories.json` 在首次启动时由 `ensure_file` 主动写出；`prefs.json` / `schedule.json` / `loops.json` **不会**在首次启动时创建，只有实际保存时才落盘（读取缺失文件返回默认值且不告警）。GPUI 若做「配置文件是否存在」的判断，不要假设 5 个文件都会存在。

### 4.2 `prefs.json`（version 4，35 字段）

| 字段（camelCase） | 类型 | 默认 | 收敛规则 |
| :--- | :--- | :--- | :--- |
| `version` | u32 | 4 | 读取时强制改写为 4；`<2` 触发一次迁移（清空 `exportScene` 并立即落盘） |
| `libraryDir` | string \| null | null | trim；空串 → null |
| `favorites` | string[] | [] | trim、去空、去重、`key.len() ≤512`、≤10 000 |
| `volume` | f32 | 0.85 | 有限数 clamp 0–1；NaN → 默认 |
| `muted` | bool | false | — |
| `playbackRate` | f32 | 1.0 | 必须命中 `config.player.rates`（EPSILON 比较）；否则 1.0 或首档 |
| `playMode` | `sequence`\|`repeat-one`\|`shuffle` | sequence | kebab-case 反序列化 |
| `sortBy` | `default`\|`name` | default | — |
| `crossfadeSeconds` | f32 | 2.0 | clamp 0–12；NaN → 默认 |
| `outputRoutes` | OutputRoute[] | 1 路 default | 按 deviceId 去重、≤8；`default` 恒首位；空列表时 default 启用 |
| `loudnessNormalization` | bool | true | — |
| `exportScene` | string | `""` | 小写；必须命中预设 id，否则置空 |
| `wallpaper` | string | `default` | 合法 id 或 `custom`（需有路径），否则 `default` |
| `customWallpaper` | string \| null | null | 扩展名 ∈ `jpg/jpeg/png/webp/mp4` |
| `wallpaperOpacity` | f32 | 0.35 | clamp 0–1；NaN → 默认 |
| `wallpaperBlur` | u32 | 0 | `min(20)` |
| `wallpaperFit` | `cover`\|`contain` | cover | 其它值 → cover |
| `uiFont` | string | `system` | trim；≤128 字符；禁止 `"` `\` `;` `{` `}` 与控制字符 |
| `autostart` | bool | false | — |
| `minimizeToTray` | bool | false | — |
| `gpuAcceleration` | bool | true | 改动需重启 |
| `autoplayOnStart` | bool | false | — |
| `preventSleep` | bool | false | — |
| `closeAction` | `ask`\|`quit`\|`tray` | ask | — |
| `modelsDir` | string \| null | null | trim、去 `\\?\` 前缀、≤512 字符；null = 默认目录 |
| `asrDownloadUrl` | string | `https://modelscope.cn/models/zengshuishui/Qwen3-ASR-onnx/tree/master/model_0.6B` | 仅 http(s)、≤1024、无空白；非法回退默认 |
| `ttsDownloadUrl` | string | `""` | 空值走内置下载源。自定义使用 HTTP(S) 文件目录，拒绝查询和片段，去掉末尾 `/` |
| `lastAsrModel` | string \| null | null | 相对路径、`/` 分隔；拒绝绝对路径、盘符、`..`；≤512 |
| `separationModel` | string \| null | null | trim、去空、≤256 字符（**前端权威**） |
| `desktopLyricsEnabled` | bool | false | 由歌词命令维护 |
| `subtitleFormat` | `srt`\|`lrc` | srt | 其它值 → srt |
| `smtc` | bool | false | 仅 Windows 读取 |
| `spatialAudio` | bool | false | 仅 Windows |
| `audioEnhancement` | bool | true | 仅 Windows |
| `mpris2` | bool | false | 仅 Linux（Wayland 会话） |

`OutputRoute`：`{ deviceId, label(≤64 字符，空→"输出设备"), volume(0–1), muted, enabled }`。

### 4.3 `config.json`

| 段 | 字段 | 默认 | 收敛 |
| :--- | :--- | :--- | :--- |
| `brand` | `title` / `subtitle` / `windowTitle` | `听风吟 · BGM` / `剪辑音频素材库` / `听风吟 · TFY BGM` | trim；空 → 默认 |
| `player` | `rates` | `[0.75,1,1.25,1.5,2]` | 去 NaN、clamp 0.25–4.0、去重（0.001 容差）、≤10；空 → 默认 |
| `player` | `volumeStep` | 0.1 | 有限且 >0 → clamp 0.01–0.5；否则默认 |
| `player` | `seekStepSeconds` | 5 | 有限且 >0 → clamp 1–60；否则默认 |
| `player` | `restartThresholdSeconds` | 3 | 有限且 >0 → clamp 1–30；否则默认 |
| `player` | `maxAutoSkips` | 3 | clamp 0–20（0 合法，表示不自动跳） |
| `player` | `loadTimeoutMs` | 20000 | clamp 1000–120000 |
| `ui.all` / `ui.favorites` | `title` / `labelEn` / `description` / `resultsTitle` | 内置文案 | 空 → 默认；`description` 支持 `{categories}` 占位符（前端 `formatCopy`） |
| `api` | `enabled` | true | 启动时读取，改动需重启 |
| `api` | `port` | 8737 | `<1024` → 8737；启动时读取，改动需重启 |
| `api` | `token` | `""` | trim、≤128 字符；非空时所有请求需鉴权 |
| `api` | `cors.mode` | `local` | `local`/`any`/`custom` |
| `api` | `cors.origins` | `[]` | trim、去尾 `/`、去重（忽略大小写）、≤16；`*` → 把 mode 改成 `any` 且不进入列表 |

`config.json` 支持热加载：`reload_config` 命令（界面「重新扫描」时调用）重新读取并更新窗口标题与文案。`api.enabled` / `api.port` 例外——只在启动时读取。

### 4.4 `categories.json`（JSON 数组）

```json
[
  {
    "id": "cheerful",
    "nameZh": "欢快",
    "nameEn": "Cheerful",
    "labelEn": "CHEERFUL / UPBEAT",
    "color": "#f59e0b",
    "icon": "sparkles",
    "title": "欢快节奏 · Cheerful & Upbeat",
    "description": "……"
  }
]
```

- `id` 即目录名；内置 6 项：`cheerful / solitude / memories / epic / gufeng / country`。
- 字段级 `#[serde(default)]`，缺失自动补全；整份损坏回退内置模板并告警。
- `color` 必须 `#RRGGBB`；非法时按数组下标从 8 色调色板取色。
- 文件缺失**不告警**（首次启动正常路径）；损坏才告警。

### 4.5 `schedule.json`（version 2）

```json
{
  "version": 2,
  "entries": [
    {
      "id": "slot-1",
      "name": "开店轻快",
      "start": "09:00",
      "end": "12:00",
      "songs": [{ "key": "cheerful/xxx", "start": "09:00", "end": "09:03" }],
      "volume": 0.55,
      "enabled": true
    }
  ]
}
```

`volume: null` = 保持当前音量。时间 `HH:MM`（接受 `9:00`，输出补零）。读取时 `normalize` + 有重叠则 `drop_overlaps` 并告警。

### 4.6 `loops.json`（version 1）

```json
{ "version": 1, "points": { "cheerful/xxx": { "start": 12.5, "end": 45.0, "enabled": true } } }
```

跨度 ≥0.2s；端点 `0–86400`；非有限数丢弃；key 必须 trim 后非空且 ≤512 字节且与原文完全一致。

### 4.7 派生数据

- `<config>/audio-index.json` 记录当前索引的根目录、开关、版本与分析条目。关闭索引会删除该文件。
- `<app_cache>/tts/speech-<UUID 简写>.wav` 保存合成预览结果。用户通过保存命令另存长期使用的 WAV。

**`subtitles/<prefix>-<hash16>.json`**（`StoredSubtitle`）

| 字段 | 说明 |
| :--- | :--- |
| `key` / `title` | 曲目稳定 key 与标题 |
| `segments[]` | `{ start, end, text }` |
| `createdAtMs` | 生成时间（编辑时不变） |
| `updatedAtMs?` | 最近编辑时间（旧记录无此字段） |
| `fileName?` / `durationMs?` | 源音频信息（旧记录无） |
| `srt` / `lrc` | 格式化文本（读取时按当前规则重建） |
| `separationModelName?` / `accompanimentModelName?` | 模型显示名 |
| `vocalsPath?` / `accompanimentPath?` | 分离交付物绝对路径 |
| `accompanimentKind?` | 新版固定 `伴奏` |
| `srtPath?` / `lrcPath?` | 分离目录下的字幕文件路径 |

**`separations/<prefix>-<hash16>/`**

| 文件 | 内容 |
| :--- | :--- |
| `vocals.wav` | 人声轨（16bit PCM，采样率由模型决定） |
| `accompaniment.wav` | 伴奏轨（固定 UVR Inst_HQ_3 的乐器输出） |
| `separation.json` | `{ key, modelId, modelName, engine, target, accompanimentModelName, vocalsPath, accompanimentPath, accompanimentKind, sampleRate, sourcePath, createdAtMs }` |
| `subtitles.srt` / `subtitles.lrc` | 生成/编辑时同步重写 |

**`<曲库根>/.tfy-trash/<epoch_ms>/<category>__<safe title>.<ext>`**：软删除回收站，可在文件管理器中手动恢复。

### 4.8 环境变量

| 变量 | 作用 | 读取位置 |
| :--- | :--- | :--- |
| `TFY_BGM_DIR` | 指定曲库根目录（优先级最高） | `library_root::resolve` |
| `TFY_FFMPEG` | 指定 ffmpeg 可执行文件或所在目录 | `loudness::ffmpeg_candidates` |
| `TFY_E2E_SONG` | 仅测试：真实链路用的歌曲路径 | `separation.rs` 测试 |
| `TFY_ASR_MODEL_DIR` | 仅测试：ASR 模型目录 | 同上 |
| `WEBKIT_DISABLE_COMPOSITING_MODE` | 关闭 GPU 加速时由应用自行设置（Linux） | `desktop::apply_gpu_acceleration` |

### 4.9 外部工具与打包资源

| 依赖 | 必需性 | 缺失表现 |
| :--- | :--- | :--- |
| `ffmpeg` | 归一化导出、人声分离、字幕归一化**必需** | 导出降级为原格式复制；分离/字幕直接报错并提示安装 |
| 随包 `models/UVR-MDX-NET-Inst_HQ_3.onnx` | 伴奏分离必需 | `resolve_accompaniment` 报错，字幕生成不可用 |
| 随包 `models/UVR-MDX-NET-Voc_FT.onnx` | 可选人声模型 | 该条目 `available=false` |
| 随包 `models/sherpa-onnx-spleeter-2stems-int8.tar.bz2` | 可选人声模型 | 该条目仍显示「首次使用时自动解压」，解压时才报缺失 |
| `assets/bg/1.mp4` / `1.thumb.jpg` / `2.jpg` | 内置壁纸 | 对应选项从列表消失 |
| ASR 模型（约 987 MB / 941 MiB，9 文件） | 字幕与桌面歌词必需 | 模型分组显示「下载」 |

### 4.10 曲库根目录解析顺序

| 顺序 | 来源 | `source` 值 | 条件 |
| :--- | :--- | :--- | :--- |
| 1 | `TFY_BGM_DIR` | `env` | 存在且是目录；无效则告警并继续 |
| 2 | `prefs.libraryDir` | `custom` | 存在且是目录；不存在则告警并回退 |
| 3 | 仓库根 `bgm/`（`CARGO_MANIFEST_DIR/../bgm`） | `bundled` | **仅 debug 构建** |
| 4 | exe 同级 `bgm/` | `external` | 存在即用 |
| 5 | 当前工作目录 `bgm/` | `external` | 存在即用 |
| 6 | 应用配置目录 `bgm/` | `external` | 存在即用 |
| 兜底 | 依次尝试在 exe 同级、配置目录 `create_dir_all` | `external` | 都失败则返回首个候选路径 + 告警 |

外置曲库会补齐分类子目录（`ensure_category_dirs`）；`bundled` / `custom` / `env` 不自动建目录。

### 4.11 打包资源清单（`tauri.conf.json`）

`../models/voices` 打包为 `voices/`，含参考 WAV 与 `prompts.json`。绿色版也需复制这组资源。

| 源 | 目标 |
| :--- | :--- |
| `../assets/bg/1.mp4` | `assets/bg/1.mp4` |
| `../assets/bg/1.thumb.jpg` | `assets/bg/1.thumb.jpg` |
| `../assets/bg/2.jpg` | `assets/bg/2.jpg` |
| `../models/sherpa-onnx-spleeter-2stems-int8.tar.bz2` | `models/…` |
| `../models/UVR-MDX-NET-Inst_HQ_3.onnx` | `models/…` |
| `../models/UVR-MDX-NET-Voc_FT.onnx` | `models/…` |

CSP：`default-src 'self'`；`media-src` 允许 `blob:` 与 `asset:`；`assetProtocol.enable=true`（scope 由运行时动态授权）。

---

## 5. 数据流与状态管理

### 5.1 状态所有权矩阵

| 状态 | 权威方 | 存储 | 反向同步 |
| :--- | :--- | :--- | :--- |
| 曲库快照（songs/categories/warnings） | **Rust** | 内存 `runtime.library` | 每次写操作/watch 返回或推送 |
| `prefs` 全量 | **Rust**（Normalize + 落盘） | `prefs.json` + 内存 `RwLock` | 命令返回值覆盖前端 |
| `prefs.libraryDir` / `modelsDir` / `lastAsrModel` / `desktopLyricsEnabled` | **Rust 独占** | 同上 | `save_prefs` 强制保留后端值 |
| `prefs.separationModel` / `subtitleFormat` / 其它 UI 偏好 | **前端** | 同上 | 整包提交 |
| `config`（品牌/播放器参数） | **Rust** | `config.json` + 内存 | `reload_config` 返回 |
| `categories` | **Rust** | `categories.json` | 命令返回新快照 |
| `schedule` | **共享**：前端编辑，Rust 校验+落盘 | `schedule.json` | 保存返回归一化后的整份 |
| `loops` | **Rust** | `loops.json` | 命令返回整份 `LoopBook` |
| 播放队列 / 指针 / 播放态 | **前端** | 内存（不持久化） | 无 |
| 播放位置 | **前端**（引擎时钟） | 内存 | HTTP 快照 |
| 字幕记录 | **Rust** | `subtitles/*.json` | 命令返回 / 列表 |
| 分离交付物 | **Rust** | `separations/*` | 随 `SeparationInfo` 返回 |
| 模型加载/下载状态 | **Rust** | 内存 + `prefs.lastAsrModel` | `model-load-status` 事件 + `ModelsInfo` |
| 歌词窗口开关 | **Rust**（持久化） + 前端镜像 | `prefs.desktopLyricsEnabled` | `desktop-lyrics-control` |
| 歌词样式 | **前端**（窗口 localStorage） | `tfy-bgm:desktop-lyrics-style` | 窗口间事件 |
| API token/CORS | **Rust** | `config.json` | `update_api_config` 返回 `ApiStatus` |

### 5.2 稳定 key 与迁移契约

`key = "<categoryId>/<fileNameStem>"`。这是整个系统的主键，**6 处数据**以它为索引：

| 数据 | 位置 |
| :--- | :--- |
| 收藏 | `prefs.favorites` |
| 循环点 | `loops.points` |
| 节目单歌曲 | `schedule.entries[].songs[].key` |
| 字幕记录 | `subtitles/*.json.key` |
| 分离目录 | `separations/<hash(key)>/` |
| 播放队列 / 当前曲目 | 前端内存 |

| 操作 | key 变化 | 需要迁移 | 不迁移的后果 |
| :--- | :--- | :--- | :--- |
| 改名 | `分类/旧主干` → `分类/新主干` | 全部 6 处 | 收藏丢失、循环点失效、字幕成孤立记录 |
| 改分类 | `旧分类/主干` → `新分类/主干` | 全部 6 处 | 同上 |
| 替换同名文件 | 不变 | 无 | — |
| 删除（软删除） | 消失 | 收藏、循环点删除；字幕**保留**为孤立记录；分离目录随字幕删除 | 残留脏数据 |
| 导入同名 | 生成新 key（` (2)`） | 无 | — |
| 外部改名 | 一增一删（不是 moved） | **不会迁移**（diff 无法识别） | 收藏/循环点变成孤立项【已知局限】 |

### 5.3 前端 store 依赖关系

```
state.ts（单一 reactive 根）
 ├─ library.ts   ← bootstrap / mutation / external change
 │   ├─ subtitles.ts（applySubtitleKeyChanges）
 │   └─ state.queue / favorites / loopPoints 重映射
 ├─ prefs.ts     ← 防抖落盘（280ms）
 ├─ player.ts    ← audio-engine.ts（发声）+ queue.ts（顺序）
 │   ├─ schedule.ts（applyScheduleEntry）
 │   └─ prefs.ts（persistPrefs / toggleFavorite）
 ├─ outputs.ts   ← audio-engine.ts（getMixStream / setDefaultOutput）
 ├─ api.ts       ← player / outputs / state + useScheduler.currentProgram
 ├─ models.ts    ← 事件 model-download-progress / model-load-status
 ├─ subtitles.ts ← models（modelReady）+ separation 目录
 ├─ desktop-lyrics.ts ← models（可用性）+ state.currentKey + audioState
 └─ settings.ts  ← prefs + api（token/CORS）
```

**循环依赖处理**：`player.ts` 与 `schedule.ts` 通过 `setEndedHandler` 注入打破循环；`api.ts` 直接 import `currentProgram` 计算 409。GPUI 建议用「服务注册表 + 单向数据流」替代这种互相 import。

### 5.4 节拍与轮询

| 节拍 | 周期 | 位置 | 作用 |
| :--- | :--- | :--- | :--- |
| 播放时钟 | `requestAnimationFrame`（~60Hz） | `audio-engine.ts:719` | 更新 `audioState.currentTime` |
| 节目单 tick | 1s | `useScheduler.ts:202` | 到点接管 |
| API 快照 | 1s + 变化立即 | `api.ts:92` | 推状态给 Rust / SSE |
| 歌词同步 | 1s + 跳变立即 | `desktop-lyrics.ts:180` | 主窗 → Rust |
| 歌词渲染 | 50ms | `lyrics.ts:578` | 窗内逐词高亮 |
| 歌词置顶巡检 | 3s | `lyrics.ts:581` | 重新声明 topmost |
| 曲库兜底扫描 | 30s | `library_watch.rs:22` | 补丢事件 |
| 下载进度节流 | 240ms | `models.rs:42` | 避免刷爆事件 |

### 5.5 并发与锁

| 锁 / 标志 | 类型 | 保护对象 | 关键语义 |
| :--- | :--- | :--- | :--- |
| `AppState.prefs/categories/config/schedule/loops` | `parking_lot::RwLock` | 各配置文件的内存副本 | 写操作「先落盘再替换内存」 |
| `AppState.runtime` | `RwLock<Runtime>` | root/source/library | 读多写少 |
| `AppState.ops_lock` | `Mutex<()>` | 曲库写操作串行 | watch 用 `try_lock`，写操作持锁期间放弃扫描 |
| `AppState.meta_cache` | `Mutex<HashMap>` | 标签缓存 | 溢出 20k 全清 |
| `AppState.watcher` | `Mutex<Option<LibraryWatcher>>` | 监听句柄 | 切换根目录时替换 |
| `ModelRuntime.loading/downloading/cancelled` | `AtomicBool` | 单任务互斥 | CAS 抢占；`Drop` 复位 loading |
| `ApiBridge.pending` | `Mutex<HashMap<u64, oneshot>>` | 待回命令 | 超时/发送失败时移除 |
| `LyricsState.enabled/snapshot/lock` | `Mutex` | 歌词状态 | 开关与快照分离 |
| `LyricsState.monitor_generation` | `AtomicU64` | 热区轮询线程代次 | 递增让旧线程退出 |
| `loadToken` / `sourceToken` | 前端 number | 解码与声部 | 丢弃过期回调 |
| `saveSequence` | 前端 number | 偏好保存响应 | 丢弃过期响应 |

### 5.6 一致性策略

| 策略 | 实现 |
| :--- | :--- |
| 先落盘后内存 | `save_prefs` / `save_schedule` / `save_loop_point` / `save_categories` 都先 `save()` 成功再写内存 |
| 原子写 | 临时文件 + rename；Windows rename 失败退化覆盖 |
| 旧事件丢弃 | `scannedAtMs` 比较（前端）；`ops_lock.try_lock`（后端） |
| 响应代次 | `saveSequence`（偏好） |
| 归一化幂等 | 所有 `normalize` 对已归一化输入不再变化（单测保证 roundtrip） |
| 损坏降级 | 每个 `load` 返回 `(value, Option<warning>)`，坏文件 → 默认 + 告警，绝不 panic |

---

## 6. gpui 分支实现建议汇总

### 6.1 差距清单（实测）

> 本节用途：**量化待消除的差距**，不是「可保留的分支差异清单」。按 §0.0 原则 4，下表中标为「缺」「旧版」「仅」的项都属于 gpui 必须补齐的内容；行数差异只用于估算工作量。

**core 模块级历史差异**（行数以 `origin/bgm-gpui@59175f0` 与 `e59ee18` 为准）。当前主线还新增 `audio_index.rs` 和 `process.rs`，偏好多 `ttsDownloadUrl`，路径模块新增 `AUDIO_INDEX_FILE`，文件操作新增按索引路径软删除。

| 文件 | gpui 行数 | 主线行数 | 差异 | 缺失能力 |
| :--- | ---: | ---: | :--- | :--- |
| `loops.rs` | 0（不存在） | 274 | **缺模块** | A/B 循环持久化与 key 迁移 |
| `schedule.rs` | 0（不存在） | 437 | **缺模块** | 节目单模型、归一化、重叠检测 |
| `loudness.rs` | 0（不存在） | 391 | **缺模块** | 预设、ffmpeg 探测、两遍 loudnorm |
| `tags.rs` | 0（不存在） | 191 | **缺模块** | 标签与时长、缓存 |
| `prefs.rs` | 246 | 1030 | 少 784 行 / 26 字段 | `crossfadeSeconds`、`outputRoutes`、导出、壁纸、系统、模型、歌词全部字段 |
| `config.rs` | 383 | 580 | 少 197 行 | `api` 段（`ApiConfig` / `CorsConfig` / `CorsMode`） |
| `library.rs` | 383 | 512 | 少 129 行 | **仅 mp3**（`AUDIO_EXTENSION="mp3"`，无 5 格式常量与优先级去重）、`Song` 无 `artist/album/genre/durationMs`、`LibrarySource` 少 `External` 变体 |
| `ops.rs` | 683 | 830 | 少 147 行 | 软删除与批量导出**已有**；缺 `ExportSettings` 参数（导出恒 `.mp3` 纯复制）、`is_mp3` 限制（导入只收 mp3）、`stem_taken` 大小写不敏感去重、`source_extension` 保留源格式 |
| `export.rs` | 124 | 369 | 少 245 行 | 无 ffmpeg 归一化决策树（`copy_same_format` / `FormatMismatch` / `SameFile`）；只有复制校验 |
| `paths.rs` | 18 | 38 | 少 20 行 | `MODELS_DIR_NAME`、`TRASH_DIR_NAME`、`strip_verbatim_prefix` |
| `categories.rs` | 464 | 464 | 一致 | — |
| `persist.rs` | 78 | 78 | 一致 | — |
| `limits.rs` | 13 | 13 | 一致 | — |

> 旧文档写的「`prefs.rs` 少 246 行 / `library.rs` 少 185 行 / `ops.rs` 少 203 行 / `export.rs` 少 124 行」有误——那些数字其实是 **gpui 文件自身的行数**，不是差额。本表按「主线 − gpui」重新计算。

**应用层功能域覆盖**

| 域 | gpui 对应文件 | 结论 |
| :--- | :--- | :--- |
| F01 扫描 | `src/library.rs`、`core/library.rs` | 有，但**仅 mp3**、无标签、`LibrarySource` 少 `external` |
| F02 文件操作 | `src/library.rs`、`core/ops.rs` | 有导入/改名/移动/软删除/批量导出；导入仅 mp3，导出恒 `.mp3` 纯复制 |
| F03 标签 | 无 | **未实现** |
| F04 监听 | 无 | **未实现** |
| F05 播放 | `src/audio.rs`（126 行） | 极简：单 sink、无淡入淡出、无 A/B 循环、倍速变调 |
| F06 队列 | `src/view.rs::random_other` | 均匀随机，无智能权重 |
| F07 多输出 | 无 | **未实现** |
| F08 节目单 | 无 | **未实现** |
| F09 导出 | `core/export.rs`（旧） | 仅 mp3 纯复制（无归一化、无格式校验） |
| F10 诊断 | 无 | **未实现** |
| F11 偏好 | `core/prefs.rs`（8 字段） | 需整体替换 |
| F12 壁纸 | 无 | **未实现** |
| F13 系统集成 | 无 | **未实现**（无托盘/自启/字体扫描） |
| F14 HTTP | 无 | **未实现** |
| F15 ASR | 无 | **未实现** |
| F16 分离 | 无 | **未实现** |
| F17 字幕 | 无 | **未实现** |
| F18 桌面歌词 | 无 | **未实现** |
| F19 音频索引 | 无 | **未实现** |
| F20 公开歌单迁移 | 无 | **未实现** |
| F21 TTS | 无 | **未实现** |

### 6.2 建议迁移批次

> 批次是**对齐顺序**，不是范围裁剪：A→E 全部完成才算对齐，任何批次不得因难度删项。每批结束都必须过 §8.3 对应门槛。

**批次 A：core 对齐（P0，不改 UI）**

1. 用主线 `src-tauri/core` **整体替换** GPUI 的 `core/`，保持 crate 名 `bgm-core`。
2. 修正 GPUI 对 `Prefs`、`AppConfig`、`Song` 的构造（新增字段有默认值，编译期会报缺失字段）。
3. 运行 `cargo test -p bgm-core`（主线 108 个测试声明），确认归一化 / 迁移 / 导出决策树全部通过。
4. **验收**：同一份 `prefs.json` 在两分支间来回读写不丢字段；`cargo check` 通过。

**风险提示**：① `Prefs` 从 8 字段扩到 35 字段，GPUI 现有的「读入 → 改一个字段 → 写回」路径会**抹掉**其余 27 个字段，替换后必须检查所有 `save` 调用点是否基于完整结构体；② 替换后扫描格式从 **mp3** 扩到 **5 种**，界面上会突然出现 flac/m4a/wav/ogg 曲目，GPUI 的播放与时长展示需先确认能处理这些容器；③ `LibrarySource` 会多出 `External`，GPUI 中所有 `match` 该枚举的位置都会编译报错——这是**好事**（强制补分支），不要用 `_ =>` 掩盖。

**批次 B：数据与播放（P1）**

1. 接入 `tags.rs`，扫描结果带 `artist/album/genre/durationMs`。
2. 接入 `ops.rs` 的软删除与批量导出；确认 `KeyChange` 迁移收藏与循环点。
3. 用 `loops.json` + 自定义 `Source` 实现 A/B 循环（先保证数据与开关可用，再优化无缝）。
4. 用双 voice 混音器实现交叉淡入淡出（先固定 2s，再可调）。
5. **验收**：改名/移动/删除后收藏与循环点数量正确；`.tfy-trash` 可恢复；3 秒循环连续播放 2 分钟无接缝。

**批次 C：时间与流程（P1）**

1. 移植 `schedule.rs` + 调度器（1s tick + 2 分钟回看 + 90s 休眠对齐）。
2. 节目单 UI 可后做，先保证「到点接管 + 时段拦截」。
3. 接入 `export.rs` + `loudness.rs`，把对话框换成 `rfd`。
4. **验收**：相邻节目不重叠；时段内手动点播被拦截；`-14` 与 `-20` 预设导出响度差约 6 LU。

**批次 D：AI 能力（P1）**

1. 接入 `models.rs`（下载 / 加载 / 分片识别），事件改为回调。
2. 接入 `separation.rs`（C API + worker 子进程），随包模型一并打包。
3. 接入 `subtitles.rs`（生成 / 编辑 / 迁移），保持 key→文件名哈希一致。
4. **验收**：下载可取消且无 `.part` 残留；分离产物存在且非空；生成的 SRT 时间轴单调；编辑后 `updatedAtMs` 更新。

**批次 E：体验与集成（P2）**

1. 曲库监听（700ms 防抖 + 30s 兜底）。
2. 可视化诊断（先 Rust 算法，后 GPUI 渲染）。
3. 壁纸（先静态图，样式自由）、系统设置（字体/自启/托盘）、HTTP 接口。
4. 桌面歌词（先不透明置顶，再透明 + 穿透）。
5. **验收**：外部脚本执行附录 C 的 14 个 action 全部成功；`curl /v1/state` 返回 `ok:true`。

### 6.3 平台能力映射（WebView → GPUI）

| 能力 | 主线实现 | GPUI 建议 | 主要风险 |
| :--- | :--- | :--- | :--- |
| 整曲解码 | `decodeAudioData` → `AudioBuffer` | `symphonia` → `Vec<Vec<f32>>` | 内存占用（约 23MB/分钟） |
| 采样级循环 | `loopStart` / `loopEnd` | 自定义 `Source` 或自建输出回调取模 | 边界 click（需 1~2ms 交叉） |
| 交叉淡入淡出 | 双 `GainNode` 线性包络 | 双 voice 混音器 + 统一时钟 | 两路漂移（必须共享 sample clock） |
| 倍速 | `playbackRate`（保音高） | 重采样（变调）；保音高需 WSOLA | 听感差异（已在 gpui README 声明） |
| 多输出 | `setSinkId` + `MediaStream` | `cpal` 多 device 输出流 + 分发 | Linux 设备 id 不稳定 |
| 频谱 / LUFS | `AnalyserNode` | `rustfft` + 手工 K 加权 + 平滑 | 参数必须逐字一致 |
| 系统媒体控件 | Media Session API | Windows SMTC / Linux MPRIS2 / macOS Now Playing | 无跨平台 crate，需分平台写 |
| 防休眠 | Screen Wake Lock | `SetThreadExecutionState` / D-Bus inhibit / `IOPMAssertion` | 权限与恢复事件 |
| 字体枚举 | `ttf-parser` | **直接复用 `fonts.rs`** | 无 |
| 壁纸视频 | `<video loop muted>` | 首帧降级 / `ffmpeg-next` 解码上纹理 | 成本高，建议后置 |
| 托盘 | Tauri tray | `tray-icon` + `muda` | 菜单事件模型不同 |
| 开机自启 | 注册表 / autostart / plist | 复用 `desktop.rs` 逻辑（去 `AppHandle`） | 无 |
| 透明置顶窗 | Tauri window builder | 平台 window level + layered window | Wayland 限制 |
| 鼠标穿透 | `set_ignore_cursor_events` | `WS_EX_TRANSPARENT` / `ignoresMouseEvents` | 热区检测需自建 |
| 文件对话框 | `tauri-plugin-dialog` | `rfd` | 无 |
| 打开/定位文件 | `tauri-plugin-opener` | `opener` crate | 无 |
| asset 协议 | `convertFileSrc` + scope | 直接读文件（无 scope 概念） | 无 |

### 6.4 建议新增依赖（GPUI 侧）

| 用途 | crate | 备注 |
| :--- | :--- | :--- |
| 解码 | `symphonia` | 默认覆盖 mp3/flac/wav/ogg；m4a 需 `isomp4` 特性【待确认】 |
| 输出 | `cpal` | 多设备、低延迟 |
| 重采样 | `rubato` | 倍速与采样率转换 |
| FFT | `rustfft` | 实时频谱 |
| 文件对话框 | `rfd` | 异步版本避免阻塞 UI |
| 托盘 | `tray-icon` + `muda` | 与 GPUI 版本兼容性需验证 |
| 自启（Win） | `windows-registry` | 主线已用 |
| 媒体控件（Win） | `windows` | `Media::SystemMediaTransportControls` |
| 媒体控件（Linux） | `zbus` | 实现 MPRIS2 |
| 防休眠（Win） | `windows` | `SetThreadExecutionState` |
| 防休眠（Linux） | `zbus` | `org.freedesktop.ScreenSaver` |
| HTTP | `axum` + `tokio` | 主线已用，可直接搬 |
| 文件监听 | `notify` | 主线已用 |
| 标签 | `lofty` | 主线已用 |
| ASR | `sherpa-onnx` + `sherpa-onnx-sys` | 主线已用 |
| 解压 | `bzip2` + `tar` | 主线已用 |
| 打开文件 | `opener` | 替代 `tauri-plugin-opener` |

### 6.5 不建议照搬的部分

| 部分 | 原因 | 替代方案 |
| :--- | :--- | :--- |
| `api-command` / `api_reply` 桥 | 为跨 WebView 边界而存在 | GPUI 内直接调用命令通道 |
| `asset_protocol_scope` 授权 | Tauri 安全模型 | 直接读文件 |
| `LibraryWatcher` 的 `AppHandle` 依赖 | 事件总线耦合 | 通道 + `cx.notify()` |
| `setEndedHandler` 回调钩子 | 隐式控制流 | `enum EndedAction` |
| `WebviewWindowBuilder` 参数 | 平台差异大 | 平台特定 window 配置 |
| Tauri `generate_context!` / `manage` | 框架特有 | 自建 `App` 结构体 + `Arc` |

### 6.6 对齐任务清单（F01–F21）

F19 随批次 A 复用 core，并在曲库与前端入口接通后完整验收。F20、F21 纳入批次 E 的独立工作台验收。

> 用法：按行拆 issue。每行的「对齐目标」都是**主线现状**，不是建议；「gpui 必做」是达到目标的最小动作；「验收」对应 §8.2 的条目编号（可回查具体步骤）。**验收只看功能与反馈语义，不看布局与样式**。

| 域 | 对齐目标（以主线为准） | gpui 必做 | 验收 |
| :--- | :--- | :--- | :--- |
| F01 | 5 格式扫描、稳定 key、同名多格式按 `flac>wav>m4a>mp3>ogg` 去重、告警文案、`LibrarySource` 四值 | 替换 `core/library.rs`、`core/categories.rs`；补 `External` 分支；播放器需能解 5 种容器 | §8.2 F01 前 3 条 |
| F02 | 导入/改名/移动/软删除/批量导出、` (2)` 序号、`KeyChange` 迁移 | 替换 `core/ops.rs`；接入 `ExportSettings`；导入改为 5 格式；导出保留源格式 | §8.2 F01/F02 剩余条目 |
| F03 | `artist/album/genre/durationMs` + 20k 条缓存（路径+大小+mtime） | 新增 `core/tags.rs`；`Song` 扩 4 字段；扫描接入 `MetaCache` | 第二次扫描不重解析 |
| F04 | notify + 700ms 防抖 + 30s 兜底 + diff（改名=增删、跨类=move） | 新增监听线程 + 通道 + `try_lock` 语义 | §8.2 F04 |
| F05 | 整曲解码、80ms 传输淡入淡出、0–12s 交叉淡变、采样级 A/B 循环、失败跳过 | 重写 `audio.rs`：解码缓存 + 双 voice 混音 + `Source` 循环 + 统一时钟 | §8.2 F05 |
| F06 | 智能随机权重（含近期惩罚、同专辑/同艺人排除）、历史 48/45min | 把 `smartPick` 移植为 `bgm-core` 纯函数，替换 `random_other` | §8.2 F06 第 4 条 |
| F07 | 8 路上限、`default` 恒首、每路独立增益、能力缺失降级提示 | 新增多设备输出（`cpal` 广播 mix bus）；`OutputRoute` 结构与 prefs 完全一致 | §8.2 F07 |
| F08 | 歌曲级时间轴、不重叠、1s tick + 2min 回看 + 90s 休眠对齐、时段拦截 | 新增 `core/schedule.rs` + 调度器 + `EndedAction` 决策 | §8.2 F08 |
| F09 | 4 预设、两遍 loudnorm、ffmpeg 三级探测、失败回退决策树 | 新增 `core/loudness.rs`；替换 `export.rs`；`rfd` 换对话框 | §8.2 F09 |
| F10 | 72 段频谱、BS.1770 双门限、削波/相位/静音/爆音判定、三档结论 | 算法先翻成 Rust 纯函数并单测，再做波形/仪表渲染 | §8.2 F10 |
| F11 | `Prefs` 35 字段、v4 迁移、后端 6 字段权威、280ms 防抖 + 退出前 flush | 整体替换 `core/prefs.rs`；补齐所有 `save` 调用点 | §8.2 F11 |
| F12 | 7 个合法 id、透明度/模糊/填充、内置缺失隐藏、视频失焦暂停 | 先静态图 + 首帧降级；字段与 id 集合保持一致 | §8.2 F12 |
| F13 | 字体扫描、自启三平台、托盘联动、关闭行为、GPU/防休眠/媒体控件 | 复用 `fonts.rs`/`desktop.rs` 逻辑；媒体控件与防休眠按平台实现或明示不支持 | §8.2 F13 |
| F14 | 33 个方法+路径（9 类）、14 action、长任务 SSE、401/403/404/409/503/504、快照字段 | 复用 `api.rs`；去掉命令桥，直接调引擎；快照结构提到 `bgm-core` 共用 | §8.2 F14 |
| F15 | 9 文件下载、取消清理、四件套校验、加载状态机、启动恢复、分片识别 | 复用 `models.rs`，`AppHandle` 换回调 | §8.2 F15 |
| F16 | 双引擎分离、worker 子进程、10 分钟串行、`.part` 保护、`separation.json` | 复用 `separation.rs`；随包模型进 gpui 打包 | §8.2 F16 |
| F17 | 强制分离前置、只识别人声、SRT/LRC 解析与编辑校验、key 迁移、哈希路径 | 复用 `subtitles.rs`；`subtitle_stem` 必须逐字一致 | §8.2 F17 |
| F18 | 透明置顶、逐词高亮、rate 本地推进、锁定穿透、样式 v4 | 多窗口 + 平台 window level；先不透明后透明 | §8.2 F18 |
| F19 | 索引增量同步、详情缓存、文件/录音查询、查重软删除 | 复用 core，接入音频工具 | §8.2 F19 |
| F20 | 3 平台公开链接、完整性提示、预览、复制及 TXT 保存 | 复用解析模块，接入原生剪贴板和对话框 | §8.2 F20 |
| F21 | 模型管理、TXT/SRT/LRC、参考音色、取消、WAV 预览与导出 | 复用 Rust 合成模块与参考音频流程 | §8.2 F21 |

**对齐完成的判定**：§6.6 全部行通过 + §8.3 五个批次门槛全过 + 同一份配置/曲库在两分支间互换后行为一致。

---

## 7. 风险、缺口与待确认项

### 7.1 主线已知边界（对齐时需保留的用户可见语义）

1. **多输出设备**仅在 Chromium 系 WebView（Windows WebView2）可用；Linux WebKitGTK 与 macOS WKWebView 无 `setSinkId`，降级为仅系统默认输出。
2. **SMTC / MPRIS2** 通过 Media Session API 实现，需要维持静音媒体元素才会出现系统控件；不是原生系统绑定。
3. **空间音效 / 音频增强**是 Web Audio 近似（HRTF 声像 + 总线压缩），不等于系统级音效。
4. **防休眠**依赖 Screen Wake Lock，仅 Chromium 可用；macOS / Linux 开关不生效。
5. **GPU 加速**开关只在 Windows / Linux 有实际效果，必须重启；macOS 无实现。
6. **ffmpeg 归一化**固定输出 mp3；目标为其它容器时只能原格式复制。
7. **壁纸**只支持单层静态图或单个 MP4，不支持播放列表与音频。
8. **节目单**按本地时间每天重复，不支持跨天、按星期或按日期例外。
9. **整曲解码**上限 30 分钟；超出直接跳过（WebView 内存约束）。对齐口径：gpui 保留同样的上限与提示，若确要放宽须先改主线，避免两分支行为分叉。
10. **外部改名**不会被识别为 `moved`（diff 只认同 `fileName` + `title` 的跨目录移动），收藏/循环点会变成孤立项。
11. **字幕记录**在曲目删除后保留为孤立记录（可预览/导出/删除），不自动清理。
12. **ASR** 只支持 Qwen3-ASR 0.6B-int8 四件套，不支持其它模型结构。

> **平台受限项在 gpui 的处理**：第 1–5 条本质是 WebView 的能力限制。gpui 若能用原生 API 做到（例如 `cpal` 多设备输出、原生 SMTC/MPRIS2），属于**允许的超越**，但必须满足两个条件：① 数据契约（`OutputRoute`、`prefs` 字段、HTTP 快照）与主线完全一致；② 主线降级时的用户可见语义（提示文案含义、`supported=false` 的判定条件）在 gpui 中仍要有对应表达，不能因为「gpui 能做到」就让同一份配置在两侧表现不一致。第 6–12 条是产品规则，**不因平台而改变**。

### 7.2 技术风险

| 风险 | 影响 | 缓解 |
| :--- | :--- | :--- |
| `prefs.json` 结构差异（8 vs 35 字段） | GPUI 写回会丢配置 | 批次 A 整体替换 + 只读过渡 |
| gpui `core` 仅支持 mp3 | 与主线的 5 格式曲库不兼容（同一目录在两侧扫描结果不同） | 整体替换 `library.rs` / `ops.rs`，并先验证播放器能解 flac/m4a/wav/ogg |
| 两个分支对同名文件生成不同序号 | 文件重复 / key 冲突 | 复用 `ops::unique_path`，勿自行实现 |
| 字幕 key→文件名哈希不一致 | 互相读不到记录 | 复用 `subtitles::subtitle_stem` |
| 分离 worker 子进程参数不一致 | 子进程启动后不退出、重复建窗 | 严格复用 `--separation-worker` 早退逻辑 |
| rodio 倍速变调 | 听感与主线不同（主线 `playbackRate` 保音高） | **对齐要求**：需实现保音高（WSOLA / phase vocoder），否则视为未对齐（§0.0 原则 5）；不允许以「已知差异」长期保留 |
| 多设备时钟漂移 | 长时间播放不同步 | 共享 mix bus，不做多路独立解码 |
| Wayland 置顶/穿透限制 | 歌词可显示，位置和置顶取决于合成器 | 保留普通窗口及悬停解锁 |
| `sherpa-onnx` 对不兼容模型 `_Exit(-1)` | 主进程被杀 | 必须保留 worker 子进程隔离 |
| ffmpeg 每次探测 spawn 3 次进程 | 打开导出对话框有延迟 | GPUI 侧缓存探测结果 |

### 7.3 待确认项（编号）

F19–F21 的实机验证项分别为目标平台麦克风和编码、三家平台当前公开分享链接、随包参考音色和模型文件。

> 说明：按 §0.0，凡「主线已有明确行为」的都不再列为待确认，直接按主线对齐；本表只保留**技术可行性未知**或**外部事实无法从代码确认**的项。产品取舍类问题一律以主线为准，不再讨论。

| 编号 | 事项 | 现状 | 处置（以主线为准） | 仍需确认的技术细节 |
| :--- | :--- | :--- | :--- | :--- |
| Q1 | gpui `rodio 0.23` 实际启用的解码特性 | `Cargo.toml` 只写 `rodio = "0.23"`，未显式列特性 | 必须覆盖 mp3 / m4a / flac / wav / ogg；缺哪个就在特性里显式打开，或改用 `symphonia` 直连 | `cargo tree -e features` 实测 + 5 种样例文件回归 |
| Q2 | 保音高倍速的实现选型 | gpui 现为变调（与主线不一致） | 必须实现保音高；具体用哪个 crate / 算法由实现方决定 | `signalsmith-stretch` / `rubato` + WSOLA 的 CPU 与延迟实测 |
| Q3 | 随包分离模型如何进 gpui 打包 | 主线 `tauri.conf.json` 已列 3 个模型；gpui 无打包配置 | 必须随包分发（否则 F16 开箱不可用），绿色版允许外置 `models/` | gpui 打包产物的资源目录约定 |
| Q4 | Wayland 歌词窗口 | 普通透明窗口，锁定态保持可交互 | 复现目标会话的定位、置顶和悬停解锁行为 | 目标桌面环境（GNOME/KDE/wlroots）实测 |
| Q5 | 旧文档「41 个 Tauri 命令」的来源 | 实测基线 `db22c65` 为 **32** 个 | 以本文实测为准，旧数字作废 | 无需确认（已按代码修正） |
| Q6 | gpui 声明的视觉基线 `203c641` 是否可解析 | `AGENTS.md` / `README.md` 提及，但该提交不在本仓库对象库 | **不适用**：UI 布局与视觉样式不作对齐要求（§0.0 原则 6），无需以该提交为准 | 无需确认；仅当需要复刻某个具体交互手感时才回查 |

---

## 8. 测试与验收清单

### 8.1 已有自动化测试（主线）

| 命令 | 覆盖 |
| :--- | :--- |
| `cd src-tauri && cargo test -p bgm-core` | 108 个测试声明：扫描、分类、偏好、配置、节目单、循环、导出、响度、持久化、标签 |
| `cargo test -p tfy-bgm --lib subtitles::tests` | 16 项：SRT/LRC 解析、往返、非法标签、超长文本拒绝、key 迁移、分离路径重映射 |
| `cargo test -p tfy-bgm --lib separation::tests` | WAV 写出、`.tar.bz2` 首次解压/替换、模型元数据预检、解码分声道、worker 非法任务拒绝、双引擎线程分配；真实模型测试默认 `#[ignore]` |
| `cargo test -p tfy-bgm --lib` | 应用层 110 个测试声明（含 `api::tests` 的 origin / 状态码映射、`state::tests` 的 diff） |
| `cargo clippy -p bgm-core --all-targets -- -D warnings` | 静态检查 |
| `cargo fmt --all -- --check` | 格式 |

CI（`.github/workflows/package.yml`）在 Linux 任务里执行 `cargo test -p bgm-core`；**没有** core 与 gpui 的同步校验。

### 8.2 功能验收清单（按域）

**F01 / F02 / F03**

- [ ] 扫描 500+ 曲目时，第二次扫描不重复解析未变化文件（`MetaCache` 命中）
- [ ] 同目录 `track.flac` + `track.mp3` 只出现一条，且有「跳过同名曲目」告警
- [ ] 未注册目录 / 根目录音频各产生一条告警，且不计入曲目数
- [ ] 导入同名文件生成 `xxx (2).mp3`，原文件未被覆盖
- [ ] 导入 `.flac` 后目标仍是 `.flac`（不被转成 mp3）
- [ ] 改名冲突时报错，源文件仍在
- [ ] 移动到已有同名文件的分类 → `same_name (2).mp3`
- [ ] 软删除后 `.tfy-trash/<批次>/` 内文件可手动恢复
- [ ] 改名后收藏、循环点、队列指针、字幕记录全部跟随

**F04**

- [ ] 向曲库复制 10 个文件，界面在 ~1 秒内自动刷新
- [ ] 目录内改名 → 提示「新增 1 首，移除 1 首」（不是「移动」）
- [ ] 跨分类移动文件 → 提示「移动 1 首」，收藏跟随
- [ ] 断网盘 / 休眠恢复后 30 秒内自动补扫

**F05 / F06**

- [ ] 切歌无静音缝；暂停/恢复无爆音
- [ ] 3 秒 A/B 循环连续播放 2 分钟无接缝、无漂移
- [ ] 交叉淡入淡出设为 0 时硬切；设为 12 时明显长淡
- [ ] 智能随机连续 20 首内不出现同专辑连播
- [ ] 连续 3 次加载失败后停住并提示（`maxAutoSkips=3`）
- [ ] `maxAutoSkips=0` 时首次失败即停
- [ ] 超过 30 分钟的曲目点击播放直接提示，不卡死

**F07**

- [ ] 开启 2 路输出后，系统默认与第二设备同时出声
- [ ] 单独调低第二路音量，系统默认不受影响
- [ ] 未授权设备列表时提示，授权后列表完整
- [ ] 第 9 路被拒绝（界面 / HTTP 409）

**F08**

- [ ] 两档节目时间重叠时保存被拒绝，提示包含冲突节目名
- [ ] 相邻节目（`10:00` 结束 / `10:00` 开始）可保存
- [ ] 时段内手动点播被拦截（界面 toast / HTTP 409）
- [ ] 最小化窗口 3 分钟后恢复，节目单仍停在正确的歌
- [ ] 歌曲播完只循环当前到点歌，不串到下一时段
- [ ] `23:59` 起始的节目被丢弃

**F09**

- [ ] `-14` 与 `-20` 预设导出同一素材，响度差约 6 LU
- [ ] 无 ffmpeg 时导出成功，结果为原格式且有提示
- [ ] `flac → mp3` 目标、无 ffmpeg → 报错且不产生文件
- [ ] 导出到源文件所在路径被拒绝（`SameFile`）
- [ ] 批量导出同名文件生成 ` (2)`

**F10**

- [ ] 含削波的素材给出「建议处理」
- [ ] 反相素材给出相位异常
- [ ] 静音 >5 秒给出严重
- [ ] 频谱 72 段随播放刷新
- [ ] 循环标记可写入并立即生效

**F11 / F12**

- [ ] 关闭「记住选择」后重启，关闭按钮重新询问
- [ ] `prefs.json` 删除后启动不报错，全部默认值
- [ ] `prefs.json` 写坏后启动有 1 条告警且可用
- [ ] 壁纸切到自定义 MP4 后，窗口失焦暂停、回焦续播
- [ ] 壁纸文件被删除后启动回退默认样式并提示

**F13**

- [ ] 字体列表包含本机中文字体，切换后全局生效
- [ ] 开机自启开关在注册表 / autostart / plist 中可见
- [ ] 关闭「最小化托盘」后 `closeAction=tray` 回退 `ask`
- [ ] 关闭 GPU 加速后重启，WebView 走软件渲染
- [ ] 播放时系统不熄屏；暂停后恢复熄屏策略

**F14**

- [ ] `curl /v1/state` 返回 `ok:true`
- [ ] 错误 token 返回 401
- [ ] 非本机网页请求收到 403（默认 CORS）
- [ ] `/v1/events` 每秒收到一条 `type:"state"`
- [ ] 14 个 action 全部成功；未知 action 404
- [ ] 节目单时段内 `/v1/play` 409

**F15 / F16 / F17 / F18**

- [ ] 模型页可切换 / 恢复目录
- [ ] 下载中途取消不残留 `.part`
- [ ] 下载完成自动加载，状态条显示成功
- [ ] 加载后识别 WAV 并显示文本
- [ ] 重启应用后模型自动恢复（`lastAsrModel`）
- [ ] 释放模型后重启不再自动加载
- [ ] 未选分离模型时生成被拒绝
- [ ] 分离产物 `vocals.wav` / `accompaniment.wav` 均存在且 >44 字节
- [ ] 生成后 `subtitles.srt` / `.lrc` 与记录 JSON 同时存在
- [ ] 分段编辑保存后 `updatedAtMs` 更新，预览与导出一致
- [ ] 原文编辑输入非法时间轴 → 报行号且不写盘
- [ ] 桌面歌词开关在未加载模型时禁用
- [ ] 歌词高亮随进度推进，倍速下不漂移
- [ ] 设置窗改字号 / 颜色 / 提前量立即生效并持久化
- [ ] 关闭歌词窗后偏好清除，重启不再自动开

**F19 音频索引**

- [ ] 首次建立需确认，启动或曲库变动时后台同步。切换根目录后不展示旧索引为当前结果。
- [ ] 同源不同格式能查重。仅移动用户选中的有效副本，确认取消时不改变文件。
- [ ] 文件和麦克风均能查询，关闭索引后搜索不可用，歌曲详情仍可访问。

**F20 公开歌单迁移**

- [ ] 三个平台各有正常输入用例，验证曲序、艺人与多来源文本预览。
- [ ] 过期、非公开及返回不完整时有相应提示，复制和 TXT 保存内容与预览一致。

**F21 语音合成**

- [ ] 双模型的下载源、安装、取消、加载、卸载及删除行为可验证。
- [ ] TXT 分段能合并短段并切开长段，SRT/LRC 保留时间轴。内置 ZipVoice 自动填入可用逐字稿。
- [ ] 参考录音期间主播放器停止发声。取消后忽略旧进度，合成 WAV 可按原生采样率预听和另存。

### 8.3 gpui 迁移验收门槛

| 阶段 | 门槛 |
| :--- | :--- |
| 批次 A | `cargo test -p bgm-core` 全绿；`prefs.json` 双向读写字段数一致（35） |
| 批次 B | 改名/移动/删除后 6 处引用全部正确；`.tfy-trash` 可恢复；循环无接缝 |
| 批次 C | 节目单不重叠校验、时段拦截、休眠对齐三项通过；导出响度误差 ≤2 LU |
| 批次 D | 下载/加载/识别/分离/字幕全链路通过；字幕文件哈希与主线一致 |
| 批次 E | 33 个方法与路径、14 个 HTTP action、长任务及 SSE 契约回归。验证壁纸、系统集成和歌词窗的平台行为，通过 F19–F21 的工作台验收 |

> 验收边界：以上门槛只检查**功能可用性与数据一致性**。界面是否「像主线」、布局是否一致、配色与动效是否相同，**不在验收范围**（§0.0 原则 6）。唯一与观感相关的硬指标是「听感一致」（淡入淡出、循环无缝、响度目标、倍速保音高）与「状态可辨」（加载/禁用/错误有明确视觉区分）。

---

## 9. 本次基线更新

本版从 `e59ee18` 更新到 `80faf7d`。新增 F19–F21，同步歌曲详情、Linux 媒体服务、Wayland 歌词降级、ASR 模型删除和本机 HTTP 长任务。各项序列化及对外契约见 §4 和附录 A–C。

## 附录 A：Tauri 命令全表（83）

| # | 命令 | 分组 | 参数 | 返回 | 关键副作用 |
| ---: | :--- | :--- | :--- | :--- | :--- |
| 1 | `bootstrap` | 启动 | — | `Bootstrap { prefs, library, config, schedule, loops, platform }` | 无（纯读） |
| 2 | `reload_config` | 启动 | — | `ConfigReload { config, warnings }` | 更新内存 config + 窗口标题 |
| 3 | `open_config_file` | 启动 | — | `()` | 文件管理器定位 |
| 4 | `system_fonts` | 启动 | — | `SystemFont[]` | 阻塞线程扫描 |
| 5 | `save_prefs` | 偏好 | `prefs: Prefs` | `Prefs` | 归一化 + 落盘 + 保留 6 个后端字段 |
| 6 | `save_schedule` | 节目单 | `schedule: Schedule` | `Schedule` | 归一化 + 重叠拒绝 + 落盘 |
| 7 | `save_loop_point` | 循环 | `key, start, end, enabled` | `LoopBook` | 校验曲目存在 + 落盘 |
| 8 | `remove_loop_point` | 循环 | `key` | `LoopBook` | 落盘 |
| 9 | `rescan_library` | 曲库 | — | `LibrarySnapshot` | 重载 categories + 重扫 + 重建监听 |
| 10 | `choose_library_dir` | 曲库 | — | `LibrarySnapshot \| null` | 目录对话框 + 持久化 + 重扫 |
| 11 | `reset_library_dir` | 曲库 | — | `LibrarySnapshot` | 清除 `libraryDir` + 重解析根目录 |
| 12 | `open_library_dir` | 曲库 | — | `()` | 打开目录 |
| 13 | `models_info` | 模型 | — | `ModelsInfo` | 重新扫描模型目录 |
| 14 | `choose_models_dir` | 模型 | — | `ModelsInfo \| null` | 目录对话框 + 持久化 |
| 15 | `reset_models_dir` | 模型 | — | `ModelsInfo` | 恢复默认目录 |
| 16 | `open_models_dir` | 模型 | — | `()` | 创建并打开目录 |
| 17 | `download_asr_model` | 模型 | — | `ModelsInfo` | 下载 9 文件 + 自动加载 + 记 `lastAsrModel` |
| 18 | `load_asr_model` | 模型 | `path: String` | `ModelsInfo` | 加载 + 记 `lastAsrModel` |
| 19 | `unload_asr_model` | 模型 | — | `ModelsInfo` | 释放 + 清 `lastAsrModel` |
| 20 | `cancel_asr_download` | 模型 | — | `bool` | 置取消标志 |
| 21 | `transcribe_wav` | 模型 | — | `String \| null` | WAV 文件对话框 + 识别 |
| 22 | `separation_models` | 分离 | — | `SeparationCatalog { vocals[], accompaniment }` | 扫描随包 + 自定义模型 |
| 23 | `generate_subtitles` | 字幕 | `key, separation_model, separation_target?` | `SubtitleResult` | 分离 + 归一化 + ASR + 落盘 |
| 24 | `export_subtitle` | 字幕 | `key, format` | `SubtitleExportResult \| null` | 保存对话框 + 写文件 |
| 25 | `subtitle_for_track` | 字幕 | `key` | `StoredSubtitle \| null` | 读记录（重建 srt/lrc） |
| 26 | `list_subtitles` | 字幕 | — | `StoredSubtitle[]` | 扫描配置目录 |
| 27 | `delete_subtitle` | 字幕 | `key` | `bool` | 删 JSON + 临时 WAV + 分离目录 |
| 28 | `reveal_subtitle_audio` | 字幕 | `key, which` | `()` | 定位 vocals/accompaniment/srt/lrc |
| 29 | `update_subtitle` | 字幕 | `key, segments` | `StoredSubtitle` | 校验 + 原子覆盖 + `updatedAtMs` |
| 30 | `update_subtitle_text` | 字幕 | `key, format, text` | `StoredSubtitle` | 解析 + 同上 |
| 31 | `set_desktop_lyrics_enabled` | 歌词 | `enabled: bool` | `LyricsControl` | 建/关窗口 + 持久化开关 |
| 32 | `sync_desktop_lyrics` | 歌词 | `snapshot: LyricsSnapshot` | `()` | 存快照 + 广播 |
| 33 | `desktop_lyrics_snapshot` | 歌词 | — | `LyricsBootstrap` | 纯读 |
| 34 | `keep_desktop_lyrics_on_top` | 歌词 | — | `bool` | 重新声明置顶 |
| 35 | `resize_desktop_lyrics` | 歌词 | `width, height` | `()` | 钳制 + 调整 + 广播尺寸 |
| 36 | `toggle_desktop_lyrics_settings` | 歌词 | — | `()` | 开/关 380×460 悬浮窗 |
| 37 | `place_desktop_lyrics` | 歌词 | `width, height` | `()` | 底部居中 |
| 38 | `set_desktop_lyrics_lock` | 歌词 | `locked, hot_zone?` | `()` | 穿透 + 热区轮询 |
| 39 | `export_info` | 导出 | — | `ExportInfo { ffmpeg, presets }` | 探测 ffmpeg |
| 40 | `export_track` | 导出 | `key` | `ExportResult \| null` | 保存对话框 + 导出 |
| 41 | `export_tracks` | 导出 | `keys` | `ExportBatchResult \| null` | 目录对话框 + 批量导出 |
| 42 | `create_category` | 分类 | `name, id?` | `CategoryOutcome` | 写 categories + 建目录 + 重扫 |
| 43 | `update_category` | 分类 | `id, name, color?` | `CategoryOutcome` | 写 categories + 重扫 |
| 44 | `open_categories_file` | 分类 | — | `()` | 定位文件 |
| 45 | `import_tracks` | 曲目 | `category` | `LibraryMutation \| null` | 文件对话框 + 复制 + 重扫 |
| 46 | `update_track` | 曲目 | `key, title, category` | `LibraryMutation` | 重命名/移动 + 迁移 |
| 47 | `move_tracks` | 曲目 | `keys, category` | `LibraryMutation` | 批量移动 + 迁移 |
| 48 | `delete_tracks` | 曲目 | `keys` | `LibraryMutation \| null` | 系统确认 + 软删除 + 迁移 |
| 49 | `api_info` | 开放接口 | — | `ApiStatus` | 纯读 |
| 50 | `api_endpoints` | 开放接口 | — | `ApiCatalog`（`categories[]` + `endpoints[]`） | 纯读 |
| 51 | `update_api_config` | 开放接口 | `token, cors` | `ApiStatus` | 写 config + 立即生效 |
| 52 | `api_publish_state` | 开放接口 | `snapshot: Value` | `()` | 缓存 + SSE 广播 |
| 53 | `api_reply` | 开放接口 | `id, ok, data?, error?, code?` | `()` | 唤醒等待中的 HTTP 请求 |
| 54 | `apply_desktop_settings` | 桌面 | `autostart, minimize_to_tray` | `()` | 托盘 + 自启同步，失败回滚 |
| 55 | `hide_to_tray` | 桌面 | — | `()` | 建托盘 + 隐藏窗口 + 开启托盘偏好 |
| 56 | `quit_app` | 桌面 | — | `()` | 置退出标记 + 退出 |
| 57 | `builtin_wallpapers` | 壁纸 | — | `WallpaperAsset[]` | 定位 + 授权资源 |
| 58 | `choose_wallpaper_file` | 壁纸 | — | `WallpaperPick \| null` | 文件对话框 + 校验 + 授权 |
| 59 | `wallpaper_media_url` | 壁纸 | `path` | `string \| null` | Linux 视频流地址 |
| 60 | `audio_index_status` | 音频索引 | — | `AudioIndexStatus` | 查询当前索引 |
| 61 | `build_audio_index` | 音频索引 | — | `AudioIndexStatus \| null` | 首次确认、建立索引 |
| 62 | `disable_audio_index` | 音频索引 | — | `AudioIndexStatus \| null` | 确认并删除索引 |
| 63 | `audio_index_for_song` | 音频索引 | `key, path` | `AudioIndexInfo \| null` | 查询曲目缓存 |
| 64 | `find_audio_duplicates` | 音频索引 | — | `DuplicateReport` | 扫描重复项 |
| 65 | `search_library_by_sound` | 音频索引 | `limit?` | `SoundSearchResult \| null` | 选择查询文件 |
| 66 | `search_library_by_recording` | 音频索引 | 原始字节、可选 `x-sound-search-limit` | `SoundSearchResult` | 用临时录音查询 |
| 67 | `delete_audio_duplicates` | 音频索引 | `paths[]` | `LibraryMutation \| null` | 确认后软删除选中副本 |
| 68 | `delete_asr_model` | ASR | `id` | `ModelsInfo` | 释放并删除指定模型，清理对应恢复目标 |
| 69 | `parse_playlist_migration_source` | 歌单迁移 | `url` | `Playlist` | 读取公开歌单 |
| 70 | `save_playlist_migration_text` | 歌单迁移 | `text` | `string \| null` | 保存原样 UTF-8 文本并返回路径 |
| 71 | `tts_models` | TTS | — | `TtsModel[]` | 查询安装与加载状态 |
| 72 | `install_tts_model` | TTS | `id, jobId` | `()` | 安装与下载进度 |
| 73 | `open_model_download_source` | 模型 | `tts` | `()` | 打开对应下载源 |
| 74 | `save_model_download_source` | 模型 | `tts, url` | `string` | 校验并保存，返回生效地址 |
| 75 | `load_tts_model` | TTS | `id` | `()` | 加载引擎 |
| 76 | `remove_tts_model` | TTS | `id, deleteFiles` | `()` | 卸载，按需删除模型文件 |
| 77 | `parse_tts_script` | TTS | `text, format` | `TtsSegment[]` | 校验并解析 `txt`/`srt`/`lrc` |
| 78 | `normalize_tts_reference` | TTS | 原始字节 | 原始 WAV 字节 | 归一化参考音频 |
| 79 | `list_tts_builtin_voices` | TTS | — | `string[]` | 枚举内置参考 WAV |
| 80 | `load_tts_builtin_reference` | TTS | `voice` | `{ audio: number[], text: string \| null }` | 读取参考音频与逐字稿 |
| 81 | `generate_tts` | TTS | `request: { family, modelId, voiceId, segments, referenceAudio?, referenceText? }, jobId` | `SynthesisResult` | 合成并缓存预览 |
| 82 | `cancel_tts_job` | TTS | `kind: install \| synthesis, jobId` | `()` | 请求取消对应任务 |
| 83 | `save_tts_audio` | TTS | `name` | `bool` | 保存 WAV，取消返回 `false` |

## 附录 B：事件全表

| 事件 | 方向 | 载荷 | 触发时机 | 订阅方 |
| :--- | :--- | :--- | :--- | :--- |
| `library-changed` | Rust → 主窗 | `{ library, added[], removed[], moved[] }` | watch 检测到内容变化 | `App.vue` → `applyExternalChange` |
| `api-command` | Rust → 主窗 | `{ id, action, params }` | HTTP 控制请求 | `api.ts` → `runAction` |
| `model-download-progress` | Rust → 主窗 | `{ file, index, totalFiles, received, fileBytes, overallReceived, overallBytes, done, error }` | 下载中每 240ms / 每文件完成 / 失败 | `models.ts` |
| `model-load-status` | Rust → 主窗 | `{ phase, path, error }` | 加载开始/成功/失败（含自动加载与恢复） | `models.ts` → 刷新 `ModelsInfo` |
| `subtitle-progress` | Rust → 主窗 | `{ stage, message, current, total, error? }` | 分离/归一化/分片识别/完成/失败 | `subtitles.ts` |
| `desktop-lyrics-control` | Rust → 主窗 | `{ enabled }` | 歌词窗口开关变化（含用户关窗） | `desktop-lyrics.ts` |
| `desktop-lyrics-data` | Rust → 歌词窗 | `LyricsSnapshot` | 主窗 `sync_desktop_lyrics` | `lyrics.ts` |
| `desktop-lyrics-size` | Rust → 主窗 | `{ width, height }` | 用户拖拽窗口 / `resize` 命令 | `lyrics.ts` / 设置窗 |
| `desktop-lyrics-hover` | Rust → 歌词窗 | `{ hovered }` | 锁定态热区进入/离开 | `lyrics.ts` |
| `desktop-lyrics-settings-state` | Rust → 歌词窗 | `{ open }` | 设置悬浮窗开/关 | 歌词窗 Aa 按钮 |
| `desktop-lyrics-style` | 歌词窗 ↔ 设置窗 | `LyricsStyle` | 任一侧修改样式 | 仅窗口内（前端常量） |
| `desktop-lyrics-command` | 设置窗 → 歌词窗 | `LyricsCommand` | 锁定 / 恢复默认 | 仅窗口内（前端常量） |
| `audio-index-progress` | Rust → 主窗 | `{ current, total, title }` 或 `null` | 分析进度，本轮完成后清空 | `useAudioIndex.ts` |
| `audio-index-updated` | Rust → 主窗 | `string[]` | 同步完成、失败或清除索引，载荷为警告 | `useAudioIndex.ts`、歌曲详情 |
| `tts-download-progress` | Rust → 主窗 | `{ jobId, id, file, received, total }` | 模型下载中 | `tts.ts` |
| `tts-synthesis-progress` | Rust → 主窗 | `{ jobId, current, total }` | 语音合成进度 | `TtsWorkspace.vue` |

## 附录 C：HTTP 接口

**路由**（9 个业务分类 / 33 个方法+路径组合；`api::catalog()` 是唯一真源，界面「第三方接入」按 `categories()` 分组展示）

```
系统与状态
GET    /v1/health                  健康检查（不校验 token）
GET    /v1/state                   播放状态快照
GET    /v1/events                  全局 SSE（event = state / task / resync；?legacy=1 无名帧）

播放控制
POST   /v1/play                    播放（body.key / body.queue）
POST   /v1/pause
POST   /v1/resume
POST   /v1/toggle
POST   /v1/next
POST   /v1/previous
POST   /v1/seek                    body.seconds
POST   /v1/volume                  body.volume 0~1
POST   /v1/crossfade               body.seconds 0~12

曲库 / 音频输出
GET    /v1/library                 曲库内容（含 format）
POST   /v1/library/rescan          重新扫描
GET    /v1/outputs                 设备 + 路由
POST   /v1/outputs                 body.deviceId + enabled/volume/muted

字幕（key 一律走查询参数 ?key=，避免 分类/曲名 的路径转义）
GET    /v1/subtitles               记录列表（?q= / limit / offset；默认仅元信息，include=content 带正文）
GET    /v1/subtitles/track         ?key= 单条记录
PUT    /v1/subtitles/track         ?key= + body.segments[] 分段更新
PUT    /v1/subtitles/text          ?key=&format=srt|lrc + body.text 原文更新
DELETE /v1/subtitles/track         ?key= 删除记录并清理交付物
GET    /v1/subtitles/export        ?key=&format=srt|lrc，附件下载
POST   /v1/subtitles/generate      body.key + body.separationModel[+separationTarget]，202 + task

人声分离
GET    /v1/separation/models       人声模型列表 + 固定伴奏模型可用性
GET    /v1/separation/result       ?key= 读取 separation.json + 文件存在性/大小
POST   /v1/separation              body.key + body.model[+target]，202 + task

模型
GET    /v1/models                  模型清单 + 已加载模型 + 加载状态
POST   /v1/models/load             body.id，202 + task
POST   /v1/models/unload           释放模型

长任务
GET    /v1/tasks                   ?kind=&status=&limit=
GET    /v1/tasks/{id}              任务详情
GET    /v1/tasks/{id}/events       逐任务 SSE（snapshot / progress / done / failed）

通用
POST   /v1/command/{action}        body 即 params（或 body.params）
```

**长任务队列**（`tasks.rs`）：字幕生成 / 人声分离 / ASR 加载共用**串行**队列；字幕与分离另按曲目 key 进程内互斥（UI 与 HTTP 共用，避免并发写同一交付目录）；提交返回 `202` 与 `data.task`；
同 `kind + key` 未完成时重复提交返回 `409`；任务只存内存（保留最近 50 条已完成任务及在途任务），重启清空；暂不支持取消。

**SSE 帧**（`event_bus.rs` + `api.rs`）默认包含 `id:`、`event:`、`data:`，每 15 秒发保活注释。
`id` 进程内单调递增；客户端带 `Last-Event-ID`（或 `?lastEventId=`）时先补发缺口内事件，历史保留 256 条，
超出窗口先发 `event: resync`（`reason = window | reset`）；`?legacy=1` 退回无 `event:` 的旧帧（只推 `state`）；全局流 `event` 为 `state` / `task` / `resync`，
逐任务流重连时读取最新快照，事件为 `snapshot`、`progress`、`done`、`failed`，终态后服务端关闭连接。旧模式跳过历史重放。

**任务快照**包含 `{ id, kind, status, key?, stage, message, current, total, error?, createdAtMs, startedAtMs?, finishedAtMs?, result? }`。`kind` 为 `subtitles`、`separation`、`asrLoad`。`status` 为 `queued`、`running`、`succeeded`、`failed`。任务 SSE 的 `data` 为 `{ type: "task", task }`。

**通用 action（14）**：`state`、`play`、`pause`、`resume`、`toggle`、`next`、`previous`、`seek`、`volume`、`crossfade`、`outputs.list`、`outputs.refresh`、`outputs.unlock`、`outputs.set`。

**状态快照字段**

```
ready, playing, position, duration, volume, muted, rate, playMode, crossfadeSeconds,
track: { key, title, category, artist, album, format, durationMs, fileName, path } | null,
queue: { length, index },
outputs: [{ deviceId, label, volume, muted, enabled }],
outputDevices: [{ deviceId, label }],
library: { root, count }
```

**鉴权**：`Authorization: Bearer <token>` / `X-TFY-Token: <token>` / SSE `?token=`；`token` 为空时全部免鉴权，`/v1/health` 永远免鉴权。

**CORS**：`local`（默认，仅 `tauri://localhost`、`http(s)://tauri.localhost`、`localhost`、`127.0.0.1`、`[::1]`）/ `any` / `custom`（≤16 条，`*` 等价 any）；无 `Origin` 直接放行。

## 附录 D：键盘快捷键

| 按键 | 行为 | 备注 |
| :--- | :--- | :--- |
| `空格` | 播放 / 暂停 | 无当前曲目时从当前列表第一首开始；节目单时段内被拦截 |
| `←` / `→` | 后退 / 前进 | 默认 5 秒，取 `config.player.seekStepSeconds` |
| `↑` / `↓` | 音量 ± | 默认 0.1，取 `config.player.volumeStep` |
| `M` | 静音 / 恢复 | 恢复时若音量为 0 则设为 0.5 |
| `/` | 聚焦搜索并全选 | 输入框内不触发 |
| `Esc` | 逐层退出 | 队列抽屉 → 歌曲详情 → 清除批量选择 → 清空搜索 |

按键在 `INPUT` / `TEXTAREA` / `SELECT` / `contentEditable` 内一律让行（`/` 除外）；`空格` 在 `button` / `a` / `[role=button]` 上不拦截。

## 附录 E：组件 → 功能域

> 本表仅用于**反查主线实现位置**（某个功能落在哪个 `.vue`），**不是 gpui 的组件拆分要求**。gpui 可自由组织视图，只需保证表中功能域可达。

| 组件 | 功能域 |
| :--- | :--- |
| `TopBar` / `Sidebar` / `Workspace` / `HeroBanner` / `CategoryFilter` / `SongTable` / `SongRow` | F01 / F02 / F11 |
| `AudioPlayer` / `QueueDrawer` / `MarqueeText` / `OutputPanel` / `ExportSettingsPopover` | F05 / F06 / F07 / F09 |
| `SchedulePage` / `ScheduleEditor` / `SongPickerDialog` | F08 |
| `SongDetailsPanel` / `SongDetailsContent` / `DiagnosticsPanel` / `Equalizer` | F10（详情缓存关联 F19） |
| `audio-tools/` | F19 |
| `playlist-migration/` | F20 |
| `tts/` | F21 |
| `SettingsDialog` / `FontSelect` / `SettingSwitch` / `ClosePrompt` | F13 / F14 |
| `WallpaperLayer` / `WallpaperPopover` | F12 |
| `SubtitlePage` | F17（+ F18 开关入口） |
| `BatchBar` / `ContextMenu` / `ModalHost` / `ToastHost` / `WarningBanner` / `WindowControls` / `Icon` | 通用交互 |

## 附录 F：近期提交时序（由旧到新）

| 提交 | 内容 | 对应功能 |
| :--- | :--- | :--- |
| `7a5cec3` | ffmpeg 响度归一化导出与场景预设 | F09 |
| `b5cff69` | 文件监听、节目单与循环管理 | F04 / F08 / F05 |
| `b7e8119` | 节目单重构为歌曲级时间轴排程 | F08 |
| `5be86a4` | 播放状态显示与跑马灯 | F05 |
| `41a9bbc` | 节目单时段拦截手动点播并即时对齐 | F08 |
| `68e5ce5` | 导出设置简化为归一化目标选择 | F09 |
| `a5fdf16` | 修正 BS.1770 响度与爆音检测 | F10 |
| `469246d` | 本机 HTTP 开放接口 | F14 |
| `e10b593` | 基本系统配置页面与桌面集成 | F13 |
| `db22c65` | 字体选择改为扫描系统已安装字体 | F13 |
| `509c3e4` | 新增已实现功能文档（旧版） | 文档 |
| `a56d738` | 字幕生成与桌面歌词 | F15 / F16 / F17 / F18 |
| `8512025` | 桌面歌词设置改为独立悬浮窗 | F18 |
| `1e68843` | 字幕分段编辑与原文编辑 | F17 |
| `765fb97` | 修复控制标记后模型自由续写 | F15 |
| `499cb88` | 添加音频分离模型文件 | F16 |
| `c8276f2` | 强制人声分离前置与分离模型支持 | F16 / F17 |
| `e59ee18` | 双引擎并行分离，伴奏固定 UVR 乐器模型 | F16 |
| `8ba4f85` | HTTP 扩展、长任务、标准 SSE | F14–F17、F20 |
| `0aba015` | 歌曲详情与诊断入口整合 | F10 |
| `630b69b` / `923684a` | Wayland 歌词、Linux 视频壁纸、平台模块拆分 | F12、F13、F18 |
| `f4a5479` / `575ab5c` | 本地音频索引、后台队列同步 | F19 |
| `142c211` 至 `80faf7d` | TTS 工作台、动态音色、模型管理和脚本分段 | F21 |

---

## 维护约定

1. 新增功能时：在 §2 增加 Fxx 小节（13 字段齐全）、更新 §1.1 总览与 §0.3 统计、必要时补 §3 流程与 §8 验收。
2. 修改 `core/` 数据结构时：同步更新 §4 的字段表与 §5.2 的迁移契约。
3. 修改命令 / 事件 / HTTP 时：同步更新附录 A / B / C，并核对 `ui/src/lib/constants.ts` 与 Rust 常量是否一致。
4. 无法确认的内容必须标 `【待确认】` 并登记到 §7.3，**不得写成事实**。
5. 每次同步 `core/` 到 gpui 分支时，更新 §6.1 的行数对比表。
6. 判断「是否已对齐」只看当前分支的代码与行为；gpui 的现状只用于估算工作量，**不能作为降低标准的依据**（§0.0）。
7. UI 布局与视觉样式的改动**不需要**更新本文；只有当功能入口、状态语义或反馈含义变化时才需要。反之，任何为「样式对齐」而推迟功能实现的做法都不成立（§0.0 原则 6）。
