//! `agent-aspect bridge` — 管理 Bridge HTTP 服务器和 Relay 远程访问。
//!
//! Bridge 是本地 HTTP 控制面（默认 127.0.0.1:7676），提供 Web UI 和 REST API。
//! 子命令涵盖进程生命周期（start/stop/restart/status）、LaunchDaemon 集成
//! （install/uninstall/keep-awake）、LAN 暴露（expose/unexpose/pair）、token 管理
//! 和 relay 配置。
//!
//! 关键不变量：
//! - bridge state 文件记录 pid + exe 绝对路径，用于进程身份验证
//! - stop 流程先 SIGTERM，等 1 秒，未退出再 SIGKILL
//! - relay URL 必须以 `ws://` 或 `wss://` 开头

use checkpoint_core::{config::Config, paths, process_guard};

use super::helpers::{bin_dir, run_launchctl};

/// Bridge 的 launchd 服务标识。
pub const BRIDGE_PLIST_LABEL: &str = "com.agent-aspect.bridge";

/// Bridge 进程运行时状态，从 `~/.agent-aspect/bridge.state.json` 反序列化。
#[derive(serde::Deserialize)]
pub struct BridgeState {
    pub pid: u32,
    pub exe: String,
    pub addr: String,
    #[allow(dead_code)]
    pub started_at: String,
}

/// Bridge 子命令入口。
///
/// `sub` 是第二个位置参数（如 "start", "stop"），
/// `args` 是第三个及之后的参数（如 "--relay-url", URL）。
pub fn cmd_bridge(sub: Option<&str>, args: &[String]) {
    match sub {
        Some("start") => {
            apply_start_options(args);
            bridge_start();
        }
        Some("stop") => bridge_stop(),
        Some("status") => bridge_status(),
        Some("token") => bridge_token(args),
        Some("password") => {
            let sub = args.first().map(|s| s.as_str());
            match sub {
                None | Some("show") => bridge_password_show(),
                Some("reset") => bridge_password_reset(),
                Some("set") => bridge_password_set(),
                Some("init") => bridge_password_init(),
                Some(other) => {
                    eprintln!("unknown password subcommand: {other}");
                    eprintln!("run 'agent-aspect bridge help' for usage");
                    std::process::exit(1);
                }
            }
        }
        Some("restart") => {
            apply_start_options(args);
            bridge_restart();
        }
        Some("install") => bridge_install(args),
        Some("uninstall") => bridge_uninstall(),
        Some("pair") => bridge_pair(),
        Some("expose") => bridge_expose(),
        Some("unexpose") => bridge_unexpose(),
        Some("relay") => bridge_relay(args),
        Some("help") | Some("--help") | Some("-h") | None => bridge_help(),
        Some(other) => {
            eprintln!("unknown bridge command: {other}");
            eprintln!("run 'agent-aspect bridge help' for usage");
            std::process::exit(1);
        }
    }
}

fn bridge_help() {
    println!("agent-aspect bridge — manage local bridge and relay access");
    println!();
    println!("Usage:");
    println!("  agent-aspect bridge start [--relay-url <wss-url>]");
    println!("  agent-aspect bridge restart [--relay-url <wss-url>]");
    println!("  agent-aspect bridge stop");
    println!("  agent-aspect bridge status");
    println!("  agent-aspect bridge token [--bridge|--relay-client|--relay-mac]");
    println!("  agent-aspect bridge password [show|reset|set|init]");
    println!("  agent-aspect bridge relay <status|set-url|unset-url|token|help>");
    println!("  agent-aspect bridge install [--keep-awake]");
    println!("  agent-aspect bridge uninstall");
    println!("  agent-aspect bridge pair|expose|unexpose");
    println!();
    println!("Relay examples:");
    println!("  agent-aspect bridge start --relay-url wss://relay.example.com/ws");
    println!("  agent-aspect bridge token --relay-client");
    println!("  agent-aspect bridge relay status");
    println!("  agent-aspect bridge relay set-url wss://relay.example.com/ws");
    println!("  agent-aspect bridge relay token --client");
    println!();
    println!("Notes:");
    println!("  Relay URLs are stored in ~/.agent-aspect/config.toml.");
    println!("  Relay tokens are stored locally and are never checked into the repo.");
    println!(
        "  --keep-awake keeps Mac reachable while locked by preventing system sleep on AC power."
    );
}

