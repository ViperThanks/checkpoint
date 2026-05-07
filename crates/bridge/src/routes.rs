//! HTTP 路由和处理器 — event 查询、会话浏览、模式切换、决策、反馈、活动聚合。
//!
//! 架构角色：所有 REST API 端点的实现。每个 handler 遵循统一模式：
//! 认证检查 → 打开 DB → 查询 → JSON 响应。
//!
//! 模块划分：
//! - 基础端点：health、beat、index、mode
//! - 事件端点：events 列表/详情/决策/反馈/pending
//! - 会话端点：overview、conversations、events、activity、messages
//! - 设备端点：devices 列表/标签更新
//! - Relay 端点：status、pairing
//! - Learn 端点：suggestions、rules
//!
//! 分页参数统一管理：DEFAULT_PAGE_SIZE / MAX_PAGE_SIZE。

use checkpoint_core::audit::{
    AuditStore, ConversationInfo, ConversationRow, DecisionRow, FeedbackRow,
};
use checkpoint_core::config::Config;
use checkpoint_core::provider_registry::ProviderRegistry;
use checkpoint_core::rule::Mode;
use checkpoint_core::title_import;
use checkpoint_core::transcript;
use checkpoint_core::transcript_sync;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::context::AppContext;
use crate::sse::{self, SharedBroadcaster};
use crate::ui;
use base64::Engine;

/// 登录速率限制：连续失败 N 次后锁定 LOGIN_LOCKOUT_SECS 秒。
const MAX_LOGIN_FAILURES: u32 = 5;
const LOGIN_LOCKOUT_SECS: u64 = 60;

/// 全局登录限流状态。
static LOGIN_GUARD: std::sync::Mutex<LoginGuard> = std::sync::Mutex::new(LoginGuard {
    fail_count: 0,
    locked_until: 0,
});

struct LoginGuard {
    fail_count: u32,
    locked_until: u64,
}

impl LoginGuard {
    /// 检查是否被锁定。返回 true 表示允许尝试登录。
    fn try_acquire(&mut self) -> bool {
        let now = chrono::Utc::now().timestamp() as u64;
        if self.locked_until > 0 && now < self.locked_until {
            return false;
        }
        if self.locked_until > 0 && now >= self.locked_until {
            self.fail_count = 0;
            self.locked_until = 0;
        }
        true
    }

    fn record_success(&mut self) {
        self.fail_count = 0;
        self.locked_until = 0;
    }

    fn record_failure(&mut self) {
        self.fail_count += 1;
        if self.fail_count >= MAX_LOGIN_FAILURES {
            self.locked_until = chrono::Utc::now().timestamp() as u64 + LOGIN_LOCKOUT_SECS;
            eprintln!(
                "agent-aspect-bridge: login locked for {LOGIN_LOCKOUT_SECS}s after {MAX_LOGIN_FAILURES} failures"
            );
        }
    }
}

// 分页默认值 — 所有列表端点共享。pub 可见性供 jobs.rs 引用。
pub const DEFAULT_PAGE_SIZE: usize = 20;
pub const MAX_PAGE_SIZE: usize = 100;
const DEFAULT_ACTIVITY_PAGE_SIZE: usize = 50;
const MAX_ACTIVITY_PAGE_SIZE: usize = 200;
const DEFAULT_MESSAGE_PAGE_SIZE: usize = 100;
const MAX_MESSAGE_PAGE_SIZE: usize = 500;
const DEFAULT_PENDING_LIMIT: usize = 20;

/// compact 聚合查询拉取大量数据后在内存中分组，不受分页限制。
const COMPACT_QUERY_LIMIT: usize = 5000;

/// 时间窗口聚合的粒度：同一 tool+action 在 30s 内的事件合并为一组。
const AGGREGATION_WINDOW_SECS: i64 = 30;

/// POST body 最大字节数（10 MiB）。超出拒绝，防止内存耗尽。
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;

/// 读取并解析请求体为 JSON，失败时返回 400 错误响应。
/// 所有 POST handler 共用，消除重复的 body-reading 样板代码。
/// 超过 MAX_BODY_BYTES 时返回 413。
pub fn read_json_body(
    request: &mut tiny_http::Request,
) -> Result<serde_json::Value, tiny_http::ResponseBox> {
    let body = read_body(request)?;
    match serde_json::from_str(&body) {
        Ok(v) => Ok(v),
        Err(e) => Err(json_response(
            400,
            &serde_json::json!({"error": format!("parse json: {e}")}),
        )),
    }
}

/// 读取请求体（带大小限制），超过 MAX_BODY_BYTES 时返回 413。
pub fn read_body(
    request: &mut tiny_http::Request,
) -> Result<String, tiny_http::ResponseBox> {
    let mut buf = Vec::with_capacity(4096);
    let reader = request.as_reader();
    let mut tmp = [0u8; 8192];
    loop {
        match reader.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > MAX_BODY_BYTES {
                    return Err(json_response(
                        413,
                        &serde_json::json!({"error": "body too large"}),
                    ));
                }
            }
            Err(e) => {
                return Err(json_response(
                    400,
                    &serde_json::json!({"error": format!("read body: {e}")}),
                ));
            }
        }
    }
    String::from_utf8(buf).map_err(|e| {
        json_response(400, &serde_json::json!({"error": format!("invalid utf8: {e}")}))
    })
}

/// 构造 JSON 响应。所有 handler 的统一出口。
pub fn json_response(status: i32, body: &serde_json::Value) -> tiny_http::ResponseBox {
    let data = std::io::Cursor::new(body.to_string().into_bytes());
    let len = data.get_ref().len();
    tiny_http::Response::new(
        status.into(),
        vec![tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap()],
        Box::new(data) as Box<dyn Read + Send>,
        Some(len),
        None,
    )
}

/// 从配置文件读取当前 mode（Guard/Allow/Learn），默认 Guard。
pub fn read_mode() -> Mode {
    let config_path = Config::config_path();
    if config_path.exists() {
        match Config::load(&config_path) {
            Ok(c) => return c.mode,
            Err(e) => eprintln!("agent-aspect-bridge: config load error: {e}"),
        }
    }
    Mode::Guard
}

/// 将 mode 写入配置文件。读取 -> 修改 -> 保存，避免丢失其他配置。
pub fn write_mode(mode: Mode) -> Result<(), String> {
    let config_path = Config::config_path();
    let mut cfg = if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
    } else {
        Config::default_config()
    };
    cfg.mode = mode;
    cfg.save(&config_path)
        .map_err(|e| format!("save config: {e}"))
}

/// GET /health 处理器。返回服务存活状态，用于健康检查。
pub fn handle_health() -> tiny_http::ResponseBox {
    json_response(200, &serde_json::json!({"status": "ok"}))
}

/// POST /login 处理器 — 用户名 + 密码登录，成功返回 Bearer token。
///
/// 认证失败统一返回 401，不泄露用户是否存在。
/// 成功时更新 last_login_at 并返回当前 bridge token。
pub fn handle_post_login(
    ctx: &AppContext,
    request: &mut tiny_http::Request,
    bridge_token: &str,
) -> tiny_http::ResponseBox {
    // 登录速率限制检查
    {
        let mut guard = LOGIN_GUARD.lock().unwrap();
        if !guard.try_acquire() {
            return json_response(429, &serde_json::json!({"error": "too many login attempts, try again later"}));
        }
    }

    let parsed = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let username = match parsed.get("username").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            LOGIN_GUARD.lock().unwrap().record_failure();
            return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
        }
    };
    let password = match parsed.get("password").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            LOGIN_GUARD.lock().unwrap().record_failure();
            return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
        }
    };

    let store = ctx.store.lock().unwrap();

    let user = match store.get_user_by_username(username) {
        Ok(Some(u)) => u,
        Ok(None) => {
            LOGIN_GUARD.lock().unwrap().record_failure();
            return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
        }
        Err(e) => {
            eprintln!("agent-aspect-bridge: login query failed: {e}");
            return json_response(500, &serde_json::json!({"error": "internal error"}));
        }
    };

    if user.disabled_at.is_some() {
        LOGIN_GUARD.lock().unwrap().record_failure();
        return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
    }

    if !checkpoint_core::password::verify_password(
        password,
        &user.password_hash,
        &user.password_salt,
    ) {
        LOGIN_GUARD.lock().unwrap().record_failure();
        return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
    }

    LOGIN_GUARD.lock().unwrap().record_success();

    let now = chrono::Utc::now().to_rfc3339();
    let _ = store.update_last_login(&user.id, &now);

    json_response(200, &serde_json::json!({"token": bridge_token}))
}

/// POST /password/change — 修改当前用户密码。
///
/// 需要 Bearer token 认证 + loopback（由 main.rs 路由层保障）。
/// 成功后前端应清除 session token，要求重新登录。
pub fn handle_post_password_change(
    ctx: &AppContext,
    request: &mut tiny_http::Request,
) -> tiny_http::ResponseBox {
    let parsed = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let old_password = match parsed.get("old_password").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
        }
    };
    let new_password = match parsed.get("new_password").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
        }
    };

    if new_password.len() < 12 {
        return json_response(400, &serde_json::json!({"error": "密码至少 12 个字符"}));
    }

    let store = ctx.store.lock().unwrap();

    let user = match store.get_user_by_username("admin") {
        Ok(Some(u)) => u,
        Ok(None) => {
            eprintln!("agent-aspect-bridge: password change: admin user not found");
            return json_response(500, &serde_json::json!({"error": "internal error"}));
        }
        Err(e) => {
            eprintln!("agent-aspect-bridge: password change query failed: {e}");
            return json_response(500, &serde_json::json!({"error": "internal error"}));
        }
    };

    if !checkpoint_core::password::verify_password(
        old_password,
        &user.password_hash,
        &user.password_salt,
    ) {
        return json_response(401, &serde_json::json!({"error": "用户名或密码错误"}));
    }

    let (hash, salt) = match checkpoint_core::password::hash_password(new_password) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("agent-aspect-bridge: password change hash failed: {e}");
            return json_response(500, &serde_json::json!({"error": "internal error"}));
        }
    };

    // 两阶段更新：先写文件，后改 DB，失败回滚
    let password_path = checkpoint_core::paths::bridge_password_path();
    let old_file = std::fs::read_to_string(&password_path).ok();
    if let Err(e) =
        checkpoint_core::user_password::overwrite_password_file(&password_path, new_password)
    {
        eprintln!("agent-aspect-bridge: password change write file failed: {e}");
        return json_response(500, &serde_json::json!({"error": "internal error"}));
    }

    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = store.update_user_password(&user.id, &hash, &salt, &now) {
        eprintln!("agent-aspect-bridge: password change DB update failed: {e}");
        // 回滚文件
        if let Some(old) = old_file {
            let _ = checkpoint_core::user_password::overwrite_password_file(&password_path, &old);
        } else {
            let _ = std::fs::remove_file(&password_path);
        }
        return json_response(500, &serde_json::json!({"error": "internal error"}));
    }

    json_response(200, &serde_json::json!({"ok": true}))
}

