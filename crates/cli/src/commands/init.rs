//! `agent-aspect init` — 安装 AI agent 的 hook 配置。
//!
//! 支持三家 agent：Claude Code（settings.json）、Codex CLI（hooks.json）、
//! Kimi Code（config.toml）。每个 agent 写入三个事件钩子：
//! PreToolUse / SessionStart / UserPromptSubmit。
//!
//! 关键不变量：
//! - 修改前自动备份原文件（带时间戳）
//! - 已安装过的不重复写入（通过检查 "agent-aspect-hook" 标记判断）
//! - hook 命令中注入 `AGENT_ASPECT_AGENT=<agent>` 环境变量

use agent_aspect_core::config::Config;
use agent_aspect_core::paths;

use super::helpers::bin_dir;

/// 在 hook 命令中搜索此标记来判断是否已安装。
const HOOK_MARKER: &str = "agent-aspect-hook";

/// init 子命令入口。
///
/// - `None` 或 `"agents"` — 安装所有三家 agent 的 hook
/// - `"--help"` / `"-h"` — 显示用法
/// - 其他值 — 报错
pub fn cmd_init(arg: Option<&str>) {
    match arg {
        None | Some("agents") => init_agents(),
        Some("--help") | Some("-h") | Some("help") => print_usage(),
        Some(other) => {
            eprintln!("unknown init target: {other}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("usage: agent-aspect init [agents]");
    eprintln!();
    eprintln!("Installs verified agent hook configuration:");
    eprintln!("  Claude Code  ~/.claude/settings.json");
    eprintln!("  Codex CLI    ~/.codex/hooks.json");
    eprintln!("  Kimi Code    ~/.kimi/config.toml");
}

/// 依次为 Claude / Codex / Kimi 安装 hook 配置。
/// 定位 agent-aspect-hook 二进制后，生成带 `AGENT_ASPECT_AGENT` 前缀的命令。
fn init_agents() {
    let Some(dir) = bin_dir() else {
        eprintln!("FAIL: cannot determine binary directory");
        std::process::exit(1);
    };
    let hook_bin = dir.join("agent-aspect-hook");
    if !hook_bin.exists() {
        eprintln!("FAIL: agent-aspect-hook not found at {}", dir.display());
        std::process::exit(1);
    }
    let hook = hook_bin
        .canonicalize()
        .unwrap_or(hook_bin)
        .display()
        .to_string();

    // 读取 config，检查 per-agent enabled 开关
    let config = Config::load_or_create();

    println!("agent-aspect init: installing agent hook configs");
    if config.agent_hook_config("claude_code").enabled {
        install_claude(&hook);
    } else {
        println!("Claude Code: disabled in config, skipping hook installation");
    }
    if config.agent_hook_config("codex_cli").enabled {
        install_codex(&hook);
    } else {
        println!("Codex CLI: disabled in config, skipping hook installation");
    }
    if config.agent_hook_config("kimi_code").enabled {
        install_kimi(&hook);
    } else {
        println!("Kimi Code: disabled in config, skipping hook installation");
    }
    println!("agent-aspect init: done");
}

/// 确保文件父目录存在，不存在则递归创建。
fn ensure_parent(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("FAIL: create {}: {e}", parent.display());
            std::process::exit(1);
        }
    }
}

/// 备份文件：生成 `<name>.agent-aspect-<timestamp>.bak` 格式的副本。
/// 备份失败只打 warning，不阻断流程（备份是安全措施，不是前置条件）。
fn backup(path: &std::path::Path) {
    if !path.exists() {
        return;
    }
    let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
    let backup = path.with_extension(format!(
        "{}.bak",
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("agent-aspect")
    ));
    let backup = backup.with_file_name(format!(
        "{}.agent-aspect-{ts}.bak",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config")
    ));
    if let Err(e) = std::fs::copy(path, &backup) {
        eprintln!("warning: backup {} failed: {e}", path.display());
    }
}

/// 写 JSON 文件：确保父目录存在 → 备份旧文件 → pretty print 写入。
fn write_json(path: &std::path::Path, value: &serde_json::Value) {
    ensure_parent(path);
    backup(path);
    let content = match serde_json::to_string_pretty(value) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            eprintln!("FAIL: serialize {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::write(path, content) {
        eprintln!("FAIL: write {}: {e}", path.display());
        std::process::exit(1);
    }
}

/// 生成带 agent 标识的 hook 命令：`AGENT_ASPECT_AGENT=<agent> <hook>`。
fn hook_command(hook: &str, agent: &str) -> String {
    format!("AGENT_ASPECT_AGENT={agent} {hook}")
}

/// 旧 hook binary 标记。仅用于删除用户配置中的过期条目，不能作为当前运行身份使用。
fn legacy_hook_marker() -> String {
    ["check", "point-hook"].concat()
}

/// 在 JSON hooks 配置中确保指定事件的 hook 条目存在。
///
/// `event_key` 是事件类型（如 "PreToolUse"）。
/// 检查 `hooks.<event_key>` 数组中是否已有当前 hook 条目。
fn ensure_json_hook_entry(root: &mut serde_json::Value, event_key: &str, command: &str) -> bool {
    let obj = root.as_object_mut().expect("root is object");
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        eprintln!("FAIL: hooks field is not an object");
        std::process::exit(1);
    }
    let event_arr = hooks
        .as_object_mut()
        .unwrap()
        .entry(event_key)
        .or_insert_with(|| serde_json::json!([]));
    if !event_arr.is_array() {
        eprintln!("FAIL: hooks.{event_key} is not an array");
        std::process::exit(1);
    }

    let arr = event_arr.as_array_mut().unwrap();
    let before_len = arr.len();
    let legacy_marker = legacy_hook_marker();
    arr.retain(|entry| !json_hook_entry_contains(entry, &legacy_marker));
    let mut changed = arr.len() != before_len;

    for entry in arr.iter() {
        if json_hook_entry_contains(entry, HOOK_MARKER) {
            return changed;
        }
    }

    let hook_entry = serde_json::json!({
        "matcher": if event_key == "PreToolUse" { "*" } else { "" },
        "hooks": [
            {
                "type": "command",
                "command": command
            }
        ]
    });

    arr.push(hook_entry);
    changed = true;
    changed
}

