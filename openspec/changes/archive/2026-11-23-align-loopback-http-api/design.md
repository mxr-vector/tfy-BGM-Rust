## Context

作者文档 `e59ee18` 记录了 16 条路由声明、17 个方法与路径组合和 14 个通用动作。实施时用 Git 核对 `bgm-tauri2@8ba4f85` 的 Rust 路由、事件总线和前端快照。该提交已经加入字幕、人声分离、ASR 和长任务路由。

[附录命令与事件映射](appendix-mapping.md)记录作者文档中的 58 个命令和 12 个事件在 GPUI 中如何执行、落盘和同步。第三方接入设置按业务展示 33 个方法与路径组合。

## HTTP

| 请求 | 返回内容 |
| --- | --- |
| `GET /v1/health` | `name`、`version`、`api`、`running`、`ready`、监听端口。 |
| `GET /v1/state` | 播放、队列、曲目、音量、时长、输出设备及曲库状态。准备前返回 503。 |
| `GET /v1/library`、`POST /v1/library/rescan` | 曲库快照，曲目包含小写扩展名 `format`。 |
| `POST /v1/play` | 接收 `key` 及可选 `queue`，返回播放快照。无有效自定义队列时使用全曲库。 |
| `POST /v1/pause`、`/v1/resume`、`/v1/toggle`、`/v1/next`、`/v1/previous` | 播放控制及执行后的快照。 |
| `POST /v1/seek`、`/v1/volume`、`/v1/crossfade` | 分别接收秒数、音量和淡入淡出秒数，返回更新后的快照。 |
| `GET /v1/outputs`、`POST /v1/outputs` | 主线输出快照字段 `supported`、`labelsAvailable`、`routed`、`error`、`devices`、`routes`，兼容已有 GPUI 字段。 |
| `POST /v1/command/{action}` | 支持通用动作及 `{"params": {…}}` 请求体。 |
| `GET /v1/events` | 通过 SSE 推送播放快照及后台任务更新。 |

JSON 成功和错误响应分别使用 `{"ok":true,"data":…}` 和 `{"ok":false,"error":"…"}`。HTTP 命令通过有界队列交给 GPUI，未处理命令在 6 秒后返回 504。节目单拦截和输出上限继续使用 409。

## 认证和事件

保留 IPv4 回环监听和回环 Host 校验。设置令牌后保护业务路由，支持 Bearer、`X-TFY-Token`，以及 SSE 查询参数 `token`。API 配置读取 GPUI 旧平铺 CORS 格式，保存时使用主线的 `api.cors.mode` 和 `api.cors.origins`。自定义来源在本机来源基础上追加，令牌与允许来源热更新。

SSE 帧包含递增 `id`。播放快照使用 `state` 和 `{"type":"state","state":…}`，任务更新使用 `task` 和 `{"type":"task","task":…}`。总线保留 256 条事件，`Last-Event-ID` 或 `lastEventId` 可请求续传。历史淘汰或服务重启导致的 ID 差异通过 `resync` 告知。旧客户端可用 `legacy=1` 接收无事件名的播放帧。空闲流定期发送心跳，撤销令牌或来源时立即断开旧流。

## 字幕与分离记录

| 请求 | 返回内容 |
| --- | --- |
| `GET /v1/subtitles` | 支持 `q`、`offset`、`limit` 和 `include=content`。默认仅返回元信息及 `segmentCount`，按创建时间倒序。 |
| `GET /v1/subtitles/track?key=…` | 完整记录、分段及 SRT/LRC 文本。 |
| `PUT /v1/subtitles/track?key=…` | 修改 `segments`，返回已保存的记录。 |
| `PUT /v1/subtitles/text?key=…` | 接收 `text` 和可选 `format`，支持 SRT 与 LRC，返回已保存的记录。 |
| `DELETE /v1/subtitles/track?key=…` | 删除字幕与其分离产物，返回 `key` 和 `deleted`。原始曲目和文件保留。 |
| `GET /v1/subtitles/export?key=…` | 下载指定格式，返回 UTF-8 文本及带 Unicode 文件名的 `Content-Disposition`。 |
| `GET /v1/separation/result?key=…` | 读取已落盘的分离记录，并返回两条音轨现有文件状态和字节数。 |

记录操作复用回环鉴权和 GPUI 命令通道，文件操作在后台执行。写入使用字幕记录锁和原有的文件发布流程，曲目正在生成字幕或分离时返回 409。成功写入后刷新字幕列表和当前歌词。分离结果通过稳定键验证目录归属，文件信息来自对应分离目录。

## 模型与长任务

| 请求 | 参数与返回内容 |
| --- | --- |
| `POST /v1/subtitles/generate` | 接收 `key`、`separationModel` 和可选的 `separationTarget`，提交字幕生成任务。 |
| `POST /v1/separation` | 接收 `key`、`model` 和可选的 `target`，提交人声分离任务。 |
| `GET /v1/separation/models` | 返回人声模型目录、内置模型和自定义模型的可用情况。 |
| `GET /v1/models` | 返回 ASR 模型目录、文件完整性及加载状态。 |
| `POST /v1/models/load` | 接收 `id`，按已校验的模型目录提交加载任务。 |
| `POST /v1/models/unload` | 释放模型并清除启动恢复项，返回 `unloaded` 与更新后的 `info`。 |
| `GET /v1/tasks` | 按创建时间倒序列出任务，支持 `kind`、`status` 和 `limit`。 |
| `GET /v1/tasks/{id}` | 返回任务状态、阶段、进度及最终结果或错误。 |
| `GET /v1/tasks/{id}/events` | 首帧返回当前 `snapshot`，再推送最新 `progress`，在 `done` 或 `failed` 后关闭。已结束的任务直接返回终态。 |

任务提交返回 `202`、`data.task` 和指向详情的 `Location`。活动任务重复提交返回 `409`。后台队列串行执行，与界面共用模型及媒体处理流程。任务失败会释放队列，后续任务继续执行。单任务 SSE 支持查询参数 `token`。

## 待验收

当前测试通过真实回环套接字验证 HTTP 和 SSE，业务动作分派由测试接收端验收。完整应用的播放、节目接管、输出设备联动、外部脚本及正式 ASR 与分离运行库仍待在可启动 GPUI 的 macOS 会话验收。
