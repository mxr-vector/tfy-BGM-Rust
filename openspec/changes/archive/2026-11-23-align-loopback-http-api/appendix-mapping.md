# 作者文档命令与事件在 GPUI 中的对应实现

命令编号和事件名称沿用作者文档的附录 A、B。运行行为参考本地 Git 的 `bgm-tauri2@8ba4f85`。下表核对功能入口、数据落点和状态传播。曲库与偏好互换、原生窗口、设备和正式模型以各功能域的验收清单为准。

## 附录 A 命令

| # | 主线命令 | GPUI 入口与行为 | 对齐备注 |
| ---: | --- | --- | --- |
| 1 | `bootstrap` | `Library::open` 读取配置、偏好、曲库、节目单和循环点，`Workspace::new` 初始化播放器和平台能力。 | 启动时直接消费 Rust 数据。 |
| 2 | `reload_config` | 界面重新扫描和 HTTP `library.rescan` 重读配置并同步原生标题、播放参数和 API 配置。 | 原生标题待实机验收。 |
| 3 | `open_config_file` | 常规设置的「应用配置」在访达选中 `config.json`。 | 待访达联验。 |
| 4 | `system_fonts` | `desktop::system_fonts` 提供字体选择器的原始族名与本地化名称。 | 待原生字体菜单联验。 |
| 5 | `save_prefs` | `Library::persist_prefs` 与 `save_extra_preference` 归一化并写回应用偏好，各设置入口传入其负责的字段。 | 34 字段和后端权威字段互换继续按 F11 验收。 |
| 6 | `save_schedule` | `save_schedule_draft` 调用 `Library::save_schedule`，校验重叠，持久化并重新对齐生效节目。 | 休眠恢复待实机验收。 |
| 7 | `save_loop_point` | 循环编辑器校验曲目和边界，保存到 `loops.json` 并刷新音源循环。 | 无缝听感待实机验收。 |
| 8 | `remove_loop_point` | 循环编辑器删除指定曲目的循环点并刷新循环状态。 | 复用节目运行中的循环切换。 |
| 9 | `rescan_library` | `Workspace::rescan_library` 重载分类、重扫、重建监听并更新界面与 HTTP 快照。 | 目录离线恢复和 FSEvents 由 F04 验收。 |
| 10 | `choose_library_dir` | 曲库目录选择器接受现存目录，保存后重扫、重新监听，取消则保留原目录。 | 待原生目录选择器联验。 |
| 11 | `reset_library_dir` | 曲库菜单恢复默认位置，清除 `libraryDir` 后重新解析、扫描与监听。 | 根解析优先级由 F01 验收。 |
| 12 | `open_library_dir` | 曲库菜单通过 `reveal_path` 将当前曲库位置交给访达。 | 待核对原生访达行为。 |
| 13 | `models_info` | 模型页和 `GET /v1/models` 复用 `ModelCatalog::scan`，提供完整性、默认模型与加载状态。 | 根目录单文件及嵌套模型已有返回字段测试。 |
| 14 | `choose_models_dir` | `choose_asr_directory` 选择目录后保存并重新扫描，目录变化释放当前会话。 | 待正式模型联验。 |
| 15 | `reset_models_dir` | 模型页调用 `apply_asr_directory(None)`，恢复默认选址并刷新目录。 | 与手动选择共用忙碌保护。 |
| 16 | `open_models_dir` | `open_asr_directory` 创建缺失目录后通知访达定位。 | 待原生联验。 |
| 17 | `download_asr_model` | `start_model_download` 启动九文件下载，完成后尝试自动加载，成功写入 `lastAsrModel`。 | 下载、续传和取消已有测试，正式权重待联验。 |
| 18 | `load_asr_model` | 模型页和 HTTP 加载任务复用 `start_asr_load`，按目录与模型 ID 加载运行时并记住已加载模型。 | 正式运行库和权重待联验。 |
| 19 | `unload_asr_model` | 模型页和 HTTP 释放会话并清除 `lastAsrModel`。 | 待正式模型、桌面歌词联验。 |
| 20 | `cancel_asr_download` | 模型页触发 `ModelDownload::cancel`，下载线程停止传输并清理临时文件。 | 已用回环下载测试验证。 |
| 21 | `transcribe_wav` | `choose_asr_wav` 选择 WAV，`transcribe_asr_wav` 调用当前会话并显示结果或错误。 | 真实推理待联验。 |
| 22 | `separation_models` | 曲目操作使用 `separation::Catalog::discover`，HTTP 使用同一库存生成 `Catalog::document`。 | 内置与自定义模型，伴奏可用性由 F16 验收。 |
| 23 | `generate_subtitles` | 曲目菜单、播放页、字幕页和 HTTP 后台任务复用 `subtitle_generation::generate`，分离后识别人声并发布字幕文件。 | 端到端结果与主线哈希归入 F17。 |
| 24 | `export_subtitle` | 播放页、字幕页按当前格式选择保存位置，HTTP 可指定格式并直接下载。 | 回环验证 UTF-8 导出，原生保存面板待验收。 |
| 25 | `subtitle_for_track` | `SubtitleEdit::open` 读取指定曲目的记录和分段，重建 SRT/LRC。 | 播放页、预览和 HTTP 详情共用记录。 |
| 26 | `list_subtitles` | `reload_subtitle_catalog` 和 HTTP 记录接口扫描配置目录。 | HTTP 搜索、分页及元数据在回环中验证。 |
| 27 | `delete_subtitle` | 字幕页二次确认后删除记录及派生物，HTTP 返回 `deleted`。 | 已验证重复删除。 |
| 28 | `reveal_subtitle_audio` | 字幕页预览提供人声、伴奏、SRT、LRC 四个访达入口。交由 `SubtitleEdit::delivery_path` 确认实际文件归属。 | 已验证合法路径与外部路径拒绝，访达待联验。 |
| 29 | `update_subtitle` | 字幕页分段编辑及 HTTP PUT 复用 `save_segments_record`，更新文本文件和时间戳。 | 已验证写回和并发旧版本拒绝。 |
| 30 | `update_subtitle_text` | 播放页原文编辑及 HTTP PUT 复用 `save_text_record`，解析后执行相同写回。 | 已验证错误输入保护和两个格式刷新。 |
| 31 | `set_desktop_lyrics_enabled` | `open_desktop_lyrics` 与 `close_desktop_lyrics` 校验模型，管理窗口并持久化启用偏好。 | 显式关窗和应用退出需实机联验。 |
| 32 | `sync_desktop_lyrics` | `poll_desktop_lyrics` 由当前曲目时间轴和播放时钟计算行与逐词高亮，直接更新歌词窗口。 | 时间轴测试覆盖跳转、暂停与倍速。 |
| 33 | `desktop_lyrics_snapshot` | 打开窗口时提供当前样式和初始占位，随后从 `poll_desktop_lyrics` 读取当前歌词。 | 窗口初帧待实机验收。 |
| 34 | `keep_desktop_lyrics_on_top` | `configure_lyrics_window` 设置原生层级，`lyrics_pointer` 轮询恢复层级。 | 多窗口、空间和全屏待实机验收。 |
| 35 | `resize_desktop_lyrics` | `apply_desktop_lyrics_style` 调整窗口，拖拽通过 `lyrics_window_size` 读回并延时保存。 | 实机拖拽待验。 |
| 36 | `toggle_desktop_lyrics_settings` | `toggle_lyrics_settings` 打开或关闭独立设置窗。 | GPUI 按独立布局设置尺寸，原生锚定待验。 |
| 37 | `place_desktop_lyrics` | `configure_lyrics_window` 通过 `fit_lyrics_window` 在屏幕可见区底部居中。 | 多屏与 Dock 位置待验。 |
| 38 | `set_desktop_lyrics_lock` | 样式锁定状态驱动 `lyrics_pointer` 切换鼠标穿透，保留解锁热区。 | 解锁热区跟随 GPUI 窗口布局，原生行为待验。 |
| 39 | `export_info` | 导出设置读取 FFmpeg 探测结果、归一化开关和预设。 | 四档响度及 FFmpeg 回退已有测试。 |
| 40 | `export_track` | `export_key` 通过保存面板调用 `Exporter`。 | 保存面板待实机。 |
| 41 | `export_tracks` | `export_selected` 准备批量请求并选择输出目录，逐项返回摘要。 | 批量冲突及保留源格式已有测试。 |
| 42 | `create_category` | 分类编辑器调用 `Library::create_category`，写入配置、建目录并重扫。 | 分类细则由 F01/F02 验收。 |
| 43 | `update_category` | 分类编辑器调用 `Library::update_category`，写入名称及颜色后重扫。 | 分类校验依赖当前 `core/`。 |
| 44 | `open_categories_file` | 常规设置的「分类配置」在访达选中 `categories.json`。 | 待访达联验。 |
| 45 | `import_tracks` | `import_files` 选择音频并调用 `Library::import` 导入当前分类。 | 批量冲突和共享核心五格式规则由 F02 验收。 |
| 46 | `update_track` | 曲目菜单「编辑曲目」可同时改名和改分类，`Library::update_track` 只生成一条 `KeyChange`。 | 已验证成功迁移收藏和循环点，以及目标冲突时保留原文件。 |
| 47 | `move_tracks` | 批量移动通过 `Library::move_batch` 执行并迁移引用。 | 冲突策略与旧数据迁移由 F02 验收。 |
| 48 | `delete_tracks` | `trash_keys` 弹系统确认，调用 `Library::trash_batch` 软删除并更新引用。 | 回收站恢复和磁盘失败场景由 F02 验收。 |
| 49 | `api_info` | `integration_settings` 展示服务状态及地址，`GET /v1/health` 返回监听与就绪状态。 | 启停与端口变更待应用联验。 |
| 50 | `api_endpoints` | `api::catalog::GROUPS` 给「第三方接入」提供分类、方法、路径、用途。 | 共 33 种方法与路径组合，与本地 Git 的上游目录核对。 |
| 51 | `update_api_config` | 设置写回 `config.json` 后调用 `Server::update_config` 热加载令牌与跨域来源。 | 已验证热更新断开旧 SSE。 |
| 52 | `api_publish_state` | `Workspace::publish_api` 把状态和曲库快照交给服务，状态变化进入 SSE。 | 已验证断线续传。 |
| 53 | `api_reply` | `poll_api` 通过一次性回复通道将数据或错误返给请求。 | 回环已验证正常请求、错误映射、超时与取消。 |
| 54 | `apply_desktop_settings` | 常规设置调用 `set_tray_settings` 并对接 macOS 启动项。 | 实际登录与托盘回滚待联验。 |
| 55 | `hide_to_tray` | 关闭提示或菜单栏入口创建或复用托盘，然后调用 `hide_to_tray`。 | 待实测隐藏后恢复及保留设置。 |
| 56 | `quit_app` | 全局退出动作、菜单栏及关闭提示调用 `cx.quit`，退出观察器收尾。 | 待验证退出时歌词和音频释放。 |
| 57 | `builtin_wallpapers` | 壁纸面板使用内置静态图片和 `wallpaper::builtin_video` 发现随包视频。 | 应用包资源和原生出帧由 F12 验收。 |
| 58 | `choose_wallpaper_file` | `choose_custom_wallpaper` 走原生选择器，校验可读资源并保存路径。 | 软件解码及偏好重读已验证。 |

