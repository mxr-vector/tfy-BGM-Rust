## ADDED Requirements

### Requirement: 桌面设置行为

macOS 设置 SHALL 支持系统字体、登录启动、关闭行为、菜单栏和正在播放期间防休眠。

#### Scenario: 关闭行为和记住选择

- **WHEN** 选择记住关闭行为，重启后在设置恢复为每次询问
- **THEN** 重启后按已保存规则处理，取消记住后关闭会再次询问

#### Scenario: 后台与恢复

- **WHEN** 播放中最小化到菜单栏，随后从菜单栏还原
- **THEN** 音频按设定继续播放，窗口可以恢复，休眠锁在规定状态下释放

### Requirement: 平台专属能力

设置 MUST 明确呈现 GPU 与 Windows、Linux 专属集成的适用范围。

#### Scenario: 平台条件

- **WHEN** 在 macOS 26+ 查看平台相关选项
- **THEN** 系统管理的 GPU 配置以及仅适用于其它平台的能力有准确的可用性说明
