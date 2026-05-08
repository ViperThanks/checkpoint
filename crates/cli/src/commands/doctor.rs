//! `agent-aspect doctor` — 安装健康检查，逐项验证所有依赖和运行状态。
//!
//! 检查项覆盖三大部分：
//! - 二进制文件：agent-aspectd / agent-aspect-hook / agent-aspect-bridge 是否在 PATH 可达
//! - 运行时状态：daemon 进程存活、Unix socket 可连、state.json / audit.db 可读
//! - 集成配置：Claude hooks 是否引用了 agent-aspect-hook
//!
//! 输出格式：`[ OK ] label message`，任何 FAIL 项导致退出码 1。

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::config::Config;
use agent_aspect_core::hook_status;
use agent_aspect_core::paths;

use super::bridge::load_and_verify_state;
use super::helpers::bin_dir;

/// 单项检查的结果等级。WARN 不算致命，FAIL 会导致最终退出码 1。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

pub struct CheckResult {
    pub status: CheckStatus,
    pub label: String,
    pub message: String,
}

/// 执行全部 11 项健康检查并打印结果。
///
/// 任何 FAIL 会导致 `exit(1)`，WARN 只是提示。
pub fn cmd_doctor() {
    let results = vec![
        check_binary("agent-aspectd"),
        check_binary("agent-aspect-hook"),
        check_binary("agent-aspect-bridge"),
        check_daemon_running(),
        check_socket_connectable(),
        check_config_toml(),
        check_audit_db(),
        check_state_json(),
        check_daemon_log(),
        check_bridge_status(),
        check_claude_hooks(),
        check_hook_status(),
    ];

    let mut has_fail = false;
    for r in &results {
        let tag = match r.status {
            CheckStatus::Ok => "OK",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => {
                has_fail = true;
                "FAIL"
            }
        };
        println!("[{:4}] {:30} {}", tag, r.label, r.message);
    }

    if has_fail {
        std::process::exit(1);
    }
}

/// 检查指定二进制是否存在于与当前可执行文件同目录下。
/// CLI 工具链约定：agent-aspect / agent-aspectd / agent-aspect-hook / agent-aspect-bridge
/// 都安装在同一目录。
fn check_binary(name: &str) -> CheckResult {
    let Some(dir) = bin_dir() else {
        return CheckResult {
            status: CheckStatus::Fail,
            label: format!("binary: {name}"),
            message: "cannot determine binary directory".into(),
        };
    };
    let path = dir.join(name);
    if path.exists() {
        CheckResult {
            status: CheckStatus::Ok,
            label: format!("binary: {name}"),
            message: path.display().to_string(),
        }
    } else {
        CheckResult {
            status: CheckStatus::Fail,
            label: format!("binary: {name}"),
            message: format!("not found at {}", path.display()),
        }
    }
}

/// 检查 daemon 进程是否存活。
/// 读取 state.json 中的 pid，用 `kill(pid, 0)` 探测（不发送信号）。
/// 如果 pid 不存在说明 state.json 是过期残留。
fn check_daemon_running() -> CheckResult {
    let state_path = paths::state_path();
    if !state_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "daemon running".into(),
            message: "state.json not found".into(),
        };
    }

    let state = std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());

    let Some(state) = state else {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "daemon running".into(),
            message: "state.json unreadable".into(),
        };
    };

    let pid = state["pid"].as_u64();
    let mode = state["mode"].as_str().unwrap_or("unknown");

    match pid {
        Some(pid) => {
            // kill(pid, 0) 检查进程是否存在
            let running = unsafe { libc::kill(pid as i32, 0) == 0 };
            if running {
                CheckResult {
                    status: CheckStatus::Ok,
                    label: "daemon running".into(),
                    message: format!("pid={pid}, mode={mode}"),
                }
            } else {
                CheckResult {
                    status: CheckStatus::Fail,
                    label: "daemon running".into(),
                    message: format!("pid={pid} not running (stale state.json)"),
                }
            }
        }
        None => CheckResult {
            status: CheckStatus::Warn,
            label: "daemon running".into(),
            message: "state.json missing pid".into(),
        },
    }
}

