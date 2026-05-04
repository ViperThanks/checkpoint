//! Agent Aspect 守护进程 — 通过 Unix socket 接收 hook 请求，执行规则引擎判定。
//!
//! 职责：
//! - 监听 Unix socket，接收 hook-cli 发来的工具使用审核请求。
//! - 对每个请求：规范化 → 规则引擎判定 → 学习规则查询 → 审计写入 → 返回判定结果。
//! - 支持 Override 请求（用户交互式覆盖判定结果）。
//! - 支持 Metadata 请求（会话元数据采集，用于 UI 展示）。
//!
//! 架构角色：Agent Aspect 安全防线的服务端。hook-cli 是无状态的薄客户端，
//! 所有规则判定和审计逻辑都在此守护进程中执行，确保策略一致性。
//!
//! 不变量：
//! - 单例运行：启动时 kill 已有 daemon 实例。
//! - 每次 accept 连接后重新读取配置，支持热重载 mode。
//! - 审计数据启动时执行 retention purge，防止数据库无限增长。

use checkpoint_core::audit::AuditStore;
use checkpoint_core::config::Config;
use checkpoint_core::decision::Action;
use checkpoint_core::event::AgentId;
use checkpoint_core::normalize::{
    normalize_claude_pre_tool_use, normalize_codex_pre_tool_use, normalize_gemini_pre_tool_use,
    normalize_kimi_pre_tool_use,
};
use checkpoint_core::paths;
use checkpoint_core::rule::{Mode, RuleEngine};
use checkpoint_core::wire::{OverrideRequest, WireRequest, WireResponse};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Mutex;

/// 全局日志文件句柄，延迟初始化。
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// 初始化日志文件：打开或创建 daemon 日志文件。
fn log_init() {
    let log_path = paths::daemon_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => {
            *LOG_FILE.lock().unwrap() = Some(f);
        }
        Err(e) => {
            eprintln!(
                "agent-aspectd: cannot open log file {}: {e}",
                log_path.display()
            );
        }
    }
}

/// 写入一条带时间戳的日志。若日志文件不可用则 fallback 到 stderr。
fn log_msg(msg: &str) {
    let line = format!(
        "[{}] {msg}\n",
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z")
    );
    let guard = LOG_FILE.lock().unwrap();
    if let Some(mut f) = guard.as_ref() {
        if let Err(e) = f.write_all(line.as_bytes()) {
            drop(guard);
            eprintln!("agent-aspectd: log write failed: {e}");
        }
    } else {
        drop(guard);
        eprintln!("agent-aspectd: {msg}");
    }
}

/// 格式化日志宏。
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log_msg(&format!($($arg)*))
    };
}

/// 从配置文件或环境变量解析运行模式。
///
/// 优先级：配置文件 > CHECKPOINT_MODE 环境变量 > 默认 Guard。
/// 配置文件不存在时生成默认配置。
fn resolve_mode() -> Mode {
    let config_path = Config::config_path();
    if config_path.exists() {
        match Config::load(&config_path) {
            Ok(cfg) => return cfg.mode,
            Err(e) => log_info!("config load error: {e}, falling back"),
        }
    } else {
        let cfg = Config::default_config();
        if let Err(e) = cfg.save(&config_path) {
            log_info!("config save error: {e}");
        }
        return cfg.mode;
    }

    // fallback: AGENT_ASPECT_MODE / CHECKPOINT_MODE env
    checkpoint_core::env_compat::env_var("AGENT_ASPECT_MODE", "CHECKPOINT_MODE")
        .and_then(|raw| raw.parse::<Mode>().ok())
        .unwrap_or(Mode::Guard)
}

/// IPC 消息最大字节数（1 MiB）。超出断开连接，防止内存耗尽。
const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;

