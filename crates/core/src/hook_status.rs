//! Hook 状态查询 — 读取各 agent 的 hook 配置并汇总状态报告。
//!
//! 提供 `AgentHookStrategy` trait 和三家 agent 的具体实现，
//! 供 CLI `hooks status` 和 Bridge `GET /hook-status` 复用。

use crate::config::Config;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// hook 命令中搜索此标记来判断是否已安装。
const HOOK_MARKER: &str = "agent-aspect-hook";

/// 旧版 hook marker（reconcile 时需要清理）。
const LEGACY_HOOK_MARKER: &str = "checkpoint-hook";

/// 判断命令是否包含旧版 marker。
fn is_legacy_command(cmd: &str) -> bool {
    cmd.contains(LEGACY_HOOK_MARKER)
}

/// 所有 agent 应安装的事件列表。
const ALL_EVENTS: &[&str] = &["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"];

/// Agent 显示标签映射。
#[allow(dead_code)]
fn agent_label(agent: &str) -> &'static str {
    match agent {
        "claude_code" => "Claude Code",
        "codex_cli" => "Codex CLI",
        "kimi_code" => "Kimi Code",
        _ => "Unknown",
    }
}

// ── 状态类型 ──────────────────────────────────────────────────────────

/// 全局 hook 状态信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookGlobalInfo {
    pub pretooluse_enabled: bool,
    pub config_path: PathBuf,
    pub hook_binary_path: Option<PathBuf>,
}

/// 单个 agent 的 hook 状态报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookAgentStatus {
    pub agent: String,
    pub label: String,
    pub enabled: bool,
    pub pretooluse_enabled: bool,
    pub metadata_enabled: bool,
    pub stop_enabled: bool,
    pub config_path: PathBuf,
    pub config_exists: bool,
    pub installed_events: Vec<String>,
    pub missing_events: Vec<String>,
    pub commands: Vec<String>,
    /// 是否存在旧版 checkpoint-hook 残留。
    pub legacy_present: bool,
    /// "ok" | "disabled" | "partial" | "missing_config" | "missing_hook_binary"
    pub status: String,
}

/// 完整的 hook 状态响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookGlobalStatus {
    pub global: HookGlobalInfo,
    pub agents: Vec<HookAgentStatus>,
}

/// reconcile 操作结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub agent: String,
    pub action: String, // "added" | "removed" | "unchanged"
    pub events_added: Vec<String>,
    pub events_removed: Vec<String>,
}

// ── Strategy trait ───────────────────────────────────────────────────

/// Agent hook 配置策略 — 封装不同 agent 的配置文件格式差异。
pub trait AgentHookStrategy: Send + Sync {
    /// agent 标识（对应 AgentId::as_str()）。
    fn agent_id(&self) -> &str;
    /// agent 显示名。
    fn label(&self) -> &'static str;
    /// agent 配置文件路径。
    fn config_path(&self) -> PathBuf;
    /// 读取已安装的事件列表。
    fn read_installed_events(&self) -> Vec<String>;
    /// 读取已安装的 hook 命令。
    fn read_commands(&self) -> Vec<String>;
    /// 是否存在旧版 checkpoint-hook 残留。
    fn has_legacy_entries(&self) -> bool;
    /// 添加 hook 条目（reconcile add）。
    fn reconcile_add(&self, hook_binary: &str) -> Result<ReconcileReport, String>;
    /// 移除 hook 条目（reconcile remove）。
    fn reconcile_remove(&self) -> Result<ReconcileReport, String>;
}

// ── Claude JSON Strategy ─────────────────────────────────────────────

/// Claude Code hook 配置策略（~/.claude/settings.json）。
pub struct ClaudeJsonStrategy;

impl AgentHookStrategy for ClaudeJsonStrategy {
    fn agent_id(&self) -> &str {
        "claude_code"
    }

    fn label(&self) -> &'static str {
        "Claude Code"
    }

    fn config_path(&self) -> PathBuf {
        paths::claude_settings_path()
    }

    fn read_installed_events(&self) -> Vec<String> {
        read_json_installed_events(&self.config_path())
    }

    fn read_commands(&self) -> Vec<String> {
        read_json_commands(&self.config_path())
    }

    fn has_legacy_entries(&self) -> bool {
        json_has_legacy(&self.config_path())
    }

    fn reconcile_add(&self, hook_binary: &str) -> Result<ReconcileReport, String> {
        reconcile_json_add(&self.config_path(), "claude", hook_binary)
    }

    fn reconcile_remove(&self) -> Result<ReconcileReport, String> {
        reconcile_json_remove(&self.config_path(), "claude")
    }
}

// ── Codex JSON Strategy ──────────────────────────────────────────────

/// Codex CLI hook 配置策略（~/.codex/hooks.json）。
pub struct CodexJsonStrategy;

