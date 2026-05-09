//! Relay 服务器核心模块。
//!
//! 职责：初始化全局状态（密钥、令牌名册、会话注册表），启动 axum HTTP 服务器。
//!
//! 架构角色：
//! - 手机端通过 HTTP API 访问本服务，请求被代理转发到 Mac Bridge（通过 WebSocket 长连接）。
//! - Mac Bridge 通过 WebSocket 连接到本服务，接收转发请求并返回响应。
//!
//! 不变量：
//! - relay.secret 和 setup.token 首次生成后持久化到 ~/.agent-aspect-relay/，重启后复用。
//! - registered_tokens.json 每次写入使用 rename(原子替换)，保证断电不丢数据。
//! - 过期的 token 在加载时被清理。

pub mod auth;
mod beat;
mod http;
mod mobile_ui;
mod protocol;
pub mod register;
mod server;
mod session;
mod token;
mod ws;

use serde::{Deserialize, Serialize};
use session::{SessionRegistry, SharedRegistry};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 手机端标准心跳间隔（秒），需和 mobile UI 的前台心跳间隔保持一致。
pub(crate) const MOBILE_HEARTBEAT_INTERVAL_SECS: i64 = 15;
/// 手机在线租约 TTL（秒）：允许 3 个心跳周期的抖动。
pub(crate) const MOBILE_LEASE_TTL_SECS: i64 = MOBILE_HEARTBEAT_INTERVAL_SECS * 3;

/// 已注册的令牌对（mac_token + client_token），持久化到 registered_tokens.json。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredTokens {
    pub mac_token: String,
    /// 旧版本持久化的 client token 原文，仅用于启动迁移；保存时不再写出。
    #[serde(default, skip_serializing)]
    pub client_token: String,
    /// 当前 client token 的 SHA-256 hash。Relay 只用 hash 做 CAS 和撤销校验。
    #[serde(default)]
    pub client_token_hash: String,
    /// 当前 client token 的轮换代数，必须与 token payload.generation 一致。
    #[serde(default)]
    pub client_generation: u64,
    pub label: String,
    pub expires_at: String,
}

/// 手机端在线租约，仅保存在内存中。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MobileLease {
    pub sid: String,
    pub device_id: String,
    pub last_seen_at: String,
    pub expires_at: String,
}

/// 所有 axum handler 共享的应用状态。
pub struct AppState {
    /// 活跃的 Mac Bridge WebSocket 会话注册表（sid → session）。
    pub registry: SharedRegistry,
    /// HMAC 签名密钥，用于 JWT 签发/验证。
    pub secret: Arc<Vec<u8>>,
    /// 初始注册令牌，用于 /api/register 和 /api/unregister 的鉴权。
    pub setup_token: String,
    /// 已注册令牌名册：sid → StoredTokens。启动时从磁盘加载，注册/注销时更新并持久化。
    pub registered_tokens: Mutex<HashMap<String, StoredTokens>>,
    /// 持久化文件路径。
    pub registered_tokens_path: PathBuf,
    /// 手机端在线租约：sid + device_id → MobileLease。
    pub mobile_leases: Mutex<HashMap<String, MobileLease>>,
    /// 注册接口速率限制器（per-IP）。
    pub register_limiter: register::SharedIpRateLimiter,
    /// per-client（per-sid）代理请求速率限制器。
    pub client_limiter: register::SharedClientRateLimiter,
}

fn mobile_lease_key(sid: &str, device_id: &str) -> String {
    format!("{sid}\0{device_id}")
}

