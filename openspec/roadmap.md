# GPUI 上游需求计划

计划依据为作者的[《已实现的功能文档》](https://chatgpt.com/library?file_id=libfile_0605bac180d8819181d1e69795c174c0&account_id=personal)，文档基线为 `bgm-tauri2@db22c65`。初始化计划时本地上游引用仍是 `63a0598`。开始 F01 前先取得上游源码并记录实际对齐的提交。

按下表依次实施。每个变更包含提案、设计、验收场景和任务清单，已有实现也要按该项的验收条件核验后再关闭。F13 放在相关业务完成之后做端到端合约验收。

| 顺位 | 条目 | OpenSpec 变更 | 当前实现基础 | 前置条目 |
| --- | --- | --- | --- | --- |
| 1 | F01 曲库扫描与分类 | [查看任务](changes/align-library-scanning/tasks.md) | 当前核心仅扫描 MP3，分类目录下只读取一层。GPUI 已有分类、搜索、缓存和内存拖排。 | 取得上游源码 |
| 2 | F03 标签与时长 | [查看任务](changes/align-track-metadata/tasks.md) | 当前只读取 MP3 标签与时长，已有元数据缓存和测试基线。 | F01 |
| 3 | F02 曲库文件操作 | [查看任务](changes/align-library-mutations/tasks.md) | 已有批量移动、导出与软删除，同名后缀和跨格式变更仍沿用旧核心。 | F01 |
| 4 | F04 曲库实时监听 | [查看任务](changes/verify-library-watcher/tasks.md) | GPUI 已接入监听、700 ms 防抖、后台扫描与 30 秒兜底。 | F01、F03 |
| 5 | F10 偏好与收藏 | [查看任务](changes/align-preferences-contract/tasks.md) | GPUI 应用层写入 v3 文件，目前仍复用 v2 共享结构做部分归一化。 | F01 |
| 6 | F05 播放核心 | [查看任务](changes/verify-playback-engine/tasks.md) | GPUI 已用后台整曲解码、双声部包络和采样级循环实现主要播放路径。 | F03、F10 |
| 7 | F08 导出与响度归一化 | [查看任务](changes/align-multiformat-export/tasks.md) | 当前 MP3 两遍归一化有实测结果，多格式原样复制和转码待按上游核对。 | F02、F03、F10 |
| 8 | F07 节目单调度 | [查看任务](changes/verify-scheduled-playback/tasks.md) | 已有节目单编辑、定时切换、存档和手动点播冲突处理。 | F05、F10 |
| 9 | F06 多输出设备 | [查看任务](changes/verify-multi-device-output/tasks.md) | 原生音频设备枚举和多路增益已接入，尚缺多设备实机端到端验证。 | F05、F10 |
| 10 | F09 可视化诊断 | [查看任务](changes/verify-audio-diagnostics/tasks.md) | 窗口已显示波形、频谱和响度，削波判定有单元测试。 | F05、F08 |
| 11 | F11 界面壁纸 | [查看任务](changes/align-video-wallpapers/tasks.md) | 静态、自定义、渐变、透明度及图像模糊已可用。MP4 当前只显示首帧。 | F10 |
| 12 | F12 系统设置与桌面集成 | [查看任务](changes/align-desktop-settings/tasks.md) | GPUI 已接入 macOS 字体、登录启动、菜单栏和防休眠。部署目标为 macOS 26+。 | F05、F06、F10 |
| 13 | F13 本机 HTTP 开放接口 | [查看任务](changes/align-loopback-http-api/tasks.md) | 已有本机服务与 14 个进程内路由测试。和新上游的外部接口兼容性待端到端核对。 | F01、F04、F05、F06、F07、F10、F12 |

## 工作方式

新增需求先创建一个 change，完成提案、设计、验收场景与任务。实现与测试完成后逐条勾选任务，再用 CLI 验证并归档。归档后的契约进入 `openspec/specs/`。