impl AgentHookStrategy for CodexJsonStrategy {
    fn agent_id(&self) -> &str {
        "codex_cli"
    }

    fn label(&self) -> &'static str {
        "Codex CLI"
    }

    fn config_path(&self) -> PathBuf {
        paths::codex_hooks_path()
    }

    fn read_installed_events(&self) -> Vec<String> {
        read_json_installed_events(&self.config_path())
    }

    fn read_commands(&self) -> Vec<String> {
        read_json_commands(&self.config_path())
    }

    fn has_legacy_entries(&self) -> bool {
        json_has_legacy(&self.config_path())
    }

    fn reconcile_add(&self, hook_binary: &str) -> Result<ReconcileReport, String> {
        reconcile_json_add(&self.config_path(), "codex", hook_binary)
    }

    fn reconcile_remove(&self) -> Result<ReconcileReport, String> {
        reconcile_json_remove(&self.config_path(), "codex")
    }
}

// ── Kimi TOML Strategy ───────────────────────────────────────────────

/// Kimi Code hook 配置策略（~/.kimi/config.toml）。
pub struct KimiTomlStrategy;

impl AgentHookStrategy for KimiTomlStrategy {
    fn agent_id(&self) -> &str {
        "kimi_code"
    }

    fn label(&self) -> &'static str {
        "Kimi Code"
    }

    fn config_path(&self) -> PathBuf {
        paths::kimi_config_path()
    }

    fn read_installed_events(&self) -> Vec<String> {
        read_kimi_installed_events(&self.config_path())
    }

    fn read_commands(&self) -> Vec<String> {
        read_kimi_commands(&self.config_path())
    }

    fn has_legacy_entries(&self) -> bool {
        kimi_has_legacy(&self.config_path())
    }

    fn reconcile_add(&self, hook_binary: &str) -> Result<ReconcileReport, String> {
        reconcile_kimi_add(&self.config_path(), hook_binary)
    }

    fn reconcile_remove(&self) -> Result<ReconcileReport, String> {
        reconcile_kimi_remove(&self.config_path())
    }
}

// ── 公开入口 ─────────────────────────────────────────────────────────

/// 返回所有策略实例。
pub fn strategies() -> Vec<Box<dyn AgentHookStrategy>> {
    vec![
        Box::new(ClaudeJsonStrategy),
        Box::new(CodexJsonStrategy),
        Box::new(KimiTomlStrategy),
    ]
}

/// 读取完整的 hook 状态报告。
pub fn read_full_status(config: &Config, hook_binary: Option<&PathBuf>) -> HookGlobalStatus {
    let config_path = Config::config_path();

    let global = HookGlobalInfo {
        pretooluse_enabled: config.pretooluse_enabled,
        config_path,
        hook_binary_path: hook_binary.cloned(),
    };

    let mut agents = Vec::new();
    for strategy in strategies() {
        let agent_id = strategy.agent_id();
        let agent_cfg = config.agent_hook_config(agent_id);
        let config_path = strategy.config_path();
        let config_exists = config_path.exists();

        let installed_events = strategy.read_installed_events();
        let missing_events: Vec<String> = ALL_EVENTS
            .iter()
            .filter(|e| !installed_events.contains(&e.to_string()))
            .map(|e| e.to_string())
            .collect();

        let commands = strategy.read_commands();
        let legacy_present = strategy.has_legacy_entries();

        let status = derive_status(
            agent_cfg.enabled,
            config_exists,
            &installed_events,
            &missing_events,
            hook_binary,
        );

        agents.push(HookAgentStatus {
            agent: agent_id.to_string(),
            label: strategy.label().to_string(),
            enabled: agent_cfg.enabled,
            pretooluse_enabled: agent_cfg.pretooluse_enabled,
            metadata_enabled: agent_cfg.metadata_enabled,
            stop_enabled: agent_cfg.stop_enabled,
            config_path,
            config_exists,
            installed_events,
            missing_events,
            commands,
            legacy_present,
            status,
        });
    }

    HookGlobalStatus { global, agents }
}

// ── 内部辅助 ─────────────────────────────────────────────────────────

/// 推导 agent hook 状态。
fn derive_status(
    enabled: bool,
    config_exists: bool,
    _installed: &[String],
    missing: &[String],
    hook_binary: Option<&PathBuf>,
) -> String {
    if !enabled {
        return "disabled".to_string();
    }
    if hook_binary.is_none() || !hook_binary.map(|p| p.exists()).unwrap_or(false) {
        return "missing_hook_binary".to_string();
    }
    if !config_exists {
        return "missing_config".to_string();
    }
    if !missing.is_empty() {
        return "partial".to_string();
    }
    "ok".to_string()
}