/// 解析 start/restart 命令的附加选项（`--relay-url`, `--no-relay`）。
/// 这些选项在启动 bridge 之前写入 config.toml，bridge 进程启动时读取。
fn apply_start_options(args: &[String]) {
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--relay-url" => {
                let Some(url) = args.get(i + 1) else {
                    eprintln!("FAIL: --relay-url requires a value");
                    std::process::exit(1);
                };
                set_relay_url(url);
                i += 2;
            }
            "--no-relay" => {
                unset_relay_url();
                i += 1;
            }
            "--help" | "-h" => {
                bridge_help();
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown bridge option: {other}");
                eprintln!("run 'agent-aspect bridge help' for usage");
                std::process::exit(1);
            }
        }
    }
}

/// 检查 pid 是否存活且确实是 agent-aspect-bridge 进程。
///
/// 返回 `(alive, verified)`:
/// - `alive=true` — pid 对应的进程存在
/// - `verified=true` — 进程的绝对路径与 `expected_exe` 完全匹配
///
/// macOS 优先使用 `proc_pidpath` 做全路径验证；失败时回退到 `ps -o comm=` 做 basename 比较。
pub fn verify_bridge_pid(pid: u32, expected_exe: &str) -> (bool, bool) {
    if unsafe { libc::kill(pid as i32, 0) != 0 } {
        return (false, false);
    }

    // On macOS, use proc_pidpath for full executable path verification.
    let expected_path = std::path::Path::new(expected_exe);

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
            let actual = std::str::from_utf8(&buf[..len as usize])
                .unwrap_or("")
                .trim();
            if !actual.is_empty() {
                return (true, actual == expected_path.to_str().unwrap_or(""));
            }
        }
        // proc_pidpath failed — fall through to basename comparison
        eprintln!(
            "warning: proc_pidpath failed for pid {pid}, falling back to basename comparison"
        );
    }

    // Fallback: basename-only comparison via ps
    let output = match std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
    {
        Ok(o) => o,
        Err(_) => return (true, false),
    };
    let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let expected_name = expected_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    // ps -o comm= may return a relative path (e.g. "target/debug/agent-aspect-bridge")
    let comm_name = std::path::Path::new(&comm)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    (true, comm_name == expected_name)
}

/// 定位当前可用的 bridge binary；改名迁移期回退旧名。
fn resolve_bridge_bin() -> Option<std::path::PathBuf> {
    bridge_bin_candidates().into_iter().next()
}

/// 所有可接受的 bridge binary 路径，按新名优先。
fn bridge_bin_candidates() -> Vec<std::path::PathBuf> {
    let Some(dir) = bin_dir() else {
        return Vec::new();
    };
    ["agent-aspect-bridge", "checkpoint-bridge"]
        .into_iter()
        .map(|name| dir.join(name))
        .filter(|p| p.exists())
        .collect()
}

/// pid 是否匹配任一新旧 bridge binary。
fn verify_bridge_pid_against_candidates(pid: u32) -> Option<String> {
    bridge_bin_candidates().into_iter().find_map(|bin| {
        let expected = bin.to_string_lossy().to_string();
        let (alive, verified) = verify_bridge_pid(pid, &expected);
        (alive && verified).then_some(expected)
    })
}

/// 当前配置期望的 bridge 端口。
fn configured_bridge_port() -> u16 {
    load_bridge_config()
        .bridge_addr
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(7676)
}

/// 查找监听指定端口的 PID。只作为 state 缺失时的迁移兜底。
fn listener_pid_on_port(port: u16) -> Option<u32> {
    let output = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix('p')?.parse::<u32>().ok())
}

/// state 文件缺失时，从监听端口恢复 bridge 状态。
fn discover_running_bridge_without_state() -> Option<BridgeState> {
    let port = read_bridge_port().unwrap_or_else(configured_bridge_port);
    let pid = listener_pid_on_port(port)?;
    let expected = verify_bridge_pid_against_candidates(pid)?;
    Some(BridgeState {
        pid,
        exe: expected,
        addr: format!("127.0.0.1:{port}"),
        started_at: String::new(),
    })
}

