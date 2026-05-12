//! Transcript 解析 — 从 provider 的 JSONL transcript 文件中读取聊天消息。
//!
//! 支持 Claude Code、Codex CLI、Kimi Code 三种格式。
//! 提供两种读取模式：
//! - 全量读取（read_transcript）：一次性解析整个文件
//! - 逐行解析（parse_transcript_line）：配合 transcript_sync 增量同步
//!
//! 输出统一的 TranscriptMessage 结构，按 role 区分 user / assistant / tool_summary。

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TranscriptMessage {
    pub role: String,
    pub timestamp: Option<String>,
    pub text: String,
    pub source: String,
    pub turn_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input_preview: Option<String>,
    pub tool_input_full: Option<String>,
    pub thinking: Option<String>,
}

/// 全量读取 transcript，返回解析后的消息列表。文件不存在或为空时返回 None。
pub fn read_transcript(
    agent: &str,
    conversation_id: &str,
    project_path: Option<&str>,
    transcript_path: Option<&str>,
) -> Option<Vec<TranscriptMessage>> {
    match agent {
        "claude_code" => read_claude_transcript(conversation_id, project_path, transcript_path),
        "codex_cli" => read_codex_transcript(transcript_path),
        "kimi_code" => read_kimi_transcript(conversation_id),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Claude Code
// ---------------------------------------------------------------------------

fn read_claude_transcript(
    session_id: &str,
    project_path: Option<&str>,
    transcript_path: Option<&str>,
) -> Option<Vec<TranscriptMessage>> {
    let file_path = resolve_claude_transcript_path(session_id, project_path, transcript_path)?;
    let content = std::fs::read_to_string(&file_path).ok()?;
    let mut messages = Vec::new();

    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let line_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match line_type {
            "user" => {
                if let Some(text) = extract_claude_user_text(&v) {
                    if !text.is_empty()
                        && !text.starts_with("<local-command-caveat>")
                        && !text.starts_with("<command-name>")
                    {
                        messages.push(TranscriptMessage {
                            role: "user".into(),
                            timestamp: None,
                            text,
                            source: "transcript".into(),
                            turn_id: None,
                            tool_name: None,
                            tool_input_preview: None,
                            tool_input_full: None,
                            thinking: None,
                        });
                    }
                }
            }
            "assistant" => {
                extract_claude_assistant_blocks(&v, &mut messages);
            }
            _ => {}
        }
    }

    if messages.is_empty() {
        None
    } else {
        Some(messages)
    }
}

/// 解析 Claude Code transcript 路径。
///
/// 优先使用 conversations 表中持久化的 transcript_path；老数据没有该字段时，
/// 再按 project_path 映射到 `~/.claude/projects/{encoded}/{session}.jsonl`。
fn resolve_claude_transcript_path(
    session_id: &str,
    project_path: Option<&str>,
    transcript_path: Option<&str>,
) -> Option<std::path::PathBuf> {
    if let Some(path) = transcript_path {
        let path = std::path::PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    let pp = project_path?;
    let dir = crate::utils::claude_project_dir(pp)?;
    let path = dir.join(format!("{session_id}.jsonl"));
    if path.exists() { Some(path) } else { None }
}

/// 从 Claude user 消息中提取文本（支持字符串和 content block 数组）。
fn extract_claude_user_text(v: &serde_json::Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    extract_text_from_content(content)
}

/// 从 Claude assistant 消息的 content blocks 中提取 thinking、text 和 tool_use。
/// thinking 块累积后附带到下一个 assistant/tool_summary 消息上。
/// text 块会合并直到遇到 tool_use，然后分别输出为独立消息。
fn extract_claude_assistant_blocks(v: &serde_json::Value, messages: &mut Vec<TranscriptMessage>) {
    let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
        return;
    };
    let Some(blocks) = content.as_array() else {
        return;
    };

    let mut text_buf = String::new();
    let mut thinking_buf: Option<String> = None;

    // flush accumulated text+thinking as an assistant message
    let flush_assistant = |text_buf: &mut String,
                           thinking_buf: &mut Option<String>,
                           messages: &mut Vec<TranscriptMessage>| {
        if !text_buf.is_empty() || thinking_buf.is_some() {
            messages.push(TranscriptMessage {
                role: "assistant".into(),
                timestamp: None,
                text: std::mem::take(text_buf),
                source: "transcript".into(),
                turn_id: None,
                tool_name: None,
                tool_input_preview: None,
                tool_input_full: None,
                thinking: thinking_buf.take(),
            });
        }
    };

    for block in blocks {
        let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match btype {
            "thinking" => {
                if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        thinking_buf = Some(match thinking_buf.take() {
                            Some(existing) => existing + "\n" + text,
                            None => text.to_string(),
                        });
                    }
                }
            }
            "text" => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text);
                    }
                }
            }
            "tool_use" => {
                flush_assistant(&mut text_buf, &mut thinking_buf, messages);
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let input_val = block
                    .get("input")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let input_preview = if input_val.is_object() {
                    Some(format_tool_summary(name, &input_val))
                } else {
                    None
                };
                let input_full = Some(truncate_str(
                    &serde_json::to_string(&input_val).unwrap_or_default(),
                    crate::constants::TOOL_INPUT_FULL_LEN,
                ));
                messages.push(TranscriptMessage {
                    role: "tool_summary".into(),
                    timestamp: None,
                    text: String::new(),
                    source: "transcript".into(),
                    turn_id: None,
                    tool_name: Some(name.to_string()),
                    tool_input_preview: input_preview,
                    tool_input_full: input_full,
                    thinking: None,
                });
            }
            _ => {}
        }
    }

    flush_assistant(&mut text_buf, &mut thinking_buf, messages);
}

