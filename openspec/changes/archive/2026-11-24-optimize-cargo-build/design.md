## Context

主线 `bgm-tauri2@8ba4f85` 统一应用与共享核心构建，并配置开发调试信息和发布链接。GPUI 使用 `catch_unwind` 将字幕和模型加载相关的异常转换为任务错误。

## Decisions

根包仍是普通构建入口，`--workspace` 包含共享核心。锁文件中 macOS 使用的 `notify-types` 要求 Rust 1.91，根包和构建说明据此声明最低版本。开发构建为工作区成员保留行号，关闭外部依赖调试信息。发布保持默认运行时优化级别与 panic 展开，测量链接优化和符号裁剪对可执行文件的影响。两组构建使用相同源代码、依赖版本、工具链、目标架构与着色器模式。

## Verification

发布体积比较使用 macOS arm64、Rust 1.100.0 和 `gpui/runtime_shaders`。基线通过 Cargo 配置覆盖将 `lto` 设为 `false`、将 `strip` 设为 `none`。两版均使用 `--release --locked`，以 Mach-O 可执行文件的字节数比较。

同一份源码的基线可执行文件为 23,418,960 字节，启用 ThinLTO 与符号裁剪后为 18,466,784 字节，减少 21.1%。优化后的可执行文件通过 macOS 严格签名检查。共享核心 96 项测试、GPUI 133 项测试和全目标编译检查通过。
