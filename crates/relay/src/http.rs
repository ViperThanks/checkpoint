//! Relay 代理层：手机端 HTTP 请求转发到 Mac Bridge。
//!
//! 职责：接收手机端 API 请求，通过 WebSocket 转发给 Mac Bridge，
//! 等待 Bridge 返回响应后回传给手机端。
//!
//! 架构角色：手机端 ↔ Relay ↔ Mac Bridge 三层架构的中间代理。
//!
//! 不变量：
//! - body 透传，不做任何修改。前端构造的 conversation_id / provider / prompt
//!   原封不动转发到 bridge。
//! - 路径白名单控制哪些 endpoint 可以被代理。
//! - 认证由 auth::VerifiedClient extractor 在 handler 之前完成。

use crate::auth::VerifiedClient;
use crate::protocol::ProxyRequest;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// 允许代理的 GET 路径（精确匹配）。
/// 须与 server.rs 路由定义保持同步。
const ALLOWED_GET_PATHS: &[&str] = &[
    "/health",
    "/overview",
    "/pending",
    "/run/context",
    "/jobs",
    "/conversations",
    "/workflows",
];

/// 允许代理的 GET 路径前缀（前缀匹配）。
const ALLOWED_GET_PREFIXES: &[&str] = &["/conversations/", "/jobs/", "/workflows/"];

/// 允许代理的 POST 路径（精确匹配）。
const ALLOWED_POST_PATHS: &[&str] = &["/jobs", "/decide", "/workflows"];

/// 允许代理的 POST 路径前缀（前缀匹配）。
const ALLOWED_POST_PREFIXES: &[&str] = &["/jobs/", "/workflows/"];

/// 允许代理的 PUT 路径（精确匹配）。
const ALLOWED_PUT_PATHS: &[&str] = &[];

/// 允许代理的 PUT 路径前缀（前缀匹配）。
const ALLOWED_PUT_PREFIXES: &[&str] = &["/workflows/"];

/// 允许代理的 DELETE 路径（精确匹配）。
const ALLOWED_DELETE_PATHS: &[&str] = &[];

/// 允许代理的 DELETE 路径前缀（前缀匹配）。
const ALLOWED_DELETE_PREFIXES: &[&str] = &["/workflows/"];

/// GET 请求代理入口：无 body。
pub async fn proxy_get(
    State(state): State<Arc<crate::AppState>>,
    client: VerifiedClient,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    proxy_request(state, client, method, uri, headers, None).await
}

/// POST 请求代理入口：携带 body 透传。
pub async fn proxy_post(
    State(state): State<Arc<crate::AppState>>,
    client: VerifiedClient,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    proxy_request(state, client, method, uri, headers, Some(body)).await
}

/// PUT 请求代理入口：携带 body 透传。
pub async fn proxy_put(
    State(state): State<Arc<crate::AppState>>,
    client: VerifiedClient,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    proxy_request(state, client, method, uri, headers, Some(body)).await
}

/// DELETE 请求代理入口：无 body。
pub async fn proxy_delete(
    State(state): State<Arc<crate::AppState>>,
    client: VerifiedClient,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    proxy_request(state, client, method, uri, headers, None).await
}

