//! 本机开放接口：HTTP JSON API + SSE 事件流，供 OBS、Stream Deck、脚本等外部项目接入。
//!
//! 设计约束：
//! - 只监听 `127.0.0.1`，默认端口 8737，避免暴露到局域网；
//! - 浏览器来源默认只信任本机（`tauri://localhost` 与 `localhost` / `127.0.0.1` / `[::1]`），
//!   可在「基本系统配置 · 第三方接入」里改为放行全部来源或追加自定义来源；
//!   非浏览器客户端（curl、Stream Deck 等）不带 `Origin`，不受影响；
//! - `config.json` 的 `api.token` 非空时，所有请求都必须带 `Authorization: Bearer`；
//!   界面保存 token 后立即生效，无需重启；
//! - 播放控制由前端执行，Rust 只负责转发命令与缓存最近一次状态快照；
//! - 曲库读取直接走 [`AppState`]，不依赖界面是否打开；
//! - 使用模型的长耗时任务（字幕生成 / 人声分离 / ASR 加载）统一进入
//!   [`crate::tasks::TaskStore`] 的串行队列，提交返回 `202` 与任务 id，
//!   进度通过 `GET /v1/tasks/{id}/events` 的 SSE 推送；
//! - SSE 严格使用标准字段：`id`（单调递增，支持 `Last-Event-ID` 续传）、
//!   `event`（`state` / `task` / `snapshot` / `progress` / `done` / `failed` / `resync`）、
//!   `data`（JSON 文本）。
//!
//! 子模块划分：
//! - [`catalog`]：业务分类与接口清单；
//! - [`bridge`]：监听、鉴权、命令转发与状态快照；
//! - [`router`]：路由、CORS 与 SSE 辅助；
//! - [`request`]：请求体 / 查询参数解析与错误分流；
//! - [`handlers`]：按业务领域拆分的处理器；
//! - [`error`]：统一响应信封。
//!
//! 接口清单见 [`catalog::catalog`]，界面「第三方接入」按 [`catalog::categories`] 的业务分类展示。

mod bridge;
mod catalog;
mod error;
mod handlers;
mod request;
mod router;

#[cfg(test)]
mod tests;

pub(crate) use bridge::CommandFailure;
pub use bridge::{ApiBridge, ApiStatus};
pub use catalog::{ApiCatalog, catalog};

/// 发给前端的事件名；与 `ui/src/lib/constants.ts` 的 `EVENTS.apiCommand` 对应。
pub const API_COMMAND_EVENT: &str = "api-command";