// ── JSON 读取 ────────────────────────────────────────────────────────

/// 从 JSON 配置中读取已安装的事件列表。
fn read_json_installed_events(path: &PathBuf) -> Vec<String> {
    let root = match read_json_file(path) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let hooks = match root.get("hooks").and_then(|h| h.as_object()) {
        Some(obj) => obj,
        None => return Vec::new(),
    };

    let mut installed = Vec::new();
    for event_key in ALL_EVENTS {
        if let Some(arr) = hooks.get(*event_key).and_then(|a| a.as_array()) {
            for entry in arr {
                if json_entry_has_marker(entry) {
                    installed.push(event_key.to_string());
                    break;
                }
            }
        }
    }
    installed
}

/// 从 JSON 配置中读取所有 hook 命令。
fn read_json_commands(path: &PathBuf) -> Vec<String> {
    let root = match read_json_file(path) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let hooks = match root.get("hooks").and_then(|h| h.as_object()) {
        Some(obj) => obj,
        None => return Vec::new(),
    };

    let mut commands = Vec::new();
    for (_event_key, arr) in hooks {
        if let Some(entries) = arr.as_array() {
            for entry in entries {
                if let Some(cmds) = entry.get("hooks").and_then(|h| h.as_array()) {
                    for h in cmds {
                        if let Some(cmd) = h.get("command").and_then(|c| c.as_str()) {
                            if cmd.contains(HOOK_MARKER) {
                                commands.push(cmd.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    commands.dedup();
    commands
}

/// 判断 JSON hook entry 是否包含当前 hook marker。
fn json_entry_has_marker(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.contains(HOOK_MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 读取 JSON 文件，失败返回 None。
fn read_json_file(path: &PathBuf) -> Option<serde_json::Value> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

/// JSON 配置中是否存在旧版 checkpoint-hook 条目。
fn json_has_legacy(path: &PathBuf) -> bool {
    let root = match read_json_file(path) {
        Some(v) => v,
        None => return false,
    };
    let hooks = match root.get("hooks").and_then(|h| h.as_object()) {
        Some(obj) => obj,
        None => return false,
    };
    for arr in hooks.values().filter_map(|v| v.as_array()) {
        for entry in arr {
            if json_entry_has_legacy(entry) {
                return true;
            }
        }
    }
    false
}

/// 判断 JSON hook entry 是否包含旧版 marker。
fn json_entry_has_legacy(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(is_legacy_command)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Kimi TOML 中是否存在旧版 checkpoint-hook 条目。
fn kimi_has_legacy(path: &PathBuf) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    content
        .lines()
        .any(|line| line.starts_with("command = ") && is_legacy_command(line))
}

/// 从 JSON 配置中移除旧版 hook entry（按事件）。
fn remove_json_legacy_entries(root: &mut serde_json::Value) -> Vec<String> {
    let mut removed = Vec::new();
    let hooks = match root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        Some(obj) => obj,
        None => return removed,
    };
    for (event_key, arr) in hooks.iter_mut() {
        let arr = match arr.as_array_mut() {
            Some(a) => a,
            None => continue,
        };
        let before = arr.len();
        arr.retain(|entry| !json_entry_has_legacy(entry));
        if arr.len() < before && !removed.contains(&event_key.clone()) {
            removed.push(event_key.clone());
        }
    }
    removed
}

/// 从 Kimi TOML 内容中移除旧版 [[hooks]] 段，返回 (cleaned, removed_events)。
fn remove_kimi_legacy_blocks(content: &str) -> (String, Vec<String>) {
    let mut output = Vec::new();
    let mut current_block = Vec::new();
    let mut in_hook = false;
    let mut removed_events = Vec::new();
    let mut current_event: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[hooks]]" {
            // Flush previous block
            if in_hook {
                let has_legacy = current_block.iter().any(|l: &String| {
                    let t = l.trim();
                    t.starts_with("command = ") && is_legacy_command(t)
                });
                if has_legacy {
                    if let Some(evt) = current_event.take() {
                        removed_events.push(evt);
                    }
                } else {
                    output.append(&mut current_block);
                }
                current_block.clear();
            }
            in_hook = true;
            current_event = None;
            current_block.push(line.to_string());
        } else if in_hook && trimmed.starts_with('[') {
            // End of hook block
            let has_legacy = current_block.iter().any(|l| {
                let t = l.trim();
                t.starts_with("command = ") && is_legacy_command(t)
            });
            if has_legacy {
                if let Some(evt) = current_event.take() {
                    removed_events.push(evt);
                }
            } else {
                output.append(&mut current_block);
            }
            current_block.clear();
            in_hook = false;
            output.push(line.to_string());
        } else if in_hook && trimmed.starts_with("event = ") {
            current_event = trimmed
                .strip_prefix("event = ")
                .map(|s| s.trim().trim_matches('"').to_string());
            current_block.push(line.to_string());
        } else if in_hook {
            current_block.push(line.to_string());
        } else {
            output.push(line.to_string());
        }
    }

    // Flush last block
    if in_hook {
        let has_legacy = current_block.iter().any(|l| {
            let t = l.trim();
            t.starts_with("command = ") && is_legacy_command(t)
        });
        if has_legacy {
            if let Some(evt) = current_event.take() {
                removed_events.push(evt);
            }
        } else {
            output.append(&mut current_block);
        }
    }

    (output.join("\n"), removed_events)
}

// ── Kimi TOML 读取 ───────────────────────────────────────────────────

/// 从 Kimi TOML 中读取已安装的事件列表。
fn read_kimi_installed_events(path: &PathBuf) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_kimi_installed_events(&content)
}

/// 从 Kimi TOML 中读取所有 hook 命令。
fn read_kimi_commands(path: &PathBuf) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_kimi_commands(&content)
}

/// 解析 Kimi TOML 内容，提取已安装的事件。
fn parse_kimi_installed_events(content: &str) -> Vec<String> {
    let mut installed = Vec::new();
    let mut current_event: Option<String> = None;
    let mut current_has_marker = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[hooks]]" {
            if let (Some(evt), true) = (current_event.take(), current_has_marker) {
                installed.push(evt);
            }
            current_event = None;
            current_has_marker = false;
        } else if trimmed.starts_with("event = ") {
            current_event = trimmed
                .strip_prefix("event = ")
                .map(|s| s.trim().trim_matches('"').to_string());
        } else if trimmed.starts_with("command = ") && trimmed.contains(HOOK_MARKER) {
            current_has_marker = true;
        } else if trimmed.starts_with('[') && !trimmed.starts_with("[[hooks]]") {
            if let (Some(evt), true) = (current_event.take(), current_has_marker) {
                installed.push(evt);
            }
            current_event = None;
            current_has_marker = false;
        }
    }
    if let (Some(evt), true) = (current_event, current_has_marker) {
        installed.push(evt);
    }
    installed
}

/// 解析 Kimi TOML 内容，提取所有 hook 命令。
fn parse_kimi_commands(content: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("command = ") && trimmed.contains(HOOK_MARKER) {
            let cmd = trimmed
                .strip_prefix("command = ")
                .map(|s| s.trim().trim_matches('"').to_string())
                .unwrap_or_default();
            if !cmd.is_empty() {
                commands.push(cmd);
            }
        }
    }
    commands.dedup();
    commands
}