/// 计算 client token 的持久化 hash。只用于等值校验，不把原文落盘。
pub(crate) fn client_token_hash(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 刷新 sid + device_id 对应的手机端在线租约。
pub(crate) async fn update_mobile_lease(
    state: &Arc<AppState>,
    sid: &str,
    device_id: &str,
) -> MobileLease {
    let now = chrono::Utc::now();
    let expires_at = now + chrono::Duration::seconds(MOBILE_LEASE_TTL_SECS);
    let lease = MobileLease {
        sid: sid.to_string(),
        device_id: device_id.to_string(),
        last_seen_at: now.to_rfc3339(),
        expires_at: expires_at.to_rfc3339(),
    };
    state
        .mobile_leases
        .lock()
        .await
        .insert(mobile_lease_key(sid, device_id), lease.clone());
    lease
}

/// 查询手机端在线租约。device_id 缺省时返回该 sid 最新的租约。
pub(crate) async fn get_mobile_lease(
    state: &Arc<AppState>,
    sid: &str,
    device_id: &str,
) -> Option<MobileLease> {
    let leases = state.mobile_leases.lock().await;
    if device_id != "-" {
        return leases.get(&mobile_lease_key(sid, device_id)).cloned();
    }
    leases
        .values()
        .filter(|lease| lease.sid == sid)
        .max_by_key(|lease| lease.last_seen_at.clone())
        .cloned()
}

/// 查询 sid 下所有手机端 lease，按 last_seen_at 倒序返回。
pub(crate) async fn get_mobile_leases_for_sid(
    state: &Arc<AppState>,
    sid: &str,
) -> Vec<MobileLease> {
    let mut leases: Vec<MobileLease> = state
        .mobile_leases
        .lock()
        .await
        .values()
        .filter(|lease| lease.sid == sid)
        .cloned()
        .collect();
    leases.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
    leases
}

/// 判断租约在当前时间是否仍有效。
pub(crate) fn mobile_lease_online(lease: &MobileLease) -> bool {
    chrono::DateTime::parse_from_rfc3339(&lease.expires_at)
        .map(|exp| exp.with_timezone(&chrono::Utc) > chrono::Utc::now())
        .unwrap_or(false)
}

/// 获取 relay 状态目录（~/.agent-aspect-relay/）。
fn relay_state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(&home).join(".agent-aspect-relay")
}

/// 获取已注册令牌的持久化文件路径。
fn registered_tokens_path() -> PathBuf {
    relay_state_dir().join("registered_tokens.json")
}

/// 从磁盘加载已注册令牌名册，自动清理过期条目。
///
/// 若文件不存在则返回空 map。解析失败或读取失败直接退出进程，
/// 因为缺少名册意味着所有客户端都无法认证。
pub fn load_registered_tokens_from(path: &Path) -> HashMap<String, StoredTokens> {
    if !path.exists() {
        return HashMap::new();
    }

    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!(
            "agent-aspect-relay: read registered tokens failed at {}: {e}",
            path.display()
        );
        std::process::exit(1);
    });

    let now = chrono::Utc::now();
    let mut tokens: HashMap<String, StoredTokens> =
        serde_json::from_str(&raw).unwrap_or_else(|e| {
            eprintln!(
                "agent-aspect-relay: parse registered tokens failed at {}: {e}",
                path.display()
            );
            std::process::exit(1);
        });

    // 清理过期 token：expires_at 已过的条目直接丢弃；旧格式原文 token 启动时迁移为 hash。
    let before = tokens.len();
    let mut migrated = false;
    tokens.retain(
        |sid, item| match chrono::DateTime::parse_from_rfc3339(&item.expires_at) {
            Ok(exp) => exp.with_timezone(&chrono::Utc) > now,
            Err(e) => {
                eprintln!("agent-aspect-relay: drop token with invalid expires_at sid={sid}: {e}");
                false
            }
        },
    );
    for item in tokens.values_mut() {
        let mut migrated_from_raw = false;
        if item.client_token_hash.is_empty() && !item.client_token.is_empty() {
            item.client_token_hash = client_token_hash(&item.client_token);
            migrated_from_raw = true;
            migrated = true;
        }
        if item.client_generation == 0 && !migrated_from_raw {
            item.client_generation = 1;
            migrated = true;
        }
        if !item.client_token.is_empty() {
            item.client_token.clear();
            migrated = true;
        }
    }

    // 有条目被清理时重新写入磁盘
    if tokens.len() != before || migrated {
        if let Err(e) = save_registered_tokens_to(path, &tokens) {
            eprintln!("agent-aspect-relay: prune expired registered tokens failed: {e}");
            std::process::exit(1);
        }
    }

    eprintln!(
        "agent-aspect-relay: loaded {} registered token pair(s)",
        tokens.len()
    );
    tokens
}

