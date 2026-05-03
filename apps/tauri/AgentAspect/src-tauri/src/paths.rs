#![allow(dead_code)]

use std::path::PathBuf;

/// AgentAspectPaths — centralized path resolution for Agent Aspect data.
///
/// Prefers `~/.agent-aspect/` (new canonical location). Falls back to
/// `~/.checkpoint/` (legacy) when the new directory does not exist but the
/// legacy one does.

/// Primary data directory. Prefers `~/.agent-aspect`, falls back to
/// `~/.checkpoint` if only the legacy directory exists.
pub fn data_dir() -> PathBuf {
    let new_dir = home_dir().join(".agent-aspect");
    if new_dir.exists() {
        return new_dir;
    }
    let legacy_dir = home_dir().join(".checkpoint");
    if legacy_dir.exists() {
        return legacy_dir;
    }
    new_dir
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