// ── JSON reconcile ───────────────────────────────────────────────────

/// 生成 hook 命令：`AGENT_ASPECT_AGENT=<agent> <hook>`。
fn hook_command(hook: &str, agent: &str) -> String {
    format!("AGENT_ASPECT_AGENT={agent} {hook}")
}

/// JSON 配置：添加 hook 条目（先清理旧版残留）。
fn reconcile_json_add(path: &PathBuf, agent: &str, hook_binary: &str) -> Result<ReconcileReport, String> {
    let mut root = read_json_file(path).unwrap_or(serde_json::json!({}));
    let command = hook_command(hook_binary, agent);

    // 清理旧版 checkpoint-hook 残留
    let legacy_removed = remove_json_legacy_entries(&mut root);

    let mut events_added = Vec::new();
    for event in ALL_EVENTS {
        if !json_event_has_hook(&root, event) {
            add_json_hook_entry(&mut root, event, &command);
            events_added.push(event.to_string());
        }
    }

    if !legacy_removed.is_empty() || !events_added.is_empty() {
        backup_file(path);
        write_json_file(path, &root)?;
    }

    Ok(ReconcileReport {
        agent: agent.to_string(),
        action: if events_added.is_empty() && legacy_removed.is_empty() {
            "unchanged".to_string()
        } else {
            "added".to_string()
        },
        events_added,
        events_removed: legacy_removed,
    })
}

