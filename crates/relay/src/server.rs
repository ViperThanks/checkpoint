//! axum 路由定义。
//!
//! 职责：将所有 HTTP endpoint 绑定到对应的 handler 函数。
//!
//! 架构角色：Relay 服务器的路由层，所有请求从这里分发。
//! - /ws — Mac Bridge WebSocket 长连接
//! - /api/register, /api/unregister — Bridge 注册/注销
//! - /api/beat-from-mobile — 心跳
//! - /api/* — 通用代理（转发到 Mac Bridge）
//! - / — 手机端 UI 页面

use crate::register;
use crate::ws;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post, put};
use std::sync::Arc;

/// POST body 最大字节数（1 MiB）。超出时 axum 返回 413。
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// 构建完整的 axum Router，挂载所有路由并注入共享状态。
pub fn app(state: Arc<crate::AppState>) -> Router {
    Router::new()
        .route("/ws", get(ws::ws_handler))
        .route("/api/register", post(register::handle_register))
        .route("/api/unregister", post(register::handle_unregister))
        .route("/api/health", get(crate::http::proxy_get))
        .route("/api/beat-from-mobile", post(crate::beat::handle_beat))
        .route("/api/mac-status", get(crate::http::handle_mac_status))
        .route("/api/overview", get(crate::http::proxy_get))
        .route("/api/pending", get(crate::http::proxy_get))
        .route("/api/run/context", get(crate::http::proxy_get))
        .route("/api/jobs", get(crate::http::proxy_get))
        .route("/api/jobs/{id}", get(crate::http::proxy_get))
        .route("/api/conversations", get(crate::http::proxy_get))
        .route("/api/conversations/{id}", get(crate::http::proxy_get))
        .route(
            "/api/conversations/{id}/messages",
            get(crate::http::proxy_get),
        )
        .route(
            "/api/conversations/{id}/messages/delta",
            get(crate::http::proxy_get),
        )
        .route(
            "/api/conversations/{id}/runtime-check",
            get(crate::http::proxy_get),
        )
        .route("/api/jobs", post(crate::http::proxy_post))
        .route("/api/jobs/{id}/cancel", post(crate::http::proxy_post))
        .route("/api/jobs/{id}/logs/delta", post(crate::http::proxy_post))
        .route("/api/decide", post(crate::http::proxy_post))
        // hook 代理路由
        .route("/api/hook-status", get(crate::http::proxy_get))
        .route("/api/hook-config", post(crate::http::proxy_post))
        // workflow 代理路由
        .route("/api/workflows", get(crate::http::proxy_get))
        .route("/api/workflows", post(crate::http::proxy_post))
        .route("/api/workflows/{id}", get(crate::http::proxy_get))
        .route("/api/workflows/{id}", put(crate::http::proxy_put))
        .route("/api/workflows/{id}", delete(crate::http::proxy_delete))
        .route("/api/workflows/{id}/run", post(crate::http::proxy_post))
        .route("/api/workflows/{id}/cancel", post(crate::http::proxy_post))
        .route(
            "/api/workflows/{id}/steps/reorder",
            put(crate::http::proxy_put),
        )
        .route(
            "/api/workflows/{id}/steps/{step_id}/logs",
            get(crate::http::proxy_get),
        )
        .route("/", get(crate::mobile_ui::serve_ui))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}
