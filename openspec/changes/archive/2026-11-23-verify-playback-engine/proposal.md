## Why

GPUI 已用后台整曲解码、双声部包络和采样级循环实现主要播放路径。将作者文档 F05 的业务边界转为可执行验收项。

## What Changes

- 对齐 30 分钟解码上限、超时、请求取消与失败跳过。
- 对齐短包络、交叉淡化、自然结束和保音高倍速。
- 验证五种曲库音频格式、两分钟 A/B 循环和切歌听感。

## Capabilities

### New Capabilities

- `playback-engine`：播放核心。

## Impact

影响 GPUI 的播放核心。前置项：F03、F11。
