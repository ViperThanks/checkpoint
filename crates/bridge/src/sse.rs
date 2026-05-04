//! SSE 实时推送 — 向浏览器 Dashboard 推送事件流。
//!
//! 架构角色：Dashboard 前端通过 `GET /stream?token=...` 建立长连接，
//! 实时接收模式切换、决策、job 状态和日志输出等事件。
//!
//! 核心不变量：
//! - broadcast 时自动清理已断开的 client（retain 过滤 send 失败的）
//! - SSE 连接绕过 tiny_http 的 chunked 编码，直接写 TCP 流
//! - token 通过 query param 传递（EventSource API 不支持自定义 header）

use std::sync::mpsc;

/// SSE 事件：event_type 为事件名，data 为 JSON 字符串。
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
}

/// 最大同时 SSE 客户端数。超出时拒绝新连接。
const MAX_SSE_CLIENTS: usize = 20;

/// 广播器：维护所有活跃 SSE 客户端的 channel 列表。
pub struct SseBroadcaster {
    clients: Vec<mpsc::Sender<Option<SseEvent>>>,
}

/// 共享广播器类型：Arc<Mutex<>> 保证线程安全。
pub type SharedBroadcaster = std::sync::Arc<std::sync::Mutex<SseBroadcaster>>;

impl SseBroadcaster {
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
        }
    }

    pub fn shared() -> SharedBroadcaster {
        std::sync::Arc::new(std::sync::Mutex::new(Self::new()))
    }

    /// 注册新客户端，返回事件接收端。超过 MAX_SSE_CLIENTS 时返回 None。
    pub fn add_client(&mut self) -> Option<mpsc::Receiver<Option<SseEvent>>> {
        if self.clients.len() >= MAX_SSE_CLIENTS {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        self.clients.push(tx);
        Some(rx)
    }

    /// 广播事件到所有活跃客户端。send 失败的客户端自动移除。
    pub fn broadcast(&mut self, event: SseEvent) {
        self.clients.retain(|tx| {
            tx.send(Some(SseEvent {
                event_type: event.event_type.clone(),
                data: event.data.clone(),
            }))
            .is_ok()
        });
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }
}

/// SSE 长连接处理：绕过 tiny_http 的 chunked 编码，直接写 TCP 流。
/// 避免 tiny_http 对小事件的缓冲延迟。
pub fn handle_sse_raw(request: tiny_http::Request, receiver: mpsc::Receiver<Option<SseEvent>>) {
    use std::io::Write;

    let mut writer = request.into_writer();

    // Write HTTP response headers directly
    let header = "HTTP/1.1 200 OK\r\n\
                  Content-Type: text/event-stream\r\n\
                  Cache-Control: no-cache\r\n\
                  Connection: keep-alive\r\n\
                  \r\n";
    if let Err(e) = writer.write_all(header.as_bytes()) {
        eprintln!("agent-aspect-bridge: SSE write header: {e}");
        return;
    }
    if let Err(e) = writer.flush() {
        eprintln!("agent-aspect-bridge: SSE flush header: {e}");
        return;
    }

    // Send events as they arrive
    loop {
        match receiver.recv() {
            Ok(Some(event)) => {
                let frame = format!("event: {}\ndata: {}\n\n", event.event_type, event.data);
                if let Err(e) = writer.write_all(frame.as_bytes()) {
                    eprintln!("agent-aspect-bridge: SSE write event: {e}");
                    return;
                }
                if let Err(e) = writer.flush() {
                    eprintln!("agent-aspect-bridge: SSE flush event: {e}");
                    return;
                }
            }
            Ok(None) | Err(_) => return, // Stream ends or broadcaster dropped
        }
    }
}

/// 从 query param `?token=xxx` 校验认证。EventSource API 不支持自定义 header，
/// 所以 SSE 端点只能通过 URL 参数传递 token。
pub fn check_query_auth(url: &str, token: &str) -> bool {
    let query = match url.split('?').nth(1) {
        Some(q) => q,
        None => return false,
    };
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some(k), Some(v)) if k == "token" => {
                return crate::auth::constant_time_eq(v.as_bytes(), token.as_bytes());
            }
            _ => {}
        }
    }
    false
}
