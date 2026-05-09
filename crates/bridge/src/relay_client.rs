//! Relay WebSocket 客户端 — 连接远程 Relay，代理手机请求到本地 Bridge。
//!
//! 架构角色：Mac Bridge 的「出站代理」。当配置了 relay_url 时，
//! 后台线程通过 WebSocket 连接到 Relay 服务器，接收手机端请求，
//! 转发到本地 localhost Bridge HTTP API，再将响应回传。
//!
//! 核心设计：
//! - 每个代理请求在独立 worker 线程执行（25s 超时），
//!   防止单个慢请求阻塞 WebSocket 读循环
//! - WebSocket 读超时 200ms，确保 worker 响应及时刷出
//! - 断线后指数退避重连（1s → 2s → 4s → ... → 30s max）
//! - 收到 sid_not_registered 时自动删 token + 重新注册

use std::collections::HashMap;
use std::sync::mpsc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// 帧类型（与 relay crate protocol 镜像，内联避免循环依赖）
// ---------------------------------------------------------------------------

/// 注册帧：WS 连接建立后发送的第一个消息，携带 mac_token 完成认证。
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct RegisterFrame {
    r#type: String,
    mac_token: String,
}

/// 从 Relay 收到的代理请求（手机端 → Relay → WS → Bridge）。
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct ProxyRequest {
    #[serde(rename = "type")]
    msg_type: String,
    request_id: String,
    method: String,
    path: String,
    query: Option<String>,
    headers: HashMap<String, String>,
    body: Option<String>,
}