/// 核心代理逻辑：路径白名单检查 → 构造 ProxyRequest → 通过 WS 发送到 Bridge → 等待响应。
///
/// 认证已由 VerifiedClient extractor 完成。超时 30 秒。
async fn proxy_request(
    state: Arc<crate::AppState>,
    client: VerifiedClient,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Option<String>,
) -> Response {
    let start = Instant::now();

    let bridge_path = uri
        .path()
        .strip_prefix("/api")
        .unwrap_or(uri.path())
        .to_string();
    let method_str = method.to_string();

    // 路径白名单校验
    let is_get = method == Method::GET;
    let is_post = method == Method::POST;
    let is_put = method == Method::PUT;
    let is_delete = method == Method::DELETE;
    let allowed = if is_get {
        ALLOWED_GET_PATHS.contains(&bridge_path.as_str())
            || ALLOWED_GET_PREFIXES
                .iter()
                .any(|p| bridge_path.starts_with(p))
    } else if is_post {
        ALLOWED_POST_PATHS.contains(&bridge_path.as_str())
            || ALLOWED_POST_PREFIXES
                .iter()
                .any(|p| bridge_path.starts_with(p))
    } else if is_put {
        ALLOWED_PUT_PATHS.contains(&bridge_path.as_str())
            || ALLOWED_PUT_PREFIXES
                .iter()
                .any(|p| bridge_path.starts_with(p))
    } else if is_delete {
        ALLOWED_DELETE_PATHS.contains(&bridge_path.as_str())
            || ALLOWED_DELETE_PREFIXES
                .iter()
                .any(|p| bridge_path.starts_with(p))
    } else {
        false
    };

    if !allowed {
        log_access(
            &method_str,
            &bridge_path,
            &client.device_id,
            403,
            Some("forbidden"),
            None,
            start,
        );
        return (StatusCode::FORBIDDEN, r#"{"error":"endpoint not allowed"}"#).into_response();
    }

    // 构造 ProxyRequest 并序列化
    let request_id = uuid::Uuid::now_v7().to_string();
    let query = uri.query().map(String::from);

    let proxy_req = ProxyRequest {
        r#type: "proxy_request".to_string(),
        request_id: request_id.clone(),
        method: method.to_string(),
        path: bridge_path.clone(),
        query,
        headers: extract_forwardable_headers(&headers),
        body,
    };

    let message = match serde_json::to_string(&proxy_req) {
        Ok(m) => m,
        Err(e) => {
            log_access(
                &method_str,
                &bridge_path,
                &client.device_id,
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

    // 通过 WebSocket 发送到 Mac Bridge，获取 oneshot receiver
    let rx = {
        let mut reg = state.registry.lock().await;
        match reg.send_request(&client.sid, request_id.clone(), message) {
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
                log_access(
                    &method_str,
                    &bridge_path,
                    &client.device_id,
                    status.as_u16(),
                    Some(error_kind),
                    None,
                    start,
                );
                return (status, format!(r#"{{"error":"{e}"}}"#)).into_response();
            }
        }
    };

    // 等待 Bridge 响应（超时 30 秒）
    let upstream_start = Instant::now();
    match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
        Ok(Ok(Ok(proxy_resp))) => {
            let upstream_ms = upstream_start.elapsed().as_millis() as u64;
            let status = proxy_resp.status;
            let error_kind = if status >= 500 {
                Some("upstream_5xx")
            } else {
                None
            };
            log_access(
                &method_str,
                &bridge_path,
                &client.device_id,
                status,
                error_kind,
                Some(upstream_ms),
                start,
            );

            let mut builder = Response::builder().status(proxy_resp.status);
            for (k, v) in &proxy_resp.headers {
                builder = builder.header(k, v);
            }
            builder
                .body(Body::from(proxy_resp.body))
                .unwrap()
                .into_response()
        }
        Ok(Ok(Err(e))) => {
            log_access(
                &method_str,
                &bridge_path,
                &client.device_id,
                502,
                Some("upstream_error"),
                Some(upstream_start.elapsed().as_millis() as u64),
                start,
            );
            (StatusCode::BAD_GATEWAY, format!(r#"{{"error":"{e}"}}"#)).into_response()
        }
        Ok(Err(_)) => {
            log_access(
                &method_str,
                &bridge_path,
                &client.device_id,
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
                &request_id,
                "mac_timeout",
            );
            log_access(
                &method_str,
                &bridge_path,
                &client.device_id,
                504,
                Some("upstream_timeout"),
                Some(upstream_start.elapsed().as_millis() as u64),
                start,
            );
            (StatusCode::GATEWAY_TIMEOUT, r#"{"error":"mac_timeout"}"#).into_response()
        }
    }
}

/// 结构化访问日志。
pub(crate) fn log_access(
    method: &str,
    path: &str,
    device_id: &str,
    status: u16,
    error_kind: Option<&str>,
    upstream_ms: Option<u64>,
    start: Instant,
) {
    let total_ms = start.elapsed().as_millis() as u64;
    let error = error_kind.unwrap_or("-");
    let upstream = upstream_ms
        .map(|m| m.to_string())
        .unwrap_or_else(|| "-".to_string());
    eprintln!(
        "relay-access: method={method} path={path} device={device_id} status={status} error={error} upstream_ms={upstream} total_ms={total_ms}"
    );
}

/// 提取需要转发到 Bridge 的请求头。
fn extract_forwardable_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for name in ["authorization", "content-type", "x-device-id"] {
        if let Some(val) = headers.get(name) {
            if let Ok(s) = val.to_str() {
                out.insert(name.to_string(), s.to_string());
            }
        }
    }
    out
}