/// 延迟探测端点 — 返回 bridge 端的收发时间戳，客户端计算 RTT。
pub fn handle_beat() -> tiny_http::ResponseBox {
    let received = chrono::Utc::now().timestamp_millis();
    let sent = chrono::Utc::now().timestamp_millis();
    json_response(
        200,
        &serde_json::json!({
            "status": "ok",
            "bridge_received_at_ms": received,
            "bridge_sent_at_ms": sent,
        }),
    )
}

/// 返回嵌入式 Dashboard HTML 页面。no-cache 确保始终返回最新版本。
pub fn handle_index() -> tiny_http::ResponseBox {
    let data = std::io::Cursor::new(ui::INDEX_HTML.as_bytes().to_vec());
    let len = data.get_ref().len();
    tiny_http::Response::new(
        200.into(),
        vec![
            tiny_http::Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            tiny_http::Header::from_bytes("Cache-Control", "no-cache").unwrap(),
        ],
        Box::new(data) as Box<dyn Read + Send>,
        Some(len),
        None,
    )
}

/// GET /mode 处理器。返回当前运行模式。
pub fn handle_get_mode() -> tiny_http::ResponseBox {
    let mode = read_mode();
    json_response(200, &serde_json::json!({"mode": mode.as_str()}))
}

/// POST /mode 处理器 — 切换 mode 并广播 SSE 事件通知所有客户端。
pub fn handle_post_mode(
    request: &mut tiny_http::Request,
    broadcaster: &SharedBroadcaster,
) -> tiny_http::ResponseBox {
    let parsed = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let mode_str = match parsed.get("mode").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return json_response(400, &serde_json::json!({"error": "missing 'mode' field"})),
    };

    let mode = match Mode::from_str(mode_str) {
        Ok(m) => m,
        Err(e) => {
            return json_response(
                400,
                &serde_json::json!({"error": format!("invalid mode: {e}")}),
            );
        }
    };

    if let Err(e) = write_mode(mode) {
        return json_response(500, &serde_json::json!({"error": e}));
    }

    // 广播 mode 变更事件到所有 SSE 客户端
    broadcaster.lock().unwrap().broadcast(sse::SseEvent {
        event_type: "mode".to_string(),
        data: mode.as_str().to_string(),
    });

    invalidate_overview_cache();

    json_response(200, &serde_json::json!({"mode": mode.as_str()}))
}

/// 从 URL query string 中提取指定参数值。
pub fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let query = url.split('?').nth(1)?;
    query
        .split('&')
        .filter_map(|pair| {
            let mut kv = pair.splitn(2, '=');
            match (kv.next(), kv.next()) {
                (Some(k), Some(v)) if k == key => Some(v),
                _ => None,
            }
        })
        .next()
}

/// 从请求头中提取指定 header 的值。
fn header_value<'a>(request: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

/// 提取设备标识：(device_id, user_agent, remote_addr)。
/// 优先使用 X-Device-Id 头，否则根据 IP + UA 哈希生成 fallback ID。
fn request_device(request: &tiny_http::Request) -> (String, Option<String>, Option<String>) {
    let user_agent = header_value(request, "User-Agent").map(|s| s.to_string());
    let remote_addr = request.remote_addr().map(|a| a.to_string());
    let remote_identity = request
        .remote_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    if let Some(id) = header_value(request, "X-Device-Id").map(str::trim) {
        if !id.is_empty() && id.len() <= 128 {
            return (id.to_string(), user_agent, remote_addr);
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(remote_identity);
    hasher.update(b"\0");
    hasher.update(user_agent.as_deref().unwrap_or("unknown"));
    let digest = hasher.finalize();
    (format!("fallback:{:x}", digest), user_agent, remote_addr)
}

/// 注册/更新设备信息并返回 device_id。POST handler 在处理业务逻辑前调用。
fn touch_device(ctx: &AppContext, request: &tiny_http::Request) -> String {
    let (device_id, user_agent, remote_addr) = request_device(request);
    let timestamp = chrono::Utc::now().to_rfc3339();
    let store = ctx.store.lock().unwrap();
    if let Err(e) = store.register_device(
        &device_id,
        user_agent.as_deref(),
        remote_addr.as_deref(),
        &timestamp,
    ) {
        eprintln!("agent-aspect-bridge: register device failed: {e}");
    }
    device_id
}

/// 判断某个 (action, tool_name) 是否适合聚合。
/// ask/deny 不聚合（用户需要逐个审批），只读工具类操作可以聚合。
fn can_aggregate(action: &str, tool_name: &str) -> bool {
    if action == "ask" || action == "deny" {
        return false;
    }
    matches!(
        tool_name,
        "Read" | "ReadFile" | "Glob" | "Grep" | "LS" | "List" | "TaskUpdate" | "Bash"
    )
}

/// 将 DecisionRow 序列化为 JSON，附带可选的 Feedback。
fn decision_to_json(d: &DecisionRow, fb: Option<&FeedbackRow>) -> serde_json::Value {
    decision_to_json_with_conv(d, fb, None)
}

/// 将 DecisionRow 序列化为 JSON，附带 Feedback 和会话信息。
fn decision_to_json_with_conv(
    d: &DecisionRow,
    fb: Option<&FeedbackRow>,
    conv: Option<&ConversationInfo>,
) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "event_id": d.event_id,
        "action": d.action,
        "rule_id": d.rule_id,
        "note": d.note,
        "timestamp": d.timestamp,
        "tool_name": d.tool_name,
        "file_path": d.file_path,
        "agent": d.agent,
        "phase": d.phase,
        "raw_payload": d.raw_payload,
        "device_id": d.device_id,
        "device_label": d.device_label,
        "feedback": fb.map(|f| serde_json::json!({
            "verdict": f.verdict,
            "note": f.note,
        })),
    });
    if let Some(ci) = conv {
        obj.as_object_mut().unwrap().insert(
            "conversation".into(),
            serde_json::json!({
                "conversation_id": ci.conversation_id,
                "conversation_db_id": ci.conversation_db_id,
                "title": ci.title,
                "title_source": ci.title_source,
                "project_path": ci.project_path,
            }),
        );
    }
    obj
}

/// 紧凑格式序列化 DecisionRow，添加 is_group=false 标记（前端区分单条与聚合组）。
fn decision_to_json_compact(d: &DecisionRow, fb: Option<&FeedbackRow>) -> serde_json::Value {
    serde_json::json!({
        "is_group": false,
        "event_id": d.event_id,
        "action": d.action,
        "rule_id": d.rule_id,
        "note": d.note,
        "timestamp": d.timestamp,
        "tool_name": d.tool_name,
        "file_path": d.file_path,
        "agent": d.agent,
        "phase": d.phase,
        "raw_payload": d.raw_payload,
        "device_id": d.device_id,
        "device_label": d.device_label,
        "feedback": fb.map(|f| serde_json::json!({
            "verdict": f.verdict,
            "note": f.note,
        })),
    })
}

/// 聚合组：同一 (agent, tool, action, rule) 在时间窗口内的多个事件合并为一组。
struct CompactGroup {
    key: String,
    count: usize,
    first_timestamp: String,
    last_timestamp: String,
    agent: String,
    tool_name: String,
    action: String,
    rule_id: Option<String>,
    sample_file_path: Option<String>,
    event_ids: Vec<String>,
}

impl CompactGroup {
    fn to_json(&self) -> serde_json::Value {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        (
            &self.agent,
            &self.tool_name,
            &self.action,
            &self.rule_id,
            &self.first_timestamp,
        )
            .hash(&mut hasher);
        let group_id = format!("{:x}", hasher.finish());

        serde_json::json!({
            "is_group": true,
            "group_id": group_id,
            "count": self.count,
            "first_timestamp": self.first_timestamp,
            "last_timestamp": self.last_timestamp,
            "agent": self.agent,
            "tool_name": self.tool_name,
            "action": self.action,
            "rule_id": self.rule_id,
            "sample_file_path": self.sample_file_path,
            "event_ids": self.event_ids,
        })
    }
}

/// 将 decision 列表按 (agent, tool, action, rule) 和时间窗口聚合。
/// 可聚合的事件在 30s 窗口内合并为 CompactGroup，其余保持单条。
fn aggregate_decisions(
    decisions: Vec<DecisionRow>,
    feedback: &HashMap<String, FeedbackRow>,
) -> Vec<serde_json::Value> {
    let mut result: Vec<serde_json::Value> = Vec::new();
    let mut current: Option<CompactGroup> = None;

    for d in decisions {
        let key = format!(
            "{}|{}|{}|{}",
            d.agent,
            d.tool_name,
            d.action,
            d.rule_id.as_deref().unwrap_or("")
        );

        if let Some(ref mut cg) = current {
            if can_aggregate(&d.action, &d.tool_name) && cg.key == key {
                if let (Ok(curr_dt), Ok(last_dt)) = (
                    chrono::DateTime::parse_from_rfc3339(&d.timestamp),
                    chrono::DateTime::parse_from_rfc3339(&cg.last_timestamp),
                ) {
                    let curr_utc = curr_dt.with_timezone(&chrono::Utc);
                    let last_utc = last_dt.with_timezone(&chrono::Utc);
                    let diff = (last_utc - curr_utc).num_seconds().abs();
                    if diff <= AGGREGATION_WINDOW_SECS {
                        cg.count += 1;
                        cg.last_timestamp = d.timestamp.clone();
                        cg.event_ids.push(d.event_id.clone());
                        if cg.sample_file_path.is_none() {
                            cg.sample_file_path = d.file_path.clone();
                        }
                        continue;
                    }
                }
            }
            result.push(cg.to_json());
            current = None;
        }

        if can_aggregate(&d.action, &d.tool_name) {
            current = Some(CompactGroup {
                key,
                count: 1,
                first_timestamp: d.timestamp.clone(),
                last_timestamp: d.timestamp.clone(),
                agent: d.agent,
                tool_name: d.tool_name,
                action: d.action,
                rule_id: d.rule_id,
                sample_file_path: d.file_path,
                event_ids: vec![d.event_id],
            });
        } else {
            let fb = feedback.get(&d.event_id);
            result.push(decision_to_json_compact(&d, fb));
        }
    }

    if let Some(cg) = current {
        result.push(cg.to_json());
    }

    result
}

