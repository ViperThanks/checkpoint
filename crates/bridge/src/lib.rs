//! Bridge crate 根模块 — 重新导出所有子模块。
//!
//! 本 crate 是 Agent Aspect 的 HTTP 服务层，提供 Web Dashboard 和 REST API。
//! 由 `main.rs` 引用后启动 HTTP server。

pub mod auth;
pub mod context;
pub mod jobs;
pub mod provider;
pub mod relay_client;
pub mod routes;
pub mod sse;
pub mod ui;
pub mod workflows;