/// 停止 bridge pid，并等待退出。
fn stop_bridge_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if unsafe { libc::kill(pid as i32, 0) != 0 } {
            return;
        }
    }
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// 读取 bridge state 文件，验证 pid 身份，过期时自动清理。
///
/// 如果 state 文件存在但 pid 已死或身份不匹配，会删除 state 和 port 文件，
/// 避免后续命令误判 bridge 还在运行。
///
/// 返回 `Some((state, true))` 表示 bridge 确实在运行。
pub fn load_and_verify_state() -> Option<(BridgeState, bool)> {
    let state_path = paths::bridge_state_path();
    if !state_path.exists() {
        return discover_running_bridge_without_state().map(|state| (state, true));
    }
    let raw = match std::fs::read_to_string(&state_path) {
        Ok(r) => r,
        Err(_) => {
            std::fs::remove_file(&state_path).ok();
            return discover_running_bridge_without_state().map(|state| (state, true));
        }
    };
    let state: BridgeState = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(_) => {
            std::fs::remove_file(&state_path).ok();
            return discover_running_bridge_without_state().map(|state| (state, true));
        }
    };
    let (_alive, verified) = verify_bridge_pid(state.pid, &state.exe);
    if !verified {
        // pid dead or wrong process — stale, clean up
        std::fs::remove_file(&state_path).ok();
        std::fs::remove_file(paths::bridge_port_path()).ok();
        return discover_running_bridge_without_state().map(|state| (state, true));
    }
    Some((state, true))
}

/// 启动 bridge 进程。
///
/// 流程：
/// 1. 先尝试停掉旧进程（可能残留）
/// 2. 定位 agent-aspect-bridge 二进制
/// 3. spawn 后等待 500ms 让 bridge 写 state/port 文件
/// 4. 验证进程仍然存活
fn bridge_start() {
    match process_guard::stop_existing(&paths::bridge_state_path(), "agent-aspect-bridge") {
        process_guard::StopResult::Stopped(pid) => {
            println!("replaced previous bridge (pid {pid})");
            std::fs::remove_file(paths::bridge_port_path()).ok();
        }
        process_guard::StopResult::WrongProcess { pid, actual } => {
            eprintln!("warning: stale bridge state pointed to pid {pid} ({actual}); not killed");
            std::fs::remove_file(paths::bridge_port_path()).ok();
        }
        process_guard::StopResult::StaleState => {
            std::fs::remove_file(paths::bridge_port_path()).ok();
        }
        process_guard::StopResult::NotFound => {}
    }

    let Some(bridge_bin) = resolve_bridge_bin() else {
        eprintln!(
            "FAIL: agent-aspect-bridge not found next to current CLI (legacy checkpoint-bridge also missing)"
        );
        std::process::exit(1);
    };

    if let Some((state, true)) = load_and_verify_state() {
        stop_bridge_pid(state.pid);
        std::fs::remove_file(paths::bridge_state_path()).ok();
        std::fs::remove_file(paths::bridge_port_path()).ok();
        println!("replaced previous bridge (pid {})", state.pid);
    }

    let mut cmd = std::process::Command::new(&bridge_bin);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env("HOME", std::env::var("HOME").unwrap_or_default());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: spawn agent-aspect-bridge: {e}");
            std::process::exit(1);
        }
    };

    let pid = child.id();

    // 等一小段时间让 bridge 写 state 文件
    std::thread::sleep(std::time::Duration::from_millis(500));

    // 验证 bridge 进程还活着
    if !process_guard::is_alive(pid) {
        eprintln!("FAIL: bridge process exited immediately");
        std::process::exit(1);
    }

    let port = read_bridge_port().unwrap_or(7676);
    println!("bridge started (pid {pid}, addr 127.0.0.1:{port})");
}

/// 停止 bridge 进程。
///
/// 优雅关闭策略：先 SIGTERM，循环检查 10 次（每次 100ms），
/// 1 秒后仍未退出则 SIGKILL。最后清理 state 和 port 文件。
fn bridge_stop() {
    let state = match load_and_verify_state() {
        Some((s, true)) => s,
        _ => {
            println!("bridge not running (no active state)");
            return;
        }
    };

    let pid = state.pid;
    stop_bridge_pid(pid);

    std::fs::remove_file(paths::bridge_state_path()).ok();
    std::fs::remove_file(paths::bridge_port_path()).ok();
    println!("bridge stopped (pid {pid})");
}

