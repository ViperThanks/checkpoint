//! Provider hook payload 归一化 — 将各 provider 的 JSON 转为 `UnifiedEvent`。
//!
//! 每个 provider 的 PreToolUse hook 发送不同的 JSON 结构。
//! 这些函数将原始 payload 转换为统一的 `UnifiedEvent`。
//! adapter.rs 中的具体适配器委托给这些函数。

use crate::error::{AgentAspectError, AgentAspectResult};
use crate::event::{AgentId, Phase, ToolInput, UnifiedEvent};
use crate::wire::ClaudeHookPayload;

/// Claude Code PreToolUse → UnifiedEvent
pub fn normalize_claude_pre_tool_use(raw: &str) -> AgentAspectResult<UnifiedEvent> {
    normalize_pre_tool_use(raw, AgentId::ClaudeCode)
}

/// Codex CLI PreToolUse → UnifiedEvent
/// Codex payload 顶层结构与 Claude 同构（额外字段 turn_id / model / permission_mode
/// 等被 serde 忽略），可直接复用同一解码路径。
pub fn normalize_codex_pre_tool_use(raw: &str) -> AgentAspectResult<UnifiedEvent> {
    normalize_pre_tool_use(raw, AgentId::CodexCli)
}

/// Kimi Code PreToolUse → UnifiedEvent
/// Kimi payload 顶层结构与 Claude 同构（session_id, cwd, tool_name, tool_input, tool_call_id），
/// 但工具名和 tool_input 字段有差异：
/// - Write → WriteFile, tool_input.path (not file_path)
/// - Edit → StrReplaceFile, tool_input.edit.old / edit.new (not old_string / new_string)
/// - Shell → Shell (not Bash)
/// 基于 2026-04-24 runtime 实验验证。
pub fn normalize_kimi_pre_tool_use(raw: &str) -> AgentAspectResult<UnifiedEvent> {
    normalize_pre_tool_use(raw, AgentId::KimiCode)
}

/// Gemini CLI BeforeTool → UnifiedEvent
/// Gemini payload 顶层结构与 Claude 同构（tool_name, tool_input），
/// 内置工具名：read_file / write_file / run_shell_command。
/// Gemini hook event name is "BeforeTool" (not "PreToolUse").
/// 基于 DOC+SOURCE 证据，待本机 runtime 实验确认。
pub fn normalize_gemini_pre_tool_use(raw: &str) -> AgentAspectResult<UnifiedEvent> {
    let payload: ClaudeHookPayload =
        serde_json::from_str(raw).map_err(AgentAspectError::ParsePayload)?;

    let hook_event = payload.hook_event_name.unwrap_or_default();
    if hook_event != "BeforeTool" {
        return Err(AgentAspectError::UnsupportedHookEvent(hook_event));
    }

    let tool_name = payload.tool_name.unwrap_or_default();
    let ti = parse_tool_input(&tool_name, &payload.tool_input);

    Ok(UnifiedEvent::new_for_agent(
        AgentId::GeminiCli,
        Phase::Before,
        "tool.request",
        &tool_name,
        ti,
    ))
}

/// 通用 PreToolUse 归一化：解析 payload、校验 hook event、提取工具输入。
fn normalize_pre_tool_use(raw: &str, agent: AgentId) -> AgentAspectResult<UnifiedEvent> {
    let payload: ClaudeHookPayload =
        serde_json::from_str(raw).map_err(AgentAspectError::ParsePayload)?;

    let tool_name = payload.tool_name.unwrap_or_default();
    let hook_event = payload.hook_event_name.unwrap_or_default();

    if hook_event != "PreToolUse" {
        return Err(AgentAspectError::UnsupportedHookEvent(hook_event));
    }

    let ti = parse_tool_input(&tool_name, &payload.tool_input);

    Ok(UnifiedEvent::new_for_agent(
        agent,
        Phase::Before,
        "tool.request",
        &tool_name,
        ti,
    ))
}

/// 按工具名从 JSON Value 提取关键字段到 ToolInput。
/// 不同 provider 对同一操作使用不同字段名（如 Kimi 用 path 而非 file_path）。
fn parse_tool_input(tool: &str, raw: &serde_json::Value) -> ToolInput {
    match tool {
        "Bash" | "Shell" => ToolInput {
            command: raw["command"].as_str().map(|s| s.to_string()),
            file_path: None,
            old_string: None,
            new_string: None,
            content: None,
            replace_all: None,
        },
        "Edit" => ToolInput {
            command: None,
            file_path: raw["file_path"].as_str().map(|s| s.to_string()),
            old_string: raw["old_string"].as_str().map(|s| s.to_string()),
            new_string: raw["new_string"].as_str().map(|s| s.to_string()),
            content: None,
            replace_all: raw["replace_all"].as_bool(),
        },
        "Write" => ToolInput {
            command: None,
            file_path: raw["file_path"].as_str().map(|s| s.to_string()),
            old_string: None,
            new_string: None,
            content: raw["content"].as_str().map(|s| s.to_string()),
            replace_all: None,
        },
        // Kimi Code uses different tool names for file operations
        "WriteFile" => ToolInput {
            command: None,
            file_path: raw["path"].as_str().map(|s| s.to_string()),
            old_string: None,
            new_string: None,
            content: raw["content"].as_str().map(|s| s.to_string()),
            replace_all: None,
        },
        "StrReplaceFile" => ToolInput {
            command: None,
            file_path: raw["path"].as_str().map(|s| s.to_string()),
            old_string: raw["edit"]["old"].as_str().map(|s| s.to_string()),
            new_string: raw["edit"]["new"].as_str().map(|s| s.to_string()),
            content: None,
            replace_all: None,
        },
        // Gemini CLI tool names
        "write_file" => ToolInput {
            command: None,
            file_path: raw["file_path"].as_str().map(|s| s.to_string()),
            old_string: None,
            new_string: None,
            content: raw["content"].as_str().map(|s| s.to_string()),
            replace_all: None,
        },
        "run_shell_command" => ToolInput {
            command: raw["command"].as_str().map(|s| s.to_string()),
            file_path: None,
            old_string: None,
            new_string: None,
            content: None,
            replace_all: None,
        },
        _ => ToolInput {
            command: None,
            file_path: None,
            old_string: None,
            new_string: None,
            content: None,
            replace_all: None,
        },
    }
}
