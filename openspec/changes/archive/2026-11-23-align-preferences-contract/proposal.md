## Why

GPUI 应用层写入 v3 文件，作者新版文档要求 v4 的 34 字段及各自默认值、归一化和读写所有权。将作者文档 F11 的业务边界转为可执行验收项。

## What Changes

- 对齐 v4 共享字段、四个权威字段及平台设置的落盘语义。
- 建立旧版、损坏和极值样本，验证原子更新及稳定键收藏。
- 用同一组文件完成主线与 GPUI 的双向读写验收。

## Capabilities

### New Capabilities

- `preferences-contract`：偏好与收藏。

## Impact

影响 GPUI 的偏好与收藏。前置项：共享核心同步。