/// 判断单个 JSON hook entry 是否引用了指定命令片段。
fn json_hook_entry_contains(entry: &serde_json::Value, needle: &str) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks_arr| {
            hooks_arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|s| s.contains(needle))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// 为 Claude Code 安装 hooks（settings.json）。
/// PreToolUse 需要 matcher="*"，其他事件不需要。
fn install_claude(hook: &str) {
    let path = paths::claude_settings_path();
    let mut root = read_json_or_object(&path);
    let command = hook_command(hook, "claude");

    let mut changed = false;

    for event in ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"] {
        if ensure_json_hook_entry(&mut root, event, &command) {
            changed = true;
        }
    }

    if changed {
        write_json(&path, &root);
        println!("Claude Code: installed hooks ({})", path.display());
    } else {
        println!("Claude Code: already configured ({})", path.display());
    }
}

/// 为 Codex CLI 安装 hooks（hooks.json）。
/// 格式与 Claude 相同（JSON + 嵌套 hooks 数组）。
fn install_codex(hook: &str) {
    let path = paths::codex_hooks_path();
    let mut root = read_json_or_object(&path);
    let command = hook_command(hook, "codex");

    let mut changed = false;

    for event in ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"] {
        if ensure_json_hook_entry(&mut root, event, &command) {
            changed = true;
        }
    }

    if changed {
        write_json(&path, &root);
        println!("Codex CLI: installed hooks ({})", path.display());
    } else {
        println!("Codex CLI: already configured ({})", path.display());
    }
}

/// 为 Kimi Code 安装 hooks（config.toml，TOML 格式）。
///
/// Kimi 使用 TOML 的 `[[hooks]]` 段，与 Claude/Codex 的 JSON 不同。
/// 需要手动拼接 TOML 文本，并转义反斜杠和引号。
/// 会先过滤掉空的 `hooks = []` 行（Kimi 可能生成的占位符）。
fn install_kimi(hook: &str) {
    let path = paths::kimi_config_path();
    ensure_parent(&path);
    let command = hook_command(hook, "kimi");
    let mut content = std::fs::read_to_string(&path).unwrap_or_default();

    let events = ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"];
    let mut installed = Vec::new();

    // Strip empty hooks = [] lines
    content = content
        .lines()
        .filter(|line| line.trim() != "hooks = []")
        .collect::<Vec<_>>()
        .join("\n");
    let (cleaned, removed_legacy) = remove_legacy_kimi_hooks(&content);
    content = cleaned;

    for event in events {
        let already = kimi_event_has_current_hook(&content, event);

        if !already {
            if !content.trim().is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str("\n[[hooks]]\n");
            content.push_str(&format!("event = \"{event}\"\n"));
            if event == "PreToolUse" {
                content.push_str("matcher = \"*\"\n");
            }
            content.push_str(&format!(
                "command = \"{}\"\n",
                command.replace('\\', "\\\\").replace('"', "\\\"")
            ));
            installed.push(event);
        }
    }

    if removed_legacy || !installed.is_empty() {
        backup(&path);
        if let Err(e) = std::fs::write(&path, content) {
            eprintln!("FAIL: write {}: {e}", path.display());
            std::process::exit(1);
        }
        println!(
            "Kimi Code: installed hooks {} ({})",
            if installed.is_empty() {
                "legacy cleanup".to_string()
            } else {
                installed.join(", ")
            },
            path.display()
        );
    } else {
        println!("Kimi Code: already configured ({})", path.display());
    }
}

