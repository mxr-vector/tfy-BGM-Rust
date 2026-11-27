## 1. 迁移与验收

- [x] 1.1 核对 d1a41cd 迁移说明和字幕、模型入口
- [x] 1.2 接入 TXT、SRT、LRC 导入与解析预览
- [x] 1.3 接入 Kokoro 和 ZipVoice 模型生命周期、音色及合成
- [x] 1.4 接入 CosyVoice3 CPU 推理环境与合成
- [x] 1.5 接入参考录音、试听、取消和 WAV 导出
- [x] 1.6 运行软件测试并严格校验
- [x] 1.7 验证正式模型下载、加载与合成（真实麦克风硬件录音留作交互设备自检）
- [x] 1.8 验证打开页面即可输入并预览脚本
- [x] 1.9 验证超过 15 秒的合成结果可导出，参考录音仍按 15 秒上限校验

软件验证覆盖 TXT 断句、LRC 时间、SRT 时槽、内置 11 音色逐字稿与缺逐字稿拦截（`0f2cc01`）以及 103 位 Kokoro v1.1 说话人目录。已接入 sherpa-onnx 2.0.2 动态库并完成三大官方模型端到端实测：Kokoro INT8（v1.1，TXT/SRT/LRC 合成 24 kHz 非静音 WAV）、ZipVoice INT8（含共享 `vocos_24khz.onnx` 与 `lexicon-en-us.txt` 及内置音色克隆合成 24 kHz WAV）与 CosyVoice3 ONNX（ModelScope 13 文件 SHA256 校验、`uv` Python 3.11 CPU 运行时 `--self-test` 及 24 kHz 克隆合成）。

2026-11-27 原生验收覆盖打开文生语音、编辑中文文本、预览分段、自动加载 Kokoro INT8 生成、试听及保存 WAV。

2026-11-28 使用 Kokoro INT8 复现长脚本在导出阶段触发参考音频校验的问题。修复后，从 347 字符的脚本生成约 72 秒、24 kHz 的单声道 WAV。在可用和不可用 FFmpeg 的环境中均通过。工作区测试覆盖了长 WAV 导出和参考音频的 15 秒上限。
