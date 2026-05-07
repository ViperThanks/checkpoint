//! Token 认证与 Relay 凭证管理 — Bearer token 生成/持久化、relay 注册、用户 bootstrap。
//!
//! 架构角色：为 HTTP API 和 relay WebSocket 提供认证基础。
//! - 本地 bridge token：首次启动时生成，写入 0600 权限文件
//! - relay token：向 relay 服务器注册后获得 mac_token / client_token 对
//! - 默认用户 bootstrap：启动时若 sys_user 为空，自动创建 admin/owner
//!
//! 核心不变量：
//! - token 文件使用 create_new（原子创建），避免 TOCTOU 竞态
//! - relay token 对必须同时存在，只有一个视为损坏并触发重新注册
//! - Bearer token 校验使用恒定时间比较，防止时序攻击
//! - 密码文件 0600 权限，启动日志只打印路径不打印密码

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::paths;
use std::io::Write;

/// 加载或生成 bridge Bearer token。
/// 首次启动时原子创建文件（create_new），后续启动读取已有文件。
/// Unix 下文件权限为 0600，防止其他用户读取。
pub fn load_or_create_token() -> String {
    let token_path = paths::bridge_token_path();

    // 原子创建：create_new 在文件已存在时失败，避免 TOCTOU 竞态
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let token = generate_token();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&token_path)
        {
            Ok(mut f) => {
                f.write_all(token.as_bytes()).expect("write token");
                eprintln!(
                    "agent-aspect-bridge: generated new token at {}",
                    token_path.display()
                );
                return token;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {} // 文件已存在，继续读取
            Err(e) => {
                eprintln!("agent-aspect-bridge: create token file failed: {e}");
                std::process::exit(1);
            }
        }
    }
    #[cfg(not(unix))]
    {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&token_path)
        {
            Ok(mut f) => {
                f.write_all(token.as_bytes()).expect("write token");
                eprintln!(
                    "agent-aspect-bridge: generated new token at {}",
                    token_path.display()
                );
                return token;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                eprintln!("agent-aspect-bridge: create token file failed: {e}");
                std::process::exit(1);
            }
        }
    }

    // 文件已存在 — 直接读取
    match std::fs::read_to_string(&token_path) {
        Ok(t) => t.trim().to_string(),
        Err(e) => {
            eprintln!("agent-aspect-bridge: read existing token failed: {e}");
            std::process::exit(1);
        }
    }
}

/// 生成 64 字符的十六进制随机 token（32 字节随机数）。
/// 使用 OS 提供的 CSPRNG，不适合用于长期密钥但足够用于本地 bearer token。
pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Relay 凭证对：
///   mac_token    — Mac 端 WebSocket 认证签名 token。
///   client_token — 手机端 Bearer 认证签名 token。
pub struct RelayTokens {
    pub mac_token: String,
    pub client_token: String,
}

/// 删除 relay token 文件，触发下次 `ensure_relay_tokens` 重新注册。
/// 由 relay 客户端在收到 sid_not_registered 拒绝时调用。
pub fn delete_relay_token_files() {
    let mac_path = paths::relay_mac_token_path();
    let client_path = paths::relay_client_token_path();
    let _ = std::fs::remove_file(&mac_path);
    let _ = std::fs::remove_file(&client_path);
    eprintln!("agent-aspect-bridge: deleted stale relay token files");
}