// ---------------------------------------------------------------------------
// Codex CLI
// ---------------------------------------------------------------------------

fn read_codex_transcript(transcript_path: Option<&str>) -> Option<Vec<TranscriptMessage>> {
    let path = transcript_path?;
    let content = std::fs::read_to_string(path).ok()?;
    let mut messages = Vec::new();

    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let line_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match line_type {
            "event_msg" => {
                let Some(payload) = v.get("payload") else {
                    continue;
                };
                let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

                match ptype {
                    "user_message" => {
                        if let Some(text) = payload.get("message").and_then(|m| m.as_str()) {
                            if !text.is_empty() {
                                messages.push(TranscriptMessage {
                                    role: "user".into(),
                                    timestamp: v
                                        .get("timestamp")
                                        .and_then(|t| t.as_str())
                                        .map(String::from),
                                    text: text.to_string(),
                                    source: "transcript".into(),
                                    turn_id: None,
                                    tool_name: None,
                                    tool_input_preview: None,
                                    tool_input_full: None,
                                    thinking: None,
                                });
                            }
                        }
                    }
                    "agent_message" => {
                        if let Some(text) = payload.get("message").and_then(|m| m.as_str()) {
                            if !text.is_empty() {
                                messages.push(TranscriptMessage {
                                    role: "assistant".into(),
                                    timestamp: v
                                        .get("timestamp")
                                        .and_then(|t| t.as_str())
                                        .map(String::from),
                                    text: text.to_string(),
                                    source: "transcript".into(),
                                    turn_id: None,
                                    tool_name: None,
                                    tool_input_preview: None,
                                    tool_input_full: None,
                                    thinking: None,
                                });
                            }
                        }
                    }
                    "reasoning" | "reasoning_summary" => {
                        if let Some(text) = payload
                            .get("summary")
                            .and_then(|s| s.as_str())
                            .or_else(|| payload.get("text").and_then(|s| s.as_str()))
                        {
                            if !text.is_empty() {
                                messages.push(TranscriptMessage {
                                    role: "assistant".into(),
                                    timestamp: v
                                        .get("timestamp")
                                        .and_then(|t| t.as_str())
                                        .map(String::from),
                                    text: String::new(),
                                    source: "transcript".into(),
                                    turn_id: None,
                                    tool_name: None,
                                    tool_input_preview: None,
                                    tool_input_full: None,
                                    thinking: Some(text.to_string()),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                let Some(payload) = v.get("payload") else {
                    continue;
                };
                let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

                if ptype == "function_call" {
                    let name = payload
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let input_str = payload
                        .get("arguments")
                        .and_then(|a| a.as_str())
                        .map(|s| s.to_string());
                    let input_preview = input_str
                        .as_ref()
                        .map(|s| format_tool_summary(name, &parse_tool_input(s)));
                    let input_full =
                        input_str.map(|s| truncate_str(&s, crate::constants::TOOL_INPUT_FULL_LEN));
                    messages.push(TranscriptMessage {
                        role: "tool_summary".into(),
                        timestamp: v
                            .get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(String::from),
                        text: String::new(),
                        source: "transcript".into(),
                        turn_id: None,
                        tool_name: Some(name.to_string()),
                        tool_input_preview: input_preview,
                        tool_input_full: input_full,
                        thinking: None,
                    });
                } else if ptype == "custom_tool_call" {
                    let name = payload
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("custom_tool");
                    let input_str = payload
                        .get("input")
                        .and_then(|a| a.as_str())
                        .map(|s| s.to_string());
                    let input_preview = input_str
                        .as_ref()
                        .map(|s| truncate_str(s, crate::constants::TOOL_INPUT_PREVIEW_LEN));
                    let input_full =
                        input_str.map(|s| truncate_str(&s, crate::constants::TOOL_INPUT_FULL_LEN));
                    messages.push(TranscriptMessage {
                        role: "tool_summary".into(),
                        timestamp: v
                            .get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(String::from),
                        text: String::new(),
                        source: "transcript".into(),
                        turn_id: None,
                        tool_name: Some(name.to_string()),
                        tool_input_preview: input_preview,
                        tool_input_full: input_full,
                        thinking: None,
                    });
                } else if ptype == "web_search_call" {
                    let query = payload
                        .get("action")
                        .and_then(|a| a.get("query"))
                        .and_then(|q| q.as_str())
                        .unwrap_or("web search");
                    messages.push(TranscriptMessage {
                        role: "tool_summary".into(),
                        timestamp: v
                            .get("timestamp")
                            .and_then(|t| t.as_str())
                            .map(String::from),
                        text: String::new(),
                        source: "transcript".into(),
                        turn_id: None,
                        tool_name: Some("web_search".to_string()),
                        tool_input_preview: Some(truncate_str(
                            query,
                            crate::constants::TOOL_INPUT_PREVIEW_LEN,
                        )),
                        tool_input_full: Some(truncate_str(
                            query,
                            crate::constants::TOOL_INPUT_FULL_LEN,
                        )),
                        thinking: None,
                    });
                }
            }
            _ => {}
        }
    }

    if messages.is_empty() {
        None
    } else {
        Some(messages)
    }
}

// ---------------------------------------------------------------------------
// Kimi Code
// ---------------------------------------------------------------------------

fn read_kimi_transcript(session_id: &str) -> Option<Vec<TranscriptMessage>> {
    let home = std::env::var("HOME").unwrap_or_default();
    let sessions_dir = format!("{home}/.kimi/sessions");

    for entry in std::fs::read_dir(&sessions_dir).ok()?.flatten() {
        let session_path = entry.path().join(session_id);
        let wire_path = session_path.join("wire.jsonl");
        if !wire_path.exists() {
            continue;
        }

        let content = std::fs::read_to_string(&wire_path).ok()?;
        let mut messages = Vec::new();

        for line in content.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };

            if v.get("type").and_then(|t| t.as_str()) == Some("metadata") {
                continue;
            }

            let Some(msg) = v.get("message") else {
                continue;
            };
            let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let timestamp = v.get("timestamp").and_then(|t| t.as_f64()).map(|ts| {
                let secs = ts as i64;
                let nsecs = ((ts - secs as f64) * 1_000_000_000.0) as u32;
                chrono::DateTime::from_timestamp(secs, nsecs)
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default()
            });

            match msg_type {
                "TurnBegin" => {
                    if let Some(inputs) = msg
                        .get("payload")
                        .and_then(|p| p.get("user_input"))
                        .and_then(|u| u.as_array())
                    {
                        let texts: Vec<&str> = inputs
                            .iter()
                            .filter_map(|i| {
                                if i.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    i.get("text").and_then(|t| t.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect();
                        let text = texts.join("\n");
                        if !text.is_empty() {
                            messages.push(TranscriptMessage {
                                role: "user".into(),
                                timestamp: timestamp.clone(),
                                text,
                                source: "transcript".into(),
                                turn_id: None,
                                tool_name: None,
                                tool_input_preview: None,
                                tool_input_full: None,
                                thinking: None,
                            });
                        }
                    }
                }
                "ContentPart" => {
                    let payload = msg.get("payload").unwrap();
                    let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if ptype == "text" {
                        if let Some(text) = payload.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                messages.push(TranscriptMessage {
                                    role: "assistant".into(),
                                    timestamp: timestamp.clone(),
                                    text: text.to_string(),
                                    source: "transcript".into(),
                                    turn_id: None,
                                    tool_name: None,
                                    tool_input_preview: None,
                                    tool_input_full: None,
                                    thinking: None,
                                });
                            }
                        }
                    }
                }
                "ToolCall" => {
                    if let Some(func) = msg.get("payload").and_then(|p| p.get("function")) {
                        let name = func
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let input_str = func
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string());
                        let input_preview = input_str
                            .as_ref()
                            .map(|s| format_tool_summary(name, &parse_tool_input(s)));
                        let input_full = input_str
                            .map(|s| truncate_str(&s, crate::constants::TOOL_INPUT_FULL_LEN));
                        messages.push(TranscriptMessage {
                            role: "tool_summary".into(),
                            timestamp: timestamp.clone(),
                            text: String::new(),
                            source: "transcript".into(),
                            turn_id: None,
                            tool_name: Some(name.to_string()),
                            tool_input_preview: input_preview,
                            tool_input_full: input_full,
                            thinking: None,
                        });
                    }
                }
                _ => {}
            }
        }

        if messages.is_empty() {
            return None;
        }
        return Some(messages);
    }

    None
}

// ---------------------------------------------------------------------------
// Public per-line parsing (for transcript_sync incremental sync)
// ---------------------------------------------------------------------------

/// Resolve the transcript JSONL file path for a conversation.
pub fn resolve_transcript_path(
    agent: &str,
    conversation_id: &str,
    project_path: Option<&str>,
    transcript_path: Option<&str>,
) -> Option<std::path::PathBuf> {
    match agent {
        "claude_code" => {
            resolve_claude_transcript_path(conversation_id, project_path, transcript_path)
        }
        "codex_cli" => {
            let path = std::path::PathBuf::from(transcript_path?);
            if path.exists() { Some(path) } else { None }
        }
        "kimi_code" => {
            let home = std::env::var("HOME").unwrap_or_default();
            let sessions_dir = format!("{home}/.kimi/sessions");
            if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
                for entry in entries.flatten() {
                    let wire_path = entry.path().join(conversation_id).join("wire.jsonl");
                    if wire_path.exists() {
                        return Some(wire_path);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Parse a single JSONL line into zero or more TranscriptMessages.
/// Dispatches by agent type.
///
/// Returns `Ok(messages)` on success (messages may be empty for metadata/unknown events).
/// Returns `Err(msg)` if the line is not valid JSON.
pub fn parse_transcript_line(agent: &str, line: &str) -> Result<Vec<TranscriptMessage>, String> {
    match agent {
        "claude_code" => parse_claude_line(line),
        "codex_cli" => parse_codex_line(line),
        "kimi_code" => parse_kimi_line(line),
        _ => Ok(Vec::new()),
    }
}

/// Parse a single Claude Code JSONL line.
pub fn parse_claude_line(line: &str) -> Result<Vec<TranscriptMessage>, String> {
    let mut messages = Vec::new();
    let v =
        serde_json::from_str::<serde_json::Value>(line).map_err(|e| format!("bad JSON: {e}"))?;
    let line_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match line_type {
        "user" => {
            if let Some(text) = extract_claude_user_text(&v) {
                if !text.is_empty()
                    && !text.starts_with("<local-command-caveat>")
                    && !text.starts_with("<command-name>")
                {
                    messages.push(TranscriptMessage {
                        role: "user".into(),
                        timestamp: None,
                        text,
                        source: "transcript".into(),
                        turn_id: None,
                        tool_name: None,
                        tool_input_preview: None,
                        tool_input_full: None,
                        thinking: None,
                    });
                }
            }
        }
        "assistant" => {
            extract_claude_assistant_blocks(&v, &mut messages);
        }
        _ => {}
    }
    Ok(messages)
}

/// Parse a single Codex CLI JSONL line.
pub fn parse_codex_line(line: &str) -> Result<Vec<TranscriptMessage>, String> {
    let mut messages = Vec::new();
    let v =
        serde_json::from_str::<serde_json::Value>(line).map_err(|e| format!("bad JSON: {e}"))?;
    let line_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match line_type {
        "event_msg" => {
            let Some(payload) = v.get("payload") else {
                return Ok(messages);
            };
            let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

            match ptype {
                "user_message" => {
                    if let Some(text) = payload.get("message").and_then(|m| m.as_str()) {
                        if !text.is_empty() {
                            messages.push(TranscriptMessage {
                                role: "user".into(),
                                timestamp: v
                                    .get("timestamp")
                                    .and_then(|t| t.as_str())
                                    .map(String::from),
                                text: text.to_string(),
                                source: "transcript".into(),
                                turn_id: None,
                                tool_name: None,
                                tool_input_preview: None,
                                tool_input_full: None,
                                thinking: None,
                            });
                        }
                    }
                }
                "agent_message" => {
                    if let Some(text) = payload.get("message").and_then(|m| m.as_str()) {
                        if !text.is_empty() {
                            messages.push(TranscriptMessage {
                                role: "assistant".into(),
                                timestamp: v
                                    .get("timestamp")
                                    .and_then(|t| t.as_str())
                                    .map(String::from),
                                text: text.to_string(),
                                source: "transcript".into(),
                                turn_id: None,
                                tool_name: None,
                                tool_input_preview: None,
                                tool_input_full: None,
                                thinking: None,
                            });
                        }
                    }
                }
                "reasoning" | "reasoning_summary" => {
                    if let Some(text) = payload
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .or_else(|| payload.get("text").and_then(|s| s.as_str()))
                    {
                        if !text.is_empty() {
                            messages.push(TranscriptMessage {
                                role: "assistant".into(),
                                timestamp: v
                                    .get("timestamp")
                                    .and_then(|t| t.as_str())
                                    .map(String::from),
                                text: String::new(),
                                source: "transcript".into(),
                                turn_id: None,
                                tool_name: None,
                                tool_input_preview: None,
                                tool_input_full: None,
                                thinking: Some(text.to_string()),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        "response_item" => {
            let Some(payload) = v.get("payload") else {
                return Ok(messages);
            };
            let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");

            if ptype == "function_call" {
                let name = payload
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let input_str = payload
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string());
                let input_preview = input_str
                    .as_ref()
                    .map(|s| format_tool_summary(name, &parse_tool_input(s)));
                let input_full =
                    input_str.map(|s| truncate_str(&s, crate::constants::TOOL_INPUT_FULL_LEN));
                messages.push(TranscriptMessage {
                    role: "tool_summary".into(),
                    timestamp: v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .map(String::from),
                    text: String::new(),
                    source: "transcript".into(),
                    turn_id: None,
                    tool_name: Some(name.to_string()),
                    tool_input_preview: input_preview,
                    tool_input_full: input_full,
                    thinking: None,
                });
            } else if ptype == "custom_tool_call" {
                let name = payload
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("custom_tool");
                let input_str = payload
                    .get("input")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string());
                let input_preview = input_str
                    .as_ref()
                    .map(|s| truncate_str(s, crate::constants::TOOL_INPUT_PREVIEW_LEN));
                let input_full =
                    input_str.map(|s| truncate_str(&s, crate::constants::TOOL_INPUT_FULL_LEN));
                messages.push(TranscriptMessage {
                    role: "tool_summary".into(),
                    timestamp: v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .map(String::from),
                    text: String::new(),
                    source: "transcript".into(),
                    turn_id: None,
                    tool_name: Some(name.to_string()),
                    tool_input_preview: input_preview,
                    tool_input_full: input_full,
                    thinking: None,
                });
            } else if ptype == "web_search_call" {
                let query = payload
                    .get("action")
                    .and_then(|a| a.get("query"))
                    .and_then(|q| q.as_str())
                    .unwrap_or("web search");
                messages.push(TranscriptMessage {
                    role: "tool_summary".into(),
                    timestamp: v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .map(String::from),
                    text: String::new(),
                    source: "transcript".into(),
                    turn_id: None,
                    tool_name: Some("web_search".to_string()),
                    tool_input_preview: Some(truncate_str(
                        query,
                        crate::constants::TOOL_INPUT_PREVIEW_LEN,
                    )),
                    tool_input_full: Some(truncate_str(
                        query,
                        crate::constants::TOOL_INPUT_FULL_LEN,
                    )),
                    thinking: None,
                });
            }
        }
        _ => {}
    }
    Ok(messages)
}

/// Parse a single Kimi Code JSONL line.
pub fn parse_kimi_line(line: &str) -> Result<Vec<TranscriptMessage>, String> {
    let mut messages = Vec::new();
    let v =
        serde_json::from_str::<serde_json::Value>(line).map_err(|e| format!("bad JSON: {e}"))?;

    if v.get("type").and_then(|t| t.as_str()) == Some("metadata") {
        return Ok(messages);
    }

    let Some(msg) = v.get("message") else {
        return Ok(messages);
    };
    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let timestamp = v.get("timestamp").and_then(|t| t.as_f64()).map(|ts| {
        let secs = ts as i64;
        let nsecs = ((ts - secs as f64) * 1_000_000_000.0) as u32;
        chrono::DateTime::from_timestamp(secs, nsecs)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default()
    });

    match msg_type {
        "TurnBegin" => {
            if let Some(inputs) = msg
                .get("payload")
                .and_then(|p| p.get("user_input"))
                .and_then(|u| u.as_array())
            {
                let texts: Vec<&str> = inputs
                    .iter()
                    .filter_map(|i| {
                        if i.get("type").and_then(|t| t.as_str()) == Some("text") {
                            i.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect();
                let text = texts.join("\n");
                if !text.is_empty() {
                    messages.push(TranscriptMessage {
                        role: "user".into(),
                        timestamp: timestamp.clone(),
                        text,
                        source: "transcript".into(),
                        turn_id: None,
                        tool_name: None,
                        tool_input_preview: None,
                        tool_input_full: None,
                        thinking: None,
                    });
                }
            }
        }
        "ContentPart" => {
            let payload = msg.get("payload").unwrap();
            let ptype = payload.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if ptype == "text" {
                if let Some(text) = payload.get("text").and_then(|t| t.as_str()) {
                    if !text.is_empty() {
                        messages.push(TranscriptMessage {
                            role: "assistant".into(),
                            timestamp: timestamp.clone(),
                            text: text.to_string(),
                            source: "transcript".into(),
                            turn_id: None,
                            tool_name: None,
                            tool_input_preview: None,
                            tool_input_full: None,
                            thinking: None,
                        });
                    }
                }
            }
        }
        "ToolCall" => {
            if let Some(func) = msg.get("payload").and_then(|p| p.get("function")) {
                let name = func
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let input_str = func
                    .get("arguments")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string());
                let input_preview = input_str
                    .as_ref()
                    .map(|s| format_tool_summary(name, &parse_tool_input(s)));
                let input_full =
                    input_str.map(|s| truncate_str(&s, crate::constants::TOOL_INPUT_FULL_LEN));
                messages.push(TranscriptMessage {
                    role: "tool_summary".into(),
                    timestamp: timestamp.clone(),
                    text: String::new(),
                    source: "transcript".into(),
                    turn_id: None,
                    tool_name: Some(name.to_string()),
                    tool_input_preview: input_preview,
                    tool_input_full: input_full,
                    thinking: None,
                });
            }
        }
        _ => {}
    }
    Ok(messages)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract a short human-readable summary from a tool call input.
/// E.g. Grep with {"pattern":"fn foo","path":"/src"} → "fn foo /src"
fn format_tool_summary(tool_name: &str, input: &serde_json::Value) -> String {
    let obj = input.as_object();
    match tool_name {
        "Read" | "ReadFile" | "read_file" => obj
            .and_then(|o| {
                o.get("file_path")
                    .or_else(|| o.get("path"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "Grep" => {
            let pattern = string_field(obj, &["pattern", "query", "q"]).unwrap_or("");
            let path = string_field(obj, &["path", "include"]).unwrap_or("");
            if path.is_empty() {
                pattern.to_string()
            } else {
                format!("{pattern} {path}")
            }
        }
        "Glob" => obj
            .and_then(|o| string_field(Some(o), &["pattern", "path"]))
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "Bash" | "shell" | "Shell" | "exec_command" | "run_shell_command" => {
            string_field(obj, &["command", "cmd"])
                .map(|s| s.to_string())
                .unwrap_or_default()
        }
        "write_stdin" => string_field(obj, &["chars", "input", "stdin"])
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "WebSearch" | "web_search" | "search_query" | "image_query" => format_search_summary(input),
        "Write" | "Edit" | "StrReplaceFile" | "WriteFile" | "write_file" | "ApplyPatch" => obj
            .and_then(|o| {
                o.get("file_path")
                    .or_else(|| o.get("path"))
                    .and_then(|v| v.as_str())
            })
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "LS" | "List" => obj
            .and_then(|o| o.get("path").and_then(|v| v.as_str()))
            .or_else(|| obj.and_then(|o| o.get("directory").and_then(|v| v.as_str())))
            .map(|s| s.to_string())
            .unwrap_or_default(),
        "NotebookEdit" => obj
            .and_then(|o| o.get("notebook_path").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_default(),
        _ => truncate_str(
            &serde_json::to_string(input).unwrap_or_default(),
            crate::constants::TOOL_SUMMARY_PREVIEW_LEN,
        ),
    }
}

fn string_field<'a>(
    obj: Option<&'a serde_json::Map<String, serde_json::Value>>,
    keys: &[&str],
) -> Option<&'a str> {
    let obj = obj?;
    keys.iter()
        .find_map(|key| obj.get(*key).and_then(|v| v.as_str()))
}

fn format_search_summary(input: &serde_json::Value) -> String {
    if let Some(obj) = input.as_object() {
        if let Some(query) = string_field(Some(obj), &["q", "query", "search_query", "pattern"]) {
            return query.to_string();
        }
        for key in ["search_query", "image_query", "queries"] {
            if let Some(items) = obj.get(key).and_then(|v| v.as_array()) {
                let queries: Vec<&str> = items
                    .iter()
                    .filter_map(|item| {
                        item.get("q")
                            .or_else(|| item.get("query"))
                            .and_then(|v| v.as_str())
                    })
                    .collect();
                if !queries.is_empty() {
                    return truncate_str(
                        &queries.join(" · "),
                        crate::constants::TOOL_SUMMARY_PREVIEW_LEN,
                    );
                }
            }
        }
    }
    truncate_str(
        &serde_json::to_string(input).unwrap_or_default(),
        crate::constants::TOOL_SUMMARY_PREVIEW_LEN,
    )
}

/// Parse a JSON string argument into a Value, or return the raw string as a fallback.
fn parse_tool_input(raw: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(raw).unwrap_or(serde_json::json!(raw))
}

fn extract_text_from_content(content: &serde_json::Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let texts: Vec<String> = arr
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect();
        if !texts.is_empty() {
            return Some(texts.join("\n"));
        }
    }
    None
}

/// Byte-oriented truncation for tool inputs and summaries.
///
/// Unlike `crate::utils::truncate_str` which counts Unicode characters, this
/// function counts bytes for the initial check and uses `"..."` (three ASCII
/// dots) as the ellipsis. This is intentional: tool input previews and full
/// payloads are JSON strings where byte-level limits are more predictable and
/// the three-dot ellipsis is safe for any encoding.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .take_while(|(i, _)| *i < max_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max_len.min(s.len()));
        format!("{}...", &s[..end])
    }
}

#[derive(Debug, Clone, Default)]
pub struct ConvStats {
    pub token_count: i64,
    pub file_size_bytes: i64,
}

/// Compute rough token count and file size for a conversation's transcript.
/// Returns `None` when the transcript path cannot be resolved (caller should skip warming).
pub fn compute_stats(
    agent: &str,
    conversation_id: &str,
    project_path: Option<&str>,
    transcript_path: Option<&str>,
) -> Option<ConvStats> {
    let file_path = resolve_transcript_path(agent, conversation_id, project_path, transcript_path)?;
    let content = std::fs::read_to_string(&file_path).ok()?;
    let bytes = content.len() as i64;
    let token_count = estimate_tokens(&content);
    Some(ConvStats {
        token_count,
        file_size_bytes: bytes,
    })
}

fn estimate_tokens(text: &str) -> i64 {
    // Rough estimate: ~4 chars per token for Chinese/English mixed content
    (text.chars().count() as i64 / 4).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_codex_transcript_from_fixture() {
        let jsonl = r#"{"timestamp":"2026-04-26T02:45:02.771Z","type":"session_meta","payload":{"id":"sess-1","cwd":"/Users/test/proj"}}
{"timestamp":"2026-04-26T02:45:05.000Z","type":"event_msg","payload":{"type":"user_message","message":"fix the login bug"}}
{"timestamp":"2026-04-26T02:45:10.000Z","type":"event_msg","payload":{"type":"agent_message","message":"I'll look at the login code first."}}
{"timestamp":"2026-04-26T02:45:15.000Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"cat src/login.rs\"}","call_id":"c1"}}
{"timestamp":"2026-04-26T02:45:20.000Z","type":"event_msg","payload":{"type":"agent_message","message":"Found the issue in the auth module."}}
"#;
        let dir = std::env::temp_dir().join(format!("codex-transcript-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let msgs = read_codex_transcript(Some(path.to_str().unwrap())).unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "fix the login bug");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].text, "I'll look at the login code first.");
        assert_eq!(msgs[2].role, "tool_summary");
        assert_eq!(msgs[2].tool_name.as_deref(), Some("shell"));
        assert_eq!(msgs[3].role, "assistant");
        assert_eq!(msgs[3].text, "Found the issue in the auth module.");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn codex_exec_command_uses_cmd_as_preview() {
        let line = r#"{"timestamp":"2026-05-12T03:00:00Z","type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"git status --short && git log -1 --oneline\",\"workdir\":\"/tmp/proj\"}"}}"#;
        let msgs = parse_codex_line(line).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "tool_summary");
        assert_eq!(msgs[0].tool_name.as_deref(), Some("exec_command"));
        assert_eq!(
            msgs[0].tool_input_preview.as_deref(),
            Some("git status --short && git log -1 --oneline")
        );
    }

    #[test]
    fn codex_write_stdin_uses_chars_as_preview() {
        let line = r#"{"timestamp":"2026-05-12T03:00:00Z","type":"response_item","payload":{"type":"function_call","name":"write_stdin","arguments":"{\"session_id\":1,\"chars\":\"q\\n\"}"}}"#;
        let msgs = parse_codex_line(line).unwrap();
        assert_eq!(msgs[0].tool_input_preview.as_deref(), Some("q\n"));
    }

    #[test]
    fn search_query_array_uses_query_text_as_preview() {
        let preview = format_tool_summary(
            "search_query",
            &serde_json::json!({
                "search_query": [
                    { "q": "Agent Aspect WKWebView" },
                    { "q": "Codex CLI Full Access" }
                ]
            }),
        );
        assert_eq!(preview, "Agent Aspect WKWebView · Codex CLI Full Access");
    }

    #[test]
    fn read_claude_transcript_from_fixture() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"help me fix the bug"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me check the code."},{"type":"tool_use","id":"call_1","name":"Read","input":{"file_path":"/src/main.rs"}}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Found it, the issue is on line 5."}]}}
"#;
        let fake_project = format!("/tmp/claude-test-{}", std::process::id());
        let encoded = fake_project.replace('/', "-");
        let home = std::env::var("HOME").unwrap_or_default();
        let proj_dir = std::path::PathBuf::from(format!("{home}/.claude/projects/{encoded}"));
        std::fs::create_dir_all(&proj_dir).unwrap();
        let path = proj_dir.join("test-session.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let msgs = read_claude_transcript("test-session", Some(&fake_project), None).unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(msgs[0].text, "help me fix the bug");
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].text, "Let me check the code.");
        assert_eq!(msgs[2].role, "tool_summary");
        assert_eq!(msgs[2].tool_name.as_deref(), Some("Read"));
        assert_eq!(msgs[3].role, "assistant");
        assert_eq!(msgs[3].text, "Found it, the issue is on line 5.");
        std::fs::remove_dir_all(&proj_dir).unwrap();
    }

    #[test]
    fn read_nonexistent_returns_none() {
        assert_eq!(read_codex_transcript(Some("/nonexistent/path.jsonl")), None);
    }
}
