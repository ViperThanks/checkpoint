//! 单实例进程守护 — 防止 bridge/daemon 重复启动。
//!
//! 通过 JSON 状态文件中的 PID 检测旧进程：
//! - PID 不存在或状态文件损坏 → 清理后启动新实例
//! - PID 指向其他进程 → 不杀，只清理状态文件
//! - PID 指向同名进程 → SIGTERM 等待 → SIGKILL 强杀
//!
//! 核心不变量：不会杀掉非预期名称的进程

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopResult {
    NotFound,
    StaleState,
    WrongProcess { pid: u32, actual: String },
    Stopped(u32),
}

/// Read pid from a JSON state file, verify the process is the expected one,
/// and kill it gracefully (SIGTERM → wait → SIGKILL).
/// Returns the old pid if a process was killed.
pub fn kill_existing(state_path: &Path, expected_name: &str) -> Option<u32> {
    match stop_existing(state_path, expected_name) {
        StopResult::Stopped(pid) => Some(pid),
        _ => None,
    }
}

/// 停止旧进程：从状态 JSON 读取 PID，验证进程名，优雅关闭。
///
/// 安全保证：
/// - 只杀 expected_name 匹配的进程
/// - PID 不存在或状态损坏时只清理文件
/// - 其他进程名的 PID 永远不会被杀
pub fn stop_existing(state_path: &Path, expected_name: &str) -> StopResult {
    let raw = match std::fs::read_to_string(state_path) {
        Ok(raw) => raw,
        Err(_) => return StopResult::NotFound,
    };
    let state: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(state) => state,
        Err(_) => {
            std::fs::remove_file(state_path).ok();
            return StopResult::StaleState;
        }
    };
    let Some(pid) = state.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32) else {
        std::fs::remove_file(state_path).ok();
        return StopResult::StaleState;
    };

    if !is_alive(pid) {
        std::fs::remove_file(state_path).ok();
        return StopResult::StaleState;
    }

    let Some(actual) = process_name(pid) else {
        return StopResult::WrongProcess {
            pid,
            actual: "unknown".to_string(),
        };
    };
    if !process_name_matches(&actual, expected_name) {
        eprintln!("process_guard: pid {pid} is '{actual}' not '{expected_name}', leaving it alone");
        std::fs::remove_file(state_path).ok();
        return StopResult::WrongProcess { pid, actual };
    }

    kill_gracefully(pid);
    std::fs::remove_file(state_path).ok();
    StopResult::Stopped(pid)
}

/// 检查进程是否存活（用 kill(pid, 0) 探测）。
pub fn is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// 获取进程的可执行文件名。macOS 用 proc_pidpath，其他平台用 ps 命令。
fn process_name(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        let len = unsafe {
            libc::proc_pidpath(
                pid as i32,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as u32,
            )
        };
        if len > 0 {
            let path = std::str::from_utf8(&buf[..len as usize])
                .unwrap_or("")
                .trim();
            if !path.is_empty() {
                return std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
            }
        }
    }

    // Fallback: ps -p <pid> -o comm=
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if comm.is_empty() {
        None
    } else {
        std::path::Path::new(&comm)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    }
}

/// 判断实际进程名是否匹配期望名。
///
/// 改名迁移期允许旧 binary 名继续作为同一身份：
/// - agent-aspect-bridge ↔ checkpoint-bridge
/// - agent-aspectd ↔ checkpointd
///
/// 这只放宽 checkpoint 自身的新旧名称，不允许任意前缀匹配。
fn process_name_matches(actual: &str, expected: &str) -> bool {
    actual == expected
        || matches!(
            (expected, actual),
            ("agent-aspect-bridge", "checkpoint-bridge")
                | ("checkpoint-bridge", "agent-aspect-bridge")
                | ("agent-aspectd", "checkpointd")
                | ("checkpointd", "agent-aspectd")
        )
}

/// 优雅关闭：先 SIGTERM 等待最多 1 秒，未退出则 SIGKILL。
fn kill_gracefully(pid: u32) {
    eprintln!("process_guard: killing stale pid {pid} (SIGTERM)");
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if !is_alive(pid) {
            return;
        }
    }
    eprintln!("process_guard: pid {pid} still alive, sending SIGKILL");
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(200));
}

#[cfg(test)]
mod tests {
    use super::process_name_matches;

    #[test]
    fn process_name_matches_current_and_legacy_bridge_names() {
        assert!(process_name_matches(
            "agent-aspect-bridge",
            "agent-aspect-bridge"
        ));
        assert!(process_name_matches(
            "checkpoint-bridge",
            "agent-aspect-bridge"
        ));
        assert!(process_name_matches(
            "agent-aspect-bridge",
            "checkpoint-bridge"
        ));
    }

    #[test]
    fn process_name_matches_current_and_legacy_daemon_names() {
        assert!(process_name_matches("agent-aspectd", "agent-aspectd"));
        assert!(process_name_matches("checkpointd", "agent-aspectd"));
        assert!(process_name_matches("agent-aspectd", "checkpointd"));
    }

    #[test]
    fn process_name_matches_rejects_unrelated_names() {
        assert!(!process_name_matches("bash", "agent-aspect-bridge"));
        assert!(!process_name_matches(
            "checkpoint-relay",
            "agent-aspect-bridge"
        ));
        assert!(!process_name_matches(
            "checkpoint-bridge-helper",
            "checkpoint-bridge"
        ));
    }
}
