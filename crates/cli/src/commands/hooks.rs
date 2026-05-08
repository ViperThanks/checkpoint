//! `agent-aspect hooks` — 管理 hook 开关和状态查询。
//!
//! 子命令：
//! - `hooks status` — 显示全局和 per-agent hook 状态
//! - `hooks enable/disable pretooluse` — 全局 PreToolUse 评估开关
//! - `hooks enable/disable <agent>` — agent enabled 开关
//! - `hooks enable/disable <agent> pretooluse` — agent pretooluse 开关
//! - `hooks reconcile` — 按 config 添加/移除 agent hook entry

use agent_aspect_core::config::Config;
use agent_aspect_core::hook_status;
use agent_aspect_core::paths;

/// hooks 子命令入口。
pub fn cmd_hooks(arg: Option<&str>) {
    let rest = arg.unwrap_or("status");
    let parts: Vec<&str> = rest.split_whitespace().collect();

    match parts.as_slice() {
        [] | ["status"] => print_hook_status(),
        ["enable", "pretooluse"] => set_global_pretooluse(true),
        ["disable", "pretooluse"] => set_global_pretooluse(false),
        ["enable", agent] => set_agent_enabled(agent, true),
        ["disable", agent] => set_agent_enabled(agent, false),
        ["enable", agent, "pretooluse"] => set_agent_pretooluse(agent, true),
        ["disable", agent, "pretooluse"] => set_agent_pretooluse(agent, false),
        ["enable", agent, "metadata"] => set_agent_flag(agent, "metadata_enabled", true),
        ["disable", agent, "metadata"] => set_agent_flag(agent, "metadata_enabled", false),
        ["enable", agent, "stop"] => set_agent_flag(agent, "stop_enabled", true),
        ["disable", agent, "stop"] => set_agent_flag(agent, "stop_enabled", false),
        ["reconcile"] => reconcile_hooks(),
        ["--help"] | ["-h"] => print_usage(),
        _ => {
            eprintln!("unknown hooks subcommand: {rest}");
            print_usage();
            std::process::exit(1);
        }
    }
}

fn print_usage() {
    eprintln!("usage: agent-aspect hooks <subcommand>");
    eprintln!();
    eprintln!("Subcommands:");
    eprintln!("  status                        Show hook configuration status");
    eprintln!("  enable/disable pretooluse     Toggle global PreToolUse evaluation");
    eprintln!("  enable/disable <agent>        Toggle agent hook installation");
    eprintln!("  enable/disable <agent> pretooluse  Toggle agent PreToolUse evaluation");
    eprintln!("  enable/disable <agent> metadata    Toggle agent metadata handling");
    eprintln!("  enable/disable <agent> stop         Toggle agent stop handling");
    eprintln!("  reconcile                     Add/remove hook entries per config");
    eprintln!();
    eprintln!("Agents: claude_code, codex_cli, kimi_code");
}

/// 打印 hook 状态报告。
fn print_hook_status() {
    let config = Config::load_or_create();
    let hook_binary = paths::hook_binary_path();

    let status = hook_status::read_full_status(&config, hook_binary.as_ref());

    println!("Hook Status");
    println!("═══════════");
    println!();
    println!("Global:");
    println!(
        "  PreToolUse evaluation: {}",
        if status.global.pretooluse_enabled {
            "ON"
        } else {
            "OFF"
        }
    );
    println!(
        "  Config path:           {}",
        status.global.config_path.display()
    );
    if let Some(ref bin) = status.global.hook_binary_path {
        println!("  Hook binary:           {}", bin.display());
    } else {
        println!("  Hook binary:           (not found)");
    }
    println!();

    for agent in &status.agents {
        let status_label = match agent.status.as_str() {
            "ok" => "✓",
            "disabled" => "✗ disabled",
            "partial" => "⚠ partial",
            "missing_config" => "⚠ no config",
            "missing_hook_binary" => "✗ no hook binary",
            other => other,
        };
        println!("{} [{}]", agent.label, status_label);
        println!(
            "  enabled:     {}",
            if agent.enabled { "ON" } else { "OFF" }
        );
        println!(
            "  pretooluse:  {}",
            if agent.pretooluse_enabled {
                "ON"
            } else {
                "OFF"
            }
        );
        println!(
            "  metadata:    {}",
            if agent.metadata_enabled { "ON" } else { "OFF" }
        );
        println!(
            "  stop:        {}",
            if agent.stop_enabled { "ON" } else { "OFF" }
        );
        println!(
            "  config:      {} ({})",
            agent.config_path.display(),
            if agent.config_exists {
                "exists"
            } else {
                "missing"
            }
        );
        if !agent.installed_events.is_empty() {
            println!("  installed:   {}", agent.installed_events.join(", "));
        }
        if !agent.missing_events.is_empty() {
            println!("  missing:     {}", agent.missing_events.join(", "));
        }
        if agent.legacy_present {
            println!("  legacy:      !! checkpoint-hook residue (run reconcile)");
        }
        if !agent.commands.is_empty() {
            println!("  command:     {}", agent.commands[0]);
        }
        println!();
    }
}