/// JSON 配置：移除 hook 条目（含旧版清理）。
fn reconcile_json_remove(path: &PathBuf, agent: &str) -> Result<ReconcileReport, String> {
    if !path.exists() {
        return Ok(ReconcileReport {
            agent: agent.to_string(),
            action: "unchanged".to_string(),
            events_added: Vec::new(),
            events_removed: Vec::new(),
        });
    }

    let mut root = match read_json_file(path) {
        Some(v) => v,
        None => {
            return Ok(ReconcileReport {
                agent: agent.to_string(),
                action: "unchanged".to_string(),
                events_added: Vec::new(),
                events_removed: Vec::new(),
            });
        }
    };

    let mut events_removed = Vec::new();

    // 移除旧版 checkpoint-hook 残留
    let legacy_removed = remove_json_legacy_entries(&mut root);
    for evt in &legacy_removed {
        if !events_removed.contains(evt) {
            events_removed.push(evt.clone());
        }
    }

    // 移除当前 agent 的条目
    let command_prefix = format!("AGENT_ASPECT_AGENT={agent}");
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        for event in ALL_EVENTS {
            if let Some(arr) = hooks.get_mut(*event).and_then(|a| a.as_array_mut()) {
                let before = arr.len();
                arr.retain(|entry| !json_entry_matches_agent(entry, &command_prefix));
                if arr.len() < before && !events_removed.contains(&event.to_string()) {
                    events_removed.push(event.to_string());
                }
            }
        }
    }

    if !events_removed.is_empty() {
        backup_file(path);
        write_json_file(path, &root)?;
    }

    Ok(ReconcileReport {
        agent: agent.to_string(),
        action: if events_removed.is_empty() {
            "unchanged".to_string()
        } else {
            "removed".to_string()
        },
        events_added: Vec::new(),
        events_removed,
    })
}

/// 判断 JSON 中某事件是否已有当前 hook。
fn json_event_has_hook(root: &serde_json::Value, event: &str) -> bool {
    root.get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().any(|e| json_entry_has_marker(e)))
        .unwrap_or(false)
}

/// 添加 JSON hook 条目。
fn add_json_hook_entry(root: &mut serde_json::Value, event: &str, command: &str) {
    let obj = root.as_object_mut().expect("root is object");
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let event_arr = hooks
        .as_object_mut()
        .unwrap()
        .entry(event)
        .or_insert_with(|| serde_json::json!([]));

    let hook_entry = serde_json::json!({
        "matcher": if event == "PreToolUse" { "*" } else { "" },
        "hooks": [
            {
                "type": "command",
                "command": command
            }
        ]
    });

    event_arr.as_array_mut().unwrap().push(hook_entry);
}

/// 判断 JSON hook entry 是否匹配指定 agent。
fn json_entry_matches_agent(entry: &serde_json::Value, agent_prefix: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.contains(HOOK_MARKER) && s.contains(agent_prefix))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 写 JSON 文件（pretty print + 换行）。
fn write_json_file(path: &PathBuf, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create_dir: {e}"))?;
    }
    let content = format!("{}\n", serde_json::to_string_pretty(value).unwrap());
    std::fs::write(path, content).map_err(|e| format!("write: {e}"))
}

// ── Kimi TOML reconcile ──────────────────────────────────────────────

/// Kimi TOML：添加 hook 条目（先清理旧版残留）。
fn reconcile_kimi_add(path: &PathBuf, hook_binary: &str) -> Result<ReconcileReport, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create_dir: {e}"))?;
    }
    let command = hook_command(hook_binary, "kimi");
    let mut content = std::fs::read_to_string(path).unwrap_or_default();

    // Strip empty hooks = [] lines
    content = content
        .lines()
        .filter(|line| line.trim() != "hooks = []")
        .collect::<Vec<_>>()
        .join("\n");

    // 清理旧版 checkpoint-hook 残留
    let (cleaned, legacy_events) = remove_kimi_legacy_blocks(&content);
    content = cleaned;

    let mut events_added = Vec::new();
    for event in ALL_EVENTS {
        if !kimi_event_has_current_hook(&content, event) {
            if !content.trim().is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str("\n[[hooks]]\n");
            content.push_str(&format!("event = \"{event}\"\n"));
            if *event == "PreToolUse" {
                content.push_str("matcher = \"*\"\n");
            }
            content.push_str(&format!(
                "command = \"{}\"\n",
                command.replace('\\', "\\\\").replace('"', "\\\"")
            ));
            events_added.push(event.to_string());
        }
    }

    if !legacy_events.is_empty() || !events_added.is_empty() {
        backup_file(path);
        std::fs::write(path, content).map_err(|e| format!("write: {e}"))?;
    }

    Ok(ReconcileReport {
        agent: "kimi_code".to_string(),
        action: if events_added.is_empty() && legacy_events.is_empty() {
            "unchanged".to_string()
        } else {
            "added".to_string()
        },
        events_added,
        events_removed: legacy_events,
    })
}

