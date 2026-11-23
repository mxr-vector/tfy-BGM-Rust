## Why

原生音频设备枚举和多路增益已接入，尚缺多设备实机端到端验证。将作者文档 F07 的业务边界转为可执行验收项。

## What Changes

- 核对默认路由、路由 ID、启停和最大数量。
- 验证热插拔、重启恢复、缺失设备和单路降级。
- 用两台物理设备验收，同时测试主音量和单路静音。

## Capabilities

### New Capabilities

- `multi-device-output`：多输出设备。

## Impact

影响 GPUI 的多输出设备。前置项：F05、F11。