/// 检查 daemon 的 Unix socket 是否可连接。
/// 即使 daemon 进程存活，如果 socket 文件不存在也说明有问题。
fn check_socket_connectable() -> CheckResult {
    let sock_path = paths::socket_path();
    if !sock_path.exists() {
        return CheckResult {
            status: CheckStatus::Fail,
            label: "unix socket".into(),
            message: format!("{} does not exist", sock_path.display()),
        };
    }
    match std::os::unix::net::UnixStream::connect(&sock_path) {
        Ok(_) => CheckResult {
            status: CheckStatus::Ok,
            label: "unix socket".into(),
            message: sock_path.display().to_string(),
        },
        Err(e) => CheckResult {
            status: CheckStatus::Fail,
            label: "unix socket".into(),
            message: format!("connect failed: {e}"),
        },
    }
}

/// 检查 config.toml 是否存在且可解析。
/// 不存在不算失败（daemon 首次启动会自动创建），但解析失败算 FAIL。
fn check_config_toml() -> CheckResult {
    let config_path = paths::config_path();
    if !config_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "config.toml".into(),
            message: "not found (daemon will create on start)".into(),
        };
    }
    match Config::load(&config_path) {
        Ok(cfg) => CheckResult {
            status: CheckStatus::Ok,
            label: "config.toml".into(),
            message: format!("mode={}", cfg.mode),
        },
        Err(e) => CheckResult {
            status: CheckStatus::Fail,
            label: "config.toml".into(),
            message: format!("parse error: {e}"),
        },
    }
}

/// 检查 audit.db 是否存在且可查询。
/// 同时输出 events / decisions 计数，帮助用户快速判断是否有审计数据。
fn check_audit_db() -> CheckResult {
    let db_path = paths::audit_db_path();
    if !db_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "audit.db".into(),
            message: "not found (daemon will create on start)".into(),
        };
    }
    match AuditStore::open(&db_path) {
        Ok(store) => match (store.event_count(), store.decision_count()) {
            (Ok(events), Ok(decisions)) => CheckResult {
                status: CheckStatus::Ok,
                label: "audit.db".into(),
                message: format!("{events} events, {decisions} decisions"),
            },
            (Err(e), _) | (_, Err(e)) => CheckResult {
                status: CheckStatus::Fail,
                label: "audit.db".into(),
                message: format!("query error: {e}"),
            },
        },
        Err(e) => CheckResult {
            status: CheckStatus::Fail,
            label: "audit.db".into(),
            message: format!("open error: {e}"),
        },
    }
}

/// 检查 state.json 的完整性。
/// 合法的 state.json 必须同时包含 mode 和 pid 两个字段。
fn check_state_json() -> CheckResult {
    let state_path = paths::state_path();
    if !state_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "state.json".into(),
            message: "not found (daemon not started yet)".into(),
        };
    }
    match std::fs::read_to_string(&state_path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    {
        Some(v) => {
            let mode = v["mode"].as_str().unwrap_or("missing");
            let pid = v["pid"]
                .as_u64()
                .map(|p| p.to_string())
                .unwrap_or_else(|| "missing".into());
            if mode != "missing" && pid != "missing" {
                CheckResult {
                    status: CheckStatus::Ok,
                    label: "state.json".into(),
                    message: format!("pid={pid}, mode={mode}"),
                }
            } else {
                CheckResult {
                    status: CheckStatus::Warn,
                    label: "state.json".into(),
                    message: format!("incomplete (mode={mode}, pid={pid})"),
                }
            }
        }
        None => CheckResult {
            status: CheckStatus::Fail,
            label: "state.json".into(),
            message: "cannot parse".into(),
        },
    }
}

/// 检查 daemon 日志文件是否存在及大小。
/// 日志文件是 daemon 运行后自动创建的，首次运行前不存在是正常情况。
fn check_daemon_log() -> CheckResult {
    let log_path = paths::daemon_log_path();
    if !log_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "agent-aspectd.log".into(),
            message: "not found (daemon will create on start)".into(),
        };
    }
    match std::fs::metadata(&log_path) {
        Ok(meta) => {
            let size = meta.len();
            CheckResult {
                status: CheckStatus::Ok,
                label: "agent-aspectd.log".into(),
                message: format!("{} ({} bytes)", log_path.display(), size),
            }
        }
        Err(e) => CheckResult {
            status: CheckStatus::Fail,
            label: "agent-aspectd.log".into(),
            message: format!("stat error: {e}"),
        },
    }
}

