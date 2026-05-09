//! Bridge 注册/注销 API — 签发令牌对并管理已注册 sid 名册。
//!
//! 职责：
//! - handle_register: 验证 setup_token，签发 (mac_token, client_token) 令牌对，
//!   持久化到 registered_tokens.json。
//! - handle_unregister: 验证 setup_token，断开活跃 WS 会话，移除令牌并持久化。
//!
//! 架构角色：Relay 的信任锚点。setup_token 是唯一准入凭证，
//! 签发后的 mac_token/client_token 分别用于 WS 连接和 HTTP 代理认证。
//!
//! 不变量：
//! - 注册时先持久化再返回，保证返回的 token 一定能在重启后使用。
//! - 持久化失败时回滚内存中的条目，避免内存/磁盘状态不一致。
//! - 注销时先断开 WS → 清理 pending request → 再删除 token，确保旧连接无法继续使用。

use crate::AppState;
use crate::auth::VerifiedClient;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 注册接口速率限制：每窗口最多尝试次数。
const REGISTER_RATE_LIMIT: usize = 10;
/// 速率限制窗口时长（秒）。
const REGISTER_RATE_WINDOW_SECS: u64 = 60;
/// 单 setup_token 允许注册的最大设备数。
const MAX_REGISTERED_DEVICES: usize = 10;
/// 手机 client token 续期后的有效期（天）。
const CLIENT_TOKEN_RENEW_TTL_DAYS: i64 = 30;

/// 滑动窗口速率限制器。
pub struct RateLimiter {
    attempts: VecDeque<std::time::Instant>,
    limit: usize,
    window_secs: u64,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::with_params(REGISTER_RATE_LIMIT, REGISTER_RATE_WINDOW_SECS)
    }

    pub fn with_params(limit: usize, window_secs: u64) -> Self {
        Self {
            attempts: VecDeque::new(),
            limit,
            window_secs,
        }
    }

    /// 尝试消费一次配额。返回 true 表示允许，false 表示限流。
    pub fn try_acquire(&mut self) -> bool {
        let now = std::time::Instant::now();
        let cutoff = now - std::time::Duration::from_secs(self.window_secs);
        while self.attempts.front().map_or(false, |t| *t < cutoff) {
            self.attempts.pop_front();
        }
        if self.attempts.len() >= self.limit {
            return false;
        }
        self.attempts.push_back(now);
        true
    }
}

/// 共享速率限制器类型。
pub type SharedRateLimiter = Arc<Mutex<RateLimiter>>;

/// per-client（per-sid）代理速率限制参数。
const PROXY_RATE_LIMIT: usize = 60;
const PROXY_RATE_WINDOW_SECS: u64 = 60;

/// per-client 滑动窗口速率限制器：sid → RateLimiter。
///
/// 每个 sid 独立计算配额，互不影响。
pub struct ClientRateLimiter {
    limiters: HashMap<String, RateLimiter>,
}

impl ClientRateLimiter {
    pub fn new() -> Self {
        Self {
            limiters: HashMap::new(),
        }
    }

    /// 尝试消费一次配额。返回 true 表示允许，false 表示限流。
    pub fn try_acquire(&mut self, sid: &str) -> bool {
        self.limiters
            .entry(sid.to_string())
            .or_insert_with(|| RateLimiter::with_params(PROXY_RATE_LIMIT, PROXY_RATE_WINDOW_SECS))
            .try_acquire()
    }
}

pub type SharedClientRateLimiter = Arc<Mutex<ClientRateLimiter>>;

/// per-IP 注册速率限制器：IP → RateLimiter。
pub struct IpRateLimiter {
    limiters: HashMap<String, RateLimiter>,
}

impl IpRateLimiter {
    pub fn new() -> Self {
        Self {
            limiters: HashMap::new(),
        }
    }

    pub fn try_acquire(&mut self, ip: &str) -> bool {
        self.limiters
            .entry(ip.to_string())
            .or_insert_with(|| RateLimiter::new())
            .try_acquire()
    }
}

pub type SharedIpRateLimiter = Arc<Mutex<IpRateLimiter>>;

#[derive(Deserialize)]
pub struct RegisterRequest {
    pub setup_token: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_ttl")]
    pub ttl_days: u32,
}

/// 默认 token 有效期 30 天。
fn default_ttl() -> u32 {
    30
}

#[derive(Deserialize)]
pub struct UnregisterRequest {
    pub setup_token: String,
    pub sid: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub sid: String,
    pub mac_token: String,
    pub client_token: String,
    pub expires_at: String,
}