/// Kimi TOML：移除 hook 条目。
fn reconcile_kimi_remove(path: &PathBuf) -> Result<ReconcileReport, String> {
    if !path.exists() {
        return Ok(ReconcileReport {
            agent: "kimi_code".to_string(),
            action: "unchanged".to_string(),
            events_added: Vec::new(),
            events_removed: Vec::new(),
        });
    }

    let content = std::fs::read_to_string(path).unwrap_or_default();

    // 移除当前 hook + 清理旧版残留
    let (mut cleaned, mut removed_events) = remove_kimi_hooks(&content);
    let (cleaned2, legacy_events) = remove_kimi_legacy_blocks(&cleaned);
    cleaned = cleaned2;
    for evt in &legacy_events {
        if !removed_events.contains(evt) {
            removed_events.push(evt.clone());
        }
    }

    if !removed_events.is_empty() {
        backup_file(path);
        std::fs::write(path, cleaned).map_err(|e| format!("write: {e}"))?;
    }

    Ok(ReconcileReport {
        agent: "kimi_code".to_string(),
        action: if removed_events.is_empty() {
            "unchanged".to_string()
        } else {
            "removed".to_string()
        },
        events_added: Vec::new(),
        events_removed: removed_events,
    })
}

/// 判断 Kimi TOML 某事件是否已指向当前 hook。
fn kimi_event_has_current_hook(content: &str, event: &str) -> bool {
    let mut in_matching_hook = false;
    let event_line = format!("event = \"{event}\"");

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[hooks]]" {
            in_matching_hook = false;
            continue;
        }
        if trimmed == event_line {
            in_matching_hook = true;
            continue;
        }
        if in_matching_hook && trimmed.starts_with("command = ") {
            return trimmed.contains(HOOK_MARKER);
        }
    }
    false
}

/// 删除 Kimi TOML 中所有 agent-aspect-hook 的 [[hooks]] 段，返回 (cleaned, removed_events)。
fn remove_kimi_hooks(content: &str) -> (String, Vec<String>) {
    let mut output = Vec::new();
    let mut current_block = Vec::new();
    let mut in_hook = false;
    let mut removed_events = Vec::new();
    let mut current_event: Option<String> = None;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[hooks]]" {
            flush_kimi_block(
                &mut output,
                &mut current_block,
                &mut in_hook,
                &mut removed_events,
                &mut current_event,
                false,
            );
            in_hook = true;
            current_event = None;
            current_block.push(line.to_string());
        } else if in_hook && trimmed.starts_with('[') {
            flush_kimi_block(
                &mut output,
                &mut current_block,
                &mut in_hook,
                &mut removed_events,
                &mut current_event,
                false,
            );
            output.push(line.to_string());
        } else if in_hook && trimmed.starts_with("event = ") {
            current_event = trimmed
                .strip_prefix("event = ")
                .map(|s| s.trim().trim_matches('"').to_string());
            current_block.push(line.to_string());
        } else if in_hook {
            current_block.push(line.to_string());
        } else {
            output.push(line.to_string());
        }
    }
    flush_kimi_block(
        &mut output,
        &mut current_block,
        &mut in_hook,
        &mut removed_events,
        &mut current_event,
        false,
    );

    (output.join("\n"), removed_events)
}

/// 将当前 Kimi hook block 写回或丢弃。
fn flush_kimi_block(
    output: &mut Vec<String>,
    current_block: &mut Vec<String>,
    in_hook: &mut bool,
    removed_events: &mut Vec<String>,
    current_event: &mut Option<String>,
    _is_legacy_check: bool,
) {
    if !*in_hook {
        return;
    }
    let is_ours = current_block.iter().any(|line| line.contains(HOOK_MARKER));
    if is_ours {
        if let Some(evt) = current_event.take() {
            removed_events.push(evt);
        }
    } else {
        output.append(current_block);
    }
    current_block.clear();
    *in_hook = false;
    *current_event = None;
}

// ── 备份 ─────────────────────────────────────────────────────────────

/// 备份文件（带时间戳）。
fn backup_file(path: &PathBuf) {
    if !path.exists() {
        return;
    }
    let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
    let backup = path.with_file_name(format!(
        "{}.agent-aspect-{ts}.bak",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config")
    ));
    let _ = std::fs::copy(path, &backup);
}

