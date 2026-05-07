//! 线路协议 — provider hook 通信的请求/响应类型。
//!
//! 定义 Agent Aspect hook 与 AI provider hook 系统之间交换的数据结构。
//! 四种请求类型：Evaluate（规则评估）、Override（人工覆盖）、Metadata（会话元数据）、Stop（停止信号）。

use crate::decision::{Action, Decision};
use crate::event::AgentId;
use serde::{Deserialize, Serialize};

/// Claude Code hook 原始 payload — 其他 provider（Codex/Kimi/Gemini）也复用此结构。
#[derive(Debug, Deserialize)]
pub struct ClaudeHookPayload {
    pub hook_event_name: Option<String>,
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: serde_json::Value,
}

/// daemon 侧的统一请求类型，通过 serde tag dispatch 分发。
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireRequest {
    Evaluate {
        payload: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<AgentId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
    },
    Override {
        event_id: String,
        original_action: Action,
        final_action: Action,
        note: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
    },
    Metadata {
        payload: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<AgentId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
    },
    Stop {
        payload: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent: Option<AgentId>,
        #[serde(skip_serializing_if = "Option::is_none")]
        device_id: Option<String>,
    },
}

/// 独立的 Override 请求体（用于 HTTP API）。
#[derive(Debug, Serialize, Deserialize)]
pub struct OverrideRequest {
    pub event_id: String,
    pub original_action: Action,
    pub final_action: Action,
    pub note: String,
}

/// daemon 内部使用的响应结构，含 event_id + action + note。
#[derive(Debug, Serialize, Deserialize)]
pub struct WireResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    pub action: Action,
    pub note: String,
}

impl WireResponse {
    /// 从 Decision 生成响应。
    pub fn from_decision(decision: &Decision) -> Self {
        Self {
            event_id: Some(decision.event_id.clone()),
            action: decision.action,
            note: decision.note.clone(),
        }
    }

    /// 便捷构造 deny 响应。
    pub fn deny(note: impl Into<String>) -> Self {
        Self {
            event_id: None,
            action: Action::Deny,
            note: note.into(),
        }
    }
}

/// Hook 响应 — 输出到 stdout 供 agent 读取。
/// Claude Code、Codex CLI、Kimi Code 均验证过可接受 `{"hookSpecificOutput":{...}}` 格式。
#[derive(Debug, Serialize)]
pub struct HookResponse {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: HookSpecificOutput,
}

#[derive(Debug, Serialize)]
pub struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: &'static str,
    #[serde(rename = "permissionDecision")]
    pub permission_decision: Action,
    #[serde(rename = "permissionDecisionReason")]
    pub permission_decision_reason: String,
}

impl HookResponse {
    /// 将 action 转换为 HookResponse；allow 时返回 None（无需输出）。
    pub fn from_action(action: Action, note: impl Into<String>) -> Option<Self> {
        Self::from_action_and_event(action, note, "PreToolUse")
    }

    /// 指定 hook event name 的版本（Gemini 用 "BeforeTool"，其余用 "PreToolUse"）。
    pub fn from_action_and_event(
        action: Action,
        note: impl Into<String>,
        event_name: &'static str,
    ) -> Option<Self> {
        match action {
            Action::Deny | Action::Ask => Some(Self {
                hook_specific_output: HookSpecificOutput {
                    hook_event_name: event_name,
                    permission_decision: action,
                    permission_decision_reason: note.into(),
                },
            }),
            _ => None,
        }
    }
}