/// 显示 bridge 当前状态。
///
/// 包含：运行状态、pid、监听地址、LAN 是否开启（及手机 URL）、
/// launchd 加载状态、keep-awake 是否启用、token 文件路径。
fn bridge_status() {
    let token_path = paths::bridge_token_path();

    // Load config for LAN info
    let config_path = Config::config_path();
    let config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
    } else {
        Config::default_config()
    };

    match load_and_verify_state() {
        Some((state, true)) => {
            println!("bridge: running");
            println!("pid: {}", state.pid);
            println!("addr: {}", state.addr);
            println!(
                "LAN: {}",
                if config.bridge_lan_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            if config.bridge_lan_enabled {
                let port = read_bridge_port().unwrap_or(7676);
                let lan_ips = discover_lan_ip();
                if let Some(ip) = lan_ips.first() {
                    println!("Phone URL: http://{ip}:{port}");
                }
            }
        }
        _ => {
            println!("bridge: stopped");
            println!(
                "LAN: {}",
                if config.bridge_lan_enabled {
                    "enabled"
                } else {
                    "disabled"
                }
            );
        }
    }

    // launchd status
    let plist_path = paths::bridge_launchd_plist_path();
    if plist_path.exists() {
        let target = format!("gui/{}/{}", unsafe { libc::getuid() }, BRIDGE_PLIST_LABEL);
        match run_launchctl("print", &[&target]) {
            Ok(_) => println!("launchd: loaded"),
            Err(_) => println!("launchd: plist exists but not loaded"),
        }
        println!(
            "keep-awake: {}",
            if bridge_plist_uses_caffeinate(&plist_path) {
                "enabled"
            } else {
                "disabled"
            }
        );
    } else {
        println!("launchd: not installed");
        println!("keep-awake: disabled");
    }

    println!("token: {}", token_path.display());
    println!("password: {}", paths::bridge_password_path().display());
}

/// 判断当前 bridge launchd plist 是否通过 caffeinate 启动。
fn bridge_plist_uses_caffeinate(plist_path: &std::path::Path) -> bool {
    std::fs::read_to_string(plist_path)
        .map(|s| s.contains("<string>/usr/bin/caffeinate</string>"))
        .unwrap_or(false)
}

/// 打印指定类型的 token。
///
/// 三种 token 类型：
/// - `Bridge`（默认）— 本地 HTTP API 的 Bearer token
/// - `RelayClient` — 手机端连 relay 的 token
/// - `RelayMac` — Mac 端向 relay 注册的 token
fn bridge_token(args: &[String]) {
    let token_kind = match args {
        [] => TokenKind::Bridge,
        [flag] if flag == "--bridge" => TokenKind::Bridge,
        [flag] if flag == "--relay-client" => TokenKind::RelayClient,
        [flag] if flag == "--relay-mac" => TokenKind::RelayMac,
        [flag] if flag == "--help" || flag == "-h" => {
            println!("Usage: agent-aspect bridge token [--bridge|--relay-client|--relay-mac]");
            println!();
            println!("  --bridge        Print local bridge bearer token (default) [debug]");
            println!("  --relay-client  Print phone-facing relay client token");
            println!("  --relay-mac     Print Mac registration token for relay");
            return;
        }
        _ => {
            eprintln!("usage: agent-aspect bridge token [--bridge|--relay-client|--relay-mac]");
            std::process::exit(1);
        }
    };

    print_token(token_kind);
}

/// 打印 bridge 默认登录密码（从 bridge.password 文件读取）。
fn bridge_password_show() {
    let path = paths::bridge_password_path();
    if !path.exists() {
        eprintln!("password file not found at {}", path.display());
        eprintln!("start bridge first to generate password");
        std::process::exit(1);
    }
    match std::fs::read_to_string(&path) {
        Ok(p) => println!("{}", p.trim()),
        Err(e) => {
            eprintln!("cannot read password: {e}");
            std::process::exit(1);
        }
    }
}