/// compact 视图：拉取 COMPACT_QUERY_LIMIT 条 decision，内存聚合后分页。
/// 前端用 group=compact 参数触发。
fn handle_get_events_compact(
    store: &AuditStore,
    limit: usize,
    offset: usize,
    action: Option<&str>,
    tool: Option<&str>,
    since: Option<&str>,
    agent: Option<&str>,
    verdict: Option<&str>,
) -> tiny_http::ResponseBox {
    let latest_only = action == Some("ask");
    let decisions = match store.query_decisions(
        COMPACT_QUERY_LIMIT,
        0,
        action,
        tool,
        since,
        agent,
        verdict,
        latest_only,
    ) {
        Ok(d) => d,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query events: {e}")}),
            );
        }
    };

    let event_ids: Vec<String> = decisions.iter().map(|d| d.event_id.clone()).collect();
    let feedback = store.feedback_for_events(&event_ids).unwrap_or_default();

    let groups = aggregate_decisions(decisions, &feedback);
    let total = groups.len();
    let events: Vec<serde_json::Value> = groups.into_iter().skip(offset).take(limit).collect();

    json_response(
        200,
        &serde_json::json!({
            "events": events,
            "total": total,
            "limit": limit,
            "offset": offset,
            "grouped": true,
        }),
    )
}

/// GET /events 处理器。支持 limit/offset/action/tool/since/agent/verdict/group 参数。
/// group=compact 触发聚合视图，默认返回原始列表。
pub fn handle_get_events(ctx: &AppContext, request: &tiny_http::Request) -> tiny_http::ResponseBox {
    let url = request.url();
    let limit = query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .min(MAX_PAGE_SIZE);
    let offset = query_param(url, "offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let action = query_param(url, "action");
    let tool = query_param(url, "tool");
    let since = query_param(url, "since");
    let agent = query_param(url, "agent");
    let verdict = query_param(url, "verdict");
    let group = query_param(url, "group").unwrap_or("none");

    let store = ctx.store.lock().unwrap();

    if group == "compact" {
        return handle_get_events_compact(
            &store, limit, offset, action, tool, since, agent, verdict,
        );
    }

    let latest_only = action == Some("ask");
    let decisions = match store.query_decisions(
        limit,
        offset,
        action,
        tool,
        since,
        agent,
        verdict,
        latest_only,
    ) {
        Ok(d) => d,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query events: {e}")}),
            );
        }
    };

    let total = store
        .count_decisions_filtered(action, tool, since, agent, verdict, latest_only)
        .unwrap_or(0);

    // 批量拉取当前页所有事件的 feedback 和会话信息，避免 N+1 查询
    let event_ids: Vec<String> = decisions.iter().map(|d| d.event_id.clone()).collect();
    let feedback = store.feedback_for_events(&event_ids).unwrap_or_default();
    let conv_info = store
        .event_conversation_info(&event_ids)
        .unwrap_or_default();

    let events: Vec<serde_json::Value> = decisions
        .into_iter()
        .map(|d| {
            let fb = feedback.get(&d.event_id);
            let ci = conv_info.get(&d.event_id);
            decision_to_json_with_conv(&d, fb, ci)
        })
        .collect();

    let result_count = events.len();
    eprintln!(
        "agent-aspect-bridge: GET /events limit={limit} offset={offset} results={result_count}"
    );

    json_response(
        200,
        &serde_json::json!({
            "events": events,
            "total": total,
            "limit": limit,
            "offset": offset,
        }),
    )
}

/// GET /events/:id 处理器。返回单个事件的详情（含 decision 和 feedback）。
pub fn handle_get_event(ctx: &AppContext, event_id: &str) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();

    match store.get_decision_with_feedback(event_id) {
        Ok(None) => json_response(404, &serde_json::json!({"error": "event not found"})),
        Ok(Some((d, fb))) => json_response(
            200,
            &serde_json::json!({
                "event_id": d.event_id,
                "action": d.action,
                "rule_id": d.rule_id,
                "note": d.note,
                "timestamp": d.timestamp,
                "tool_name": d.tool_name,
                "file_path": d.file_path,
                "agent": d.agent,
                "phase": d.phase,
                "raw_payload": d.raw_payload,
                "device_id": d.device_id,
                "device_label": d.device_label,
                "feedback": fb.map(|f| serde_json::json!({
                    "verdict": f.verdict,
                    "note": f.note,
                })),
            }),
        ),
        Err(e) => json_response(
            500,
            &serde_json::json!({"error": format!("query event: {e}")}),
        ),
    }
}

/// POST /decide 处理器 — 用户对 ask 事件做出 allow/deny 决策。
/// 校验：事件必须存在且最新 decision 为 ask。决策后广播 SSE。
pub fn handle_post_decide(
    ctx: &AppContext,
    request: &mut tiny_http::Request,
    broadcaster: &SharedBroadcaster,
) -> tiny_http::ResponseBox {
    let device_id = touch_device(ctx, request);
    let parsed = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let event_id = match parsed.get("event_id").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return json_response(400, &serde_json::json!({"error": "missing 'event_id'"})),
    };

    let action = match parsed.get("action").and_then(|v| v.as_str()) {
        Some("allow") | Some("deny") => parsed.get("action").unwrap().as_str().unwrap(),
        _ => {
            return json_response(
                400,
                &serde_json::json!({"error": "action must be 'allow' or 'deny'"}),
            );
        }
    };

    let note = parsed
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or("user decision from bridge");

    // DB 操作完成后释放 store lock，再做广播
    {
        let store = ctx.store.lock().unwrap();

        // 校验：事件必须存在且最新 decision 为 ask
        match store.latest_decision_for_event(event_id) {
            Ok(None) => {
                return json_response(404, &serde_json::json!({"error": "event not found"}));
            }
            Ok(Some(d)) if d.action != "ask" => {
                return json_response(
                    409,
                    &serde_json::json!({"error": format!("event latest action is '{}', not 'ask'", d.action)}),
                );
            }
            Err(e) => {
                return json_response(
                    500,
                    &serde_json::json!({"error": format!("lookup event: {e}")}),
                );
            }
            _ => {}
        }

        let timestamp = chrono::Utc::now().to_rfc3339();
        if let Err(e) = store.insert_decision_for_device(
            event_id,
            action,
            Some("[aspect-user-override]"),
            note,
            &timestamp,
            Some(&device_id),
        ) {
            return json_response(
                500,
                &serde_json::json!({"error": format!("insert decision: {e}")}),
            );
        }
    }

    // 广播决策 + 会话更新（单次 lock，store 已释放）
    {
        let mut bc = broadcaster.lock().unwrap();
        bc.broadcast(sse::SseEvent {
            event_type: "decision".to_string(),
            data: event_id.to_string(),
        });
        bc.broadcast(sse::SseEvent {
            event_type: "conversation_update".to_string(),
            data: serde_json::json!({"type": "decision"}).to_string(),
        });
    }

    invalidate_overview_cache();

    json_response(
        200,
        &serde_json::json!({
            "event_id": event_id,
            "action": action,
            "rule_id": "[aspect-user-override]",
            "note": note,
            "device_id": device_id,
        }),
    )
}

/// GET /pending 处理器。返回所有 action=ask 的未决事件（等待用户决策）。
pub fn handle_get_pending(ctx: &AppContext) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();

    let decisions = match store.pending_asks(DEFAULT_PENDING_LIMIT) {
        Ok(d) => d,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query pending: {e}")}),
            );
        }
    };

    let mut config = checkpoint_core::config::Config::load_or_create();
    config.approval_review.sanitize();
    let review_cfg = &config.approval_review;

    let events: Vec<serde_json::Value> = decisions
        .into_iter()
        .map(|d| {
            let review = build_approval_review(&d, review_cfg);
            serde_json::json!({
                "event_id": d.event_id,
                "action": d.action,
                "rule_id": d.rule_id,
                "note": d.note,
                "timestamp": d.timestamp,
                "tool_name": d.tool_name,
                "file_path": d.file_path,
                "device_id": d.device_id,
                "device_label": d.device_label,
                "review": review,
            })
        })
        .collect();

    let count = events.len();
    json_response(200, &serde_json::json!({"events": events, "count": count}))
}

/// 根据 ApprovalReviewConfig 为每条 pending decision 生成 review payload。
/// 前端只负责渲染，不重复拼业务语义。
fn build_approval_review(
    d: &checkpoint_core::audit::DecisionRow,
    cfg: &checkpoint_core::config::ApprovalReviewConfig,
) -> serde_json::Value {
    let agent = agent_display_label(&d.agent);

    let command = if cfg.show_command {
        extract_review_command(&d.tool_name, d.raw_payload.as_deref())
    } else {
        None
    };

    let payload_preview = if cfg.show_payload_preview {
        truncate_review_payload(d.raw_payload.as_deref(), cfg.payload_preview_chars)
    } else {
        None
    };

    // 构建 chips
    let mut chips: Vec<String> = Vec::new();
    if cfg.show_agent {
        chips.push(agent.clone());
    }
    chips.push(d.tool_name.clone());
    if cfg.show_rule {
        if let Some(ref rid) = d.rule_id {
            chips.push(rid.clone());
        }
    }

    let summary = format!("{} 需要审批", d.tool_name);
    let risk_reason = d
        .note
        .split_once(": ")
        .map(|(_, rest)| rest.to_string())
        .unwrap_or_else(|| d.note.clone());

    serde_json::json!({
        "view": cfg.default_view,
        "summary": summary,
        "risk_reason": cfg.show_rule.then_some(risk_reason),
        "agent": cfg.show_agent.then_some(agent),
        "tool_name": d.tool_name,
        "file_path": cfg.show_file_path.then_some(d.file_path.clone()),
        "command": command,
        "device_label": cfg.show_device.then_some(d.device_label.clone()),
        "payload_preview": payload_preview,
        "chips": chips,
    })
}

/// 从 raw_payload 提取 command 字段（Bash/shell 等命令行工具）。
/// 同时读取顶层 `command` 和嵌套 `tool_input.command`，兼容 hook payload 与 normalize 后的格式。
fn extract_review_command(tool_name: &str, raw_payload: Option<&str>) -> Option<String> {
    let raw = raw_payload?;
    let val: serde_json::Value = serde_json::from_str(raw).ok()?;
    match tool_name {
        "Bash" | "shell" | "Shell" | "exec_command" | "run_shell_command" => val
            .get("command")
            .or_else(|| val.get("tool_input").and_then(|ti| ti.get("command")))
            .and_then(|v| v.as_str())
            .map(String::from),
        _ => None,
    }
}