/// 从 Unix socket 流中读取全部数据（直到 EOF 或超过大小限制）。
fn read_fully(stream: &mut UnixStream) -> Option<String> {
    let mut buf = Vec::with_capacity(256 * 1024);
    let mut tmp = [0u8; 8192];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > MAX_IPC_MESSAGE_BYTES {
                    log_info!("IPC message too large ({} bytes), dropping connection", buf.len());
                    return None;
                }
            }
            Err(e) => {
                log_info!("read error: {e}");
                return None;
            }
        }
    }
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// 处理单个客户端连接：读取请求 → 分发到对应处理器 → 写回响应。
///
/// 支持三种请求类型：Evaluate（工具使用审核）、Override（用户覆盖）、Metadata（会话元数据）。
fn handle_client(mut stream: UnixStream, store: &AuditStore, engine: &RuleEngine) {
    let raw = match read_fully(&mut stream) {
        Some(r) => r,
        None => return,
    };

    if raw.is_empty() {
        return;
    }

    let request: WireRequest = match serde_json::from_str(&raw) {
        Ok(request) => request,
        Err(e) => {
            let resp = WireResponse::deny(format!("internal error: parse wire request: {e}"));
            write_wire_response(&mut stream, &resp);
            return;
        }
    };

    match request {
        WireRequest::Evaluate {
            payload,
            agent,
            device_id,
        } => {
            let device_id = device_id.unwrap_or_else(|| "local-hook".to_string());
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(e) = store.register_device(&device_id, Some("agent-aspect-hook"), None, &now)
            {
                log_info!("register device failed: {e}");
            }

            // 根据 agent 类型选择对应的 payload 规范化函数
            let normalize_fn = match agent {
                Some(AgentId::CodexCli) => normalize_codex_pre_tool_use,
                Some(AgentId::KimiCode) => normalize_kimi_pre_tool_use,
                Some(AgentId::GeminiCli) => normalize_gemini_pre_tool_use,
                Some(AgentId::ClaudeCode) | None => normalize_claude_pre_tool_use,
                Some(other) => {
                    log_info!("unsupported agent: {other}, denying");
                    let resp = WireResponse::deny(format!("unsupported agent: {other}"));
                    write_wire_response(&mut stream, &resp);
                    return;
                }
            };

            match normalize_fn(&payload) {
                Ok(event) => {
                    // 先运行静态规则引擎，再检查学习规则。
                    // 只有静态规则没有匹配且默认为 Allow 时，才查询学习规则。
                    let mut decision = engine.evaluate(&event);
                    if decision.rule_id.is_none() && decision.action == Action::Allow {
                        if let Ok(true) =
                            store.has_learned_allow(event.agent.as_str(), &event.tool_name)
                        {
                            decision.rule_id = Some("[aspect-learned]".to_string());
                            decision.note = "[aspect-learned] auto-allowed".to_string();
                        }
                    }

                    // 审计写入：记录事件和判定结果（失败只记日志，不阻塞流程）
                    if let Err(e) = store.insert_event(
                        &event.id,
                        event.phase.as_str(),
                        &event.event_type,
                        event.agent.as_str(),
                        &event.tool_name,
                        event.tool_input.file_path.as_deref(),
                        &event.timestamp,
                        &payload,
                    ) {
                        log_info!("audit write event failed: {e}");
                    }
                    if let Err(e) = store.insert_decision_for_device(
                        &decision.event_id,
                        decision.action.as_str(),
                        decision.rule_id.as_deref(),
                        &decision.note,
                        &chrono::Utc::now().to_rfc3339(),
                        Some(&device_id),
                    ) {
                        log_info!("audit write decision failed: {e}");
                    }

                    let resp = WireResponse::from_decision(&decision);
                    write_wire_response(&mut stream, &resp);
                }
                Err(e) => {
                    log_info!("normalize error: {e}");
                    let resp = WireResponse::deny(format!("internal error: {e}"));
                    write_wire_response(&mut stream, &resp);
                }
            }
        }
        WireRequest::Override {
            event_id,
            original_action,
            final_action,
            note,
            device_id,
        } => {
            let override_request = OverrideRequest {
                event_id,
                original_action,
                final_action,
                note,
            };
            handle_override(&override_request, store, &mut stream, device_id.as_deref());
        }
        WireRequest::Metadata { payload, agent, .. } => {
            handle_metadata(&payload, agent.as_ref(), store, &mut stream);
        }
        WireRequest::Stop {
            payload,
            agent,
            device_id,
        } => {
            handle_stop(
                &payload,
                agent.as_ref(),
                device_id.as_deref(),
                store,
                &mut stream,
            );
        }
    }
}