/// 重置 admin 密码：生成新随机密码，更新 SQLite 和文件。
fn bridge_password_reset() {
    let store = open_bridge_store();
    match checkpoint_core::user_password::reset_admin_password(&store) {
        Ok(new_pwd) => println!("{new_pwd}"),
        Err(e) => {
            eprintln!("reset failed: {e}");
            std::process::exit(1);
        }
    }
}

/// 设置 admin 密码：从 TTY 非回显读取，或从 stdin pipe 读取。
fn bridge_password_set() {
    let new_pwd = if atty::is(atty::Stream::Stdin) {
        match rpassword::prompt_password("new password: ") {
            Ok(p) => p,
            Err(e) => {
                eprintln!("read password: {e}");
                std::process::exit(1);
            }
        }
    } else {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_line(&mut buf) {
            eprintln!("read password: {e}");
            std::process::exit(1);
        }
        buf.trim().to_string()
    };
    if new_pwd.len() < 12 {
        eprintln!("password must be at least 12 characters");
        std::process::exit(1);
    }
    let store = open_bridge_store();
    if let Err(e) = checkpoint_core::user_password::set_admin_password(&store, &new_pwd) {
        eprintln!("set failed: {e}");
        std::process::exit(1);
    }
    eprintln!("password updated");
}

/// 仅当 sys_user 为空时初始化 admin 用户。
fn bridge_password_init() {
    let store = open_bridge_store();
    if let Err(e) = checkpoint_core::user_password::init_admin_user(&store) {
        eprintln!("init failed: {e}");
        std::process::exit(1);
    }
    eprintln!(
        "admin user initialized (password at {})",
        paths::bridge_password_path().display()
    );
}

/// 打开 bridge 的 SQLite 数据库连接（CLI 用）。
fn open_bridge_store() -> checkpoint_core::audit::AuditStore {
    let db_path = paths::audit_db_path();
    checkpoint_core::audit::AuditStore::open(&db_path).unwrap_or_else(|e| {
        eprintln!("cannot open database: {e}");
        std::process::exit(1);
    })
}

/// Token 类型枚举，对应不同的 token 文件和用途。
enum TokenKind {
    Bridge,
    RelayClient,
    RelayMac,
}

/// 根据 token 类型返回对应的文件路径。
fn token_path(kind: TokenKind) -> std::path::PathBuf {
    match kind {
        TokenKind::Bridge => paths::bridge_token_path(),
        TokenKind::RelayClient => paths::relay_client_token_path(),
        TokenKind::RelayMac => paths::relay_mac_token_path(),
    }
}

/// 从文件读取并打印 token（trim 后输出到 stdout）。
fn print_token(kind: TokenKind) {
    let token_path = token_path(kind);
    if !token_path.exists() {
        eprintln!("token file not found at {}", token_path.display());
        eprintln!("start bridge first to generate token");
        std::process::exit(1);
    }
    match std::fs::read_to_string(&token_path) {
        Ok(t) => println!("{}", t.trim()),
        Err(e) => {
            eprintln!("cannot read token: {e}");
            std::process::exit(1);
        }
    }
}

/// 加载 bridge 相关配置，不存在时返回默认值。
fn load_bridge_config() -> Config {
    let config_path = Config::config_path();
    if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
    } else {
        Config::default_config()
    }
}

/// 保存配置到 config.toml，失败时直接退出。
fn save_bridge_config(config: &Config) {
    let config_path = Config::config_path();
    if let Err(e) = config.save(&config_path) {
        eprintln!("FAIL: cannot save config: {e}");
        std::process::exit(1);
    }
}

/// 校验 relay URL 格式：必须以 `ws://` 或 `wss://` 开头。
/// 非强制但会警告不以 `/ws` 结尾的 URL（通常应该以 `/ws` 结尾）。
fn validate_relay_url(url: &str) {
    if !(url.starts_with("wss://") || url.starts_with("ws://")) {
        eprintln!("FAIL: relay URL must start with ws:// or wss://");
        std::process::exit(1);
    }
    if !url.ends_with("/ws") {
        eprintln!("warning: relay URL usually ends with /ws");
    }
}

/// 将 relay URL 写入 config.toml。
fn set_relay_url(url: &str) {
    validate_relay_url(url);
    let mut config = load_bridge_config();
    config.relay_url = Some(url.to_string());
    save_bridge_config(&config);
    println!("relay_url set: {url}");
}

