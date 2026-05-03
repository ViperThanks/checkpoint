//! Agent Aspect Hook CLI — Agent 工具使用拦截的入口点。
//!
//! 职责：
//! - 从 stdin 读取 hook payload，检测 agent 类型（Claude/Codex/Kimi/Gemini）。
//! - 通过 Unix socket 将请求发送给 daemon 进行规则判定。
//! - 根据判定结果输出 agent 兼容的 hook 响应（JSON 格式）。
//! - 支持 --interactive 模式：用户可在终端交互式覆盖判定结果。
//!
//! 架构角色：Agent hook 配置的入口点，被各 AI agent 作为 pre-tool-use hook 调用。
//! 本身无状态，所有判定逻辑委托给 daemon。
//!
//! 不变量：
//! - daemon 不可达时默认 Allow（不阻塞 agent 工作流）。
//! - Metadata 请求（SessionStart/UserPromptSubmit）不输出 hook 响应。
//! - 交互模式通过 /dev/tty 读取用户输入，不占用 stdin（stdin 被 hook payload 使用）。

use checkpoint_core::decision::Action;
use checkpoint_core::event::AgentId;
use checkpoint_core::paths;
use checkpoint_core::wire::{HookResponse, WireRequest, WireResponse};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::str::FromStr;

/// CLI 入口：读取 payload → 检测 agent → 连接 daemon → 发送请求 → 处理响应。
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let interactive = args.iter().any(|a| a == "--interactive" || a == "-i");

    let payload = read_stdin();
    let agent = detect_agent(&payload);
    let sock_path = paths::socket_path();

    // 连接 daemon；不可达时默认 Allow
    let mut stream = match UnixStream::connect(&sock_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agent-aspect-hook: daemon not reachable: {e}");
            // daemon not running -> allow by default, no output
            return;
        }
    };

    // 根据事件类型选择请求路径
    let hook_event = extract_hook_event(&payload);
    let request = if hook_event == "PreToolUse" || hook_event == "BeforeTool" {
        // 工具使用审核路径：需要规则判定
        WireRequest::Evaluate {
            payload,
            agent: Some(agent),
            device_id: Some("local-hook".to_string()),
        }
    } else if hook_event == "Stop" {
        // Stop 信号路径：通知 daemon 收敛对应 job
        WireRequest::Stop {
            payload,
            agent: Some(agent),
            device_id: Some("local-hook".to_string()),
        }
    } else {
        // 会话元数据路径：仅供审计，不需要判定
        WireRequest::Metadata {
            payload,
            agent: Some(agent),
            device_id: Some("local-hook".to_string()),
        }
    };

    let body = match serde_json::to_vec(&request) {
        Ok(body) => body,
        Err(e) => {
            eprintln!("agent-aspect-hook: wire serialize failed: {e}");
            return;
        }
    };

    // 写入请求并 shutdown 写端，通知 daemon 请求已完整
    if let Err(e) = stream.write_all(&body) {
        eprintln!("agent-aspect-hook: ipc write failed: {e}");
        return;
    }
    stream.shutdown(std::net::Shutdown::Write).ok();

    // 读取 daemon 响应
    let mut resp = String::new();
    match stream.read_to_string(&mut resp) {
        Ok(_) => {
            let internal: WireResponse = match serde_json::from_str(&resp) {
                Ok(response) => response,
                Err(e) => {
                    eprintln!("agent-aspect-hook: wire parse failed: {e}");
                    return;
                }
            };

            // Metadata 和 Stop 请求不需要输出 hook 响应
            if matches!(
                request,
                WireRequest::Metadata { .. } | WireRequest::Stop { .. }
            ) {
                return;
            }

            if interactive {
                handle_interactive(
                    internal.action,
                    &internal.note,
                    internal.event_id.as_deref(),
                );
            } else {
                emit_hook_response(internal.action, &internal.note);
            }
        }
        Err(e) => {
            eprintln!("agent-aspect-hook: ipc read failed: {e}");
        }
    }
}

