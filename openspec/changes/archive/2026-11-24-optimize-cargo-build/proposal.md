## Why

GPUI 使用 Cargo 默认构建配置，调试产物占用较多磁盘。统一应用与共享核心的构建入口，调整发布二进制大小，并验证日常使用的构建命令与任务错误恢复。

## What Changes

- 将共享核心纳入 Cargo 工作区。
- 开发构建保留项目行号并关闭外部依赖调试信息。
- 以同一源代码和工具链比较发布二进制，保留合适的链接与符号配置。

## Capabilities

### New Capabilities

- `cargo-build`：统一开发测试与发布构建配置。

## Impact

根目录 `Cargo.toml`、工作区锁文件和构建说明。
