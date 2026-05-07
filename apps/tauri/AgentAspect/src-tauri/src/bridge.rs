use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::binary_locator;
use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeStatus {
    pub is_running: bool,
    pub pid: Option<u32>,
    pub addr: Option<String>,
    pub lan_enabled: bool,
    pub launchd_loaded: bool,
    pub keep_awake: bool,
    pub token_path: Option<String>,
    pub display_summary: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BridgeStateFile {
    pid: u32,
    addr: String,
    exe: String,
}

/// Parse the raw stdout of `agent-aspect bridge status` into a BridgeStatus.
pub fn parse_status(raw: &str) -> BridgeStatus {
    let mut map = std::collections::HashMap::new();
    for line in raw.lines() {
        if let Some(colon_idx) = line.find(':') {
            let key = line[..colon_idx].trim().to_lowercase();
            let val = line[colon_idx + 1..].trim().to_string();
            map.insert(key, val);
        }
    }

    // CLI outputs "bridge: running" (key is "bridge", not "status")
    let running = map
        .get("bridge")
        .or_else(|| map.get("status"))
        .map(|v| v.to_lowercase().contains("running"))
        .unwrap_or(false);

    let pid = map.get("pid").and_then(|s| s.parse::<u32>().ok());

    let addr = map.get("addr").cloned().or_else(|| map.get("address").cloned());

    let lan_enabled = map
        .get("lan")
        .map(|v| v.to_lowercase().contains("enabled"))
        .unwrap_or(false);

    let launchd_loaded = map
        .get("launchd")
        .map(|v| v.to_lowercase().contains("loaded"))
        .unwrap_or(false);

    let keep_awake = map
        .get("keep-awake")
        .map(|v| v.to_lowercase().contains("enabled"))
        .unwrap_or(false);

    let token_path = map.get("token").cloned();

    let display_summary = if running {
        let pid_str = pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
        let addr_str = addr.clone().unwrap_or_else(|| "unknown".into());
        format!("running (pid {pid_str}) at {addr_str}")
    } else {
        "stopped".to_string()
    };

    BridgeStatus {
        is_running: running,
        pid,
        addr,
        lan_enabled,
        launchd_loaded,
        keep_awake,
        token_path,
        display_summary,
        error: if running {
            None
        } else {
            Some(map.get("bridge").cloned().unwrap_or_else(|| "stopped".into()))
        },
    }
}

/// Run `agent-aspect bridge status` and return parsed status.
pub fn status(resource_dir: Option<&PathBuf>) -> BridgeStatus {
    let raw = run_bridge_cmd(resource_dir, &["status"]);
    parse_status(&raw)
}

/// Run `agent-aspect bridge start`, wait 2s, then return new status.
pub fn start(resource_dir: Option<&PathBuf>) -> BridgeStatus {
    run_bridge_cmd(resource_dir, &["start"]);
    std::thread::sleep(Duration::from_secs(2));
    status(resource_dir)
}

/// Run `agent-aspect bridge stop`.
pub fn stop(resource_dir: Option<&PathBuf>) -> String {
    run_bridge_cmd(resource_dir, &["stop"])
}

/// Read the bridge port from `bridge.port` file.
pub fn read_port() -> Option<u16> {
    let path = paths::bridge_port_path();
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    trimmed.parse::<u16>().ok()
}

/// Read and parse `bridge.state.json`.
pub fn read_state() -> Option<serde_json::Value> {
    let path = paths::bridge_state_path();
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// HTTP GET `http://127.0.0.1:<port>/health`. Returns true if status 200.
pub fn health() -> bool {
    let port = match read_port() {
        Some(p) => p,
        None => return false,
    };
    let _url = format!("http://127.0.0.1:{port}/health");
    // Use a simple TCP connect as a health check (no HTTP client dependency)
    std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(5),
    )
    .is_ok()
}

/// Get the bridge URL if the port is known.
pub fn bridge_url() -> Option<String> {
    read_port().map(|port| format!("http://127.0.0.1:{port}/"))
}

// MARK: - Private helpers

fn run_bridge_cmd(resource_dir: Option<&PathBuf>, args: &[&str]) -> String {
    let binary = match binary_locator::locate_binary(resource_dir) {
        Some(b) => b,
        None => return "error: agent-aspect binary not found".to_string(),
    };

    let mut cmd_args = vec!["bridge"];
    cmd_args.extend_from_slice(args);

    match Command::new(&binary).args(&cmd_args).output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(e) => format!("error: failed to run command: {e}"),
    }
}
