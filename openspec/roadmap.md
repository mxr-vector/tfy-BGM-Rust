# GPUI 上游对齐计划

依据作者提供的[《gpui 迁移功能说明文档》](https://chatgpt.com/library?file_id=libfile_2e1e06852724819182c5784379a24ec9&account_id=personal)。文档基线为 `bgm-tauri2@e59ee18`，日期为 2026-11-22。本轮同步采用主线 `bgm-tauri2@8ba4f85`，`core/` 已原样同步该提交。

沿用新版文档 F01–F18 编号。按前置关系逐项推进，已有 GPUI 行为须用新版验收场景核对。共享核心从确认过的 `src-tauri/core` 原样同步，前端行为用 Rust 和 GPUI 实现。验收覆盖功能入口、状态反馈、互换数据及听感，界面沿用本项目的独立设计。

| 顺位 | 功能域 | OpenSpec 变更 | 当前实现与待补差异 | 前置 |
| --- | --- | --- | --- | --- |
| 1 | F01 曲库扫描与分类 | [归档任务](changes/archive/2026-11-23-align-library-scanning/tasks.md) | 已验证四种来源、外置目录回退、分类单层扫描、五格式及同名优先级、中文排序和推荐拖排。 | 共享核心同步 |
| 2 | F03 标签与时长 | [已归档](changes/archive/2026-11-23-align-track-metadata/tasks.md) | 已接入共享五格式解析，验证艺人、时长和文件替换后的缓存失效。缓存上限由共享核心实现。 | F01 |
| 3 | F11 偏好与收藏 | [归档任务](changes/archive/2026-11-23-align-preferences-contract/tasks.md) | 已对齐 v4 的 34 字段、旧版迁移、四个权威字段和核心级双向读取。通过 280 ms 防抖与强制写回测试，退出和托盘生命周期待实机验收。 | 共享核心同步 |
| 4 | F02 曲库文件操作 | [归档任务](changes/archive/2026-11-23-align-library-mutations/tasks.md) | 已验证多格式写操作、部分成功报告、当前曲目换键、同名冲突、软删除恢复和六处引用迁移。播放中操作和原生结果窗口待实机验收。 | F01、F11；字幕与分离迁移联验 F17 |
| 5 | F04 曲库实时监听 | [归档任务](changes/archive/2026-11-23-verify-library-watcher/tasks.md) | 已验证批量增量、改名和移动分类、切库隔离、30 秒兜底计时及离线恢复保护。真实 FSEvents 批量刷新时延待实机验收。 | F01–F03 |
| 6 | F05 播放引擎 | [归档任务](changes/archive/2026-11-23-verify-playback-engine/tasks.md) | 已验证五格式解码、请求取消、自然播完的淡入、路径变化后的恢复、0.25× 至 4× 保音高倍速与失败跳过。待实机验收切歌听感与提示。 | F03、F11 |
| 7 | F06 队列与智能随机 | [归档任务](changes/archive/2026-11-23-align-playback-queue/tasks.md) | 已对照新版源码核对队列状态、48 条/45 分钟历史和随机权重，验证重复条目的按位移除和曲目换键后的权重保留。待实机联验播放状态。 | F03、F05 |
| 8 | F08 节目单调度 | [归档任务](changes/archive/2026-11-23-verify-scheduled-playback/tasks.md) | 已按主线 `8ba4f85` 复核保存/启用即对齐、歌曲时间裁剪和休眠对齐。当前曲目从曲库移除后回退到可用的已到点曲目。待实机验收最小化、休眠、调度播放与界面/API 拦截。 | F05、F06、F11 |
| 9 | F09 导出与响度归一化 | [归档任务](changes/archive/2026-11-23-align-multiformat-export/tasks.md) | 已按主线 `8ba4f85` 复核四档响度、编码失败回退和批量同名保护。从实际曲库扫描结果验证五格式建议名、原样复制和批量转 MP3。待实测原生保存框、目录选择器与结果提示。 | F02、F03、F11 |
| 10 | F15 ASR 模型管理 | [归档任务](changes/archive/2026-11-23-align-asr-models/tasks.md) | 已接九文件下载、独立进程加载、启动恢复和 WAV 分片识别。已验证异常会话失效、只复用仍在运行的进程及拒绝不兼容运行库。正式 2.0.2 动态库及 Qwen3 已验证加载、重启恢复和歌曲识别。 | F11 |
| 11 | F16 人声分离引擎 | [归档任务](changes/archive/2026-11-23-align-source-separation/tasks.md) | 已接曲目菜单、双引擎、模型预检和原子产物替换。已用正式运行库验证 UVR 分离落盘，原生菜单待验收。 | F03、F15 |
| 12 | F17 字幕生成与编辑 | [归档任务](changes/archive/2026-11-23-align-subtitle-workflow/tasks.md) | 已接入双轨分离、仅人声识别、字幕进度和落盘，以及编辑、导出和稳定键迁移。正式模型已通过本机接口完成双轨分离、识别和 SRT/LRC 落盘，原生入口待联验。 | F02、F04、F15、F16 |
| 13 | F07 多输出设备 | [归档任务](changes/archive/2026-11-23-verify-multi-device-output/tasks.md) | 已验证默认输出缺失时继续播放、设备重接的游标接续、独立静音和路由恢复。待双设备实测热插拔和长期同步。 | F05、F11 |
| 14 | F10 音频诊断 | [归档任务](changes/archive/2026-11-23-verify-audio-diagnostics/tasks.md) | 已验证响度双门限、72 段频谱平滑、瞬时响度与下混峰值窗口、三档异常判定。待实机验收诊断窗口与循环标记。 | F05、F09 |
| 15 | F12 界面壁纸 | [归档任务](changes/archive/2026-11-23-align-video-wallpapers/tasks.md) | 已接静音循环、窗口暂停和缺失资源回退，验证软件解码循环、暂停续播及偏好恢复。待实机验收原生出帧和窗口交互，并验证完整应用包。 | F11 |
| 16 | F13 系统设置与桌面集成 | [归档任务](changes/archive/2026-11-23-align-desktop-settings/tasks.md) | 已核对四页设置，补平台说明、菜单栏回退和屏幕唤醒生命周期。已验证启动项文件、偏好往返及 IOKit 锁，待实测窗口、登录和输出设备。 | F05、F07、F11 |
| 17 | F18 桌面歌词 | [归档任务](changes/archive/2026-11-23-align-desktop-lyrics/tasks.md) | 已接入 macOS 置顶歌词窗、锁定穿透、独立样式窗和 v4 偏好迁移。已验证启用状态写回，置顶、穿透及样式待实机验收。 | F05、F11、F15、F17 |
| 18 | F14 本机 HTTP 与接口验收 | [归档任务](changes/archive/2026-11-23-align-loopback-http-api/tasks.md) | 已接入 33 个方法与路径组合，通过回环测试。附录命令和事件已逐项映射，设置页提供完整接口目录。正式模型和字幕任务已联验，映射表的原生操作待验收。 | 相关功能域完成 |

F02 与 F17 的集成测试共用曲目稳定键，验证手动恢复及派生文件迁移失败后的原数据。F04 验收外部变化时采用相同约定。

## 软件验收

已用无窗口 GPUI 工作区、实际回环 HTTP、主线分离模型文件及测试 C API 动态库联验节目单冲突、输出路由、ASR 加载恢复、Spleeter/UVR、字幕发布和任务 SSE。桌面歌词主动释放偏好已通过持久化测试。签名测试包已验证 macOS 26.0 门槛和壁纸、模型、动态库资源。

本轮变更已归档，正式规格收录于 `openspec/specs/`。实际设备、原生窗口和正式打包的待验收项保留在各自归档任务中。

## 跨功能契约

- 配置使用 `prefs.json` v4、`config.json`、`categories.json`、`schedule.json` v2、`loops.json` v1。互换验收覆盖字段、默认值、非法输入和旧数据迁移。GPUI 专属外观偏好单独存放。
- 派生数据包括 `subtitles/`、`separations/` 与曲库的 `.tfy-trash/`。稳定键关联收藏、循环点、节目单、队列、字幕记录与分离目录。改名、移动、软删除、同名替换分别验收。
- 按附录 A 核对 **58 个命令**的参数、结果和错误语义，按附录 B 核对 **10 个后端事件和 2 个歌词窗口内部事件**。GPUI 可由原生动作、通道或回调承载。
- 按附录 C 核对 **16 个路由声明、17 个方法与路径组合、14 个通用 action**。验收包含 token、CORS、SSE、请求超时、冲突和状态快照。
- 文档 §8.3 的批次门槛依次覆盖共享核心与偏好互换、写操作与循环、调度与导出、模型到字幕全链路、桌面与接口。验收通过的任务已勾选，剩余事项见各归档清单。

## GPUI 独立界面

曲库、场景封套和歌曲页的现有验收见[曲库工作区契约](specs/music-workspace/spec.md)。新入口沿用[界面设计规范](../docs/ui-design.md)。

- [播放与设置交互归档](changes/archive/2026-11-23-refine-player-settings/tasks.md)
- [播放页滚动歌词与切歌归档](changes/archive/2026-11-23-scroll-now-playing-lyrics/tasks.md)
- [播放、诊断与壁纸内存优化归档](changes/archive/2026-11-24-reduce-gpui-memory-retention/tasks.md)
- [工作区交互升级与原生验收](changes/archive/2026-11-27-refine-desktop-experience/tasks.md)
- [曲库不可读与重连恢复归档](changes/archive/2026-11-28-refine-library-recovery/tasks.md)
- [多音乐文件夹与界面交互升级归档](changes/archive/2026-11-28-upgrade-library-folders-and-ux/tasks.md)


## 上游最新同步（`origin/bgm-tauri2@0f2cc01`）

已完成基于主线 `bgm-tauri2@0f2cc01` 的业务与共享核心全量同步：`core/` 已原样同步该提交（仅在 `#[cfg(test)]` 下保留 2 处 macOS 测试兼容守卫，非测试代码逐字节一致），同步 `prefs.json` v5（`ttsDownloadUrls`）、`bgm_core::ops::trash_indexed_files`、`bgm_core::loudness::normalize_wav`、`bgm_core::process`，并完整接入内置 11 个参考音色（`models/voices/` + `prompts.json`）及新版内置壁纸（`assets/bg/` 动态发现、移除旧版 `1.mp4`/`2.jpg`）、曲目菜单「在 Finder 中显示」、应用单实例锁与唤起聚焦。

| 功能 | OpenSpec | 当前进度 |
| --- | --- | --- |
| F19 音频索引、以声搜声与查重 | [归档任务](changes/archive/2026-11-28-align-audio-index/tasks.md) | 已对齐主线行为与纯 Rust FFT/subband 指纹引擎，完成真实多格式建索引、7.99s 片段命中（91%）、3 曲相似聚组（100%）与 `.tfy-trash/` 按文件软删除实测（保留收藏与循环点）。 |
| F20 公开歌单迁移 | [归档任务](changes/archive/2026-11-28-align-playlist-migration/tasks.md) | 已对齐网易云、QQ 音乐（适配 `i.y.qq.com/qzone-music`）、汽水音乐抓取、SSRF/Redirect 防护与 Fake-IP TUN 下 DoH 公网解析回退，完成三平台真实公开歌单抓取实测。 |
| F21 文生语音 | [归档任务](changes/archive/2026-11-28-align-text-to-speech/tasks.md) | 已对齐 5 种官方模型规格与校验、`prefs.ttsDownloadUrls` 持久化和下载源验证、Kokoro v1.1（103 说话人）、ZipVoice 共享词典/声码器、CosyVoice3 Python 3.11 CPU 运行时、11 个内置参考音色及缺逐字稿拦截（`0f2cc01`）；已完成 Kokoro INT8、ZipVoice INT8 与 CosyVoice3 ONNX 真实下载与 CPU 合成实测。已修复长脚本导出误触发参考音频的 15 秒限制，并用 Kokoro INT8 验证约 72 秒 WAV 输出。当前结果就绪后，底部主按钮切换为试听。 |