/// 清除 config.toml 中的 relay_url 字段（设为 None）。
fn unset_relay_url() {
    let mut config = load_bridge_config();
    config.relay_url = None;
    save_bridge_config(&config);
    println!("relay_url unset");
}

/// Relay 子命令入口（`agent-aspect bridge relay <status|set-url|unset-url|token>`）。
fn bridge_relay(args: &[String]) {
    match args.first().map(|s| s.as_str()) {
        Some("status") | None => bridge_relay_status(),
        Some("set-url") => {
            let Some(url) = args.get(1) else {
                eprintln!("usage: agent-aspect bridge relay set-url <ws-url>");
                std::process::exit(1);
            };
            set_relay_url(url);
        }
        Some("unset-url") => unset_relay_url(),
        Some("token") => bridge_relay_token(&args[1..]),
        Some("help") | Some("--help") | Some("-h") => bridge_relay_help(),
        Some(other) => {
            eprintln!("unknown relay command: {other}");
            eprintln!("run 'agent-aspect bridge relay help' for usage");
            std::process::exit(1);
        }
    }
}

fn bridge_relay_help() {
    println!("agent-aspect bridge relay — manage relay configuration");
    println!();
    println!("Usage:");
    println!("  agent-aspect bridge relay status");
    println!("  agent-aspect bridge relay set-url <ws-url>");
    println!("  agent-aspect bridge relay unset-url");
    println!("  agent-aspect bridge relay token [--client|--mac]");
    println!();
    println!("Examples:");
    println!("  agent-aspect bridge relay set-url wss://relay.example.com/ws");
    println!("  agent-aspect bridge relay token --client");
}

/// 显示 relay 配置状态（URL、token 路径、bridge 运行状态）。
fn bridge_relay_status() {
    let config = load_bridge_config();
    println!(
        "relay: {}",
        if config.relay_url.is_some() {
            "configured"
        } else {
            "disabled"
        }
    );
    match config.relay_url.as_deref() {
        Some(url) => println!("url: {url}"),
        None => println!("url: (not set)"),
    }
    println!(
        "client_token: {}",
        paths::relay_client_token_path().display()
    );
    println!("mac_token: {}", paths::relay_mac_token_path().display());

    if let Some((state, true)) = load_and_verify_state() {
        println!("bridge: running (pid {})", state.pid);
    } else {
        println!("bridge: stopped");
    }
}

/// 打印 relay 相关 token（`--client` 手机端 或 `--mac` Mac 注册端）。
fn bridge_relay_token(args: &[String]) {
    let token_kind = match args {
        [] => TokenKind::RelayClient,
        [flag] if flag == "--client" || flag == "--relay-client" => TokenKind::RelayClient,
        [flag] if flag == "--mac" || flag == "--relay-mac" => TokenKind::RelayMac,
        [flag] if flag == "--help" || flag == "-h" => {
            println!("Usage: agent-aspect bridge relay token [--client|--mac]");
            println!();
            println!("  --client  Print phone-facing relay token (default)");
            println!("  --mac     Print Mac registration token");
            return;
        }
        _ => {
            eprintln!("usage: agent-aspect bridge relay token [--client|--mac]");
            std::process::exit(1);
        }
    };
    print_token(token_kind);
}

/// 先 stop 再 start。
fn bridge_restart() {
    bridge_stop();
    bridge_start();
}

/// 从 `~/.agent-aspect/bridge.port` 读取实际监听端口号。
/// bridge 启动后将绑定端口写入此文件。
fn read_bridge_port() -> Option<u16> {
    let port_path = paths::bridge_port_path();
    if !port_path.exists() {
        return None;
    }
    std::fs::read_to_string(&port_path)
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
}