/// 回传给 Relay 的代理响应（Bridge 处理结果 → WS → Relay → 手机端）。
#[derive(serde::Serialize, serde::Deserialize, Debug)]
struct ProxyResponse {
    r#type: String,
    request_id: String,
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

// ---------------------------------------------------------------------------
// 公共 API
// ---------------------------------------------------------------------------

/// Relay 客户端配置：连接和认证参数。
pub struct RelayConfig {
    pub relay_url: String,
    pub mac_token: String,
    pub client_token: String,
    pub bridge_token: String,
    pub bridge_port: u16,
}

/// 在后台线程启动 relay 客户端。非阻塞，调用后立即返回。
pub fn spawn_relay_client(config: RelayConfig) {
    std::thread::Builder::new()
        .name("relay-client".into())
        .spawn(move || relay_client_loop(config))
        .expect("spawn relay client thread");
}

// ---------------------------------------------------------------------------
// 主循环：指数退避重连 + 自动恢复
// ---------------------------------------------------------------------------

/// Relay 客户端主循环。断线后指数退避重连，sid 失效时自动重新注册。
fn relay_client_loop(mut config: RelayConfig) {
    let mut backoff_secs: u64 = 1;
    const MAX_BACKOFF_SECS: u64 = 30;

    loop {
        let mut was_ready = false;
        match connect_and_serve(&config, &mut was_ready) {
            Ok(()) => {
                eprintln!("agent-aspect-bridge: relay client disconnected");
                if was_ready {
                    backoff_secs = 1;
                }
            }
            Err(e) => {
                if was_ready {
                    backoff_secs = 1;
                }
                if e.contains("sid_not_registered")
                    || e.contains("token_expired")
                    || e.contains("token_revoked")
                {
                    eprintln!("agent-aspect-bridge: relay rejected token — re-registering");
                    crate::auth::delete_relay_token_files();
                    match crate::auth::ensure_relay_tokens(&config.relay_url) {
                        Ok(new_tokens) => {
                            config.mac_token = new_tokens.mac_token;
                            config.client_token = new_tokens.client_token;
                            backoff_secs = 1;
                            continue; // retry immediately
                        }
                        Err(reg_err) => {
                            eprintln!("agent-aspect-bridge: relay re-register failed: {reg_err}");
                        }
                    }
                } else {
                    eprintln!("agent-aspect-bridge: relay client error: {e}");
                }
            }
        }
        eprintln!("agent-aspect-bridge: relay reconnecting in {backoff_secs}s");
        std::thread::sleep(Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(MAX_BACKOFF_SECS);
    }
}

// ---------------------------------------------------------------------------
// WebSocket 连接 + 请求代理主循环
// ---------------------------------------------------------------------------

/// 构建 25s 超时的 HTTP agent，用于向本地 Bridge 发请求。
fn build_ureq_agent() -> ureq::Agent {
    use ureq::config::Config;
    Config::builder()
        .timeout_global(Some(Duration::from_secs(25)))
        .build()
        .into()
}

/// 建立连接、完成注册、进入请求代理循环。
/// was_ready 标记是否曾成功注册（用于区分首次失败和断线重连）。
fn connect_and_serve(config: &RelayConfig, was_ready: &mut bool) -> Result<(), String> {
    let mut socket = tungstenite::connect(&config.relay_url)
        .map_err(|e| format!("ws connect: {e}"))?
        .0;

    // Register with signed mac_token
    let reg = RegisterFrame {
        r#type: "register".to_string(),
        mac_token: config.mac_token.clone(),
    };
    socket
        .send(tungstenite::Message::Text(
            serde_json::to_string(&reg)
                .map_err(|e| format!("serialize register: {e}"))?
                .into(),
        ))
        .map_err(|e| format!("send register: {e}"))?;

    // Wait for first response: could be error frame (register rejected) or
    // a heartbeat ping (register accepted). We need to peek at the first
    // message to detect rejection.
    let first_msg = socket
        .read()
        .map_err(|e| format!("ws read after register: {e}"))?;

    match &first_msg {
        tungstenite::Message::Text(text) => {
            let parsed: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
            if parsed.get("type").and_then(|v| v.as_str()) == Some("error") {
                let message = parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(format!("register_rejected: {message}"));
            }
        }
        tungstenite::Message::Close(_) => {
            return Err("register_rejected: connection closed".to_string());
        }
        _ => {}
    }

    eprintln!(
        "agent-aspect-bridge: relay registered to {}",
        config.relay_url
    );
    *was_ready = true;

    // Set a 200ms read timeout after the registration handshake so outbound
    // messages are flushed promptly instead of waiting for the next relay ping.
    set_read_timeout(&mut socket, Duration::from_millis(200));

    // Channel for worker threads (and pong) to send outbound messages
    let (outbound_tx, outbound_rx) = mpsc::channel::<String>();

    // Handle the first message we already read
    match &first_msg {
        tungstenite::Message::Text(text) => {
            handle_text(text, config, &outbound_tx);
        }
        tungstenite::Message::Ping(data) => {
            socket
                .send(tungstenite::Message::Pong(data.clone()))
                .map_err(|e| format!("pong: {e}"))?;
        }
        _ => {}
    }
    // Drain any outbound messages from the first message handling
    while let Ok(out) = outbound_rx.try_recv() {
        socket
            .send(tungstenite::Message::Text(out.into()))
            .map_err(|e| format!("send: {e}"))?;
    }

    loop {
        // 1. Read from WebSocket — returns after at most 200ms (read timeout)
        match socket.read() {
            Ok(tungstenite::Message::Text(text)) => {
                handle_text(&text, config, &outbound_tx);
            }
            Ok(tungstenite::Message::Ping(data)) => {
                socket
                    .send(tungstenite::Message::Pong(data))
                    .map_err(|e| format!("pong: {e}"))?;
            }
            Ok(tungstenite::Message::Close(_)) => {
                eprintln!("agent-aspect-bridge: relay connection closed by server");
                return Ok(());
            }
            Err(tungstenite::Error::Io(ref e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                // Read timed out — no inbound message, continue to drain outbound
            }
            Err(e) => return Err(format!("ws read: {e}")),
            Ok(_) => {}
        }

        // 2. Drain all pending outbound messages (worker responses + pongs)
        while let Ok(msg) = outbound_rx.try_recv() {
            socket
                .send(tungstenite::Message::Text(msg.into()))
                .map_err(|e| format!("send: {e}"))?;
        }
    }
}

/// 处理从 Relay 收到的 WS 文本消息：分发 proxy_request 到 worker 线程，ping 回 pong。
fn handle_text(text: &str, config: &RelayConfig, outbound_tx: &mpsc::Sender<String>) {
    let parsed: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
    };

    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "proxy_request" => {
            let req: ProxyRequest = match serde_json::from_value(parsed) {
                Ok(r) => r,
                Err(_) => return,
            };

            // Spawn a worker thread — never blocks the WS read loop
            let tx = outbound_tx.clone();
            let bridge_token = config.bridge_token.clone();
            let bridge_port = config.bridge_port;
            std::thread::spawn(move || {
                let agent = build_ureq_agent();
                let resp = proxy_to_localhost(&req, &bridge_token, bridge_port, &agent);
                let json = serde_json::to_string(&resp).unwrap_or_default();
                let _ = tx.send(json);
            });
        }
        "ping" => {
            let ts = parsed
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let pong = serde_json::json!({"type": "pong", "timestamp": ts}).to_string();
            let _ = outbound_tx.send(pong);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 辅助函数
// ---------------------------------------------------------------------------

/// 设置 WebSocket 底层 TCP 流的读超时。确保 socket.read() 最多阻塞 timeout，
/// 让 outbound 消息能及时刷出（不等 Relay 下一次 ping）。
fn set_read_timeout(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    timeout: Duration,
) {
    match socket.get_mut() {
        tungstenite::stream::MaybeTlsStream::Plain(tcp) => {
            tcp.set_read_timeout(Some(timeout)).ok();
        }
        tungstenite::stream::MaybeTlsStream::Rustls(stream) => {
            stream.get_mut().set_read_timeout(Some(timeout)).ok();
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// 本地 HTTP 代理（在 worker 线程中运行）
// ---------------------------------------------------------------------------

/// 将 Relay 转发的请求通过 HTTP 发送到本地 Bridge，返回响应。
fn proxy_to_localhost(
    req: &ProxyRequest,
    bridge_token: &str,
    bridge_port: u16,
    agent: &ureq::Agent,
) -> ProxyResponse {
    let url = match &req.query {
        Some(q) => format!("http://127.0.0.1:{bridge_port}{}?{q}", req.path),
        None => format!("http://127.0.0.1:{bridge_port}{}", req.path),
    };

    match req.method.as_str() {
        "GET" => {
            let r = agent
                .get(&url)
                .header("Authorization", &format!("Bearer {bridge_token}"))
                .call();
            match r {
                Ok(resp) => read_response(resp, &req.request_id),
                Err(e) => handle_ureq_error(e, &req.request_id),
            }
        }
        "POST" => {
            let body = req.body.as_deref().unwrap_or("");
            let ct = req
                .headers
                .get("content-type")
                .map(|s| s.as_str())
                .unwrap_or("application/json");
            let r = agent
                .post(&url)
                .header("Authorization", &format!("Bearer {bridge_token}"))
                .header("Content-Type", ct)
                .send(body);
            match r {
                Ok(resp) => read_response(resp, &req.request_id),
                Err(e) => handle_ureq_error(e, &req.request_id),
            }
        }
        _ => err_response(&req.request_id, 400, "unsupported method"),
    }
}

fn read_response(mut resp: ureq::http::Response<ureq::Body>, request_id: &str) -> ProxyResponse {
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    ProxyResponse {
        r#type: "proxy_response".to_string(),
        request_id: request_id.to_string(),
        status,
        headers: HashMap::from([("Content-Type".to_string(), ct)]),
        body,
    }
}

fn handle_ureq_error(e: ureq::Error, request_id: &str) -> ProxyResponse {
    match &e {
        ureq::Error::StatusCode(code) => ProxyResponse {
            r#type: "proxy_response".to_string(),
            request_id: request_id.to_string(),
            status: *code,
            headers: HashMap::new(),
            body: format!("{{\"error\":\"upstream returned {code}\"}}"),
        },
        _ => err_response(request_id, 502, &format!("localhost proxy: {e}")),
    }
}

fn err_response(request_id: &str, status: u16, message: &str) -> ProxyResponse {
    ProxyResponse {
        r#type: "proxy_response".to_string(),
        request_id: request_id.to_string(),
        status,
        headers: HashMap::from([("Content-Type".to_string(), "application/json".to_string())]),
        body: serde_json::json!({"error": message}).to_string(),
    }
}
