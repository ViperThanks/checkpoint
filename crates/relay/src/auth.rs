//! Relay 认证层 — VerifiedClient extractor。
//!
//! 集中处理：Bearer token 提取 → 签名验证 → 角色检查 → sid 注册状态。
//! http.rs 和 beat.rs 通过 axum extractor 自动完成认证，无需手动步骤。

use crate::token;
use axum::extract::FromRequestParts;
use axum::http::{HeaderMap, StatusCode, request::Parts};
use axum::response::{IntoResponse, Response};
use std::borrow::Borrow;
use std::sync::Arc;

/// 已认证的客户端信息。通过 axum extractor 自动验证。
///
/// 使用 axum State 提取模式：从路由状态中借用 Arc<AppState>，
/// 不依赖 Extension 层。
pub struct VerifiedClient {
    pub sid: String,
    pub device_id: String,
}

impl<S> FromRequestParts<S> for VerifiedClient
where
    S: Borrow<Arc<crate::AppState>> + Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app = state.borrow();
        let path = parts.uri.path().to_string();
        let method = parts.method.to_string();

        let device_id = parts
            .headers
            .get("x-device-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string();

        let raw_token = extract_bearer_token(&parts.headers).ok_or_else(|| {
            log_auth_failure(&method, &path, &device_id, 401, "auth_failed");
            (
                StatusCode::UNAUTHORIZED,
                r#"{"error":"missing bearer token"}"#,
            )
                .into_response()
        })?;

        let verified = token::verify_token(&app.secret, &raw_token).map_err(|e| {
            log_auth_failure(&method, &path, &device_id, 401, "auth_failed");
            (StatusCode::UNAUTHORIZED, format!(r#"{{"error":"{e}"}}"#)).into_response()
        })?;

        if verified.payload.role != "client" {
            log_auth_failure(&method, &path, &device_id, 403, "wrong_role");
            return Err((StatusCode::FORBIDDEN, r#"{"error":"wrong_token_role"}"#).into_response());
        }

        let sid = verified.payload.sid;

        if !app.registered_tokens.lock().await.contains_key(&sid) {
            log_auth_failure(&method, &path, &device_id, 401, "sid_not_registered");
            return Err((
                StatusCode::UNAUTHORIZED,
                r#"{"error":"sid_not_registered"}"#,
            )
                .into_response());
        }

        // per-client 速率限制
        if !app.client_limiter.lock().await.try_acquire(&sid) {
            log_auth_failure(&method, &path, &device_id, 429, "rate_limited");
            return Err(
                (StatusCode::TOO_MANY_REQUESTS, r#"{"error":"rate_limited"}"#).into_response(),
            );
        }

        Ok(VerifiedClient { sid, device_id })
    }
}

/// 认证失败时输出访问日志。
fn log_auth_failure(method: &str, path: &str, device_id: &str, status: u16, error_kind: &str) {
    eprintln!(
        "relay-access: method={method} path={path} device={device_id} status={status} error={error_kind} upstream_ms=- total_ms=-"
    );
}

/// 从 Authorization 头提取 Bearer token 值。
pub(crate) fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.to_string())
}
