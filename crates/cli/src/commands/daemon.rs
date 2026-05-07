//! `agent-aspect daemon` — 管理 agent-aspectd daemon 进程的生命周期。
//!
//! daemon 是核心 IPC 服务：接收 hook 请求 → normalize → rule engine 裁决 → 审计。
//! 本模块提供 start/stop/restart/status 四个子命令。
//!
//! 进程管理依赖 `process_guard` crate 做单例守护（PID state 文件 + kill 验证）。

use agent_aspect_core::{paths, process_guard};

use super::helpers::bin_dir;

/// Daemon 运行时状态，从 `~/.agent-aspect/state.json` 反序列化。
#[derive(serde::Deserialize)]
struct DaemonState {
    pid: u32,
    #[allow(dead_code)]
    mode: String,
    #[allow(dead_code)]
    exe: String,
}

/// daemon 子命令入口。
pub fn cmd_daemon(sub: Option<&str>) {
    match sub {
        Some("start") => daemon_start(),
        Some("stop") => daemon_stop(),
        Some("restart") => daemon_restart(),
        Some("status") => daemon_status(),
        _ => {
            eprintln!("usage: agent-aspect daemon <start|stop|restart|status>");
            std::process::exit(1);
        }
    }
}

/// 从 state.json 读取 daemon 状态。不存在或解析失败返回 None。
fn load_state() -> Option<DaemonState> {
    let path = paths::state_path();
    if !path.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 启动 daemon 进程。
///
/// 流程：
/// 1. 先用 process_guard 清理可能残留的旧进程
/// 2. 定位 agent-aspectd 二进制（与当前 CLI 同目录）
/// 3. spawn 后等待 500ms 让 daemon 写 state 文件
/// 4. 验证进程仍然存活
fn daemon_start() {
    match process_guard::stop_existing(&paths::state_path(), "agent-aspectd") {
        process_guard::StopResult::Stopped(pid) => {
            println!("replaced previous daemon (pid {pid})");
        }
        process_guard::StopResult::WrongProcess { pid, actual } => {
            eprintln!("warning: stale daemon state pointed to pid {pid} ({actual}); not killed");
        }
        process_guard::StopResult::StaleState | process_guard::StopResult::NotFound => {}
    }

    let Some(dir) = bin_dir() else {
        eprintln!("FAIL: cannot determine binary directory");
        std::process::exit(1);
    };
    let daemon_bin = dir.join("agent-aspectd");
    if !daemon_bin.exists() {
        eprintln!("FAIL: agent-aspectd not found at {}", daemon_bin.display());
        std::process::exit(1);
    }

    let mut cmd = std::process::Command::new(&daemon_bin);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("HOME", std::env::var("HOME").unwrap_or_default());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: spawn agent-aspectd: {e}");
            std::process::exit(1);
        }
    };

    let pid = child.id();

    // 等 daemon 写 state 文件
    std::thread::sleep(std::time::Duration::from_millis(500));

    if !process_guard::is_alive(pid) {
        eprintln!("FAIL: daemon process exited immediately");
        std::process::exit(1);
    }

    println!("daemon started (pid {pid})");
}

/// 停止 daemon 进程。
/// 通过 `process_guard::stop_existing` 完成（它处理了 pid 验证和 state 清理）。
fn daemon_stop() {
    match process_guard::stop_existing(&paths::state_path(), "agent-aspectd") {
        process_guard::StopResult::Stopped(pid) => println!("daemon stopped (pid {pid})"),
        process_guard::StopResult::NotFound => println!("daemon not running (no state file)"),
        process_guard::StopResult::StaleState => {
            println!("daemon not running (stale state cleaned)")
        }
        process_guard::StopResult::WrongProcess { pid, actual } => {
            println!("daemon not stopped: state pid {pid} belongs to {actual}")
        }
    }
}

/// 先 stop 再 start。
fn daemon_restart() {
    daemon_stop();
    daemon_start();
}

/// 显示 daemon 当前状态。
/// 读 state.json 获取 pid，用 `process_guard::is_alive` 检查进程是否存活。
fn daemon_status() {
    let state = match load_state() {
        Some(s) => s,
        None => {
            println!("daemon: stopped");
            return;
        }
    };

    if process_guard::is_alive(state.pid) {
        println!("daemon: running (pid {})", state.pid);
    } else {
        println!("daemon: pid={} not running (stale state)", state.pid);
    }
}