/// 处理会话元数据请求（SessionStart / UserPromptSubmit）。
///
/// 从 payload 中提取会话信息，upsert 到 conversation 表。
/// 这类请求始终返回 Allow，不需要规则判定。
fn handle_metadata(
    payload: &str,
    agent: Option<&AgentId>,
    store: &AuditStore,
    stream: &mut UnixStream,
) {
    use checkpoint_core::conversation::{
        conversation_db_id, extract_prompt_metadata, extract_session_start_metadata, generate_title,
    };

    let agent_str = match agent {
        Some(a) => a.as_str(),
        None => "claude_code",
    };

    // 提取 hook 事件类型以决定处理路径
    let hook_event = serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    let update = match hook_event.as_str() {
        "SessionStart" => extract_session_start_metadata(payload),
        "UserPromptSubmit" => extract_prompt_metadata(payload),
        _ => {
            // 未知事件类型：直接返回 Allow
            let resp = WireResponse {
                event_id: None,
                action: Action::Allow,
                note: String::new(),
            };
            write_wire_response(stream, &resp);
            return;
        }
    };

    let Some(session_id) = update.session_id.as_deref() else {
        let resp = WireResponse {
            event_id: None,
            action: Action::Allow,
            note: "no session_id".to_string(),
        };
        write_wire_response(stream, &resp);
        return;
    };

    let db_id = conversation_db_id(agent_str, session_id);
    let now = chrono::Utc::now().to_rfc3339();

    // 为新会话生成 fallback 标题
    let fallback_title = generate_title(agent_str, update.project_path.as_deref(), None);

    if let Err(e) = store.upsert_conversation_from_metadata(
        &db_id,
        agent_str,
        session_id,
        &fallback_title,
        update.project_path.as_deref(),
        &now,
        update.transcript_path.as_deref(),
        update.title.as_deref(),
        update.title_source.as_deref(),
    ) {
        log_info!("metadata upsert failed: {e}");
    }

    let resp = WireResponse {
        event_id: None,
        action: Action::Allow,
        note: String::new(),
    };
    write_wire_response(stream, &resp);
}

/// 处理 Stop hook：找到对应 running job 并写入 stop_requested_at marker。
///
/// 匹配策略（按优先级）：
/// 1. provider + conversation_id（从 session_id 提取）
/// 2. provider + project_path（fallback，仅当 conversation_id 不可用时）
///
/// 找不到匹配 job 时静默返回，不写 audit，避免非 bridge job 产生的 Stop 事件造成 noise。
fn handle_stop(
    payload: &str,
    agent: Option<&AgentId>,
    device_id: Option<&str>,
    store: &AuditStore,
    stream: &mut UnixStream,
) {
    use checkpoint_core::conversation::extract_conversation_id;

    let agent_str = match agent {
        Some(a) => a.as_str(),
        None => "claude_code",
    };
    let device_id = device_id.unwrap_or("local-hook");
    let now = chrono::Utc::now().to_rfc3339();

    // Extract conversation_id from payload for job matching
    let conversation_id = extract_conversation_id(agent_str, payload);
    let provider = agent_str;

    // Try to find matching running job:
    // 1. Primary: provider + conversation_id
    // 2. Fallback: provider + project_path (covers jobs that haven't bound conversation_id yet)
    let matched_job = conversation_id
        .as_deref()
        .and_then(|cid| {
            store
                .find_running_job_by_conversation(provider, cid)
                .ok()
                .flatten()
        })
        .or_else(|| {
            let project_path =
                checkpoint_core::conversation::extract_project_path(agent_str, payload);
            project_path.as_deref().and_then(|pp| {
                store
                    .find_running_job_by_project(provider, pp)
                    .ok()
                    .flatten()
            })
        });

    if let Some(job) = matched_job {
        // Only write audit when we actually matched a running job
        let event_id = uuid::Uuid::now_v7().to_string();
        let tool_name = if let Some(ref wf_id) = job.workflow_id {
            format!("[aspect-stop-from-workflow-{wf_id}]")
        } else {
            "[aspect-stop]".to_string()
        };
        let _ = store.insert_event(
            &event_id,
            "stop",
            "hook.stop",
            agent_str,
            &tool_name,
            None,
            &now,
            payload,
        );
        let _ = store.insert_decision_for_device(
            &event_id,
            "allow",
            None,
            &tool_name,
            &now,
            Some(device_id),
        );
        if let Err(e) = store.set_stop_requested_at(&job.id, &now) {
            log_info!("stop marker write failed for job {}: {e}", job.id);
        } else {
            log_info!(
                "[aspect-stop] marker set for job {} (provider={}, conv={:?})",
                job.id,
                provider,
                job.conversation_id
            );
        }

        // 如果 job 属于 manual 模式的 workflow，写入推进信号
        if let Some(ref wf_id) = job.workflow_id {
            if let Ok(Some(wf)) = store.get_workflow(wf_id) {
                if wf.advance_mode == "manual" {
                    let _ = store.insert_workflow_advance_signal(
                        wf_id,
                        None, // step_id 由 bridge 根据 workflow 状态推断
                        agent_str,
                        "stop",
                        &now,
                    );
                    log_info!(
                        "[aspect-stop] advance signal queued for workflow {} (manual mode)",
                        wf_id
                    );
                }
            }
        }
        let resp = WireResponse {
            event_id: Some(event_id),
            action: Action::Allow,
            note: String::new(),
        };
        write_wire_response(stream, &resp);
    } else {
        // No matching running job — silent return, no audit noise
        let resp = WireResponse {
            event_id: None,
            action: Action::Allow,
            note: String::new(),
        };
        write_wire_response(stream, &resp);
    }
}