/// 安装 bridge 的 launchd plist，实现开机自启。
///
/// `--keep-awake` 会用 `/usr/bin/caffeinate -s` 包住 bridge：
/// - 允许锁屏、允许显示器熄灭；
/// - 接 AC 电源时阻止系统睡眠，避免 Mac 锁屏后 relay WebSocket 断开；
/// - 不在电池上强行保活，避免误伤续航。
fn bridge_install(args: &[String]) {
    let keep_awake = parse_install_options(args);
    let plist_path = paths::bridge_launchd_plist_path();

    let Some(dir) = bin_dir() else {
        eprintln!("FAIL: cannot determine binary directory");
        std::process::exit(1);
    };
    let bridge_bin = dir.join("agent-aspect-bridge");
    let bridge_bin = if bridge_bin.exists() {
        bridge_bin
    } else {
        dir.join("checkpoint-bridge")
    };
    if !bridge_bin.exists() {
        eprintln!(
            "FAIL: agent-aspect-bridge not found at {}",
            bridge_bin.display()
        );
        std::process::exit(1);
    }
    let bridge_abs = bridge_bin.canonicalize().unwrap_or(bridge_bin);

    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::create_dir_all(paths::checkpoint_dir()).ok();

    let log_stdout = paths::checkpoint_dir().join("agent-aspect-bridge.stdout.log");
    let log_stderr = paths::checkpoint_dir().join("agent-aspect-bridge.stderr.log");

    let program_arguments = if keep_awake {
        format!(
            r#"        <string>/usr/bin/caffeinate</string>
        <string>-s</string>
        <string>{bin}</string>"#,
            bin = bridge_abs.display()
        )
    } else {
        format!(r#"        <string>{}</string>"#, bridge_abs.display())
    };

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
{program_arguments}
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>
"#,
        label = BRIDGE_PLIST_LABEL,
        program_arguments = program_arguments,
        stdout = log_stdout.display(),
        stderr = log_stderr.display(),
    );

    if let Err(e) = std::fs::write(&plist_path, &plist) {
        eprintln!("FAIL: write plist: {e}");
        std::process::exit(1);
    }
    println!("wrote {}", plist_path.display());

    let target = format!("gui/{}", unsafe { libc::getuid() });
    match run_launchctl("bootstrap", &[&target, plist_path.to_str().unwrap()]) {
        Ok(msg) => {
            if msg.is_empty() {
                println!("service loaded (bootstrap OK)");
            } else {
                println!("bootstrap: {msg}");
            }
            if keep_awake {
                println!("keep-awake: enabled (caffeinate -s)");
            }
        }
        Err(e) => {
            eprintln!("FAIL: bootstrap failed: {e}");
            eprintln!(
                "  plist written to {} but service not loaded",
                plist_path.display()
            );
            eprintln!(
                "  try: launchctl bootstrap {} {}",
                target,
                plist_path.display()
            );
            std::process::exit(1);
        }
    }
}

/// 解析 `agent-aspect bridge install` 的安装选项。
fn parse_install_options(args: &[String]) -> bool {
    let mut keep_awake = false;
    for arg in args {
        match arg.as_str() {
            "--keep-awake" => keep_awake = true,
            "--help" | "-h" => {
                println!("Usage: agent-aspect bridge install [--keep-awake]");
                println!();
                println!(
                    "  --keep-awake  Prevent system sleep on AC power while bridge is running"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown bridge install option: {other}");
                eprintln!("usage: agent-aspect bridge install [--keep-awake]");
                std::process::exit(1);
            }
        }
    }
    keep_awake
}

/// 卸载 bridge launchd 服务。
/// 先停手动启动的 bridge 进程，再 bootout launchd 服务，最后删 plist。
fn bridge_uninstall() {
    let plist_path = paths::bridge_launchd_plist_path();

    // 先尝试停掉手动启动的 bridge 进程
    bridge_stop();

    if !plist_path.exists() {
        println!("bridge plist not found (nothing to uninstall)");
        return;
    }

    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, BRIDGE_PLIST_LABEL);
    match run_launchctl("bootout", &[&target]) {
        Ok(msg) => {
            if msg.is_empty() {
                println!("service unloaded");
            } else {
                println!("bootout: {msg}");
            }
        }
        Err(e) => {
            // 可能没 loaded，不致命
            eprintln!("bootout: {e} (may already be unloaded)");
        }
    }

    if let Err(e) = std::fs::remove_file(&plist_path) {
        eprintln!("remove plist: {e}");
    } else {
        println!("removed {}", plist_path.display());
    }
}