/// 截断 raw_payload 用于审批预览。
fn truncate_review_payload(raw_payload: Option<&str>, max_chars: usize) -> Option<String> {
    let raw = raw_payload?;
    if raw.len() <= max_chars {
        Some(raw.to_string())
    } else {
        // 按 char boundary 截断
        let end = raw
            .char_indices()
            .take_while(|(i, _)| *i < max_chars)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max_chars.min(raw.len()));
        Some(format!("{}...", &raw[..end]))
    }
}

/// 返回 agent 字段的可读展示名。
fn agent_display_label(agent: &str) -> String {
    match agent {
        "claude_code" => "Claude Code".to_string(),
        "codex_cli" => "Codex CLI".to_string(),
        "kimi_code" => "Kimi Code".to_string(),
        "gemini_cli" => "Gemini CLI".to_string(),
        _ => agent.to_string(),
    }
}

/// POST /events/:id/feedback 处理器 — 对已完成事件提交反馈（useful/noisy/wrong/unsure）。
pub fn handle_post_feedback(
    ctx: &AppContext,
    event_id: &str,
    request: &mut tiny_http::Request,
) -> tiny_http::ResponseBox {
    let device_id = touch_device(ctx, request);
    let parsed = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let verdict = match parsed.get("verdict").and_then(|v| v.as_str()) {
        Some(v @ "useful") | Some(v @ "noisy") | Some(v @ "wrong") | Some(v @ "unsure") => v,
        _ => {
            return json_response(
                400,
                &serde_json::json!({"error": "verdict must be 'useful', 'noisy', 'wrong', or 'unsure'"}),
            );
        }
    };

    let note = parsed.get("note").and_then(|v| v.as_str()).unwrap_or("");

    let store = ctx.store.lock().unwrap();

    if !store.event_exists(event_id).unwrap_or(false) {
        return json_response(404, &serde_json::json!({"error": "event not found"}));
    }

    let timestamp = chrono::Utc::now().to_rfc3339();
    if let Err(e) = store.insert_feedback(event_id, verdict, note, &timestamp) {
        return json_response(
            500,
            &serde_json::json!({"error": format!("insert feedback: {e}")}),
        );
    }

    json_response(
        200,
        &serde_json::json!({
            "event_id": event_id,
            "verdict": verdict,
            "note": note,
            "device_id": device_id,
        }),
    )
}

/// GET /devices 处理器。列出所有已注册的设备。
pub fn handle_get_devices(
    ctx: &AppContext,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    touch_device(ctx, request);
    let store = ctx.store.lock().unwrap();
    match store.list_devices() {
        Ok(devices) => {
            let items: Vec<serde_json::Value> = devices
                .into_iter()
                .map(|d| {
                    serde_json::json!({
                        "device_id": d.device_id,
                        "label": d.label,
                        "user_agent": d.user_agent,
                        "remote_addr": d.remote_addr,
                        "first_seen": d.first_seen,
                        "last_seen": d.last_seen,
                    })
                })
                .collect();
            json_response(200, &serde_json::json!({"devices": items}))
        }
        Err(e) => json_response(
            500,
            &serde_json::json!({"error": format!("query devices: {e}")}),
        ),
    }
}

/// PUT /devices/:id 处理器。更新设备的显示标签（最多 80 字符）。
pub fn handle_put_device_label(
    ctx: &AppContext,
    device_id: &str,
    request: &mut tiny_http::Request,
) -> tiny_http::ResponseBox {
    let parsed = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let label = parsed
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if label.len() > 80 {
        return json_response(400, &serde_json::json!({"error": "label too long"}));
    }

    let store = ctx.store.lock().unwrap();
    match store.update_device_label(device_id, label) {
        Ok(true) => json_response(
            200,
            &serde_json::json!({"device_id": device_id, "label": label}),
        ),
        Ok(false) => json_response(404, &serde_json::json!({"error": "device not found"})),
        Err(e) => json_response(
            500,
            &serde_json::json!({"error": format!("update device: {e}")}),
        ),
    }
}

// ---- 会话端点 ----

/// 后台线程调用的 auto_import 入口。
/// 由 main.rs 中的后台线程每 5 分钟调用一次。
pub fn auto_import_titles_bg(store: &AuditStore, limit: usize) {
    auto_import_titles(store, limit);
}

/// 后台 stats warming：为未缓存 token_count/file_size 的会话计算统计。
/// 遍历 transcript 文件不在请求热路径上执行，不阻塞 store 锁。
pub fn warm_uncached_stats_bg(store: &AuditStore, limit: usize) {
    let convs = match store.list_conversations_for_stats_warming(limit) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("agent-aspect-bridge: warm stats query failed: {e}");
            return;
        }
    };
    for conv in &convs {
        let stats = match checkpoint_core::transcript::compute_stats(
            &conv.agent,
            &conv.conversation_id,
            conv.project_path.as_deref(),
            conv.transcript_path.as_deref(),
        ) {
            Some(s) => s,
            None => continue,
        };
        if let Err(e) =
            store.update_cached_stats(&conv.id, stats.token_count, stats.file_size_bytes)
        {
            eprintln!("agent-aspect-bridge: warm stats update failed: {e}");
        }
    }
    if !convs.is_empty() {
        eprintln!(
            "agent-aspect-bridge: warmed stats for {} conversation(s)",
            convs.len()
        );
    }
}

// ---- Overview 缓存 ----

/// Overview 响应缓存：10 秒 TTL，按 agent + limit 独立缓存。
/// SSE 事件（mode/decision/conversation_update/job_update）触发 dirty 标记，
/// 下次请求时重新计算。
struct OverviewCacheEntry {
    cache_key: String, // "agent:limit" 格式
    data: serde_json::Value,
    expires_at: std::time::Instant,
}

static OVERVIEW_CACHE: std::sync::Mutex<Option<OverviewCacheEntry>> = std::sync::Mutex::new(None);
static OVERVIEW_CACHE_DIRTY: AtomicBool = AtomicBool::new(true);

const OVERVIEW_CACHE_TTL_SECS: u64 = 10;

/// 检查 overview 缓存，命中且未过期且 key 匹配则返回。
fn overview_cache_get(cache_key: &str) -> Option<serde_json::Value> {
    if OVERVIEW_CACHE_DIRTY.load(Ordering::Relaxed) {
        return None;
    }
    let guard = OVERVIEW_CACHE.lock().ok()?;
    let entry = guard.as_ref()?;
    if entry.cache_key == cache_key && entry.expires_at > std::time::Instant::now() {
        Some(entry.data.clone())
    } else {
        None
    }
}

/// 写入 overview 缓存。
fn overview_cache_put(cache_key: &str, data: serde_json::Value) {
    if let Ok(mut guard) = OVERVIEW_CACHE.lock() {
        *guard = Some(OverviewCacheEntry {
            cache_key: cache_key.to_string(),
            data,
            expires_at: std::time::Instant::now()
                + std::time::Duration::from_secs(OVERVIEW_CACHE_TTL_SECS),
        });
        OVERVIEW_CACHE_DIRTY.store(false, Ordering::Relaxed);
    }
}

/// 标记 overview 缓存为脏，下次请求时重新计算。
/// 在 SSE 广播事件的 handler 中调用（mode/decision/conversation_update/job_update）。
pub fn invalidate_overview_cache() {
    OVERVIEW_CACHE_DIRTY.store(true, Ordering::Relaxed);
}

/// 请求阶段耗时日志。每个请求线程独立实例。
pub struct RequestTiming {
    pub queue_start: std::time::Instant,
    route_start: std::time::Instant,
}

impl RequestTiming {
    pub fn new(queue_start: std::time::Instant) -> Self {
        Self {
            queue_start,
            route_start: std::time::Instant::now(),
        }
    }

    /// 输出请求耗时日志。
    pub fn log(&self, method: &tiny_http::Method, path: &str) {
        let total_ms = self.queue_start.elapsed().as_millis();
        let queue_ms = self
            .route_start
            .duration_since(self.queue_start)
            .as_millis();
        let route_ms = total_ms - queue_ms;

        if total_ms > 2000 {
            eprintln!(
                "agent-aspect-bridge: SLOW {method} {path} total_ms={total_ms} queue_ms={queue_ms} route_ms={route_ms}"
            );
        } else if total_ms > 500 {
            eprintln!(
                "agent-aspect-bridge: slow {method} {path} total_ms={total_ms} queue_ms={queue_ms} route_ms={route_ms}"
            );
        }
    }
}

/// 从 provider 的 transcript 文件自动导入标题和最后活跃时间。
fn auto_import_titles(store: &AuditStore, limit: usize) {
    auto_import_claude_sessions(store, limit.max(250));
    auto_import_codex_sessions(store, limit.max(250));
    if let Ok(convs) = store.list_conversations_for_title_import(limit) {
        for conv in &convs {
            if let Some((title, source)) = title_import::import_title_for(
                &conv.agent,
                &conv.conversation_id,
                conv.project_path.as_deref(),
                conv.transcript_path.as_deref(),
            ) {
                let _ = store.update_conversation_title(&conv.id, &title, &source);
            }
            if let Some(messages) = checkpoint_core::transcript::read_transcript(
                &conv.agent,
                &conv.conversation_id,
                conv.project_path.as_deref(),
                conv.transcript_path.as_deref(),
            ) {
                if let Some(latest) = messages.iter().filter_map(|m| m.timestamp.as_deref()).max() {
                    let _ = store.touch_conversation(&conv.id, latest);
                }
            }
        }
    }
}