/// 设置全局 PreToolUse 评估开关。
fn set_global_pretooluse(value: bool) {
    let config_path = Config::config_path();
    let mut cfg = Config::load_or_create();
    cfg.pretooluse_enabled = value;
    if let Err(e) = cfg.save(&config_path) {
        eprintln!("FAIL: save config: {e}");
        std::process::exit(1);
    }
    println!(
        "Global PreToolUse evaluation: {}",
        if value { "ENABLED" } else { "DISABLED" }
    );
    if !value {
        println!("Note: rule evaluation is skipped, but metadata/stop hooks remain active.");
    }
}

/// 设置 agent enabled 开关。
fn set_agent_enabled(agent: &str, value: bool) {
    validate_agent(agent);
    let config_path = Config::config_path();
    let mut cfg = Config::load_or_create();
    cfg.agent_hooks
        .entry(agent.to_string())
        .or_default()
        .enabled = value;
    if let Err(e) = cfg.save(&config_path) {
        eprintln!("FAIL: save config: {e}");
        std::process::exit(1);
    }
    println!(
        "{} hook: {}",
        agent_label(agent),
        if value { "ENABLED" } else { "DISABLED" }
    );
    if !value {
        println!(
            "Note: run 'agent-aspect hooks reconcile' to remove hook entries from agent config."
        );
    }
}

/// 设置 agent pretooluse 开关。
fn set_agent_pretooluse(agent: &str, value: bool) {
    validate_agent(agent);
    let config_path = Config::config_path();
    let mut cfg = Config::load_or_create();
    cfg.agent_hooks
        .entry(agent.to_string())
        .or_default()
        .pretooluse_enabled = value;
    if let Err(e) = cfg.save(&config_path) {
        eprintln!("FAIL: save config: {e}");
        std::process::exit(1);
    }
    println!(
        "{} PreToolUse evaluation: {}",
        agent_label(agent),
        if value { "ENABLED" } else { "DISABLED" }
    );
}

/// 设置 agent metadata/stop 开关。
fn set_agent_flag(agent: &str, flag: &str, value: bool) {
    validate_agent(agent);
    let config_path = Config::config_path();
    let mut cfg = Config::load_or_create();
    let entry = cfg.agent_hooks.entry(agent.to_string()).or_default();
    match flag {
        "metadata_enabled" => entry.metadata_enabled = value,
        "stop_enabled" => entry.stop_enabled = value,
        _ => unreachable!(),
    }
    if let Err(e) = cfg.save(&config_path) {
        eprintln!("FAIL: save config: {e}");
        std::process::exit(1);
    }
    let flag_label = match flag {
        "metadata_enabled" => "metadata handling",
        "stop_enabled" => "stop handling",
        _ => flag,
    };
    println!(
        "{} {}: {}",
        agent_label(agent),
        flag_label,
        if value { "ENABLED" } else { "DISABLED" }
    );
}

/// 按 config 添加/移除 agent hook entry。
fn reconcile_hooks() {
    let config = Config::load_or_create();
    let hook_binary = paths::hook_binary_path();
    let Some(hook_binary) = hook_binary else {
        eprintln!("FAIL: cannot locate agent-aspect-hook binary");
        std::process::exit(1);
    };
    let hook_str = hook_binary.display().to_string();

    let mut has_error = false;
    for strategy in hook_status::strategies() {
        let agent_id = strategy.agent_id();
        let agent_cfg = config.agent_hook_config(agent_id);

        let result = if agent_cfg.enabled {
            strategy.reconcile_add(&hook_str)
        } else {
            strategy.reconcile_remove()
        };

        let report = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("FAIL: {} reconcile error: {e}", strategy.label());
                has_error = true;
                continue;
            }
        };

        match report.action.as_str() {
            "added" => {
                println!(
                    "{}: added {} hook(s) ({})",
                    strategy.label(),
                    report.events_added.len(),
                    report.events_added.join(", ")
                );
            }
            "removed" => {
                println!(
                    "{}: removed {} hook(s) ({})",
                    strategy.label(),
                    report.events_removed.len(),
                    report.events_removed.join(", ")
                );
            }
            "unchanged" => {
                println!("{}: no changes", strategy.label());
            }
            _ => {}
        }
    }

    if has_error {
        std::process::exit(1);
    }
}

/// 校验 agent 名称。
fn validate_agent(agent: &str) {
    if !["claude_code", "codex_cli", "kimi_code"].contains(&agent) {
        eprintln!("unknown agent: {agent}");
        eprintln!("valid agents: claude_code, codex_cli, kimi_code");
        std::process::exit(1);
    }
}

/// agent 显示标签。
fn agent_label(agent: &str) -> &'static str {
    match agent {
        "claude_code" => "Claude Code",
        "codex_cli" => "Codex CLI",
        "kimi_code" => "Kimi Code",
        _ => "Unknown",
    }
}
