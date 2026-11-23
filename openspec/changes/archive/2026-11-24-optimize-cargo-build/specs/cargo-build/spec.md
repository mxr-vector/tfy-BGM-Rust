## Purpose

定义 GPUI 应用与共享核心的 Cargo 开发、测试和发布配置，兼顾调试产物大小、项目代码的报错定位，以及字幕和模型加载任务的异常恢复。

## ADDED Requirements

### Requirement: 统一构建与测试

根工作区 SHALL 包含 GPUI 应用和共享核心，并以 GPUI 应用作为默认构建入口。

#### Scenario: 执行工作区测试

- **WHEN** 从根目录执行 Cargo 工作区测试
- **THEN** 测试包含应用与共享核心两个包

#### Scenario: 日常开发构建

- **WHEN** 从根目录执行开发构建
- **THEN** 项目成员保留行号，外部依赖关闭调试信息

### Requirement: 发布构建

发布构建 SHALL 使用锁定依赖，并保留任务 panic 的展开行为。

#### Scenario: 构建发布应用

- **WHEN** 使用发布配置构建 GPUI
- **THEN** 生成可用于现有打包流程的二进制，字幕与模型加载任务能继续捕获可展开的 panic
