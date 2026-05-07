//! 文件系统路径 — DB、配置、PID、socket、launchd plist 等标准位置。
//!
//! M44 后只读写 `~/.agent-aspect/`，不再回退旧目录，避免 socket、token、
//! config 和 state 分裂成两套运行身份。

use std::path::PathBuf;

const AGENT_ASPECT_DIR: &str = ".agent-aspect";
const SOCKET_FILE: &str = "ipc.sock";
const AUDIT_DB_FILE: &str = "audit.db";
const CONFIG_FILE: &str = "config.toml";
const STATE_FILE: &str = "state.json";

/// 获取可靠的 home 目录。
///
/// 策略：
/// 1. $HOME 环境变量存在且非空 → 使用（测试环境通过此方式隔离）
/// 2. 否则通过 getpwuid(getuid()) 获取真实用户目录
/// 3. 都失败 → 回退到 /tmp
fn home_dir() -> PathBuf {
    let env_home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
    if let Some(h) = env_home {
        return PathBuf::from(h);
    }

    #[cfg(unix)]
    {
        // Safety: getpwuid returns a thread-local or static buffer on success, null on failure.
        // We read pw_dir immediately and convert to owned String before the pointer can be reused.
        let pw_home = unsafe {
            libc::getpwuid(libc::getuid()).as_ref().and_then(|pw| {
                let c_str = std::ffi::CStr::from_ptr(pw.pw_dir).to_bytes();
                std::str::from_utf8(c_str)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(String::from)
            })
        };
        if let Some(ph) = pw_home {
            return PathBuf::from(ph);
        }
    }

    PathBuf::from("/tmp")
}

/// 基础目录：唯一使用 `~/.agent-aspect/`。
fn base_dir() -> PathBuf {
    home_dir().join(AGENT_ASPECT_DIR)
}

fn join_in_base(file_name: &str) -> PathBuf {
    base_dir().join(file_name)
}

/// Unix domain socket 路径（用于 CLI ↔ daemon IPC）。
pub fn socket_path() -> PathBuf {
    join_in_base(SOCKET_FILE)
}

/// SQLite 审计数据库路径。
pub fn audit_db_path() -> PathBuf {
    join_in_base(AUDIT_DB_FILE)
}

/// TOML 配置文件路径。
pub fn config_path() -> PathBuf {
    join_in_base(CONFIG_FILE)
}

/// daemon 运行时状态文件路径。
pub fn state_path() -> PathBuf {
    join_in_base(STATE_FILE)
}

/// Agent Aspect 数据目录。
pub fn agent_aspect_dir() -> PathBuf {
    base_dir()
}

/// daemon 日志路径。
pub fn daemon_log_path() -> PathBuf {
    join_in_base("agent-aspectd.log")
}

pub fn daemon_stdout_log_path() -> PathBuf {
    join_in_base("agent-aspectd.stdout.log")
}

pub fn daemon_stderr_log_path() -> PathBuf {
    join_in_base("agent-aspectd.stderr.log")
}

/// launchd plist 路径 — 用于 macOS 自动启动 daemon。
pub fn launchd_plist_path() -> PathBuf {
    home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join("com.agent-aspect.daemon.plist")
}

/// bridge 的 launchd plist 路径。
pub fn bridge_launchd_plist_path() -> PathBuf {
    home_dir()
        .join("Library")
        .join("LaunchAgents")
        .join("com.agent-aspect.bridge.plist")
}

/// bridge 认证 token 文件。
pub fn bridge_token_path() -> PathBuf {
    join_in_base("bridge.token")
}

/// bridge 默认用户密码文件。
pub fn bridge_password_path() -> PathBuf {
    join_in_base("bridge.password")
}

/// relay macOS 端 token。
pub fn relay_mac_token_path() -> PathBuf {
    join_in_base("relay.mac_token")
}

/// relay 客户端 token。
pub fn relay_client_token_path() -> PathBuf {
    join_in_base("relay.client_token")
}

/// bridge 动态端口文件（bridge 启动时写入实际监听端口）。
pub fn bridge_port_path() -> PathBuf {
    join_in_base("bridge.port")
}

/// relay 初始配对 token。
pub fn relay_setup_token_path() -> PathBuf {
    join_in_base("relay.setup_token")
}

/// bridge 运行时状态 JSON。
pub fn bridge_state_path() -> PathBuf {
    join_in_base("bridge.state.json")
}

/// Claude Code settings.json — 用于注入 hook 配置。
pub fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

/// Codex CLI hooks.json — 用于注入 hook 配置。
pub fn codex_hooks_path() -> PathBuf {
    home_dir().join(".codex").join("hooks.json")
}

/// Kimi Code config.toml — 用于读取/注入 hook 配置。
pub fn kimi_config_path() -> PathBuf {
    home_dir().join(".kimi").join("config.toml")
}