#[derive(Serialize)]
pub struct RenewResponse {
    pub sid: String,
    pub client_token: String,
    pub expires_at: String,
}

/// 处理注册请求：签发 mac + client 令牌对。
///
/// 流程：速率限制（per-IP） → 验证 setup_token → 设备数检查 →
/// 生成 sid → 签发两个 token → 持久化 → 返回。
/// 持久化失败时回滚内存条目。
pub async fn handle_register(
    State(state): State<std::sync::Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    Json(body): Json<RegisterRequest>,
) -> Response {
    // per-IP 速率限制检查
    let client_ip = addr.ip().to_string();
    if !state.register_limiter.lock().await.try_acquire(&client_ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "rate_limited"})),
        )
            .into_response();
    }

    // 验证 setup_token
    if body.setup_token != state.setup_token {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid setup_token"})),
        )
            .into_response();
    }

    // 设备数量上限检查
    {
        let tokens = state.registered_tokens.lock().await;
        if tokens.len() >= MAX_REGISTERED_DEVICES {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "error": "device_limit_reached",
                    "max_devices": MAX_REGISTERED_DEVICES,
                })),
            )
                .into_response();
        }
    }

    let sid = uuid::Uuid::now_v7().to_string();
    let now = chrono::Utc::now();
    let iat = now.timestamp();
    let exp = now + chrono::Duration::days(body.ttl_days as i64);
    let exp_ts = exp.timestamp();
    let expires_at = exp.to_rfc3339();

    // 签发 mac token（用于 WebSocket 连接认证）
    let mac_jti = uuid::Uuid::now_v7().to_string();
    let mac_payload = crate::token::TokenPayload {
        ver: 1,
        sid: sid.clone(),
        role: "mac".to_string(),
        iat,
        exp: exp_ts,
        jti: mac_jti,
        generation: 1,
    };
    let mac_token = crate::token::sign_token(&state.secret, &mac_payload);

    // 签发 client token（用于 HTTP 代理认证）
    let client_jti = uuid::Uuid::now_v7().to_string();
    let client_payload = crate::token::TokenPayload {
        ver: 1,
        sid: sid.clone(),
        role: "client".to_string(),
        iat,
        exp: exp_ts,
        jti: client_jti,
        generation: 1,
    };
    let client_token = crate::token::sign_token(&state.secret, &client_payload);

    // 先持久化再返回：保证返回的 token 在重启后可用。
    // 持久化失败时回滚内存条目。
    {
        let mut tokens = state.registered_tokens.lock().await;
        tokens.insert(
            sid.clone(),
            crate::StoredTokens {
                mac_token: mac_token.clone(),
                client_token: String::new(),
                client_token_hash: crate::client_token_hash(&client_token),
                client_generation: 1,
                label: body.label.clone(),
                expires_at: expires_at.clone(),
            },
        );
        if let Err(e) = crate::save_registered_tokens_to(&state.registered_tokens_path, &tokens) {
            tokens.remove(&sid);
            eprintln!("agent-aspect-relay: persist registered tokens failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "persist_registered_tokens_failed"})),
            )
                .into_response();
        }
    }

    eprintln!(
        "agent-aspect-relay: registered sid {} (label={})...",
        &sid[..8.min(sid.len())],
        body.label
    );

    (
        StatusCode::OK,
        Json(RegisterResponse {
            sid,
            mac_token,
            client_token,
            expires_at,
        }),
    )
        .into_response()
}

