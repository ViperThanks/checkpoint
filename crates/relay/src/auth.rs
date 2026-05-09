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
#[derive(Debug)]
pub struct VerifiedClient {
    pub sid: String,
    pub device_id: String,
    pub raw_token: String,
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

        let sid = verified.payload.sid.clone();

        {
            let tokens = app.registered_tokens.lock().await;
            let Some(stored) = tokens.get(&sid) else {
                log_auth_failure(&method, &path, &device_id, 401, "sid_not_registered");
                return Err((
                    StatusCode::UNAUTHORIZED,
                    r#"{"error":"sid_not_registered"}"#,
                )
                    .into_response());
            };
            let request_hash = crate::client_token_hash(&raw_token);
            if stored.client_token_hash != request_hash
                || stored.client_generation != verified.payload.generation
            {
                log_auth_failure(&method, &path, &device_id, 401, "token_revoked");
                return Err(
                    (StatusCode::UNAUTHORIZED, r#"{"error":"token_revoked"}"#).into_response()
                );
            }
        }

        // per-client 速率限制
        if !app.client_limiter.lock().await.try_acquire(&sid) {
            log_auth_failure(&method, &path, &device_id, 429, "rate_limited");
            return Err(
                (StatusCode::TOO_MANY_REQUESTS, r#"{"error":"rate_limited"}"#).into_response(),
            );
        }

        Ok(VerifiedClient {
            sid,
            device_id,
            raw_token,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::register::{ClientRateLimiter, IpRateLimiter};
    use crate::session::SessionRegistry;
    use axum::extract::FromRequestParts;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    fn test_state() -> Arc<crate::AppState> {
        Arc::new(crate::AppState {
            registry: Arc::new(Mutex::new(SessionRegistry::new())),
            secret: Arc::new(b"test-secret-key-32-bytes-long!!!!".to_vec()),
            setup_token: "setup".to_string(),
            registered_tokens: Mutex::new(HashMap::new()),
            registered_tokens_path: std::env::temp_dir().join(format!(
                "agent-aspect-relay-auth-test-{}.json",
                uuid::Uuid::now_v7()
            )),
            mobile_leases: Mutex::new(HashMap::new()),
            register_limiter: Arc::new(Mutex::new(IpRateLimiter::new())),
            client_limiter: Arc::new(Mutex::new(ClientRateLimiter::new())),
        })
    }

    fn signed_token(state: &crate::AppState, sid: &str, role: &str) -> String {
        let now = chrono::Utc::now();
        let payload = token::TokenPayload {
            ver: 1,
            sid: sid.to_string(),
            role: role.to_string(),
            iat: now.timestamp(),
            exp: (now + chrono::Duration::hours(1)).timestamp(),
            jti: uuid::Uuid::now_v7().to_string(),
            generation: 1,
        };
        token::sign_token(&state.secret, &payload)
    }

    async fn extract_with_token(
        state: Arc<crate::AppState>,
        token: &str,
    ) -> Result<VerifiedClient, Response> {
        let req = axum::http::Request::builder()
            .uri("/api/overview")
            .header("Authorization", format!("Bearer {token}"))
            .header("X-Device-Id", "phone-a")
            .body(())
            .unwrap();
        let (mut parts, _) = req.into_parts();
        VerifiedClient::from_request_parts(&mut parts, &state).await
    }

    #[tokio::test]
    async fn rejects_wrong_role_token() {
        let state = test_state();
        let token = signed_token(&state, "sid-1", "mac");
        let err = extract_with_token(state, &token).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_unregistered_sid() {
        let state = test_state();
        let token = signed_token(&state, "sid-1", "client");
        let err = extract_with_token(state, &token).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_revoked_client_token() {
        let state = test_state();
        let sid = "sid-1";
        let old_token = signed_token(&state, sid, "client");
        let new_token = signed_token(&state, sid, "client");
        state.registered_tokens.lock().await.insert(
            sid.to_string(),
            crate::StoredTokens {
                mac_token: "mac-token".to_string(),
                client_token: String::new(),
                client_token_hash: crate::client_token_hash(&new_token),
                client_generation: 1,
                label: "phone".to_string(),
                expires_at: (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            },
        );

        let err = extract_with_token(state, &old_token).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    }
}