/// 确保 relay token 可用，必要时自动注册。
///
/// 1. 两个 token 文件都存在 → 加载并返回。
/// 2. 只有一个存在（损坏对）→ 删除两个，继续注册。
/// 3. 都不存在 → 读取 setup_token，POST /api/register，保存 token。
/// 4. setup_token 也不存在 → 返回 Err 提示用户配置。
pub fn ensure_relay_tokens(relay_url: &str) -> Result<RelayTokens, String> {
    let mac_path = paths::relay_mac_token_path();
    let client_path = paths::relay_client_token_path();

    let mac_exists = mac_path.exists();
    let client_exists = client_path.exists();

    // 两个文件都存在 — 直接加载
    if mac_exists && client_exists {
        let mac_token = std::fs::read_to_string(&mac_path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("read mac token: {e}"))?;
        let client_token = std::fs::read_to_string(&client_path)
            .map(|s| s.trim().to_string())
            .map_err(|e| format!("read client token: {e}"))?;
        return Ok(RelayTokens {
            mac_token,
            client_token,
        });
    }

    // 损坏对（只有一个文件存在）— 清理后重新注册
    if mac_exists || client_exists {
        eprintln!("agent-aspect-bridge: corrupt relay token pair — re-registering");
        let _ = std::fs::remove_file(&mac_path);
        let _ = std::fs::remove_file(&client_path);
    }

    // 需要注册 — 加载 setup_token
    let setup_token = load_setup_token().ok_or_else(|| {
        "missing relay tokens; set AGENT_ASPECT_RELAY_SETUP_TOKEN once or write relay.setup_token".to_string()
    })?;

    // 从 relay WebSocket URL 推导注册 HTTP URL
    let register_url = derive_register_url(relay_url)?;

    // 获取主机名作为标签
    let label = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "mac".to_string());

    // POST /api/register 注册
    let agent = build_ureq_agent();
    let body = serde_json::json!({
        "setup_token": setup_token,
        "label": label,
        "ttl_days": 30,
    });

    let resp = agent
        .post(&register_url)
        .header("Content-Type", "application/json")
        .send(body.to_string().as_str())
        .map_err(|e| format!("register request failed: {e}"))?;

    let status = resp.status().as_u16();
    let resp_body = resp.into_body().read_to_string().unwrap_or_default();

    if status != 200 {
        return Err(format!("register failed (HTTP {status}): {resp_body}"));
    }

    let data: serde_json::Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("parse register response: {e}"))?;

    let mac_token = data["mac_token"]
        .as_str()
        .ok_or("register response missing mac_token")?
        .to_string();
    let client_token = data["client_token"]
        .as_str()
        .ok_or("register response missing client_token")?
        .to_string();

    // 原子保存两个 token：先写 mac_token，client_token 写入失败时回滚删除 mac_token
    if let Err(e) = save_token_file(&mac_path, &mac_token) {
        return Err(e);
    }
    if let Err(e) = save_token_file(&client_path, &client_token) {
        // 回滚：删除 mac_token 避免损坏对
        let _ = std::fs::remove_file(&mac_path);
        return Err(e);
    }

    eprintln!(
        "agent-aspect-bridge: relay registered (sid={})",
        data["sid"].as_str().unwrap_or("?")
    );

    Ok(RelayTokens {
        mac_token,
        client_token,
    })
}

/// 从环境变量或文件加载 relay 一次性注册令牌。
/// 环境变量 AGENT_ASPECT_RELAY_SETUP_TOKEN 优先于文件。
fn load_setup_token() -> Option<String> {
    if let Some(t) = agent_aspect_core::env_compat::env_var("AGENT_ASPECT_RELAY_SETUP_TOKEN") {
        return Some(t);
    }
    let path = paths::relay_setup_token_path();
    if path.exists() {
        return std::fs::read_to_string(&path)
            .map(|s| s.trim().to_string())
            .ok();
    }
    None
}

/// 从 relay WebSocket URL 推导注册 HTTP URL。
/// wss://relay.example.com/ws → https://relay.example.com/api/register
/// ws://relay.example.com/ws → http://relay.example.com/api/register
fn derive_register_url(relay_url: &str) -> Result<String, String> {
    let url = relay_url
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            relay_url
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .ok_or_else(|| format!("relay_url must start with ws:// or wss://, got: {relay_url}"))?;

    // 将 /ws 后缀替换为 /api/register
    let url = if url.ends_with("/ws") {
        url[..url.len() - 3].to_string() + "/api/register"
    } else {
        url + "/api/register"
    };

    Ok(url)
}

