## Why

GPUI 需要支持主线的分离、识别、预览、编辑、导出和读取历史记录。

## What Changes

- 实现曲目入口、字幕页及分离模型确认，强制校验两路产物并仅识别人声。
- 对齐进度阶段、16 kHz 单声道归一化、SRT/LRC 输出和记录字段、路径及排序。
- 实现分段和原文编辑、导出、打开音频与删除。联验稳定键迁移、孤立记录和迁移回滚。

## Capabilities

### New Capabilities

- `subtitle-workflow`：F17 字幕生成与编辑。

## Impact

根据作者文档基线 `e59ee18` 规划 F17 字幕生成与编辑，实施时核对了 `bgm-tauri2@8ba4f85`。前置项为 F02、F04、F15、F16。