// ── 测试 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// JSON 解析：空文件返回空列表。
    #[test]
    fn json_empty_file_returns_no_events() {
        let dir = crate::test_util::unique_temp_dir("hook_status_json_empty");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"{}").unwrap();

        let events = read_json_installed_events(&path);
        assert!(events.is_empty());
    }

    /// JSON 解析：完整的 hook 配置返回四个事件。
    #[test]
    fn json_full_config_returns_all_events() {
        let dir = crate::test_util::unique_temp_dir("hook_status_json_full");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let content = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }],
                "SessionStart": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }],
                "UserPromptSubmit": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string(&content).unwrap()).unwrap();

        let events = read_json_installed_events(&path);
        assert_eq!(events.len(), 4);
        assert!(events.contains(&"PreToolUse".to_string()));
        assert!(events.contains(&"Stop".to_string()));
    }

    /// JSON 解析：部分安装返回 partial 缺失。
    #[test]
    fn json_partial_config_reports_missing() {
        let dir = crate::test_util::unique_temp_dir("hook_status_json_partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let content = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string(&content).unwrap()).unwrap();

        let events = read_json_installed_events(&path);
        assert_eq!(events, vec!["PreToolUse"]);

        let missing: Vec<String> = ALL_EVENTS
            .iter()
            .filter(|e| !events.contains(&e.to_string()))
            .map(|e| e.to_string())
            .collect();
        assert_eq!(missing.len(), 3);
        assert!(missing.contains(&"Stop".to_string()));
    }

    /// Kimi TOML 解析：完整的 hook 配置返回四个事件。
    #[test]
    fn kimi_full_config_returns_all_events() {
        let content = r#"
[[hooks]]
event = "PreToolUse"
matcher = "*"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"

[[hooks]]
event = "SessionStart"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"

[[hooks]]
event = "UserPromptSubmit"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"

[[hooks]]
event = "Stop"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"
"#;
        let events = parse_kimi_installed_events(content);
        assert_eq!(events.len(), 4);
        assert!(events.contains(&"PreToolUse".to_string()));
        assert!(events.contains(&"Stop".to_string()));
    }

    /// Kimi TOML 解析：不包含 marker 的事件不被计入。
    #[test]
    fn kimi_other_hooks_not_counted() {
        let content = r#"
[[hooks]]
event = "PreToolUse"
command = "some-other-hook"

[[hooks]]
event = "Stop"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"
"#;
        let events = parse_kimi_installed_events(content);
        assert_eq!(events, vec!["Stop"]);
    }

    /// derive_status：disabled agent 返回 "disabled"。
    #[test]
    fn status_disabled_when_not_enabled() {
        let status = derive_status(false, true, &[], &[], None);
        assert_eq!(status, "disabled");
    }

    /// derive_status：完整安装返回 "ok"。
    #[test]
    fn status_ok_when_all_installed() {
        // 创建临时文件作为 hook binary
        let dir = crate::test_util::unique_temp_dir("hook_status_ok");
        std::fs::create_dir_all(&dir).unwrap();
        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        let status = derive_status(
            true,
            true,
            &[
                "PreToolUse".into(),
                "SessionStart".into(),
                "UserPromptSubmit".into(),
                "Stop".into(),
            ],
            &[],
            Some(&hook_bin),
        );
        assert_eq!(status, "ok");
    }

    /// derive_status：部分安装返回 "partial"。
    #[test]
    fn status_partial_when_events_missing() {
        let dir = crate::test_util::unique_temp_dir("hook_status_partial");
        std::fs::create_dir_all(&dir).unwrap();
        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        let status = derive_status(
            true,
            true,
            &["PreToolUse".into()],
            &["Stop".into()],
            Some(&hook_bin),
        );
        assert_eq!(status, "partial");
    }

    /// JSON reconcile add：添加缺失的事件。
    #[test]
    fn json_reconcile_add_missing_events() {
        let dir = crate::test_util::unique_temp_dir("hook_reconcile_add");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();

        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        let report = reconcile_json_add(&path, "claude", hook_bin.to_str().unwrap()).unwrap();
        assert_eq!(report.action, "added");
        assert_eq!(report.events_added.len(), 4);

        // 验证文件内容
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(root.get("hooks").unwrap().get("PreToolUse").is_some());
        assert!(root.get("hooks").unwrap().get("Stop").is_some());
    }

    /// JSON reconcile add：幂等，重复调用不增加条目。
    #[test]
    fn json_reconcile_add_is_idempotent() {
        let dir = crate::test_util::unique_temp_dir("hook_reconcile_idem");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, "{}").unwrap();

        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        reconcile_json_add(&path, "claude", hook_bin.to_str().unwrap()).unwrap();
        let report = reconcile_json_add(&path, "claude", hook_bin.to_str().unwrap()).unwrap();
        assert_eq!(report.action, "unchanged");
        assert!(report.events_added.is_empty());
    }

    /// JSON reconcile remove：移除指定 agent 的条目。
    #[test]
    fn json_reconcile_remove_agent() {
        let dir = crate::test_util::unique_temp_dir("hook_reconcile_rm");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let initial = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();

        let report = reconcile_json_remove(&path, "claude").unwrap();
        assert_eq!(report.action, "removed");
        assert_eq!(report.events_removed.len(), 2);

        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let pre_arr = root
            .get("hooks")
            .unwrap()
            .get("PreToolUse")
            .unwrap()
            .as_array()
            .unwrap();
        assert!(pre_arr.is_empty());
    }

    /// Kimi TOML reconcile remove：移除 agent-aspect-hook 段。
    #[test]
    fn kimi_reconcile_remove() {
        let content = r#"default_model = "kimi"

[[hooks]]
event = "PreToolUse"
matcher = "*"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"

[models.kimi]
provider = "managed:kimi"

[[hooks]]
event = "Stop"
command = "AGENT_ASPECT_AGENT=kimi /usr/local/bin/agent-aspect-hook"
"#;
        let (cleaned, removed) = remove_kimi_hooks(content);
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&"PreToolUse".to_string()));
        assert!(removed.contains(&"Stop".to_string()));
        assert!(!cleaned.contains("agent-aspect-hook"));
        assert!(cleaned.contains("[models.kimi]"));
    }

    // ── Legacy cleanup tests ──────────────────────────────────────────

    /// JSON reconcile add：legacy + current 同时存在时，reconcile 后只剩 agent-aspect-hook。
    #[test]
    fn json_reconcile_add_removes_legacy_and_adds_current() {
        let dir = crate::test_util::unique_temp_dir("hook_legacy_mixed");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let initial = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "*",
                        "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude target/release/checkpoint-hook"}]
                    },
                    {
                        "matcher": "*",
                        "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                    }
                ],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude target/debug/checkpoint-hook"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();

        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        let report = reconcile_json_add(&path, "claude", hook_bin.to_str().unwrap()).unwrap();

        // PreToolUse: legacy removed, current kept → no add
        // Stop: legacy removed, current missing → added
        assert_eq!(report.events_added.len(), 3); // SessionStart, UserPromptSubmit, Stop
        assert!(report.events_removed.contains(&"PreToolUse".to_string()));
        assert!(report.events_removed.contains(&"Stop".to_string()));

        let _root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("checkpoint-hook"));
        assert!(content.contains("agent-aspect-hook"));
    }

    /// JSON reconcile add：只有 legacy 时，reconcile 后替换成 current。
    #[test]
    fn json_reconcile_add_replaces_legacy_only() {
        let dir = crate::test_util::unique_temp_dir("hook_legacy_only");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let initial = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "checkpoint-hook"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();

        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        let report = reconcile_json_add(&path, "claude", hook_bin.to_str().unwrap()).unwrap();

        assert!(report.events_removed.contains(&"PreToolUse".to_string()));
        // All 4 events added (legacy PreToolUse was removed, current needs to be added)
        assert_eq!(report.events_added.len(), 4);

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("checkpoint-hook"));
        assert!(content.contains("agent-aspect-hook"));
    }

    /// JSON reconcile：其他非 agent-aspect hook 不得被误删。
    #[test]
    fn json_reconcile_preserves_other_hooks() {
        let dir = crate::test_util::unique_temp_dir("hook_legacy_other");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let initial = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "*",
                        "hooks": [{"type": "command", "command": "my-custom-hook --arg"}]
                    },
                    {
                        "matcher": "*",
                        "hooks": [{"type": "command", "command": "checkpoint-hook"}]
                    }
                ],
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "some-other-tool"}]
                }]
            }
        });
        std::fs::write(&path, serde_json::to_string(&initial).unwrap()).unwrap();

        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        reconcile_json_add(&path, "claude", hook_bin.to_str().unwrap()).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();

        // Other hooks preserved
        assert!(content.contains("my-custom-hook"));
        assert!(content.contains("some-other-tool"));
        // Legacy removed
        assert!(!content.contains("checkpoint-hook"));
        // Current added
        assert!(content.contains("agent-aspect-hook"));
    }

    /// Kimi TOML reconcile add：legacy + current 同时存在时清理 legacy。
    #[test]
    fn kimi_reconcile_add_removes_legacy() {
        let dir = crate::test_util::unique_temp_dir("hook_kimi_legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let content = r#"default_model = "kimi"

[[hooks]]
event = "PreToolUse"
matcher = "*"
command = "target/release/checkpoint-hook"

[[hooks]]
event = "Stop"
command = "target/debug/checkpoint-hook"

[models.kimi]
provider = "managed:kimi"
"#;
        std::fs::write(&path, content).unwrap();

        let hook_bin = dir.join("agent-aspect-hook");
        std::fs::write(&hook_bin, "").unwrap();
        let _report = reconcile_kimi_add(&path, hook_bin.to_str().unwrap()).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(!result.contains("checkpoint-hook"));
        assert!(result.contains("agent-aspect-hook"));
        assert!(result.contains("[models.kimi]"));
    }

    /// Kimi TOML reconcile：其他 hook 不得被误删。
    #[test]
    fn kimi_reconcile_preserves_other_hooks() {
        let content = r#"default_model = "kimi"

[[hooks]]
event = "PreToolUse"
matcher = "*"
command = "my-custom-hook"

[[hooks]]
event = "Stop"
command = "target/release/checkpoint-hook"
"#;
        let (cleaned, _removed) = remove_kimi_legacy_blocks(content);
        assert!(cleaned.contains("my-custom-hook"));
        assert!(!cleaned.contains("checkpoint-hook"));
    }
}
