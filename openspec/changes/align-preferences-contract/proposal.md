## Why

GPUI 应用层写入 v3 文件，目前仍复用 v2 共享结构做部分归一化。 将作者文档 F10 的业务边界转为可执行验收项。

## What Changes

- 比对 v3 共享字段及 平台专属设置的落盘语义。
- 建立旧版、损坏和极值样本，验证原子更新及稳定键收藏。
- 用同一组文件完成主线与 GPUI 的双向读写验收。

## Capabilities

### New Capabilities

- `preferences-contract`：偏好与收藏。

## Impact

影响 GPUI 的偏好与收藏。前置项：F01。