/// 将已注册令牌名册持久化到磁盘。
///
/// 采用 write-to-tmp + rename 策略，保证原子性。Unix 下临时文件权限为 0600。
/// 失败时回滚：删除临时文件并返回错误。
pub fn save_registered_tokens_to(
    path: &Path,
    tokens: &HashMap<String, StoredTokens>,
) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("registered token path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {} failed: {e}", dir.display()))?;

    // 用 UUID v7 生成唯一临时文件名，避免并发冲突
    let tmp_path = path.with_extension(format!("json.tmp-{}", uuid::Uuid::now_v7()));
    let body = serde_json::to_vec_pretty(tokens)
        .map_err(|e| format!("serialize registered tokens failed: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)
            .map_err(|e| format!("open {} failed: {e}", tmp_path.display()))?;
        file.write_all(&body)
            .map_err(|e| format!("write {} failed: {e}", tmp_path.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync {} failed: {e}", tmp_path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&tmp_path, &body)
            .map_err(|e| format!("write {} failed: {e}", tmp_path.display()))?;
    }

    // 原子替换：rename 在同文件系统上是原子操作
    std::fs::rename(&tmp_path, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        format!(
            "rename {} to {} failed: {e}",
            tmp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

/// 加载或首次生成 HMAC 签名密钥。
///
/// 优先使用环境变量 RELAY_SECRET，否则从 ~/.agent-aspect-relay/relay.secret 读取。
/// 文件不存在时生成 32 字节随机密钥并持久化（权限 0600）。
fn load_or_create_secret() -> Vec<u8> {
    if let Ok(s) = std::env::var("RELAY_SECRET") {
        return s.into_bytes();
    }

    let dir = relay_state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("relay.secret");

    if path.exists() {
        return std::fs::read(&path).unwrap_or_else(|e| {
            eprintln!("agent-aspect-relay: read secret failed: {e}");
            std::process::exit(1);
        });
    }

    // 首次生成：32 字节密码学安全随机数
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random");
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(&buf)
            .expect("write secret");
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &buf).expect("write secret");
    }

    eprintln!("agent-aspect-relay: generated secret at {}", path.display());
    buf.to_vec()
}

/// 加载或首次生成 setup token（用于 /api/register 鉴权）。
///
/// 优先使用环境变量 RELAY_SETUP_TOKEN，否则从 ~/.agent-aspect-relay/setup.token 读取。
/// 文件不存在时生成 64 字符十六进制 token 并持久化（权限 0600）。
fn load_or_create_setup_token() -> String {
    if let Ok(t) = std::env::var("RELAY_SETUP_TOKEN") {
        return t;
    }

    let dir = relay_state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("setup.token");

    if path.exists() {
        return std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|e| {
                eprintln!("agent-aspect-relay: read setup token failed: {e}");
                std::process::exit(1);
            });
    }

    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random");
    let token: String = buf.iter().map(|b| format!("{b:02x}")).collect();

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(token.as_bytes())
            .expect("write setup token");
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &token).expect("write setup token");
    }

    eprintln!(
        "agent-aspect-relay: generated setup token at {}",
        path.display()
    );
    token
}

/// 启动 Relay HTTP 服务器。由 main.rs 的 `#[tokio::main]` 调用。
pub async fn run_server() {
    let listen_addr =
        std::env::var("RELAY_LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let secret = Arc::new(load_or_create_secret());
    let setup_token = load_or_create_setup_token();
    let registered_tokens_path = registered_tokens_path();
    let registered_tokens = load_registered_tokens_from(&registered_tokens_path);

    let state = Arc::new(AppState {
        registry: Arc::new(Mutex::new(SessionRegistry::new())),
        secret,
        setup_token,
        registered_tokens: Mutex::new(registered_tokens),
        registered_tokens_path,
        mobile_leases: Mutex::new(HashMap::new()),
        register_limiter: Arc::new(Mutex::new(register::IpRateLimiter::new())),
        client_limiter: Arc::new(Mutex::new(register::ClientRateLimiter::new())),
    });

    let app = server::app(state);

    let listener = tokio::net::TcpListener::bind(&listen_addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("agent-aspect-relay: bind {listen_addr} failed: {e}");
            std::process::exit(1);
        });

    eprintln!("agent-aspect-relay: listening on {listen_addr}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap_or_else(|e| {
        eprintln!("agent-aspect-relay: server error: {e}");
        std::process::exit(1);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_registered_tokens_migrates_legacy_client_token_without_generation_bump() {
        let path = std::env::temp_dir().join(format!(
            "agent-aspect-relay-legacy-token-{}.json",
            uuid::Uuid::now_v7()
        ));
        let raw_client = "legacy-client-token";
        let body = serde_json::json!({
            "sid-legacy": {
                "mac_token": "mac-token",
                "client_token": raw_client,
                "label": "legacy",
                "expires_at": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()
            }
        });
        std::fs::write(&path, body.to_string()).expect("write legacy token file");

        let loaded = load_registered_tokens_from(&path);
        let token = loaded.get("sid-legacy").expect("loaded token");
        assert_eq!(token.client_token_hash, client_token_hash(raw_client));
        assert_eq!(token.client_generation, 0);
        assert!(token.client_token.is_empty());

        let persisted = std::fs::read_to_string(&path).expect("read migrated token file");
        assert!(!persisted.contains("legacy-client-token"));
    }
}