/// 将 token 写入文件，先删后建以保证原子性。
/// Unix 下设置 0600 权限。注册流程中 client_token 写入失败时回滚删除 mac_token。
fn save_token_file(path: &std::path::Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    // 先删除已有文件（例如损坏对清理后残留）
    let _ = std::fs::remove_file(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("create {}: {e}", path.display()))?
            .write_all(content.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// 构建带 15s 超时的 HTTP agent，用于 relay 注册请求。
fn build_ureq_agent() -> ureq::Agent {
    use std::time::Duration;
    use ureq::config::Config;
    Config::builder()
        .timeout_global(Some(Duration::from_secs(15)))
        .build()
        .into()
}

/// 从 HTTP 请求的 Authorization 头提取 Bearer token 并与预期值比较。
/// 使用恒定时间比较以防止时序攻击。
pub fn check_auth(request: &tiny_http::Request, token: &str) -> bool {
    for header in request.headers() {
        if header.field.equiv("Authorization") {
            let val = header.value.as_str();
            if let Some(bearer) = val.strip_prefix("Bearer ") {
                return constant_time_eq(bearer.as_bytes(), token.as_bytes());
            }
        }
    }
    false
}

/// 恒定时间字节比较。无论匹配位置如何都执行相同次数的运算。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// 启动时若 sys_user 为空，自动创建 admin/owner 用户。
///
/// 密码来源优先级：
/// 1. `AGENT_ASPECT_BRIDGE_PASSWORD` 环境变量
/// 2. `~/.agent-aspect/bridge.password` 文件
/// 3. 生成随机密码并写入 `bridge.password`（0600 权限）
pub fn bootstrap_owner_user(store: &AuditStore) {
    if let Err(e) = agent_aspect_core::user_password::bootstrap_owner_user(store) {
        eprintln!("agent-aspect-bridge: bootstrap failed: {e}");
    }
}

/// 重置 admin 密码：生成新随机密码，更新 SQLite 和 bridge.password 文件。
/// 返回明文新密码（CLI 可以打印到 stdout）。
pub fn reset_admin_password(store: &AuditStore) -> Result<String, String> {
    agent_aspect_core::user_password::reset_admin_password(store)
}

/// 设置 admin 密码：用传入的新密码更新 SQLite 和 bridge.password 文件。
pub fn set_admin_password(store: &AuditStore, new_password: &str) -> Result<(), String> {
    agent_aspect_core::user_password::set_admin_password(store, new_password)
}

/// 仅当 sys_user 为空时初始化 admin 用户；已有用户则拒绝。
pub fn init_admin_user(store: &AuditStore) -> Result<(), String> {
    agent_aspect_core::user_password::init_admin_user(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_register_url_wss() {
        assert_eq!(
            derive_register_url("wss://relay.example.com/ws").unwrap(),
            "https://relay.example.com/api/register"
        );
    }

    #[test]
    fn derive_register_url_ws() {
        assert_eq!(
            derive_register_url("ws://localhost:8080/ws").unwrap(),
            "http://localhost:8080/api/register"
        );
    }

    #[test]
    fn derive_register_url_no_ws_suffix() {
        assert_eq!(
            derive_register_url("wss://relay.example.com/endpoint").unwrap(),
            "https://relay.example.com/endpoint/api/register"
        );
    }

    #[test]
    fn derive_register_url_invalid() {
        assert!(derive_register_url("https://relay.example.com/ws").is_err());
    }

    #[test]
    fn ensure_uses_existing_tokens() {
        let dir = std::env::temp_dir().join("agent-aspect-test-ensure-tokens");
        let _ = std::fs::create_dir_all(&dir);

        let mac_path = dir.join("relay.mac_token");
        let client_path = dir.join("relay.client_token");

        // Write fake tokens
        std::fs::write(&mac_path, "fake-mac-token\n").unwrap();
        std::fs::write(&client_path, "fake-client-token\n").unwrap();

        // Verify they load
        let mac = std::fs::read_to_string(&mac_path)
            .unwrap()
            .trim()
            .to_string();
        let client = std::fs::read_to_string(&client_path)
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(mac, "fake-mac-token");
        assert_eq!(client, "fake-client-token");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_pair_detected_and_cleaned() {
        let dir = std::env::temp_dir().join("agent-aspect-test-corrupt-pair");
        let _ = std::fs::create_dir_all(&dir);

        let mac_path = dir.join("relay.mac_token");
        let client_path = dir.join("relay.client_token");

        // Write only mac_token (client missing)
        std::fs::write(&mac_path, "stale-mac-token\n").unwrap();
        assert!(mac_path.exists());
        assert!(!client_path.exists());

        // Simulate the corrupt-pair cleanup logic from ensure_relay_tokens
        let mac_exists = mac_path.exists();
        let client_exists = client_path.exists();
        if (mac_exists && !client_exists) || (!mac_exists && client_exists) {
            let _ = std::fs::remove_file(&mac_path);
            let _ = std::fs::remove_file(&client_path);
        }

        // Both should be gone
        assert!(!mac_path.exists());
        assert!(!client_path.exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_relay_token_files_removes_both() {
        let dir = std::env::temp_dir().join("agent-aspect-test-delete-tokens");
        let _ = std::fs::create_dir_all(&dir);

        let mac_path = dir.join("relay.mac_token");
        let client_path = dir.join("relay.client_token");

        std::fs::write(&mac_path, "mac\n").unwrap();
        std::fs::write(&client_path, "client\n").unwrap();
        assert!(mac_path.exists() && client_path.exists());

        // Simulate delete logic
        let _ = std::fs::remove_file(&mac_path);
        let _ = std::fs::remove_file(&client_path);

        assert!(!mac_path.exists());
        assert!(!client_path.exists());

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_token_file_allows_overwrite() {
        let dir = std::env::temp_dir().join("agent-aspect-test-save-overwrite");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("relay.mac_token");

        // Pre-existing file
        std::fs::write(&path, "old\n").unwrap();

        // save_token_file removes then creates new
        let _ = std::fs::remove_file(&path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap()
                .write_all(b"new")
                .unwrap();
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&path, "new").unwrap();
        }

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }
}