/// 从 ~/.claude/projects 导入 Claude Code 会话。
///
/// Claude Code 的 hook 事件可能因为权限模式、版本差异或会话启动时机没有进入
/// audit.db。这里用 transcript 文件本身作为兜底索引，确保真实存在的会话能进入
/// conversations 表。subagents 暂不作为独立主会话导入，后续编排层再建模。
fn auto_import_claude_sessions(store: &AuditStore, limit: usize) {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let root = PathBuf::from(&home).join(".claude/projects");
    let mut transcripts = Vec::new();
    collect_claude_transcripts(&root, &mut transcripts);
    transcripts.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
    });
    transcripts.reverse();

    for transcript_path in transcripts.into_iter().take(limit) {
        let Some(session_id) = transcript_path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let last_seen_at = file_modified_rfc3339(&transcript_path)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let project_path = claude_transcript_cwd(&transcript_path);
        let (title, source) = title_import::import_title_for(
            "claude_code",
            session_id,
            project_path.as_deref(),
            Some(transcript_path.to_string_lossy().as_ref()),
        )
        .unwrap_or_else(|| ("Claude Code".to_string(), "fallback".to_string()));
        let db_id = checkpoint_core::conversation::conversation_db_id("claude_code", session_id);
        let _ = store.upsert_conversation_from_metadata(
            &db_id,
            "claude_code",
            session_id,
            "Claude Code",
            project_path.as_deref(),
            &last_seen_at,
            Some(transcript_path.to_string_lossy().as_ref()),
            Some(&title),
            Some(&source),
        );
        if let Some(permission_mode) = claude_transcript_initial_permission_mode(&transcript_path) {
            let _ = store.backfill_runtime_permission_mode(
                &db_id,
                &permission_mode,
                Some("claude_code"),
            );
        }
    }
}

/// 收集 Claude 主会话 JSONL。`subagents/` 内文件属于父会话的执行细节，先跳过。
fn collect_claude_transcripts(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().and_then(|n| n.to_str()) == Some("subagents") {
                continue;
            }
            collect_claude_transcripts(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

/// 从 Claude JSONL 前几行提取 cwd，避免反解 `-Users-...` 目录名的歧义。
fn claude_transcript_cwd(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines().take(16) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if let Some(cwd) = v.get("cwd").and_then(|x| x.as_str()) {
            return Some(cwd.to_string());
        }
        if let Some(cwd) = v
            .get("message")
            .and_then(|m| m.get("cwd"))
            .and_then(|x| x.as_str())
        {
            return Some(cwd.to_string());
        }
    }
    None
}

/// 从 Claude transcript 早期 user 元数据提取初始 permissionMode。
///
/// 这是历史会话兜底：没有 hook 事件落库时，auto-import 仍能恢复 bypass/default。
fn claude_transcript_initial_permission_mode(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines().take(256) {
        if let Some(mode) = checkpoint_core::conversation::extract_permission_mode(line) {
            return Some(mode);
        }
    }
    None
}

/// 从 ~/.codex/session_index.jsonl 导入 Codex CLI 会话。
/// 读取每个 thread 的 rollout 文件路径、标题、cwd 等元数据。
fn auto_import_codex_sessions(store: &AuditStore, limit: usize) {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };
    let index_path = PathBuf::from(&home).join(".codex/session_index.jsonl");
    let content = match fs::read_to_string(&index_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    for line in content.lines().rev().take(limit) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(thread_id) = v.get("id").and_then(|x| x.as_str()) else {
            continue;
        };
        let title = v
            .get("thread_name")
            .and_then(|x| x.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Codex CLI");
        let updated_at = v
            .get("updated_at")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
        let Some(transcript_path) = find_codex_rollout(&home, thread_id) else {
            continue;
        };
        let last_seen_at =
            file_modified_rfc3339(&transcript_path).unwrap_or_else(|| updated_at.clone());
        let project_path = codex_rollout_cwd(&transcript_path);
        let db_id = checkpoint_core::conversation::conversation_db_id("codex_cli", thread_id);
        let _ = store.upsert_conversation_from_metadata(
            &db_id,
            "codex_cli",
            thread_id,
            title,
            project_path.as_deref(),
            &last_seen_at,
            Some(transcript_path.to_string_lossy().as_ref()),
            Some(title),
            Some("provider"),
        );
    }
}

/// 获取文件修改时间，转换为 RFC3339 格式。
fn file_modified_rfc3339(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.to_rfc3339())
}

/// 在 ~/.codex/sessions 下递归查找包含 thread_id 的 .jsonl 文件。
fn find_codex_rollout(home: &str, thread_id: &str) -> Option<PathBuf> {
    let root = PathBuf::from(home).join(".codex/sessions");
    find_file_containing(&root, thread_id)
}

fn find_file_containing(dir: &Path, needle: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file_containing(&path, needle) {
                return Some(found);
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.ends_with(".jsonl") && name.contains(needle))
        {
            return Some(path);
        }
    }
    None
}

fn codex_rollout_cwd(path: &Path) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    for line in content.lines().take(8) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|x| x.as_str()) == Some("session_meta") {
            if let Some(cwd) = v
                .get("payload")
                .and_then(|p| p.get("cwd"))
                .and_then(|x| x.as_str())
            {
                return Some(cwd.to_string());
            }
        }
    }
    None
}

fn format_bytes(b: i64) -> String {
    if b == 0 {
        return "0 B".into();
    }
    if b < 1024 {
        return format!("{b} B");
    }
    if b < 1024 * 1024 {
        return format!("{:.1} KB", b as f64 / 1024.0);
    }
    format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
}

fn format_tokens(t: i64) -> String {
    if t == 0 {
        return "0".into();
    }
    if t < 1000 {
        return t.to_string();
    }
    if t < 1_000_000 {
        return format!("{:.1}K", t as f64 / 1000.0);
    }
    format!("{:.1}M", t as f64 / 1_000_000.0)
}

fn conversation_to_json(c: &ConversationRow, registry: &ProviderRegistry) -> serde_json::Value {
    conversation_to_json_with_counts(c, c.ask_count, c.deny_count, registry)
}

fn conversation_to_json_with_counts(
    c: &ConversationRow,
    ask_count: i64,
    deny_count: i64,
    registry: &ProviderRegistry,
) -> serde_json::Value {
    let token_count = c.cached_token_count.unwrap_or(c.token_count);
    let file_size_bytes = c.cached_file_size_bytes.unwrap_or(c.file_size_bytes);
    let (can_resume, resume_source, resume_id, resume_note) = if registry.can_resume(&c.agent) {
        (true, "cli", Some(c.conversation_id.clone()), None::<String>)
    } else {
        (
            false,
            "unknown",
            None::<String>,
            Some("This provider does not support resume.".to_string()),
        )
    };

    // Runtime health status: 从 resume_cost_mode 或 identity 字段推断
    let runtime_health = if c.identity_version == 0 && c.model_id == "unknown" {
        serde_json::json!({"status": "unknown"})
    } else {
        let status = match c.resume_cost_mode.as_deref() {
            Some("critical") => "critical",
            Some("warning") => "warning",
            Some("ok") => "ok",
            _ => {
                // 未做过 runtime-check 时，从 warning 字段推断
                if c.last_runtime_warning.is_some() {
                    "warning"
                } else {
                    "ok"
                }
            }
        };
        serde_json::json!({
            "status": status,
            "model_id": c.model_id,
            "runtime_profile": c.runtime_profile,
            "permission_mode": c.permission_mode,
            "last_check_at": c.last_runtime_check_at,
            "last_warning": c.last_runtime_warning,
            "identity_version": c.identity_version,
        })
    };

    serde_json::json!({
        "id": c.id,
        "agent": c.agent,
        "conversation_id": c.conversation_id,
        "can_resume": can_resume,
        "resume_source": resume_source,
        "resume_id": resume_id,
        "resume_note": resume_note,
        "title": c.title,
        "title_source": c.title_source,
        "project_path": c.project_path,
        "started_at": c.started_at,
        "last_seen_at": c.last_seen_at,
        "event_count": c.event_count,
        "ask_count": ask_count,
        "pending_ask_count": ask_count,
        "deny_count": deny_count,
        "token_count": token_count,
        "token_count_label": format_tokens(token_count),
        "file_size_bytes": file_size_bytes,
        "file_size_label": format_bytes(file_size_bytes),
        "runtime_health": runtime_health,
    })
}

pub fn handle_get_run_context(ctx: &AppContext) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();

    let context = match store.get_run_context() {
        Ok(c) => c,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query run context: {e}")}),
            );
        }
    };

    let projects: Vec<serde_json::Value> = context
        .projects
        .iter()
        .map(|p| {
            serde_json::json!({
                "path": p.path,
                "agents": p.agents,
                "conversation_count": p.conversation_count,
            })
        })
        .collect();

    // 批量查询 decision counts，消除 N+1
    let recent_ids: Vec<&str> = context
        .recent_conversations
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    let counts_map = store
        .batch_conversation_decision_counts(&recent_ids)
        .unwrap_or_default();

    let recent: Vec<serde_json::Value> = context
        .recent_conversations
        .iter()
        .map(|c| {
            let (ask_count, deny_count) = counts_map
                .get(&c.id)
                .copied()
                .unwrap_or((c.ask_count, c.deny_count));
            conversation_to_json_with_counts(c, ask_count, deny_count, &ctx.registry)
        })
        .collect();

    let provider_availability: Vec<serde_json::Value> = ctx
        .registry
        .enabled_providers()
        .iter()
        .map(|p| {
            let a = ctx.resolver.availability(p);
            serde_json::json!({
                "provider": a.provider,
                "available": a.available,
                "binary_path": a.binary_path,
                "error": a.error,
            })
        })
        .collect();

    let onboarding = context.projects.is_empty();

    json_response(
        200,
        &serde_json::json!({
            "projects": projects,
            "recent_conversations": recent,
            "provider_availability": provider_availability,
            "onboarding": onboarding,
        }),
    )
}

pub fn handle_get_overview(
    ctx: &AppContext,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    let url = request.url();
    let limit = query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .min(MAX_PAGE_SIZE);
    let offset = query_param(url, "offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let agent = query_param(url, "agent");

    let cache_key = format!("{}:{}", agent.unwrap_or(""), limit);

    // 缓存命中时直接返回（仅第一页且无 offset）
    if offset == 0 {
        if let Some(cached) = overview_cache_get(&cache_key) {
            return json_response(200, &cached);
        }
    }

    let store = ctx.store.lock().unwrap();

    let conversations = match store.list_conversations(limit, offset, agent) {
        Ok(c) => c,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query conversations: {e}")}),
            );
        }
    };

    let total = store.count_conversations(agent).unwrap_or(0);

    let active_agents: Vec<String> = {
        let mut agents = std::collections::HashSet::new();
        for c in &conversations {
            agents.insert(c.agent.clone());
        }
        agents.into_iter().collect()
    };

    // 批量查询 decision counts，消除 N+1
    let conv_ids: Vec<&str> = conversations.iter().map(|c| c.id.as_str()).collect();
    let counts_map = store
        .batch_conversation_decision_counts(&conv_ids)
        .unwrap_or_default();

    // 直接使用缓存值，缺失时 fallback 到 conversation_to_json_with_counts 中的默认值。
    // stats warming 已移到后台 auto_import 线程，不在热路径读 transcript。
    let convs_json: Vec<serde_json::Value> = conversations
        .iter()
        .map(|c| {
            let (ask_count, deny_count) = counts_map
                .get(&c.id)
                .copied()
                .unwrap_or((c.ask_count, c.deny_count));
            conversation_to_json_with_counts(c, ask_count, deny_count, &ctx.registry)
        })
        .collect();

    let result_count = convs_json.len();
    eprintln!(
        "agent-aspect-bridge: GET /overview limit={limit} offset={offset} results={result_count}"
    );

    let response_body = serde_json::json!({
        "conversations": convs_json,
        "total": total,
        "active_agents": active_agents,
    });

    // 写入缓存（仅第一页）
    if offset == 0 {
        overview_cache_put(&cache_key, response_body.clone());
    }

    json_response(200, &response_body)
}

