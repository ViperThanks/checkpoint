//! WebSocket 长连接处理（Mac Bridge ↔ Relay）。
//!
//! 职责：管理 Mac Bridge 的 WebSocket 连接生命周期，包括认证注册、
//! 消息读写（proxy_response/pong）、心跳 ping、断线清理。
//!
//! 架构角色：Relay 与 Mac Bridge 之间的唯一通信通道。所有 HTTP 代理请求
//! 通过 SessionRegistry 转入 WS 写通道，Bridge 响应通过读通道接收。
//!
//! 不变量：
//! - WS 连接建立后第一条消息必须是 "register" 帧，携带有效的 mac_token。
//! - 同一 sid 的新连接会踢掉旧连接（通过 shutdown channel 通知旧连接退出）。
//! - 连接断开时，所有未完成的 pending request 被标记为失败。

use crate::token;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// 心跳 ping 间隔（秒）。
const HB_INTERVAL_SECS: u64 = 5;
/// 连续未收到 pong 的最大次数，超过则判定连接死亡并断开。
const MAX_MISSED_PONGS: u32 = 3;

/// WebSocket 升级入口。axum 自动处理 HTTP → WS 协议升级。
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<crate::AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, state))
}

/// WebSocket 连接的核心处理循环。
///
/// 启动三个并发任务：
/// 1. read_task：从 Bridge 接收 proxy_response / pong / close 消息
/// 2. write_task：将队列中的消息发送到 Bridge
/// 3. heartbeat_task：每 5 秒发送 ping 帧
///
/// 任意一个任务退出即触发整体清理。
async fn handle_ws(socket: WebSocket, state: Arc<crate::AppState>) {
    let (mut ws_sink, mut ws_stream) = socket.split();

    // 第一条消息必须是 register 帧，携带签名的 mac_token
    let sid = match wait_for_register(&mut ws_stream, &mut ws_sink, &state).await {
        Some(sid) => sid,
        None => {
            return;
        }
    };

    // 注册到会话注册表，获取消息接收通道和 shutdown 信号
    let (tx, mut rx) = mpsc::channel::<String>(32);
    let (shutdown_rx, connection_id) = {
        let mut reg = state.registry.lock().await;
        reg.register(sid.clone(), tx)
    };

    let token = sid.clone();
    let token_write = sid.clone();
    let connection_id_read = connection_id.clone();
    let connection_id_cleanup = connection_id.clone();
    let registry_read = state.registry.clone();
    let registry_write = state.registry.clone();

    // pong 状态标志：read_task 收到 pong 时设为 true，heartbeat_task 每轮检查并重置
    let pong_received = Arc::new(AtomicBool::new(true));
    let pong_received_read = pong_received.clone();
    let pong_received_hb = pong_received.clone();

    // 读任务：从 Mac Bridge 接收消息并分发
    let read_task = tokio::spawn(async move {
        while let Some(msg) = ws_stream.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let parsed: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

                    match msg_type {
                        // Bridge 返回的代理响应：完成对应的 pending request
                        "proxy_response" => {
                            if let Ok(resp) =
                                serde_json::from_value::<crate::protocol::ProxyResponse>(parsed)
                            {
                                let rid = resp.request_id.clone();
                                registry_read
                                    .lock()
                                    .await
                                    .complete_request(&token, &rid, resp);
                            }
                        }
                        "pong" => {
                            pong_received_read.store(true, Ordering::Relaxed);
                        }
                        other => {
                            eprintln!(
                                "agent-aspect-relay: unknown WS frame type '{other}' from sid {}...",
                                &token[..8.min(token.len())]
                            );
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(_) => break,
                _ => {}
            }
        }
        // 读循环退出 = Bridge 断开，清理会话
        registry_read
            .lock()
            .await
            .unregister_if_current(&token, &connection_id_read);
    });

    // 写任务：将待发送消息推入 WebSocket
    let write_task = tokio::spawn(async move {
        while let Some(text) = rx.recv().await {
            if ws_sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // 心跳任务：每 HB_INTERVAL_SECS 秒发送 ping 帧，检测连接活性。
    // 连续 MAX_MISSED_PONGS 次未收到 pong 则判定连接死亡，触发断连。
    let hb_token = sid.clone();
    let hb_registry = state.registry.clone();
    let (hb_cancel_tx, mut hb_cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(HB_INTERVAL_SECS));
        let mut missed_count: u32 = 0;
        // 跳过首次立即触发，等待第一个间隔
        tokio::select! {
            _ = &mut hb_cancel_rx => { return; },
            _ = interval.tick() => {},
        }
        loop {
            interval.tick().await;

            // 检查上一轮 pong 状态
            if pong_received_hb.swap(false, Ordering::Relaxed) {
                missed_count = 0;
            } else {
                missed_count += 1;
                if missed_count >= MAX_MISSED_PONGS {
                    eprintln!(
                        "agent-aspect-relay: WS heartbeat dead — {} missed pongs for sid {}...",
                        missed_count,
                        &hb_token[..8.min(hb_token.len())]
                    );
                    // 注销会话以触发整体清理（read_task/write_task 会随之退出）
                    hb_registry.lock().await.unregister(&hb_token);
                    break;
                }
            }

            let ping = serde_json::json!({
                "type": "ping",
                "timestamp": chrono::Utc::now().to_rfc3339(),
            })
            .to_string();
            let reg = hb_registry.lock().await;
            if let Some(tx) = reg.get_sender(&hb_token) {
                if tx.send(ping).await.is_err() {
                    break;
                }
            } else {
                break;
            }
        }
    });

    // 等待任意一个任务退出或收到 shutdown 信号（被新连接踢掉）
    tokio::select! {
        _ = read_task => {},
        _ = write_task => {},
        _ = shutdown_rx => {
            eprintln!("agent-aspect-relay: session kicked for sid {}...", &token_write[..8.min(token_write.len())]);
        },
    }

    // 清理：停止心跳、标记未完成请求失败、注销会话
    let _ = hb_cancel_tx.send(());
    heartbeat_task.abort();
    registry_write.lock().await.fail_pending_if_current(
        &token_write,
        &connection_id_cleanup,
        "mac_disconnected",
    );
    registry_write
        .lock()
        .await
        .unregister_if_current(&token_write, &connection_id_cleanup);
}

/// WS 注册超时：连接建立后必须在此时限内发送 register 帧。
const WS_REGISTER_TIMEOUT_SECS: u64 = 10;

/// 等待 WebSocket 连接的第一条 register 帧，验证 mac_token 并返回 sid。
///
/// 验证流程：消息类型必须为 "register" → mac_token 非空 → 签名有效 →
/// 角色为 "mac" → sid 在已注册名册中。任一步失败则关闭连接。
/// 超时未发送 register 帧则断开，防止连接悬挂 DoS。
async fn wait_for_register(
    ws_stream: &mut (impl StreamExt<Item = Result<Message, axum::Error>> + Unpin),
    ws_sink: &mut (impl SinkExt<Message, Error = axum::Error> + Unpin),
    state: &Arc<crate::AppState>,
) -> Option<String> {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(WS_REGISTER_TIMEOUT_SECS),
        ws_stream.next(),
    )
    .await;

    let msg = match result {
        Ok(Some(msg)) => msg,
        Ok(None) => {
            let _ = ws_sink.close().await;
            return None;
        }
        Err(_) => {
            eprintln!("agent-aspect-relay: WS register timeout — closing connection");
            let _ = ws_sink.close().await;
            return None;
        }
    };
    let text = match msg {
        Ok(Message::Text(t)) => t,
        _ => {
            let _ = ws_sink.close().await;
            return None;
        }
    };

    let frame: serde_json::Value = serde_json::from_str(&text).ok()?;
    if frame.get("type")?.as_str()? != "register" {
        let _ = ws_sink.close().await;
        return None;
    }

    let mac_token = frame.get("mac_token")?.as_str()?;
    if mac_token.is_empty() {
        let _ = ws_sink.close().await;
        return None;
    }

    // 验证 mac_token 的签名和有效期
    let verified = match token::verify_token(&state.secret, mac_token) {
        Ok(v) => v,
        Err(e) => {
            send_error(ws_sink, &format!("invalid_token: {e}")).await;
            let _ = ws_sink.close().await;
            return None;
        }
    };

    // 只有 mac 角色的 token 才能建立 WebSocket 连接
    if verified.payload.role != "mac" {
        send_error(ws_sink, "wrong_token_role").await;
        let _ = ws_sink.close().await;
        return None;
    }

    let sid = verified.payload.sid;

    // sid 和 mac_token 原文必须匹配名册，防止已轮换或已注销的旧 token 建立 WS。
    {
        let tokens = state.registered_tokens.lock().await;
        let Some(stored) = tokens.get(&sid) else {
            eprintln!("agent-aspect-relay: WS register rejected — sid not registered");
            send_error(ws_sink, "sid_not_registered").await;
            let _ = ws_sink.close().await;
            return None;
        };
        if stored.mac_token != mac_token {
            eprintln!("agent-aspect-relay: WS register rejected — mac token revoked");
            send_error(ws_sink, "token_revoked").await;
            let _ = ws_sink.close().await;
            return None;
        }
    }

    Some(sid)
}

/// 向 WebSocket 对端发送错误帧。
async fn send_error(
    ws_sink: &mut (impl SinkExt<Message, Error = axum::Error> + Unpin),
    message: &str,
) {
    let error_frame = serde_json::json!({
        "type": "error",
        "message": message,
    })
    .to_string();
    let _ = ws_sink.send(Message::Text(error_frame.into())).await;
}