/// 处理手机端 client token 续期请求。
///
/// 只轮换 client_token，不重签 mac_token，避免手机续期导致 Mac Bridge 断连。
/// StoredTokens::expires_at 是历史兼容字段；续期后它表示最新 client_token
/// 的过期时间。mac_token 自身的 exp 仍以 token payload 为准。
pub async fn handle_renew(
    State(state): State<std::sync::Arc<AppState>>,
    client: VerifiedClient,
) -> Response {
    let now = chrono::Utc::now();
    let iat = now.timestamp();
    let requested_exp = now + chrono::Duration::days(CLIENT_TOKEN_RENEW_TTL_DAYS);
    let renewed;

    {
        let mut tokens = state.registered_tokens.lock().await;
        let Some(stored) = tokens.get_mut(&client.sid) else {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "sid_not_registered"})),
            )
                .into_response();
        };

        let request_hash = crate::client_token_hash(&client.raw_token);
        if stored.client_token_hash != request_hash {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "token_revoked"})),
            )
                .into_response();
        }

        let current_verified = match crate::token::verify_token(&state.secret, &client.raw_token) {
            Ok(v) if v.payload.role == "client" => v,
            Ok(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid_token"})),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response();
            }
        };

        if current_verified.payload.generation != stored.client_generation {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "token_revoked"})),
            )
                .into_response();
        }

        let mac_verified = match crate::token::verify_token(&state.secret, &stored.mac_token) {
            Ok(v) if v.payload.role == "mac" => v,
            Ok(_) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "invalid_token"})),
                )
                    .into_response();
            }
            Err(e) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": e.to_string()})),
                )
                    .into_response();
            }
        };

        // client token 不能活得比 mac_token 更久，否则 Mac 侧 token 过期重注册后，
        // 手机仍会拿着旧 sid 的有效 client token 打到一个永远没有 Mac 的会话。
        let exp_ts = requested_exp.timestamp().min(mac_verified.payload.exp);
        if exp_ts <= now.timestamp() {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "token_expired"})),
            )
                .into_response();
        }
        let exp = chrono::DateTime::from_timestamp(exp_ts, 0).unwrap_or(requested_exp);
        let expires_at = exp.to_rfc3339();
        let payload = crate::token::TokenPayload {
            ver: 1,
            sid: client.sid.clone(),
            role: "client".to_string(),
            iat,
            exp: exp.timestamp(),
            jti: uuid::Uuid::now_v7().to_string(),
            generation: stored.client_generation + 1,
        };
        let client_token = crate::token::sign_token(&state.secret, &payload);

        let previous_client_hash = stored.client_token_hash.clone();
        let previous_generation = stored.client_generation;
        let previous_expires_at = stored.expires_at.clone();
        stored.client_token_hash = crate::client_token_hash(&client_token);
        stored.client_generation = payload.generation;
        stored.expires_at = expires_at.clone();

        if let Err(e) = crate::save_registered_tokens_to(&state.registered_tokens_path, &tokens) {
            if let Some(stored) = tokens.get_mut(&client.sid) {
                stored.client_token_hash = previous_client_hash;
                stored.client_generation = previous_generation;
                stored.expires_at = previous_expires_at;
            }
            eprintln!("agent-aspect-relay: persist renewed client token failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "persist_registered_tokens_failed"})),
            )
                .into_response();
        }
        renewed = (client_token, expires_at);
    }
    let (client_token, expires_at) = renewed;

    (
        StatusCode::OK,
        Json(RenewResponse {
            sid: client.sid,
            client_token,
            expires_at,
        }),
    )
        .into_response()
}

