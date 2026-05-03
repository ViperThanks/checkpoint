//! 文件系统路径 — DB、配置、PID、socket、launchd plist 等标准位置。
//!
//! 路径读取优先级：
//! 1. `~/.agent-aspect/`（新）
//! 2. `~/.checkpoint/`（旧兼容）
//! 3. `/tmp`（$HOME 缺失时 fallback）
//!
//! 新安装写入 `~/.agent-aspect/`；若只存在旧目录，则沿用旧目录以保持兼容。

use std::path::PathBuf;

const AGENT_ASPECT_DIR: &str = ".agent-aspect";
const LEGACY_DIR: &str = ".checkpoint";
const SOCKET_FILE: &str = "ipc.sock";
const AUDIT_DB_FILE: &str = "audit.db";
const CONFIG_FILE: &str = "config.toml";
const STATE_FILE: &str = "state.json";

/// 基础目录。优先 `~/.agent-aspect/`，不存在时回退 `~/.checkpoint/`。
fn base_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let new_dir = PathBuf::from(&home).join(AGENT_ASPECT_DIR);
    if new_dir.exists() {
        return new_dir;
    }
    let legacy_dir = PathBuf::from(&home).join(LEGACY_DIR);
    if legacy_dir.exists() {
        return legacy_dir;
    }
    // 两者都不存在：返回新目录（写入时使用）
    new_dir
}

fn join_in_base(file_name: &str) -> PathBuf {
    base_dir().join(file_name)
}

/// 是否正在使用旧目录（doctor 用来提示迁移）。
pub fn using_legacy_dir() -> bool {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let new_dir = PathBuf::from(&home).join(AGENT_ASPECT_DIR);
    let legacy_dir = PathBuf::from(&home).join(LEGACY_DIR);
    !new_dir.exists() && legacy_dir.exists()
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

/// 当前活跃的数据目录。
pub fn checkpoint_dir() -> PathBuf {
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join("com.agent-aspect.daemon.plist")
}

/// bridge 的 launchd plist 路径。
pub fn bridge_launchd_plist_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".claude").join("settings.json")
}

/// Codex CLI hooks.json — 用于注入 hook 配置。
pub fn codex_hooks_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".codex").join("hooks.json")
}

/// Kimi Code config.toml — 用于读取/注入 hook 配置。
pub fn kimi_config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".kimi").join("config.toml")
}
