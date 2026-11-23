## Why

当前 MP3 两遍归一化有实测结果，多格式原样复制和转码待按上游核对。将作者文档 F09 的业务边界转为可执行验收项。

## What Changes

- 对齐预设和 ffmpeg 查找顺序，核对不同源与目标扩展名组合。
- 覆盖 ffmpeg 缺失、错误返回、两遍输出以及同名批量导出。
- 保存 −14 与 −20 LUFS 测量及无 ffmpeg 原样复制验收结果。

## Capabilities

### New Capabilities

- `multiformat-export`：导出与响度归一化。

## Impact

影响 GPUI 的导出与响度归一化。前置项：F02、F03、F11。
