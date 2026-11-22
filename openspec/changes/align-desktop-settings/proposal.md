## Why

GPUI 已接入 macOS 字体、登录启动、菜单栏和防休眠。部署目标为 macOS 26+。 将作者文档 F12 的业务边界转为可执行验收项。

## What Changes

- 核对三组设置和平台标注，明确桌面行为与主线相同的部分。
- 用 macOS 26+ 验证字体、登录启动、托盘恢复及关闭选择持久化。
- 验证防休眠取得和释放、输出设置以及文档注明的平台能力。

## Capabilities

### New Capabilities

- `desktop-settings`：系统设置与桌面集成。

## Impact

影响 GPUI 的系统设置与桌面集成。前置项：F05、F06、F10。
