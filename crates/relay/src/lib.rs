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
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

/// 已注册的令牌对（mac_token + client_token），持久化到 registered_tokens.json。
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StoredTokens {
    pub mac_token: String,
    pub client_token: String,
    pub label: String,
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
    /// 注册接口速率限制器（per-IP）。
    pub register_limiter: register::SharedIpRateLimiter,
    /// per-client（per-sid）代理请求速率限制器。
    pub client_limiter: register::SharedClientRateLimiter,
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

    // 清理过期 token：expires_at 已过的条目直接丢弃
    let before = tokens.len();
    tokens.retain(
        |sid, item| match chrono::DateTime::parse_from_rfc3339(&item.expires_at) {
            Ok(exp) => exp.with_timezone(&chrono::Utc) > now,
            Err(e) => {
                eprintln!("agent-aspect-relay: drop token with invalid expires_at sid={sid}: {e}");
                false
            }
        },
    );

    // 有条目被清理时重新写入磁盘
    if tokens.len() != before {
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
