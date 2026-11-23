## Why

GPUI 已接入监听、700 ms 防抖、后台扫描与 30 秒兜底。将作者文档 F04 的业务边界转为可执行验收项。

## What Changes

- 核对覆盖五种格式的文件事件及旧扫描代次检查。
- 用本地盘测 10 文件批次刷新延迟，并覆盖改名和移动。
- 验证监听失败、漏事件 30 秒兜底和切库重建。

## Capabilities

### New Capabilities

- `library-watcher`：曲库实时监听。

## Impact

影响 GPUI 的曲库实时监听。前置项：F01、F03。