/// 处理注销请求：断开 WS、清理 pending request、删除 token 并持久化。
///
/// 持久化失败时回滚：把刚删除的条目重新插入内存。
pub async fn handle_unregister(
    State(state): State<std::sync::Arc<AppState>>,
    Json(body): Json<UnregisterRequest>,
) -> Response {
    if body.setup_token != state.setup_token {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid setup_token"})),
        )
            .into_response();
    }

    // 先断开活跃 WS 会话，让旧连接无法继续代理请求
    {
        let mut reg = state.registry.lock().await;
        reg.fail_pending(&body.sid, "session_unregistered");
        reg.unregister(&body.sid);
    }

    // 删除 token 并持久化，确保重启后该 sid 也不能使用
    // 持久化失败时回滚，避免下次重启时 sid 被复活
    {
        let mut tokens = state.registered_tokens.lock().await;
        let removed = tokens.remove(&body.sid);
        if let Err(e) = crate::save_registered_tokens_to(&state.registered_tokens_path, &tokens) {
            if let Some(stored) = removed {
                tokens.insert(body.sid.clone(), stored);
            }
            eprintln!("agent-aspect-relay: persist registered token removal failed: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "persist_registered_tokens_failed"})),
            )
                .into_response();
        }
    }

    eprintln!(
        "agent-aspect-relay: unregistered sid {}...",
        &body.sid[..8.min(body.sid.len())]
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({"status": "unregistered"})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionRegistry;
    use crate::token;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn test_state() -> Arc<AppState> {
        let secret = b"test-secret-key-32-bytes-long!!!!".to_vec();
        Arc::new(AppState {
            registry: Arc::new(Mutex::new(SessionRegistry::new())),
            secret: Arc::new(secret.clone()),
            setup_token: "test-setup-token".to_string(),
            registered_tokens: Mutex::new(HashMap::new()),
            registered_tokens_path: test_tokens_path(),
            mobile_leases: Mutex::new(HashMap::new()),
            register_limiter: Arc::new(Mutex::new(IpRateLimiter::new())),
            client_limiter: Arc::new(Mutex::new(ClientRateLimiter::new())),
        })
    }

    fn test_connect_info() -> axum::extract::ConnectInfo<std::net::SocketAddr> {
        axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 12345)))
    }

    fn test_tokens_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "agent-aspect-relay-registered-tokens-test-{}-{}.json",
            std::process::id(),
            uuid::Uuid::now_v7()
        ))
    }

    fn read_persisted_tokens(path: &std::path::Path) -> serde_json::Value {
        let raw = std::fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    async fn extract_json(resp: Response) -> serde_json::Value {
        let status = resp.status();
        let body = resp.into_body();
        let bytes = axum::body::to_bytes(body, 4096).await.unwrap();
        let val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Inject status for convenience
        let mut map = val.as_object().unwrap().clone();
        map.insert("_status".to_string(), serde_json::json!(status.as_u16()));
        serde_json::Value::Object(map)
    }

    #[tokio::test]
    async fn register_generates_signed_token_pair() {
        let state = test_state();
        let req = RegisterRequest {
            setup_token: "test-setup-token".to_string(),
            label: "test-mac".to_string(),
            ttl_days: 30,
        };

        let resp = handle_register(State(state.clone()), test_connect_info(), Json(req)).await;
        let body = extract_json(resp).await;
        assert_eq!(body["_status"].as_u64().unwrap(), 200);

        let sid = body["sid"].as_str().unwrap();
        let mac_token = body["mac_token"].as_str().unwrap();
        let client_token = body["client_token"].as_str().unwrap();

        // Verify mac token
        let mac_verified = token::verify_token(&state.secret, mac_token).unwrap();
        assert_eq!(mac_verified.payload.role, "mac");
        assert_eq!(mac_verified.payload.sid, sid);

        // Verify client token
        let client_verified = token::verify_token(&state.secret, client_token).unwrap();
        assert_eq!(client_verified.payload.role, "client");
        assert_eq!(client_verified.payload.sid, sid);

        // Same sid
        assert_eq!(mac_verified.payload.sid, client_verified.payload.sid);

        // Stored in registered_tokens
        let stored = state.registered_tokens.lock().await;
        assert!(stored.contains_key(sid));
        assert_eq!(stored.get(sid).unwrap().label, "test-mac");

        let persisted = read_persisted_tokens(&state.registered_tokens_path);
        assert_eq!(persisted[sid]["label"], "test-mac");
        assert!(persisted[sid].get("client_token").is_none());
        assert!(persisted[sid]["client_token_hash"].as_str().is_some());
        assert_eq!(persisted[sid]["client_generation"].as_u64(), Some(1));

        let loaded = crate::load_registered_tokens_from(&state.registered_tokens_path);
        assert!(loaded.contains_key(sid));
        assert_eq!(loaded.get(sid).unwrap().label, "test-mac");
    }

    #[tokio::test]
    async fn register_rejects_bad_setup_token() {
        let state = test_state();
        let req = RegisterRequest {
            setup_token: "wrong-token".to_string(),
            label: "test".to_string(),
            ttl_days: 30,
        };

        let resp = handle_register(State(state), test_connect_info(), Json(req)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unregister_removes_tokens_and_blocks_reuse() {
        let state = test_state();

        // Register
        let reg_req = RegisterRequest {
            setup_token: "test-setup-token".to_string(),
            label: "test".to_string(),
            ttl_days: 1,
        };
        let reg_resp =
            handle_register(State(state.clone()), test_connect_info(), Json(reg_req)).await;
        let body = extract_json(reg_resp).await;
        let sid = body["sid"].as_str().unwrap().to_string();

        // Confirm registered
        assert!(state.registered_tokens.lock().await.contains_key(&sid));

        // Unregister
        let unreg_req = UnregisterRequest {
            setup_token: "test-setup-token".to_string(),
            sid: sid.clone(),
        };
        let unreg_resp = handle_unregister(State(state.clone()), Json(unreg_req)).await;
        assert_eq!(unreg_resp.status(), StatusCode::OK);

        // Confirm removed from registered_tokens
        assert!(!state.registered_tokens.lock().await.contains_key(&sid));

        let persisted = read_persisted_tokens(&state.registered_tokens_path);
        assert!(persisted.get(&sid).is_none());
    }

    #[tokio::test]
    async fn renew_rotates_client_token_and_keeps_mac_token() {
        let state = test_state();
        let reg_req = RegisterRequest {
            setup_token: "test-setup-token".to_string(),
            label: "test".to_string(),
            ttl_days: 30,
        };
        let reg_resp =
            handle_register(State(state.clone()), test_connect_info(), Json(reg_req)).await;
        let body = extract_json(reg_resp).await;
        let sid = body["sid"].as_str().unwrap().to_string();
        let old_client_token = body["client_token"].as_str().unwrap().to_string();
        let mac_token = body["mac_token"].as_str().unwrap().to_string();

        let resp = handle_renew(
            State(state.clone()),
            VerifiedClient {
                sid: sid.clone(),
                device_id: "test-phone".to_string(),
                raw_token: old_client_token.clone(),
            },
        )
        .await;
        let renewed = extract_json(resp).await;
        assert_eq!(renewed["_status"].as_u64().unwrap(), 200);
        let new_client_token = renewed["client_token"].as_str().unwrap();
        assert_ne!(new_client_token, old_client_token);
        let mac_verified = token::verify_token(&state.secret, &mac_token).unwrap();
        let client_verified = token::verify_token(&state.secret, new_client_token).unwrap();
        assert!(client_verified.payload.exp <= mac_verified.payload.exp);

        let tokens = state.registered_tokens.lock().await;
        let stored = tokens.get(&sid).unwrap();
        assert_eq!(stored.mac_token, mac_token);
        assert_eq!(
            stored.client_token_hash,
            crate::client_token_hash(new_client_token)
        );
        assert_eq!(stored.client_generation, client_verified.payload.generation);
        assert!(stored.client_token.is_empty());
    }

    #[tokio::test]
    async fn renew_rejects_stale_raw_token_after_rotation() {
        let state = test_state();
        let reg_req = RegisterRequest {
            setup_token: "test-setup-token".to_string(),
            label: "test".to_string(),
            ttl_days: 30,
        };
        let reg_resp =
            handle_register(State(state.clone()), test_connect_info(), Json(reg_req)).await;
        let body = extract_json(reg_resp).await;
        let sid = body["sid"].as_str().unwrap().to_string();
        let old_client_token = body["client_token"].as_str().unwrap().to_string();

        let first = handle_renew(
            State(state.clone()),
            VerifiedClient {
                sid: sid.clone(),
                device_id: "test-phone".to_string(),
                raw_token: old_client_token.clone(),
            },
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let stale = handle_renew(
            State(state),
            VerifiedClient {
                sid,
                device_id: "test-phone".to_string(),
                raw_token: old_client_token,
            },
        )
        .await;
        assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn beat_refreshes_mobile_lease_after_auth() {
        let state = test_state();
        let sid = "sid-lease".to_string();
        let now = chrono::Utc::now();
        let client_payload = token::TokenPayload {
            ver: 1,
            sid: sid.clone(),
            role: "client".to_string(),
            iat: now.timestamp(),
            exp: (now + chrono::Duration::hours(1)).timestamp(),
            jti: uuid::Uuid::now_v7().to_string(),
            generation: 1,
        };
        let client_token = token::sign_token(&state.secret, &client_payload);
        state.registered_tokens.lock().await.insert(
            sid.clone(),
            crate::StoredTokens {
                mac_token: "mac-token".to_string(),
                client_token: String::new(),
                client_token_hash: crate::client_token_hash(&client_token),
                client_generation: 1,
                label: "lease-test".to_string(),
                expires_at: (now + chrono::Duration::hours(1)).to_rfc3339(),
            },
        );

        let resp = crate::beat::handle_beat(
            State(state.clone()),
            VerifiedClient {
                sid: sid.clone(),
                device_id: "phone-a".to_string(),
                raw_token: client_token,
            },
            Json(crate::beat::BeatRequest {
                request_id: "beat-1".to_string(),
                device_id: "phone-a".to_string(),
                client_sent_at_ms: 123,
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let lease = crate::get_mobile_lease(&state, &sid, "phone-a")
            .await
            .expect("lease refreshed");
        assert_eq!(lease.device_id, "phone-a");
        assert!(crate::mobile_lease_online(&lease));
    }

    #[tokio::test]
    async fn unregister_rejects_bad_setup_token() {
        let state = test_state();
        let unreg_req = UnregisterRequest {
            setup_token: "wrong".to_string(),
            sid: "any".to_string(),
        };
        let resp = handle_unregister(State(state), Json(unreg_req)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
