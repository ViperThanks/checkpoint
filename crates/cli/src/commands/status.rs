//! `agent-aspect status` — 显示 daemon 运行状态、当前模式和审计计数。
//!
//! 不经过 IPC，直接探测 Unix socket 是否可连接来判断 daemon 存活；
//! 模式从 daemon 写的 state.json 读取（而非本进程 env），保证与 daemon 实际状态一致。

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::paths;

/// 显示 agent-aspect 系统当前状态概览。
///
/// 输出三行信息：
/// 1. `daemon: running/stopped` — 通过尝试连接 Unix socket 判断
/// 2. `mode: <mode> (from state.json)` — 从 daemon 维护的 state.json 读取
/// 3. `events:` / `decisions:` — audit.db 中的记录计数
pub fn cmd_status() {
    let sock_path = paths::socket_path();
    let db_path = paths::audit_db_path();

    // daemon alive check
    let alive = std::os::unix::net::UnixStream::connect(&sock_path).is_ok();
    println!("daemon: {}", if alive { "running" } else { "stopped" });

    // mode: 从 daemon 写的 state.json 读，不读自己进程的 env
    let state_path = paths::state_path();
    let mode_str = if state_path.exists() {
        std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v["mode"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        "not running".to_string()
    };
    println!("mode: {mode_str} (from state.json)");

    // counts from audit.db
    if db_path.exists() {
        if let Ok(store) = AuditStore::open(&db_path) {
            match store.event_count() {
                Ok(n) => println!("events: {n}"),
                Err(e) => eprintln!("events: error ({e})"),
            }
            match store.decision_count() {
                Ok(n) => println!("decisions: {n}"),
                Err(e) => eprintln!("decisions: error ({e})"),
            }
        } else {
            eprintln!("audit.db: cannot open");
        }
    } else {
        println!("audit.db: not found");
    }
}