/// 检查 bridge 进程是否存活。
/// 复用 bridge.rs 的 `load_and_verify_state`，它同时验证 pid 存活和进程身份。
fn check_bridge_status() -> CheckResult {
    let state_path = paths::bridge_state_path();
    if !state_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "bridge".into(),
            message: "not running".into(),
        };
    }
    match load_and_verify_state() {
        Some((state, true)) => CheckResult {
            status: CheckStatus::Ok,
            label: "bridge".into(),
            message: format!("running (pid {}, {})", state.pid, state.addr),
        },
        _ => CheckResult {
            status: CheckStatus::Warn,
            label: "bridge".into(),
            message: "stale state (cleaned up)".into(),
        },
    }
}

/// 检查 Claude Code 的 settings.json 是否配置了 agent-aspect-hook。
/// 支持 Claude Code 的嵌套结构 `hooks.PreToolUse[].hooks[].command`
/// 和扁平结构 `hooks.PreToolUse[].command` 两种格式。
fn check_claude_hooks() -> CheckResult {
    let settings_path = paths::claude_settings_path();
    if !settings_path.exists() {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "Claude hooks config".into(),
            message: format!("{} not found", settings_path.display()),
        };
    }

    let content = match std::fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                status: CheckStatus::Fail,
                label: "Claude hooks config".into(),
                message: format!("read error: {e}"),
            };
        }
    };

    let val: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return CheckResult {
                status: CheckStatus::Fail,
                label: "Claude hooks config".into(),
                message: format!("parse error: {e}"),
            };
        }
    };

    let hooks = val.get("hooks");
    let Some(hooks) = hooks else {
        return CheckResult {
            status: CheckStatus::Warn,
            label: "Claude hooks config".into(),
            message: "settings.json exists but has no 'hooks' key".into(),
        };
    };

    // 真实 Claude Code hooks 结构：hooks.PreToolUse[].hooks[].command
    // 也兼容扁平结构：hooks.PreToolUse[].command
    let has_hook = hooks
        .as_object()
        .map(|obj| {
            obj.values().any(|v| {
                v.as_array().map_or(false, |arr| {
                    arr.iter().any(|item| contains_agent_aspect_hook(item))
                })
            })
        })
        .unwrap_or(false);

    if has_hook {
        CheckResult {
            status: CheckStatus::Ok,
            label: "Claude hooks config".into(),
            message: "agent-aspect-hook found in hooks".into(),
        }
    } else {
        CheckResult {
            status: CheckStatus::Warn,
            label: "Claude hooks config".into(),
            message: "hooks configured but agent-aspect-hook not referenced".into(),
        }
    }
}

// 递归查找 item 中是否包含 agent-aspect-hook 引用
// 真实结构：item.hooks[].command（嵌套）
// 扁平结构：item.command（兼容）
fn contains_agent_aspect_hook(item: &serde_json::Value) -> bool {
    // 扁平：item.command
    if let Some(cmd) = item.get("command").and_then(|c| c.as_str()) {
        if cmd.contains("agent-aspect-hook") {
            return true;
        }
    }
    // 嵌套：item.hooks[].command
    if let Some(hooks) = item.get("hooks").and_then(|h| h.as_array()) {
        for h in hooks {
            if let Some(cmd) = h.get("command").and_then(|c| c.as_str()) {
                if cmd.contains("agent-aspect-hook") {
                    return true;
                }
            }
        }
    }
    false
}

/// 检查所有 agent 的 hook 状态（enabled、installed events、missing events）。
fn check_hook_status() -> CheckResult {
    let config = Config::load_or_create();
    let hook_binary = paths::hook_binary_path();

    let status = hook_status::read_full_status(&config, hook_binary.as_ref());

    let all_ok = status.agents.iter().all(|a| a.status.as_str() == "ok");

    let details: Vec<String> = status
        .agents
        .iter()
        .map(|a| {
            let flag = match a.status.as_str() {
                "ok" => "ok",
                "disabled" => "disabled",
                "partial" => "partial",
                "missing_config" => "no-config",
                "missing_hook_binary" => "no-binary",
                other => other,
            };
            format!(
                "{}:{}({})",
                a.label,
                flag,
                if a.enabled { "en" } else { "dis" }
            )
        })
        .collect();

    if all_ok {
        CheckResult {
            status: CheckStatus::Ok,
            label: "agent hooks".into(),
            message: details.join(", "),
        }
    } else {
        let issues: Vec<&str> = status
            .agents
            .iter()
            .filter(|a| a.status.as_str() != "ok")
            .map(|a| a.label.as_str())
            .collect();
        CheckResult {
            status: CheckStatus::Warn,
            label: "agent hooks".into(),
            message: format!("{}: run `agent-aspect hooks status` for details", issues.join(", ")),
        }
    }
}
