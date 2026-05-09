//! 心跳处理器 — 手机端发送心跳，Relay 代理到 Bridge 并返回全链路延迟。
//!
//! 职责：手机端通过 POST /api/beat-from-mobile 发送心跳请求，
//! Relay 将其代理为 GET /beat 转发到 Mac Bridge，收集全链路时间戳
//! （client_sent → relay_received → bridge_received → bridge_sent → relay_sent），
//! 返回 beat_ack 供手机端诊断网络延迟。
//!
//! 架构角色：独立的健康检测通道，不经过通用 http.rs 代理路径，
//! 因为需要注入额外的时间戳信息。
//!
//! 不变量：
//! - 超时 10 秒（比通用代理的 30 秒更短，因为心跳期望快速响应）。
//! - 认证由 auth::VerifiedClient extractor 在 handler 之前完成。

use crate::auth::VerifiedClient;
use crate::protocol::ProxyRequest;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;

#[derive(Deserialize)]
pub struct BeatRequest {
    pub request_id: String,
    #[serde(default)]
    pub device_id: String,
    /// 手机端发送时间戳（毫秒），用于计算端到端延迟。
    pub client_sent_at_ms: i64,
}

/// 处理心跳请求：代理到 Bridge → 收集全链路时间戳 → 返回 beat_ack。
///
/// 认证已由 VerifiedClient extractor 完成。
pub async fn handle_beat(
    State(state): State<Arc<crate::AppState>>,
    client: VerifiedClient,
    axum::Json(body): axum::Json<BeatRequest>,
) -> Response {
    let start = Instant::now();
    let path = "/beat-from-mobile";
    let device_id = if body.device_id.is_empty() {
        client.device_id
    } else {
        body.device_id.clone()
    };
    let lease = crate::update_mobile_lease(&state, &client.sid, &device_id).await;

    let relay_received_at_ms = chrono::Utc::now().timestamp_millis();

    // 构造代理请求，转发为 GET /beat 到 Mac Bridge
    let proxy_req = ProxyRequest {
        r#type: "proxy_request".to_string(),
        request_id: body.request_id.clone(),
        method: "GET".to_string(),
        path: "/beat".to_string(),
        query: None,
        headers: std::collections::HashMap::new(),
        body: None,
    };

    let message = match serde_json::to_string(&proxy_req) {
        Ok(m) => m,
        Err(e) => {
            crate::http::log_access(
                "POST",
                path,
                &device_id,
                500,
                Some("relay_error"),
                None,
                start,
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(r#"{{"error":"serialize failed: {e}"}}"#),
            )
                .into_response();
        }
    };

    // 通过 WS 发送到 Bridge
    let rx = {
        let mut reg = state.registry.lock().await;
        match reg.send_request(&client.sid, body.request_id.clone(), message) {
            Ok(rx) => rx,
            Err(e) => {
                let error_kind = if e.contains("offline") || e.contains("not found") {
                    "mac_offline"
                } else {
                    "relay_error"
                };
                let status = if error_kind == "mac_offline" {
                    StatusCode::SERVICE_UNAVAILABLE
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                crate::http::log_access(
                    "POST",
                    path,
                    &device_id,
                    status.as_u16(),
                    Some(error_kind),
                    None,
                    start,
                );
                return (status, format!(r#"{{"error":"{e}"}}"#)).into_response();
            }
        }
    };

    // 等待 Bridge 响应（超时 10 秒）
    let upstream_start = Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), rx).await;

    let relay_sent_at_ms = chrono::Utc::now().timestamp_millis();

    match result {
        Ok(Ok(Ok(proxy_resp))) => {
            let upstream_ms = upstream_start.elapsed().as_millis() as u64;
            let error_kind = if proxy_resp.status >= 500 {
                Some("upstream_5xx")
            } else {
                None
            };
            crate::http::log_access(
                "POST",
                path,
                &device_id,
                proxy_resp.status,
                error_kind,
                Some(upstream_ms),
                start,
            );

            let bridge_data: serde_json::Value =
                serde_json::from_str(&proxy_resp.body).unwrap_or_default();
            let bridge_received = bridge_data
                .get("bridge_received_at_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let bridge_sent = bridge_data
                .get("bridge_sent_at_ms")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);

            let ack = serde_json::json!({
                "type": "beat_ack",
                "request_id": body.request_id,
                "device_id": device_id,
                "client_sent_at_ms": body.client_sent_at_ms,
                "relay_received_at_ms": relay_received_at_ms,
                "bridge_received_at_ms": bridge_received,
                "bridge_sent_at_ms": bridge_sent,
                "relay_sent_at_ms": relay_sent_at_ms,
                "mobile_last_seen_at": lease.last_seen_at,
                "mobile_lease_expires_at": lease.expires_at,
                "status": "ok",
            });
            (StatusCode::OK, axum::Json(ack)).into_response()
        }
        Ok(Ok(Err(e))) => {
            crate::http::log_access(
                "POST",
                path,
                &device_id,
                502,
                Some("upstream_error"),
                Some(upstream_start.elapsed().as_millis() as u64),
                start,
            );
            (StatusCode::BAD_GATEWAY, format!(r#"{{"error":"{e}"}}"#)).into_response()
        }
        Ok(Err(_)) => {
            crate::http::log_access(
                "POST",
                path,
                &device_id,
                502,
                Some("request_cancelled"),
                Some(upstream_start.elapsed().as_millis() as u64),
                start,
            );
            (StatusCode::BAD_GATEWAY, r#"{"error":"request cancelled"}"#).into_response()
        }
        Err(_) => {
            state.registry.lock().await.fail_pending_request(
                &client.sid,
                &body.request_id,
                "mac_timeout",
            );
            crate::http::log_access(
                "POST",
                path,
                &device_id,
                504,
                Some("upstream_timeout"),
                Some(upstream_start.elapsed().as_millis() as u64),
                start,
            );
            (StatusCode::GATEWAY_TIMEOUT, r#"{"error":"mac_timeout"}"#).into_response()
        }
    }
}
