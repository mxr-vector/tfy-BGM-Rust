## Why

已有节目单编辑、定时切换、存档和手动点播冲突处理。将作者文档 F08 的业务边界转为可执行验收项。

## What Changes

- 核对时段规范、歌曲裁剪、保存即生效和节目音量。
- 验证过期节目及未到点歌曲、重叠提示和界面/API 拦截。
- 用可控时钟与物理机分别验证最小化、两分钟回看及休眠恢复。

## Capabilities

### New Capabilities

- `scheduled-playback`：节目单调度。

## Impact

影响 GPUI 的节目单调度。前置项：F05、F11。
