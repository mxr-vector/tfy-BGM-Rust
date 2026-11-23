## Why

主线要求字幕生成以前置分离为入口。人声模型可选，伴奏固定为 UVR-MDX-NET-Inst_HQ_3。

## What Changes

- 核对随包模型清单、资源路径、自定义 Spleeter/UVR 发现、ONNX 元数据预检和目标确认。
- 移植 ffmpeg 解码、sherpa-onnx-sys 和隔离 worker，实现按时长选择并行/串行。
- 校验并原子替换完整产物，保留失败前的有效交付物，对齐记录、迁移与删除。

## Capabilities

### New Capabilities

- `source-separation`：F16 双引擎人声分离。

## Impact

对齐 `bgm-tauri2@e59ee18` 的 F16 双引擎人声分离。前置项为 F03、F15。
