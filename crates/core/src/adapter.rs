//! Provider 适配器 — 将各 AI agent 的 hook 差异抽象为统一 trait。
//!
//! 每个 provider 有自己的 payload 格式、工具命名、transcript 结构。
//! `AgentAdapter` trait 定义了 normalize（输入归一化）和 format_response（输出封装）。
//! `AgentId::adapter()` 按枚举变体返回对应实现。

use crate::error::AgentAspectResult;
use crate::event::{AgentId, UnifiedEvent};
use crate::wire::HookResponse;

/// Contract for integrating a new AI agent into Agent Aspect.
///
/// Each supported agent implements this trait to define:
/// 1. How to detect its hook payloads (normalize)
/// 2. How to format deny/ask responses it understands (response envelope)
///
/// Current implementations live in `normalize.rs` as standalone functions.
/// As agents grow in complexity (e.g., Gemini's BeforeTool vs PreToolUse),
/// migrate each into a named impl of this trait.
///
/// # Usage in daemon
///
/// ```ignore
/// let adapter = agent.adapter();
/// let event = adapter.normalize(raw_payload)?;
/// // ... rule evaluation ...
/// if let Some(resp) = adapter.format_response(action, note) {
///     println!("{}", serde_json::to_string(&resp).unwrap());
/// }
/// ```
///
/// # Adding a new agent
///
/// 1. Add variant to `AgentId` enum in `event.rs`
/// 2. Implement `AgentAdapter` for that agent
/// 3. Register in `AgentId::adapter()` below
/// 4. Add detection heuristic in `hook-cli/src/main.rs`
/// 5. Route in `daemon/src/main.rs`
pub trait AgentAdapter: Send + Sync {
    /// Which agent this adapter handles.
    fn agent_id(&self) -> AgentId;

    /// The hook event name this agent emits (e.g., "PreToolUse", "BeforeTool").
    fn hook_event_name(&self) -> &'static str;

    /// Parse a raw hook payload into a normalized UnifiedEvent.
    fn normalize(&self, raw_payload: &str) -> AgentAspectResult<UnifiedEvent>;

    /// Format a deny/ask response the agent understands.
    /// Returns None for allow (no output needed).
    fn format_response(
        &self,
        action: crate::decision::Action,
        note: String,
    ) -> Option<HookResponse>;
}

// ---------------------------------------------------------------------------
// Concrete adapters (inline for now, split to files when they grow)
// ---------------------------------------------------------------------------

struct ClaudeCodeAdapter;
struct CodexCliAdapter;
struct KimiCodeAdapter;
struct GeminiCliAdapter;

impl AgentAdapter for ClaudeCodeAdapter {
    fn agent_id(&self) -> AgentId {
        AgentId::ClaudeCode
    }

    fn hook_event_name(&self) -> &'static str {
        "PreToolUse"
    }

    fn normalize(&self, raw_payload: &str) -> AgentAspectResult<UnifiedEvent> {
        crate::normalize::normalize_claude_pre_tool_use(raw_payload)
    }

    fn format_response(
        &self,
        action: crate::decision::Action,
        note: String,
    ) -> Option<HookResponse> {
        HookResponse::from_action_and_event(action, note, self.hook_event_name())
    }
}

impl AgentAdapter for CodexCliAdapter {
    fn agent_id(&self) -> AgentId {
        AgentId::CodexCli
    }

    fn hook_event_name(&self) -> &'static str {
        "PreToolUse"
    }

    fn normalize(&self, raw_payload: &str) -> AgentAspectResult<UnifiedEvent> {
        crate::normalize::normalize_codex_pre_tool_use(raw_payload)
    }

    fn format_response(
        &self,
        action: crate::decision::Action,
        note: String,
    ) -> Option<HookResponse> {
        HookResponse::from_action_and_event(action, note, self.hook_event_name())
    }
}

impl AgentAdapter for KimiCodeAdapter {
    fn agent_id(&self) -> AgentId {
        AgentId::KimiCode
    }

    fn hook_event_name(&self) -> &'static str {
        "PreToolUse"
    }

    fn normalize(&self, raw_payload: &str) -> AgentAspectResult<UnifiedEvent> {
        crate::normalize::normalize_kimi_pre_tool_use(raw_payload)
    }

    fn format_response(
        &self,
        action: crate::decision::Action,
        note: String,
    ) -> Option<HookResponse> {
        HookResponse::from_action_and_event(action, note, self.hook_event_name())
    }
}

impl AgentAdapter for GeminiCliAdapter {
    fn agent_id(&self) -> AgentId {
        AgentId::GeminiCli
    }

    fn hook_event_name(&self) -> &'static str {
        "BeforeTool"
    }

    fn normalize(&self, raw_payload: &str) -> AgentAspectResult<UnifiedEvent> {
        crate::normalize::normalize_gemini_pre_tool_use(raw_payload)
    }

    fn format_response(
        &self,
        action: crate::decision::Action,
        note: String,
    ) -> Option<HookResponse> {
        HookResponse::from_action_and_event(action, note, self.hook_event_name())
    }
}

impl AgentId {
    /// 返回该 agent 的适配器实例。未注册运行时验证的 agent 返回 None。
    pub fn adapter(&self) -> Option<Box<dyn AgentAdapter>> {
        match self {
            AgentId::ClaudeCode => Some(Box::new(ClaudeCodeAdapter)),
            AgentId::CodexCli => Some(Box::new(CodexCliAdapter)),
            AgentId::KimiCode => Some(Box::new(KimiCodeAdapter)),
            AgentId::GeminiCli => Some(Box::new(GeminiCliAdapter)),
            _ => None,
        }
    }
}