/// 通过 ifconfig 发现本机局域网 IP（排除 127.x 和 169.254.x）。
/// 只匹配 en0/en1/... 主接口，忽略子接口（en0x 等）。
fn discover_lan_ip() -> Vec<String> {
    let output = std::process::Command::new("ifconfig").output().ok();
    let Some(output) = output else {
        return vec![];
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut ips = Vec::new();
    let mut in_en = false;
    for line in text.lines() {
        let trimmed = line.trim();
        // Match en0, en1, en2, ... but not en0x or en0.1 subinterfaces
        if trimmed.starts_with("en")
            && trimmed
                .get(2..)
                .map(|s| s.bytes().next().map_or(false, |c| c.is_ascii_digit()))
                .unwrap_or(false)
        {
            in_en = true;
        } else if !trimmed.is_empty() && !trimmed.starts_with(' ') && !trimmed.starts_with('\t') {
            in_en = false;
        }
        if in_en && trimmed.contains("inet ") {
            if let Some(addr) = trimmed.strip_prefix("inet ") {
                let ip = addr.trim().split(' ').next().unwrap_or("");
                if !ip.starts_with("127.") && !ip.starts_with("169.254.") && !ip.is_empty() {
                    ips.push(ip.to_string());
                }
            }
        }
    }
    ips
}

/// 显示配对信息（bridge 运行状态、本地/手机 URL、token 提示）。
/// 用户在手机浏览器输入 Phone URL + token 完成 pairing。
fn bridge_pair() {
    let (state, _) = match load_and_verify_state() {
        Some(s) => s,
        None => {
            eprintln!("Bridge is not running. Start it first: agent-aspect bridge start");
            std::process::exit(1);
        }
    };

    let config_path = Config::config_path();
    let config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
    } else {
        Config::default_config()
    };

    let port = read_bridge_port().unwrap_or(7676);
    let lan_ips = discover_lan_ip();

    // Read token for hint
    let token_path = paths::bridge_token_path();
    let token_hint = if token_path.exists() {
        match std::fs::read_to_string(&token_path) {
            Ok(t) => {
                let full = t.trim();
                if full.len() >= 8 {
                    full[..8].to_string()
                } else {
                    full.to_string()
                }
            }
            Err(_) => "(unreadable)".to_string(),
        }
    } else {
        "(not found)".to_string()
    };

    println!("Bridge is running (pid {})", state.pid);
    println!();
    println!("Local URL:   http://127.0.0.1:{port}");

    if lan_ips.is_empty() {
        println!("Phone URL:   (no LAN IP discovered)");
    } else if lan_ips.len() == 1 {
        println!("Phone URL:   http://{}:{port}", lan_ips[0]);
    } else {
        for (i, ip) in lan_ips.iter().enumerate() {
            if i == 0 {
                println!("Phone URL:   http://{ip}:{port}");
            } else {
                println!("             http://{ip}:{port}");
            }
        }
    }

    println!();
    println!("Token hint:  {token_hint}...");
    println!("Full token:  agent-aspect bridge token");
    println!("Token file:  {}", token_path.display());

    if !config.bridge_lan_enabled {
        println!();
        println!("Note: Bridge is not exposed to LAN.");
        println!("      Run 'agent-aspect bridge expose' to enable phone access.");
    }
}

/// 将 bridge 监听地址改为 0.0.0.0:7676，允许局域网设备访问，然后重启。
fn bridge_expose() {
    let config_path = Config::config_path();
    let mut config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
    } else {
        Config::default_config()
    };

    config.bridge_addr = "0.0.0.0:7676".to_string();
    config.bridge_lan_enabled = true;

    if let Err(e) = config.save(&config_path) {
        eprintln!("FAIL: cannot save config: {e}");
        std::process::exit(1);
    }

    println!("Bridge LAN access enabled (addr set to 0.0.0.0:7676)");
    bridge_restart();
}

/// 将 bridge 监听地址改回 127.0.0.1:7676，禁止局域网访问，然后重启。
fn bridge_unexpose() {
    let config_path = Config::config_path();
    let mut config = if config_path.exists() {
        Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
    } else {
        Config::default_config()
    };

    config.bridge_addr = "127.0.0.1:7676".to_string();
    config.bridge_lan_enabled = false;

    if let Err(e) = config.save(&config_path) {
        eprintln!("FAIL: cannot save config: {e}");
        std::process::exit(1);
    }

    println!("Bridge LAN access disabled (addr set to 127.0.0.1:7676)");
    bridge_restart();
}
