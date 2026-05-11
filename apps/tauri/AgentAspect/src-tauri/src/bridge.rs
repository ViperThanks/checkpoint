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

    let addr = map
        .get("addr")
        .cloned()
        .or_else(|| map.get("address").cloned());

    let lan_enabled = map
        .get("lan")
        .map(|v| v.to_lowercase().contains("enabled"))
        .unwrap_or(false);

    let launchd_loaded = map
        .get("launchd")
        .map(|v| v.eq_ignore_ascii_case("loaded"))
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
            Some(
                map.get("bridge")
                    .cloned()
                    .unwrap_or_else(|| "stopped".into()),
            )
        },
    }
}

/// Run `agent-aspect bridge status` and return parsed status.
pub fn status(resource_dir: Option<&PathBuf>) -> BridgeStatus {
    if binary_locator::locate_binary(resource_dir).is_none() {
        return runtime_status(Some("agent-aspect binary not found"));
    }

    let raw = run_bridge_cmd(resource_dir, &["status"]);
    let parsed = parse_status(&raw);
    if parsed.is_running || !health() {
        return parsed;
    }

    // A stale or unavailable CLI must not hide a reachable bridge from the
    // desktop shell. The HTTP health endpoint is the runtime authority here.
    runtime_status(parsed.error.as_deref())
}

/// Run `agent-aspect bridge start`, wait 2s, then return new status.
pub fn start(resource_dir: Option<&PathBuf>) -> BridgeStatus {
    if binary_locator::locate_binary(resource_dir).is_some() {
        run_bridge_cmd(resource_dir, &["start"]);
    } else if !health() {
        start_bridge_direct(resource_dir);
    }
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
#[allow(dead_code)]
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

fn runtime_status(error_hint: Option<&str>) -> BridgeStatus {
    let state = read_state_file();
    let running = health();
    let pid = state.as_ref().map(|s| s.pid);
    let addr = state
        .as_ref()
        .map(|s| s.addr.clone())
        .or_else(|| read_port().map(|port| format!("127.0.0.1:{port}")));
    let launchd = launchd_info();
    let token_path = paths::bridge_token_path().to_string_lossy().to_string();
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
        lan_enabled: false,
        launchd_loaded: launchd.loaded,
        keep_awake: launchd.keep_awake,
        token_path: Some(token_path),
        display_summary,
        error: if running {
            None
        } else {
            Some(error_hint.unwrap_or("stopped").to_string())
        },
    }
}

fn read_state_file() -> Option<BridgeStateFile> {
    let data = std::fs::read_to_string(paths::bridge_state_path()).ok()?;
    serde_json::from_str(&data).ok()
}

struct LaunchdInfo {
    loaded: bool,
    keep_awake: bool,
}

fn launchd_info() -> LaunchdInfo {
    let uid = std::env::var("UID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(current_uid)
        .unwrap_or(0);
    let target = format!("gui/{uid}/com.agent-aspect.bridge");
    let output = Command::new("/bin/launchctl")
        .args(["print", &target])
        .output();

    let Ok(output) = output else {
        return LaunchdInfo {
            loaded: false,
            keep_awake: false,
        };
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let text = format!("{stdout}\n{stderr}");

    LaunchdInfo {
        loaded: output.status.success() && text.contains("com.agent-aspect.bridge"),
        keep_awake: text.contains("/usr/bin/caffeinate") || text.contains("\n\t\t-s\n"),
    }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    let output = Command::new("/usr/bin/id").arg("-u").output().ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

fn start_bridge_direct(resource_dir: Option<&PathBuf>) {
    let Some(binary) = binary_locator::locate_bridge_binary(resource_dir) else {
        return;
    };
    let _ = Command::new(binary).spawn();
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_status_reads_running_bridge() {
        let status = parse_status(
            r#"bridge: running
pid: 42
addr: 127.0.0.1:7676
LAN: disabled
launchd: loaded
keep-awake: enabled
token: /Users/example/.agent-aspect/bridge.token"#,
        );

        assert!(status.is_running);
        assert_eq!(status.pid, Some(42));
        assert_eq!(status.addr.as_deref(), Some("127.0.0.1:7676"));
        assert!(status.launchd_loaded);
        assert!(status.keep_awake);
    }

    #[test]
    fn parse_status_does_not_treat_not_loaded_as_loaded() {
        let status = parse_status(
            r#"bridge: stopped
LAN: disabled
launchd: not loaded
keep-awake: disabled"#,
        );

        assert!(!status.is_running);
        assert!(!status.launchd_loaded);
        assert!(!status.keep_awake);
    }
}