/// 从 payload 中提取 hook_event_name 字段。
fn extract_hook_event(payload: &str) -> String {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("hook_event_name")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

/// 检测当前请求来自哪个 AI agent。
///
/// 检测优先级：
/// 1. `CHECKPOINT_AGENT` 环境变量（显式指定）
/// 2. Payload 启发式：tool_name / tool_input 字段模式匹配
///
/// 只返回有对应 normalize 路径的 agent（Claude/Codex/Kimi）。
/// 未知环境变量值被忽略，回退到启发式检测。
/// 最终兜底为 Claude Code（向后兼容）。
fn detect_agent(payload: &str) -> AgentId {
    let supported = |agent: AgentId| -> bool {
        matches!(
            agent,
            AgentId::ClaudeCode | AgentId::CodexCli | AgentId::KimiCode
        )
    };

    // 1. 显式环境变量（只接受已支持的 agent）
    if let Some(val) =
        checkpoint_core::env_compat::env_var("AGENT_ASPECT_AGENT", "CHECKPOINT_AGENT")
    {
        if let Ok(agent) = AgentId::from_str(&val) {
            if supported(agent) {
                return agent;
            }
            eprintln!(
                "agent-aspect-hook: AGENT_ASPECT_AGENT={val} not yet supported, falling back to heuristic"
            );
        }
    }

    // 2. Payload 启发式检测
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(payload) {
        let tool_name = parsed["tool_name"].as_str().unwrap_or("");
        let ti = &parsed["tool_input"];

        // Kimi: WriteFile / StrReplaceFile / Shell
        if matches!(tool_name, "WriteFile" | "StrReplaceFile" | "Shell") {
            return AgentId::KimiCode;
        }

        // Codex: has turn_id field (unique to Codex payload)
        if parsed.get("turn_id").is_some() {
            return AgentId::CodexCli;
        }

        // Claude: Edit / Write / Bash with file_path field
        if matches!(tool_name, "Edit" | "Write" | "Bash" | "Read") && ti.get("file_path").is_some()
        {
            return AgentId::ClaudeCode;
        }
    }

    // 兜底：Claude Code（向后兼容）
    AgentId::ClaudeCode
}

/// 向 stdout 输出 agent 兼容的 hook 响应。
///
/// 三种 agent（Claude Code、Codex CLI、Kimi Code）都接受相同的 JSON 格式，
/// 无需按 agent 分支。
fn emit_hook_response(action: Action, note: &str) {
    if let Some(out) = HookResponse::from_action(action, note.to_string()) {
        match serde_json::to_string(&out) {
            Ok(body) => println!("{body}"),
            Err(e) => eprintln!("agent-aspect-hook: hook response serialize failed: {e}"),
        }
    }
}

/// 交互模式处理：在终端显示判定结果，允许用户覆盖。
///
/// - Allow：直接输出。
/// - Deny：询问是否覆盖为 Allow。
/// - Ask：询问是否允许，否决则覆盖为 Deny。
///
/// 覆盖操作通过 Override 请求发送给 daemon 记录审计日志。
fn handle_interactive(action: Action, note: &str, event_id: Option<&str>) {
    match action {
        Action::Allow => {
            println!("[agent-aspect] allowed.");
        }
        Action::Deny => {
            eprintln!("[agent-aspect] DENIED: {note}");
            eprintln!("[agent-aspect] Override? [y/N] ");
            if read_yes_tty() {
                send_override(
                    event_id,
                    Action::Deny,
                    Action::Allow,
                    "user override: deny -> allow",
                );
                println!("[agent-aspect] overridden to allow.");
            } else {
                // 保持 deny，审计已记录原始判定
                emit_hook_response(Action::Deny, note);
            }
        }
        Action::Ask => {
            eprintln!("[agent-aspect] ASK: {note}");
            eprintln!("[agent-aspect] Proceed? [y/N] ");
            if read_yes_tty() {
                send_override(
                    event_id,
                    Action::Ask,
                    Action::Allow,
                    "user approved: ask -> allow",
                );
                // allow 不需要输出 hook 响应
            } else {
                send_override(
                    event_id,
                    Action::Ask,
                    Action::Deny,
                    "user rejected: ask -> deny",
                );
                emit_hook_response(Action::Deny, note);
            }
        }
        _ => {
            println!("[agent-aspect] allowed.");
        }
    }
}

/// 向 daemon 发送 Override 请求，记录用户覆盖决策到审计日志。
///
/// 失败只记 stderr，不阻塞主流程。
fn send_override(
    event_id: Option<&str>,
    original_action: Action,
    final_action: Action,
    note: &str,
) {
    let Some(event_id) = event_id else {
        eprintln!("agent-aspect-hook: override skipped: missing event_id");
        return;
    };

    let sock_path = paths::socket_path();
    let mut stream = match UnixStream::connect(&sock_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agent-aspect-hook: override send failed: {e}");
            return;
        }
    };
    let msg = WireRequest::Override {
        event_id: event_id.to_string(),
        original_action,
        final_action,
        note: note.to_string(),
        device_id: Some("local-hook".to_string()),
    };
    let body = match serde_json::to_vec(&msg) {
        Ok(body) => body,
        Err(e) => {
            eprintln!("agent-aspect-hook: override serialize failed: {e}");
            return;
        }
    };
    if let Err(e) = stream.write_all(&body) {
        eprintln!("agent-aspect-hook: override write failed: {e}");
        return;
    }
    stream.shutdown(std::net::Shutdown::Write).ok();
    // 等待 daemon 处理完成
    let _ = stream.read_to_string(&mut String::new());
}

/// 从 /dev/tty 读取用户确认（y/yes），绕过已被 hook payload 占用的 stdin。
///
/// 非 TTY 环境或 CHECKPOINT_ASSUME_NO_TTY 设置时默认返回 false。
fn read_yes_tty() -> bool {
    if checkpoint_core::env_compat::env_var_is_set(
        "AGENT_ASPECT_ASSUME_NO_TTY",
        "CHECKPOINT_ASSUME_NO_TTY",
    ) {
        return false;
    }

    let mut tty = match File::open("/dev/tty") {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut buf = [0u8; 64];
    if let Ok(n) = tty.read(&mut buf) {
        let s = std::str::from_utf8(&buf[..n]).unwrap_or("");
        s.trim().eq_ignore_ascii_case("y") || s.trim().eq_ignore_ascii_case("yes")
    } else {
        false
    }
}

/// 从 stdin 读取全部内容（hook payload）。
fn read_stdin() -> String {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .unwrap_or_default();
    buf
}