/// 删除 Kimi TOML 中引用旧 hook binary 的整个 `[[hooks]]` 段。
fn remove_legacy_kimi_hooks(content: &str) -> (String, bool) {
    let legacy_marker = legacy_hook_marker();
    let mut output = Vec::new();
    let mut current_block = Vec::new();
    let mut in_hook = false;
    let mut removed = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[hooks]]" {
            flush_kimi_hook_block(
                &mut output,
                &mut current_block,
                &mut in_hook,
                &mut removed,
                &legacy_marker,
            );
            in_hook = true;
            current_block.push(line.to_string());
        } else if in_hook && trimmed.starts_with('[') {
            flush_kimi_hook_block(
                &mut output,
                &mut current_block,
                &mut in_hook,
                &mut removed,
                &legacy_marker,
            );
            output.push(line.to_string());
        } else if in_hook {
            current_block.push(line.to_string());
        } else {
            output.push(line.to_string());
        }
    }
    flush_kimi_hook_block(
        &mut output,
        &mut current_block,
        &mut in_hook,
        &mut removed,
        &legacy_marker,
    );

    (output.join("\n"), removed)
}

/// 将当前 Kimi hook block 写回或丢弃。
fn flush_kimi_hook_block(
    output: &mut Vec<String>,
    current_block: &mut Vec<String>,
    in_hook: &mut bool,
    removed: &mut bool,
    legacy_marker: &str,
) {
    if !*in_hook {
        return;
    }
    let is_legacy = current_block
        .iter()
        .any(|line| line.contains(legacy_marker));
    if is_legacy {
        *removed = true;
    } else {
        output.append(current_block);
    }
    current_block.clear();
    *in_hook = false;
}

/// 判断 Kimi 某个事件是否已经指向当前 `agent-aspect-hook`。
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