pub fn handle_get_conversations(
    ctx: &AppContext,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    // Alias for /overview
    handle_get_overview(ctx, request)
}

pub fn handle_get_conversation(ctx: &AppContext, cid: &str) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();

    match store.get_conversation(cid) {
        Ok(None) => json_response(404, &serde_json::json!({"error": "conversation not found"})),
        Ok(Some(c)) => {
            let (ask_count, deny_count) = store
                .current_conversation_decision_counts(&c.id)
                .unwrap_or((c.ask_count, c.deny_count));
            json_response(
                200,
                &conversation_to_json_with_counts(&c, ask_count, deny_count, &ctx.registry),
            )
        }
        Err(e) => json_response(
            500,
            &serde_json::json!({"error": format!("query conversation: {e}")}),
        ),
    }
}

/// GET /conversations/:id/runtime-check — 强制刷新 runtime identity 并返回 drift 检测结果。
pub fn handle_get_conversation_runtime_check(
    ctx: &AppContext,
    cid: &str,
) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();

    let conv = match store.get_conversation(cid) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return json_response(404, &serde_json::json!({"error": "conversation not found"}));
        }
        Err(e) => return json_response(500, &serde_json::json!({"error": format!("query: {e}")})),
    };

    // 1. 探测当前环境 identity
    let current_identity =
        checkpoint_core::runtime_profile::probe_identity(&conv.agent, conv.project_path.as_deref());

    // 2. 构造存储的 identity
    let stored_identity = checkpoint_core::runtime_profile::RuntimeIdentity {
        model_id: conv.model_id.clone(),
        profile_name: conv.runtime_profile.clone(),
        workspace_path: conv.project_path.clone(),
        config_hash: conv.runtime_profile_hash.clone(),
        permission_mode: conv.permission_mode.clone(),
        entrypoint: conv.entrypoint.clone(),
        toolchain_fingerprint: conv.toolchain_fingerprint.clone(),
    };

    // 3. 比较
    let health = checkpoint_core::runtime_profile::compute_runtime_health(
        &stored_identity,
        &current_identity,
    );

    // 4. 更新 check 时间和 warning
    let warning_text = if health.warnings.is_empty() {
        None
    } else {
        Some(
            health
                .warnings
                .iter()
                .map(|m| format!("{}: {} → {}", m.field, m.recorded, m.current))
                .collect::<Vec<_>>()
                .join("; "),
        )
    };
    let cost_mode = match health.status {
        checkpoint_core::runtime_profile::RuntimeHealthStatus::Critical => Some("critical"),
        checkpoint_core::runtime_profile::RuntimeHealthStatus::Warning => Some("warning"),
        checkpoint_core::runtime_profile::RuntimeHealthStatus::Ok => Some("ok"),
    };
    let _ = store.update_runtime_warning(cid, warning_text.as_deref(), cost_mode);

    // 5. 返回完整结果
    json_response(
        200,
        &serde_json::json!({
            "conversation_id": conv.conversation_id,
            "stored_identity": stored_identity,
            "current_identity": current_identity,
            "health": health,
        }),
    )
}

pub fn handle_get_conversation_events(
    ctx: &AppContext,
    cid: &str,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    let url = request.url();
    let limit = query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .min(MAX_PAGE_SIZE);
    let offset = query_param(url, "offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let view = query_param(url, "view").unwrap_or("raw");

    let store = ctx.store.lock().unwrap();

    let total = match store.count_conversation_events(cid) {
        Ok(n) => n,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("count conversation events: {e}")}),
            );
        }
    };

    let decisions = match store.get_conversation_events(cid, limit, offset) {
        Ok(d) => d,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query conversation events: {e}")}),
            );
        }
    };

    if view == "compact" {
        let event_ids: Vec<String> = decisions.iter().map(|d| d.event_id.clone()).collect();
        let feedback = store.feedback_for_events(&event_ids).unwrap_or_default();
        let groups = aggregate_decisions(decisions, &feedback);
        let events: Vec<serde_json::Value> = groups.into_iter().collect();
        return json_response(
            200,
            &serde_json::json!({
                "events": events,
                "total": total,
                "limit": limit,
                "offset": offset,
                "grouped": true,
            }),
        );
    }

    let event_ids: Vec<String> = decisions.iter().map(|d| d.event_id.clone()).collect();
    let feedback = store.feedback_for_events(&event_ids).unwrap_or_default();

    let events: Vec<serde_json::Value> = decisions
        .into_iter()
        .map(|d| {
            let fb = feedback.get(&d.event_id);
            decision_to_json(&d, fb)
        })
        .collect();

    json_response(
        200,
        &serde_json::json!({
            "events": events,
            "total": total,
            "limit": limit,
            "offset": offset,
        }),
    )
}

// ---- Activity API ----

fn extract_turn_id(raw: &Option<String>) -> Option<String> {
    raw.as_ref().and_then(|s| {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        v.get("turn_id")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    })
}

fn aggregate_codex_activities(
    conv: &ConversationRow,
    events: &[checkpoint_core::audit::EventRow],
    decisions: &[DecisionRow],
) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;

    let mut turn_groups: BTreeMap<String, Vec<&checkpoint_core::audit::EventRow>> = BTreeMap::new();
    let mut turn_order: Vec<String> = Vec::new();

    for e in events {
        let turn_id = extract_turn_id(&e.raw_payload).unwrap_or_else(|| "unknown".to_string());
        if !turn_groups.contains_key(&turn_id) {
            turn_order.push(turn_id.clone());
        }
        turn_groups.entry(turn_id).or_default().push(e);
    }

    let decision_map: std::collections::HashMap<&str, &DecisionRow> =
        decisions.iter().map(|d| (d.event_id.as_str(), d)).collect();

    turn_order
        .iter()
        .rev()
        .map(|turn_id| {
            let group = &turn_groups[turn_id];
            let started = group.first().map(|e| e.timestamp.as_str()).unwrap_or("");
            let ended = group.last().map(|e| e.timestamp.as_str()).unwrap_or("");

            let mut tool_counts: BTreeMap<&str, i64> = BTreeMap::new();
            let mut allow_count = 0i64;
            let mut ask_count = 0i64;
            let mut deny_count = 0i64;

            for e in group {
                *tool_counts.entry(e.tool_name.as_str()).or_insert(0) += 1;
                if let Some(d) = decision_map.get(e.id.as_str()) {
                    match d.action.as_str() {
                        "allow" => allow_count += 1,
                        "ask" => ask_count += 1,
                        "deny" => deny_count += 1,
                        _ => {}
                    }
                }
            }

            let tools_summary: Vec<serde_json::Value> = tool_counts
                .iter()
                .map(|(name, count)| serde_json::json!({"tool_name": name, "count": count}))
                .collect();

            let display_turn = if turn_id.len() > 12 {
                format!("{}\u{2026}", &turn_id[..12])
            } else {
                turn_id.clone()
            };

            serde_json::json!({
                "kind": "tool_group",
                "provider": conv.agent,
                "conversation_id": conv.conversation_id,
                "turn_id": turn_id,
                "title_or_prompt": display_turn,
                "started_at": started,
                "ended_at": ended,
                "tools_summary": tools_summary,
                "action_summary": {
                    "allow": allow_count,
                    "ask": ask_count,
                    "deny": deny_count,
                },
            })
        })
        .collect()
}

fn aggregate_time_window_activities(
    conv: &ConversationRow,
    events: &[checkpoint_core::audit::EventRow],
    decisions: &[DecisionRow],
) -> Vec<serde_json::Value> {
    let decision_map: std::collections::HashMap<&str, &DecisionRow> =
        decisions.iter().map(|d| (d.event_id.as_str(), d)).collect();

    struct Window {
        tool_name: String,
        started_at: String,
        ended_at: String,
        count: i64,
        allow: i64,
        ask: i64,
        deny: i64,
    }

    let mut windows: Vec<Window> = Vec::new();

    for e in events {
        let d = decision_map.get(e.id.as_str());
        let action = d.map(|d| d.action.as_str()).unwrap_or("allow");

        if let Some(last) = windows.last_mut() {
            if last.tool_name == e.tool_name {
                if let (Ok(curr_dt), Ok(last_dt)) = (
                    chrono::DateTime::parse_from_rfc3339(&e.timestamp),
                    chrono::DateTime::parse_from_rfc3339(&last.ended_at),
                ) {
                    let diff = (curr_dt.with_timezone(&chrono::Utc)
                        - last_dt.with_timezone(&chrono::Utc))
                    .num_seconds()
                    .abs();
                    if diff <= AGGREGATION_WINDOW_SECS {
                        last.count += 1;
                        last.ended_at = e.timestamp.clone();
                        match action {
                            "allow" => last.allow += 1,
                            "ask" => last.ask += 1,
                            "deny" => last.deny += 1,
                            _ => {}
                        }
                        continue;
                    }
                }
            }
        }

        windows.push(Window {
            tool_name: e.tool_name.clone(),
            started_at: e.timestamp.clone(),
            ended_at: e.timestamp.clone(),
            count: 1,
            allow: if action == "allow" { 1 } else { 0 },
            ask: if action == "ask" { 1 } else { 0 },
            deny: if action == "deny" { 1 } else { 0 },
        });
    }

    windows
        .iter()
        .rev()
        .map(|w| {
            serde_json::json!({
                "kind": "tool_group",
                "provider": conv.agent,
                "conversation_id": conv.conversation_id,
                "turn_id": null,
                "title_or_prompt": format!("{} (\u{00d7}{})", w.tool_name, w.count),
                "started_at": w.started_at,
                "ended_at": w.ended_at,
                "tools_summary": [{"tool_name": w.tool_name, "count": w.count}],
                "action_summary": {
                    "allow": w.allow,
                    "ask": w.ask,
                    "deny": w.deny,
                },
            })
        })
        .collect()
}

