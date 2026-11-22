## Why

已有本机服务与 14 个进程内路由测试。和新上游的外部接口兼容性待端到端核对。 将作者文档 F13 的业务边界转为可执行验收项。

## What Changes

- 获取上游 API 参考实现，逐条记录请求、响应及 SSE 样本。
- 校对 17 条路由、14 个通用动作、字段、超时及错误码。
- 在应用运行时用 curl 与 SSE 客户端验收，检查已对齐的外部脚本兼容性。

## Capabilities

### New Capabilities

- `loopback-http-api`：本机 HTTP 开放接口。

## Impact

影响 GPUI 的本机 HTTP 开放接口。前置项：F01、F04、F05、F06、F07、F10、F12。
