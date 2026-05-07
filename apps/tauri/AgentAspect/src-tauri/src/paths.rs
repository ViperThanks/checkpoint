#![allow(dead_code)]

use std::path::PathBuf;

/// AgentAspectPaths — centralized path resolution for Agent Aspect data.
///
/// M44 后只使用 `~/.agent-aspect/`，不再保留旧目录 fallback。
pub fn data_dir() -> PathBuf {
    home_dir().join(".agent-aspect")
}

pub fn bridge_port_path() -> PathBuf {
    data_dir().join("bridge.port")
}

pub fn bridge_state_path() -> PathBuf {
    data_dir().join("bridge.state.json")
}

pub fn bridge_token_path() -> PathBuf {
    data_dir().join("bridge.token")
}

pub fn bridge_password_path() -> PathBuf {
    data_dir().join("bridge.password")
}

pub fn daemon_log_path() -> PathBuf {
    data_dir().join("agent-aspectd.log")
}

pub fn audit_db_path() -> PathBuf {
    data_dir().join("audit.db")
}

fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}