fn aggregate_transcript_activities(
    conv: &ConversationRow,
    messages: &[checkpoint_core::transcript::TranscriptMessage],
) -> Vec<serde_json::Value> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Group {
        title: String,
        started_at: String,
        ended_at: String,
        tool_counts: BTreeMap<String, i64>,
        total: i64,
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut current = Group::default();

    for m in messages {
        if m.role == "user" {
            if current.total > 0 {
                groups.push(current);
            }
            current = Group {
                title: if m.text.trim().is_empty() {
                    "用户回合".to_string()
                } else {
                    let title = m.text.trim().replace('\n', " ");
                    if title.chars().count() > 36 {
                        title.chars().take(36).collect::<String>() + "…"
                    } else {
                        title
                    }
                },
                started_at: m
                    .timestamp
                    .clone()
                    .unwrap_or_else(|| conv.last_seen_at.clone()),
                ended_at: m
                    .timestamp
                    .clone()
                    .unwrap_or_else(|| conv.last_seen_at.clone()),
                tool_counts: BTreeMap::new(),
                total: 0,
            };
            continue;
        }

        if m.role != "tool_summary" {
            continue;
        }

        if current.title.is_empty() {
            current.title = "工具活动".to_string();
            current.started_at = m
                .timestamp
                .clone()
                .unwrap_or_else(|| conv.last_seen_at.clone());
        }

        let tool = m.tool_name.as_deref().unwrap_or("tool").to_string();
        *current.tool_counts.entry(tool).or_insert(0) += 1;
        current.total += 1;
        current.ended_at = m
            .timestamp
            .clone()
            .unwrap_or_else(|| conv.last_seen_at.clone());
    }

    if current.total > 0 {
        groups.push(current);
    }

    groups
        .into_iter()
        .rev()
        .map(|g| {
            let tools_summary: Vec<serde_json::Value> = g
                .tool_counts
                .iter()
                .map(|(name, count)| serde_json::json!({"tool_name": name, "count": count}))
                .collect();

            serde_json::json!({
                "kind": "transcript_tool_group",
                "provider": conv.agent,
                "conversation_id": conv.conversation_id,
                "turn_id": null,
                "title_or_prompt": g.title,
                "started_at": g.started_at,
                "ended_at": g.ended_at,
                "tools_summary": tools_summary,
                "action_summary": {
                    "allow": g.total,
                    "ask": 0,
                    "deny": 0,
                },
                "source": "transcript",
            })
        })
        .collect()
}

pub fn handle_get_conversation_activity(
    ctx: &AppContext,
    cid: &str,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    let url = request.url();
    let limit = query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_ACTIVITY_PAGE_SIZE)
        .min(MAX_ACTIVITY_PAGE_SIZE);
    let offset = query_param(url, "offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    let store = ctx.store.lock().unwrap();

    let conv = match store.get_conversation(cid) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return json_response(404, &serde_json::json!({"error": "conversation not found"}));
        }
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query conversation: {e}")}),
            );
        }
    };

    // Fetch ALL events for the conversation, then aggregate, then paginate on activities.
    // This prevents pagination from splitting a single turn across pages.
    let events = match store.get_conversation_all_events(cid) {
        Ok(e) => e,
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query activity: {e}")}),
            );
        }
    };

    let event_ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
    let decisions = store
        .get_decisions_for_events(&event_ids)
        .unwrap_or_default();

    let all_activities = if conv.agent == "codex_cli" {
        let transcript_activities = transcript::read_transcript(
            &conv.agent,
            &conv.conversation_id,
            conv.project_path.as_deref(),
            conv.transcript_path.as_deref(),
        )
        .map(|messages| aggregate_transcript_activities(&conv, &messages))
        .unwrap_or_default();

        if transcript_activities.is_empty() {
            aggregate_codex_activities(&conv, &events, &decisions)
        } else {
            transcript_activities
        }
    } else {
        aggregate_time_window_activities(&conv, &events, &decisions)
    };

    let total = all_activities.len();
    let activities: Vec<serde_json::Value> = all_activities
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect();

    json_response(
        200,
        &serde_json::json!({
            "activities": activities,
            "conversation": conversation_to_json(&conv, &ctx.registry),
            "total": total,
            "limit": limit,
            "offset": offset,
        }),
    )
}

// ---------------------------------------------------------------------------
// Messages API — real chat content from provider transcripts
// ---------------------------------------------------------------------------

pub fn handle_get_conversation_messages(
    ctx: &AppContext,
    cid: &str,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    let route_start = std::time::Instant::now();
    let url = request.url();
    let limit = query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MESSAGE_PAGE_SIZE)
        .min(MAX_MESSAGE_PAGE_SIZE);
    let offset = query_param(url, "offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    let db_open_start = std::time::Instant::now();
    let store = match AuditStore::open(&ctx.db_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(500, &serde_json::json!({"error": format!("open db: {e}")}));
        }
    };
    let db_open_ms = db_open_start.elapsed().as_millis();

    let conv_start = std::time::Instant::now();
    let conv = match store.get_conversation(cid) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return json_response(404, &serde_json::json!({"error": "conversation not found"}));
        }
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query conversation: {e}")}),
            );
        }
    };
    let conv_ms = conv_start.elapsed().as_millis();

    // Incremental sync from transcript into cache
    // cache_id = cid (DB hash), provider_session_id = conv.conversation_id (for path resolution)
    let sync_start = std::time::Instant::now();
    let sync_result = transcript_sync::sync_conversation_messages(
        &store,
        &conv.agent,
        cid,
        &conv.conversation_id,
        conv.project_path.as_deref(),
        conv.transcript_path.as_deref(),
    );
    let sync_ms = sync_start.elapsed().as_millis();

    // Read from cache; fall back to activity-based summary if cache is empty
    let read_start = std::time::Instant::now();
    let total = sync_result.total_messages as usize;
    let (page, source) = if total > 0 {
        match store.get_conversation_messages(cid, limit, offset) {
            Ok(msgs) => (msgs, "cache"),
            Err(e) => {
                return json_response(
                    500,
                    &serde_json::json!({"error": format!("query messages: {e}")}),
                );
            }
        }
    } else {
        // Activity fallback — not persisted to conversation_messages
        let fallback = collect_activity_fallback(&store, cid);
        let fb_total = fallback.len();
        let page: Vec<serde_json::Value> = if offset >= fb_total {
            Vec::new()
        } else {
            let end = fb_total.saturating_sub(offset);
            let start = end.saturating_sub(limit);
            fallback[start..end].to_vec()
        };
        (page, "activity")
    };

    let display_total = if source == "activity" {
        let fb = collect_activity_fallback(&store, cid);
        fb.len()
    } else {
        total
    };
    let read_ms = read_start.elapsed().as_millis();

    eprintln!(
        "agent-aspect-bridge: GET /conversations/{cid}/messages limit={limit} offset={offset} total={display_total} source={source} synced={} open_ms={db_open_ms} conv_ms={conv_ms} sync_ms={sync_ms} read_ms={read_ms} route_ms={}",
        sync_result.messages_synced,
        route_start.elapsed().as_millis()
    );

    let sync_info = serde_json::json!({
        "last_synced_at": store.get_sync_state(cid).ok().flatten().and_then(|s| s.last_synced_at),
        "last_error": sync_result.last_error,
    });

    json_response(
        200,
        &serde_json::json!({
            "conversation": conversation_to_json(&conv, &ctx.registry),
            "messages": page,
            "total": display_total,
            "limit": limit,
            "offset": offset,
            "sync": sync_info,
        }),
    )
}

pub fn handle_get_conversation_messages_delta(
    ctx: &AppContext,
    cid: &str,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    let route_start = std::time::Instant::now();
    let url = request.url();
    let limit = query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MESSAGE_PAGE_SIZE)
        .min(MAX_MESSAGE_PAGE_SIZE);
    let after = query_param(url, "after")
        .or_else(|| query_param(url, "cursor"))
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    let db_open_start = std::time::Instant::now();
    let store = match AuditStore::open(&ctx.db_path) {
        Ok(s) => s,
        Err(e) => {
            return json_response(500, &serde_json::json!({"error": format!("open db: {e}")}));
        }
    };
    let db_open_ms = db_open_start.elapsed().as_millis();

    let conv_start = std::time::Instant::now();
    let conv = match store.get_conversation(cid) {
        Ok(Some(c)) => c,
        Ok(None) => {
            return json_response(404, &serde_json::json!({"error": "conversation not found"}));
        }
        Err(e) => {
            return json_response(
                500,
                &serde_json::json!({"error": format!("query conversation: {e}")}),
            );
        }
    };
    let conv_ms = conv_start.elapsed().as_millis();

    // Incremental sync
    let sync_start = std::time::Instant::now();
    let sync_result = transcript_sync::sync_conversation_messages(
        &store,
        &conv.agent,
        cid,
        &conv.conversation_id,
        conv.project_path.as_deref(),
        conv.transcript_path.as_deref(),
    );
    let sync_ms = sync_start.elapsed().as_millis();

    let total = sync_result.total_messages;

    // Get messages with seq > after; fall back to activity if cache is empty
    let read_start = std::time::Instant::now();
    let (page, cursor, has_more) = if total > 0 {
        // P3 fix: fetch limit+1 to distinguish "exactly one page" from "more pages"
        match store.get_conversation_messages_after_seq(cid, after, limit + 1) {
            Ok(msgs) => {
                let more = msgs.len() > limit;
                let page = if more { msgs[..limit].to_vec() } else { msgs };
                let c = page
                    .last()
                    .and_then(|m| m.get("seq"))
                    .and_then(|s| s.as_i64())
                    .unwrap_or(after);
                (page, c, more)
            }
            Err(e) => {
                return json_response(
                    500,
                    &serde_json::json!({"error": format!("query messages: {e}")}),
                );
            }
        }
    } else {
        // Activity fallback — index-based cursor (legacy behavior)
        let fallback = collect_activity_fallback(&store, cid);
        let fb_total = fallback.len();
        let start = (after as usize).min(fb_total);
        let end = (start + limit).min(fb_total);
        let page = fallback[start..end].to_vec();
        (page, end as i64, end < fb_total)
    };
    let read_ms = read_start.elapsed().as_millis();

    eprintln!(
        "agent-aspect-bridge: GET /conversations/{cid}/messages/delta after={after} limit={limit} returned={} total={total} open_ms={db_open_ms} conv_ms={conv_ms} sync_ms={sync_ms} read_ms={read_ms} route_ms={}",
        page.len(),
        route_start.elapsed().as_millis()
    );

    let sync_info = serde_json::json!({
        "last_synced_at": store.get_sync_state(cid).ok().flatten().and_then(|s| s.last_synced_at),
        "last_error": sync_result.last_error,
    });

    json_response(
        200,
        &serde_json::json!({
            "conversation": conversation_to_json(&conv, &ctx.registry),
            "messages": page,
            "cursor": cursor,
            "total": total,
            "limit": limit,
            "has_more": has_more,
            "sync": sync_info,
        }),
    )
}