/// 处理用户覆盖请求：记录覆盖决策到审计日志。
///
/// 覆盖来源可能是交互式终端确认（hook-cli --interactive）或远程 UI 操作。
fn handle_override(
    msg: &OverrideRequest,
    store: &AuditStore,
    stream: &mut UnixStream,
    device_id: Option<&str>,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let device_id = device_id.unwrap_or("local-hook");
    if let Err(e) = store.register_device(device_id, Some("agent-aspect-hook"), None, &now) {
        log_info!("register override device failed: {e}");
    }
    if let Err(e) = store.insert_decision_for_device(
        &msg.event_id,
        msg.final_action.as_str(),
        Some("[aspect-user-override]"),
        &msg.note,
        &now,
        Some(device_id),
    ) {
        log_info!("override audit write failed: {e}");
    }

    let resp = WireResponse {
        event_id: Some(msg.event_id.clone()),
        action: msg.final_action,
        note: msg.note.clone(),
    };
    write_wire_response(stream, &resp);
}

/// 将 WireResponse 序列化为 JSON 并写入 Unix socket 流。
fn write_wire_response(stream: &mut UnixStream, response: &WireResponse) {
    match serde_json::to_vec(response) {
        Ok(body) => {
            if let Err(e) = stream.write_all(&body) {
                log_info!("write response failed: {e}");
            }
        }
        Err(e) => {
            log_info!("serialize response failed: {e}");
        }
    }
}

/// Daemon 主入口。
///
/// 流程：初始化日志 → 单例锁 → 加载规则引擎 → 绑定 Unix socket →
/// 启动时清理过期审计数据 → 进入 accept 循环（每次连接热重载配置）。
fn main() {
    log_init();

    // 单例守卫：杀掉已运行的旧 daemon 实例
    let state_path = paths::state_path();
    if let Some(old_pid) =
        checkpoint_core::process_guard::kill_existing(&state_path, "agent-aspectd")
    {
        log_info!("replaced previous daemon (pid {old_pid})");
    }

    let mut mode = resolve_mode();
    let mut engine = RuleEngine::with_defaults(mode);

    let sock_path = paths::socket_path();
    let db_path = paths::audit_db_path();
    if let Some(parent) = sock_path.parent() {
        let _ = std::fs::create_dir_all(parent);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }

    // 清理可能残留的旧 socket 文件
    if sock_path.exists() {
        std::fs::remove_file(&sock_path).ok();
    }

    let store = AuditStore::open(&db_path).expect("open audit store");

    // 数据库文件权限限制为 owner-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&db_path, std::fs::Permissions::from_mode(0o600));
    }

    let listener = UnixListener::bind(&sock_path).expect("bind unix socket");

    // Socket 文件权限限制为 owner-only
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600));
    }

    // 启动时按 retention_days 清理过期审计数据
    let config_path = Config::config_path();
    let retention_days = if config_path.exists() {
        Config::load(&config_path)
            .map(|c| c.audit_retention_days)
            .unwrap_or(90)
    } else {
        90
    };
    if retention_days > 0 {
        if let Some(before) = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(retention_days as i64))
            .map(|dt| dt.to_rfc3339())
        {
            match store.purge_before(&before) {
                Ok((ev, dec)) => {
                    if ev > 0 || dec > 0 {
                        log_info!(
                            "purged {ev} events, {dec} decisions older than {retention_days} days"
                        );
                    }
                }
                Err(e) => log_info!("purge failed: {e}"),
            }
        }
    }

    // 写入运行状态文件（供外部查询 daemon 状态）
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let write_state = |mode: Mode| {
        let state = serde_json::json!({
            "mode": mode.as_str(),
            "pid": std::process::id(),
            "exe": exe,
        });
        if let Err(e) = std::fs::write(&state_path, state.to_string()) {
            log_info!("write state failed: {e}");
        }
    };
    write_state(mode);

    log_info!("listening on {} (mode: {})", sock_path.display(), mode);

    // 主循环：每次 accept 后热重载配置
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                // 每次连接检查配置是否变更，支持热重载
                let new_mode = resolve_mode();
                if new_mode != mode {
                    log_info!("mode reload: {} -> {}", mode, new_mode);
                    mode = new_mode;
                    engine = RuleEngine::with_defaults(mode);
                    write_state(mode);
                }
                handle_client(s, &store, &engine);
            }
            Err(e) => log_info!("accept error: {e}"),
        }
    }
}