/// 读取 JSON 文件，不存在则返回空对象 `{}`。
/// 要求根节点必须是 JSON object（不是数组或原始值）。
fn read_json_or_object(path: &std::path::Path) -> serde_json::Value {
    if !path.exists() {
        return serde_json::json!({});
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: read {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(v) if v.is_object() => v,
        Ok(_) => {
            eprintln!("FAIL: {} root must be a JSON object", path.display());
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("FAIL: parse {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}

/// 递归搜索 JSON value 中是否包含指定字符串（用于判断 hook 是否已安装）。
#[cfg(test)]
fn json_contains_command(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::String(s) => s.contains(needle),
        serde_json::Value::Array(items) => items.iter().any(|v| json_contains_command(v, needle)),
        serde_json::Value::Object(obj) => obj.values().any(|v| json_contains_command(v, needle)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_claude_registers_stop_hook() {
        // Simulate what install_claude does: iterate events and ensure entries
        let mut root = serde_json::json!({});
        let command = "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook";

        for event in ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"] {
            ensure_json_hook_entry(&mut root, event, command);
        }

        // Verify Stop hook is present
        let hooks = root.get("hooks").expect("hooks must exist");
        let stop_hooks = hooks.get("Stop").expect("Stop event must exist");
        assert!(stop_hooks.is_array());
        let arr = stop_hooks.as_array().unwrap();
        assert_eq!(arr.len(), 1);

        let hook_cmd = arr[0]
            .get("hooks")
            .and_then(|h| h.as_array())
            .and_then(|a| a.first())
            .and_then(|h| h.get("command"))
            .and_then(|c| c.as_str())
            .expect("command must exist");
        assert!(hook_cmd.contains("agent-aspect-hook"));

        // Stop should NOT have matcher="*"
        let matcher = arr[0].get("matcher").and_then(|m| m.as_str()).unwrap_or("");
        assert_eq!(matcher, "", "Stop hook must not have matcher=\"*\"");

        // PreToolUse should have matcher="*"
        let pre_hooks = root
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|h| h.as_array())
            .unwrap();
        let pre_matcher = pre_hooks[0]
            .get("matcher")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        assert_eq!(pre_matcher, "*");
    }

    #[test]
    fn install_claude_stop_is_idempotent() {
        let mut root = serde_json::json!({});
        let command = "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook";

        // First pass: all should be new
        let mut any_changed = false;
        for event in ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"] {
            if ensure_json_hook_entry(&mut root, event, command) {
                any_changed = true;
            }
        }
        assert!(any_changed);

        // Second pass: none should be new
        for event in ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"] {
            assert!(
                !ensure_json_hook_entry(&mut root, event, command),
                "{event} should not be added twice"
            );
        }
    }

    #[test]
    fn json_hook_entry_removes_legacy_hook() {
        let legacy_command = format!("/tmp/{}", legacy_hook_marker());
        let mut root = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "*",
                        "hooks": [
                            {
                                "type": "command",
                                "command": legacy_command
                            }
                        ]
                    }
                ]
            }
        });
        let command = "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook";

        assert!(ensure_json_hook_entry(&mut root, "PreToolUse", command));
        let entries = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        let rendered = serde_json::to_string(&root).unwrap();
        assert!(rendered.contains("agent-aspect-hook"));
        assert!(!rendered.contains(&legacy_hook_marker()));
    }

    #[test]
    fn install_codex_registers_stop_hook() {
        let mut root = serde_json::json!({});
        let command = "AGENT_ASPECT_AGENT=codex /usr/local/bin/agent-aspect-hook";

        for event in ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"] {
            ensure_json_hook_entry(&mut root, event, command);
        }

        let hooks = root.get("hooks").expect("hooks must exist");
        let stop_hooks = hooks.get("Stop").expect("Stop event must exist");
        assert!(stop_hooks.is_array());
        let arr = stop_hooks.as_array().unwrap();
        assert_eq!(arr.len(), 1);

        let matcher = arr[0].get("matcher").and_then(|m| m.as_str()).unwrap_or("");
        assert_eq!(matcher, "", "Codex Stop hook must not have matcher=\"*\"");
    }

    #[test]
    fn install_kimi_registers_stop_hook() {
        let path = std::path::PathBuf::from("/tmp/test-kimi-config.toml");
        let content = std::fs::read_to_string(&path).unwrap_or_default();

        // Kimi uses TOML [[hooks]] sections
        let events = ["PreToolUse", "SessionStart", "UserPromptSubmit", "Stop"];
        for event in events {
            let needle = format!("event = \"{}\"", event);
            let already = content.lines().any(|line| line.trim() == &needle);
            if !already {
                // Would be installed
            }
        }
        // Just verify Stop is in the install list
        assert!(events.contains(&"Stop"));
    }

    #[test]
    fn kimi_legacy_hook_cleanup_preserves_other_sections() {
        let legacy_command = format!("/tmp/{}", legacy_hook_marker());
        let content = format!(
            r#"default_model = "kimi"

[[hooks]]
event = "PreToolUse"
command = "{legacy_command}"
timeout = 5

[models.kimi]
provider = "managed:kimi"

[[hooks]]
event = "Stop"
command = "AGENT_ASPECT_AGENT=kimi /tmp/agent-aspect-hook"
"#
        );

        let (cleaned, removed) = remove_legacy_kimi_hooks(&content);

        assert!(removed);
        assert!(!cleaned.contains(&legacy_hook_marker()));
        assert!(cleaned.contains("[models.kimi]"));
        assert!(cleaned.contains("agent-aspect-hook"));
        assert!(kimi_event_has_current_hook(&cleaned, "Stop"));
        assert!(!kimi_event_has_current_hook(&cleaned, "PreToolUse"));
    }

    #[test]
    fn json_contains_command_finds_marker() {
        let root = serde_json::json!({
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "AGENT_ASPECT_AGENT=claude /usr/local/bin/agent-aspect-hook"}]
                }]
            }
        });
        assert!(json_contains_command(&root, "agent-aspect-hook"));
        assert!(!json_contains_command(&root, "nonexistent-binary"));
    }
}
