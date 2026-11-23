## Context

契约来自作者文档 §2 F16、§4.7 落盘约定和 §8.2 验收步骤。对齐 `bgm-tauri2@8ba4f85` 的 `src-tauri/src/separation.rs` 和 sherpa-onnx `v2.0.2` 的 C API。

## Decisions

模型资源从 macOS 应用的 `Contents/Resources/models`、开发可执行文件旁的 `models` 和配置模型目录的 `separation` 子目录解析。Spleeter 压缩包只将所需的两份 ONNX 解压至任务暂存目录。UVR 读取 ONNX 顶层元数据，角色未确定的自定义模型由用户确认。伴奏固定用 `UVR-MDX-NET-Inst_HQ_3.onnx`。

主进程写入任务 JSON，并启动同一二进制的 `--separation-worker` 模式。worker 将输入解码为 44.1 kHz 双声道浮点 PCM。十分钟以内并行调用两套引擎，超过十分钟串行调用。输出保留引擎报告的采样率。

输出目录沿用 §4.7 的稳定键。两条 WAV 和 `separation.json` 一同发布。macOS 通过目录交换更新原有结果，保留原目录的 SRT/LRC，并在成功替换后移除旧 `instrumental.wav`。落盘前在记录锁下核对源音频身份，曲库移动或变更导致校验失败时保留已有结果。

## Verification

用主线 Git 中的三个模型文件验证 ONNX 元数据兼容性。用 sherpa-onnx 2.0.2 C 头文件编译的测试动态库验证独立进程、固定伴奏、Spleeter/UVR 及 UVR 残差输出。测试曲目更改、原子替换和保留字幕。正式动态库、推理结果和原生界面列为后续验收项。
