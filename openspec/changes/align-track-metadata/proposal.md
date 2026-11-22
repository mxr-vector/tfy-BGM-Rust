## Why

当前只读取 MP3 标签与时长，已有元数据缓存和测试基线。 将作者文档 F03 的业务边界转为可执行验收项。

## What Changes

- 比对上游标签字段、截断规则、时长和缓存键。
- 让列表、诊断和随机策略使用共享元数据。
- 用五种音频 fixture 验证标签、时长及缓存命中与失效。

## Capabilities

### New Capabilities

- `track-metadata`：标签与时长。

## Impact

影响 GPUI 的标签与时长。前置项：F01。
