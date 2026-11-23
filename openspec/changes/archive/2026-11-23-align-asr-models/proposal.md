## Why

GPUI 需要管理主线既有的 Qwen3-ASR 0.6B-int8 模型，作为字幕与歌词的前置能力。

## What Changes

- 接入默认/自选目录、扫描、默认下载源和固定九文件清单，暴露主线模型信息与进度。
- 实现逐文件校验、已完成文件复用、取消清理和下载/加载互斥。
- 接入 sherpa-onnx 四件套加载、下载后自动加载、启动恢复、释放及 WAV 分片识别。

## Capabilities

### New Capabilities

- `asr-models`：F15 ASR 模型管理。

## Impact

对齐 `bgm-tauri2@e59ee18` 的 F15 ASR 模型管理。前置项为 F11。