fn collect_activity_fallback(store: &AuditStore, cid: &str) -> Vec<serde_json::Value> {
    let events = store.get_conversation_all_events(cid).unwrap_or_default();
    let event_ids: Vec<String> = events.iter().map(|e| e.id.clone()).collect();
    let decisions_raw = store
        .get_decisions_for_events(&event_ids)
        .unwrap_or_default();
    let decisions: HashMap<String, DecisionRow> = decisions_raw
        .into_iter()
        .map(|d| (d.event_id.clone(), d))
        .collect();

    let mut result = Vec::new();
    for ev in &events {
        let action = decisions
            .get(&ev.id)
            .map(|d| d.action.as_str())
            .unwrap_or("allow");
        result.push(serde_json::json!({
            "role": "tool_summary",
            "timestamp": ev.timestamp,
            "text": "",
            "source": "activity",
            "turn_id": null,
            "tool_name": ev.tool_name,
            "tool_input_preview": ev.raw_payload.as_ref().and_then(|p| {
                serde_json::from_str::<serde_json::Value>(p).ok()
                    .and_then(|v| v.get("tool_input").cloned())
                    .map(|ti| {
                        let s = serde_json::to_string(&ti).unwrap_or_default();
                        if s.len() > 120 { let end = s.char_indices().take_while(|(i,_)| *i < 120).last().map(|(i,c)| i + c.len_utf8()).unwrap_or(120); format!("{}...", &s[..end]) } else { s }
                    })
            }),
            "action": action,
        }));
    }
    result
}

// ---- Rules API ----

pub fn handle_get_rules() -> tiny_http::ResponseBox {
    use checkpoint_core::rule::RuleEngine;
    let engine = RuleEngine::with_defaults(read_mode());
    let rules: Vec<serde_json::Value> = engine
        .rules()
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "description": r.description,
            })
        })
        .collect();
    json_response(200, &serde_json::json!({"rules": rules}))
}

pub fn handle_get_job_kinds() -> tiny_http::ResponseBox {
    let kinds = crate::jobs::JobRunner::available_kinds();
    json_response(200, &serde_json::json!({"kinds": kinds}))
}

// ---- Relay Status / Pairing API ----

/// Derive the mobile-facing HTTPS URL from the WebSocket relay_url.
/// wss://relay.viper.mom/ws → https://relay.viper.mom/
fn derive_mobile_url(relay_url: &str) -> Option<String> {
    let url = relay_url
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            relay_url
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })?;
    // Strip /ws or trailing slash to get the base URL
    let url = if url.ends_with("/ws") {
        url[..url.len() - 3].to_string()
    } else if url.ends_with('/') {
        url[..url.len() - 1].to_string()
    } else {
        url
    };
    Some(url)
}

/// Decode the payload of a cp_rt1 token without verifying the signature.
/// Used to extract sid/exp for display purposes.
fn decode_token_payload(token: &str) -> Option<serde_json::Value> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || parts[0] != "cp_rt1" {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn handle_get_relay_status() -> tiny_http::ResponseBox {
    let config = Config::load_or_create();

    let relay_url = config.relay_url.or_else(|| {
        checkpoint_core::env_compat::env_var("AGENT_ASPECT_RELAY_URL", "CHECKPOINT_RELAY_URL")
    });

    let enabled = relay_url.is_some();
    let mobile_url = relay_url.as_deref().and_then(derive_mobile_url);

    let client_token_path = checkpoint_core::paths::relay_client_token_path();
    let client_token_available = client_token_path.exists();

    // Check if token is not expired (best-effort)
    let connected = if enabled && client_token_available {
        if let Ok(token_str) = std::fs::read_to_string(&client_token_path) {
            let token_str = token_str.trim();
            if let Some(payload) = decode_token_payload(token_str) {
                let exp = payload.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
                let now = chrono::Utc::now().timestamp();
                exp > now
            } else {
                true // Can't decode, assume connected
            }
        } else {
            false
        }
    } else {
        false
    };

    json_response(
        200,
        &serde_json::json!({
            "enabled": enabled,
            "connected": connected,
            "relay_url": relay_url,
            "mobile_url": mobile_url,
            "client_token_available": client_token_available,
        }),
    )
}

pub fn handle_get_relay_pairing() -> tiny_http::ResponseBox {
    let config = Config::load_or_create();

    let relay_url = config.relay_url.or_else(|| {
        checkpoint_core::env_compat::env_var("AGENT_ASPECT_RELAY_URL", "CHECKPOINT_RELAY_URL")
    });

    let mobile_url = match relay_url.as_deref().and_then(derive_mobile_url) {
        Some(u) => u,
        None => {
            return json_response(400, &serde_json::json!({"error": "relay not configured"}));
        }
    };

    let client_token_path = checkpoint_core::paths::relay_client_token_path();
    let client_token = match std::fs::read_to_string(&client_token_path) {
        Ok(t) => t.trim().to_string(),
        Err(_) => {
            return json_response(
                400,
                &serde_json::json!({"error": "relay not registered — no client token"}),
            );
        }
    };

    // Decode token to get sid and expires_at
    let payload = decode_token_payload(&client_token).unwrap_or_default();
    let sid = payload
        .get("sid")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let exp_ts = payload.get("exp").and_then(|v| v.as_i64()).unwrap_or(0);
    let expires_at = if exp_ts > 0 {
        chrono::DateTime::from_timestamp(exp_ts, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| exp_ts.to_string())
    } else {
        "unknown".to_string()
    };

    json_response(
        200,
        &serde_json::json!({
            "mobile_url": mobile_url,
            "client_token": client_token,
            "expires_at": expires_at,
            "sid": sid,
        }),
    )
}

#[cfg(test)]
mod relay_tests {
    use super::*;

    #[test]
    fn derive_mobile_url_wss() {
        assert_eq!(
            derive_mobile_url("wss://relay.viper.mom/ws"),
            Some("https://relay.viper.mom".to_string())
        );
    }

    #[test]
    fn derive_mobile_url_ws() {
        assert_eq!(
            derive_mobile_url("ws://localhost:8080/ws"),
            Some("http://localhost:8080".to_string())
        );
    }

    #[test]
    fn derive_mobile_url_invalid() {
        assert_eq!(derive_mobile_url("https://relay.example.com"), None);
    }

    #[test]
    fn decode_token_payload_valid() {
        // Create a fake cp_rt1 token with a known payload
        let payload = serde_json::json!({
            "ver": 1,
            "sid": "test-sid-123",
            "role": "client",
            "iat": 1000000,
            "exp": 2000000,
            "jti": "jti-abc"
        });
        let payload_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&payload).unwrap());
        let fake_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"fakesig");
        let token = format!("cp_rt1.{payload_b64}.{fake_sig}");

        let decoded = decode_token_payload(&token).unwrap();
        assert_eq!(decoded["sid"].as_str().unwrap(), "test-sid-123");
        assert_eq!(decoded["role"].as_str().unwrap(), "client");
        assert_eq!(decoded["exp"].as_i64().unwrap(), 2000000);
    }

    #[test]
    fn decode_token_payload_invalid_format() {
        assert!(decode_token_payload("not-a-token").is_none());
        assert!(decode_token_payload("cp_rt1.onlytwo").is_none());
    }
}

// ---- Learn / Suggestions API ----

pub fn handle_get_learn_suggestions(ctx: &AppContext) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();
    if let Err(e) = checkpoint_core::learn::generate_suggestions(&store) {
        eprintln!("generate suggestions: {e}");
    }
    match store.list_pending_suggestions(100) {
        Ok(suggestions) => {
            let json: Vec<serde_json::Value> = suggestions
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id, "title": s.title, "reason": s.reason,
                        "confidence": s.confidence, "agent": s.agent, "tool_name": s.tool_name,
                        "project_path": s.project_path, "pattern": s.pattern,
                        "sample_event_ids": s.sample_event_ids, "sample_count": s.sample_count,
                        "suggested_action": s.suggested_action, "status": s.status,
                        "created_at": s.created_at,
                    })
                })
                .collect();
            json_response(200, &serde_json::json!({"suggestions": json}))
        }
        Err(e) => json_response(
            500,
            &serde_json::json!({"error": format!("query suggestions: {e}")}),
        ),
    }
}

pub fn handle_post_suggestion_action(
    ctx: &AppContext,
    id: &str,
    action: &str,
) -> tiny_http::ResponseBox {
    if !matches!(action, "accepted" | "rejected") {
        return json_response(400, &serde_json::json!({"error": "invalid action"}));
    }
    let store = ctx.store.lock().unwrap();
    match store.get_suggestion(id) {
        Ok(None) => json_response(404, &serde_json::json!({"error": "suggestion not found"})),
        Ok(Some(s)) if s.status != "pending" => json_response(
            409,
            &serde_json::json!({"error": format!("suggestion already {}", s.status)}),
        ),
        Err(e) => json_response(500, &serde_json::json!({"error": format!("query: {e}")})),
        _ => {
            if let Err(e) =
                store.update_suggestion_status(id, action, &chrono::Utc::now().to_rfc3339())
            {
                return json_response(500, &serde_json::json!({"error": format!("update: {e}")}));
            }
            json_response(200, &serde_json::json!({"id": id, "status": action}))
        }
    }
}

pub fn handle_get_learn_rules(ctx: &AppContext) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();
    match store.list_accepted_suggestions() {
        Ok(rules) => {
            let json: Vec<serde_json::Value> = rules
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "agent": r.agent,
                        "tool_name": r.tool_name,
                        "pattern": r.pattern,
                        "action": r.suggested_action,
                        "sample_count": r.sample_count,
                        "accepted_at": r.resolved_at,
                    })
                })
                .collect();
            json_response(200, &serde_json::json!({"rules": json}))
        }
        Err(e) => json_response(
            500,
            &serde_json::json!({"error": format!("query learned rules: {e}")}),
        ),
    }
}