## 附录 B 事件

| 主线事件 | GPUI 对应通道 | 核对范围 |
| --- | --- | --- |
| `library-changed` | `LibraryWatcher → WatchScan → LibraryChange → apply_library_change` | 用新快照与增删移动差异更新队列、循环点、字幕和 HTTP 缓存。FSEvents 实机待验。 |
| `api-command` | `api::dispatch → api::Command → Workspace::poll_api` | 请求携带动作、参数和独立的回复通道。已有回环测试。 |
| `model-download-progress` | `ModelDownload::progress → poll_model_download` | 设置页轮询字节数和文件进度，下载完成或失败后刷新状态。 |
| `model-load-status` | `start_asr_load` 更新加载字段和会话，模型页与 HTTP 读取该状态。 | 手动、自动与启动恢复共用加载流程，实际权重待验。 |
| `subtitle-progress` | `subtitle_generation::Stage` 由界面生成任务更新提示，由 HTTP 生成任务更新全局及单任务 SSE。 | 分离、归一化、分段识别、保存和终态，正式推理待验。 |
| `desktop-lyrics-control` | `desktop_lyrics::Event::Close` 经 `lyrics_event_handler` 更新窗口句柄和启用偏好。 | 窗口开关与系统关窗待验。 |
| `desktop-lyrics-data` | `Workspace::poll_desktop_lyrics → Overlay::update_content` | 从当前曲目时间轴更新前后行和逐词进度。 |
| `desktop-lyrics-size` | `lyrics_window_size → size_to_save → save_lyrics_window_size` | 原生尺寸读回、延迟保存及设置页同步。 |
| `desktop-lyrics-hover` | `desktop::lyrics_pointer → Overlay::update_content` | 锁定状态下的进入、离开及鼠标穿透。 |
| `desktop-lyrics-settings-state` | `toggle_lyrics_settings` 与 `close_lyrics_settings` 更新设置窗句柄。 | 打开、关闭和窗口消失后清理。 |
| `desktop-lyrics-style` | `Event::Style → apply_desktop_lyrics_style` | 样式统一写回并同步主窗、歌词窗和设置窗。 |
| `desktop-lyrics-command` | 歌词窗和设置窗的锁定、恢复默认动作向同一处理器提交样式。 | 样式重读已测试，跨窗口实机反馈待验。 |
