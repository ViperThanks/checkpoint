//! `agent-aspect launchd` — 管理 macOS launchd 服务集成。
//!
//! 通过生成 plist + launchctl bootstrap/bootout 实现 daemon 的开机自启。
//! plist 中设置 `RunAtLoad` + `KeepAlive` 保证 daemon 持续运行。
//! 仅支持 macOS，因为依赖 launchctl 和 ~/Library/LaunchAgents。

use agent_aspect_core::paths;

use super::helpers::{bin_dir, run_launchctl};

/// launchd 服务标识，对应 plist 文件名和 Label 键。
pub const PLIST_LABEL: &str = "com.agent-aspect.daemon";

/// launchd 子命令入口。
///
/// - `install` — 写 plist 并 bootstrap 到当前 GUI domain
/// - `uninstall` — bootout 服务并删除 plist
/// - `status` — 检查 plist 和 launchd 加载状态
pub fn cmd_launchd(sub: Option<&str>) {
    match sub {
        Some("install") => launchd_install(),
        Some("uninstall") => launchd_uninstall(),
        Some("status") => launchd_status(),
        _ => {
            eprintln!("usage: agent-aspect launchd <install|uninstall|status>");
            std::process::exit(1);
        }
    }
}

/// 生成 launchd plist 并 bootstrap 服务。
///
/// 流程：
/// 1. 定位 agent-aspectd 绝对路径（canonicalize 解析符号链接）
/// 2. 写 plist 到 ~/Library/LaunchAgents/
/// 3. launchctl bootstrap gui/<uid> <plist>
///
/// 不变量：plist 中使用 canonicalize 后的路径，避免 symlink 变化导致 launchd 找不到二进制。
fn launchd_install() {
    let plist_path = paths::launchd_plist_path();

    // 找到 agent-aspectd 绝对路径
    let Some(dir) = bin_dir() else {
        eprintln!("FAIL: cannot determine binary directory");
        std::process::exit(1);
    };
    let daemon_bin = dir.join("agent-aspectd");
    if !daemon_bin.exists() {
        eprintln!("FAIL: agent-aspectd not found at {}", daemon_bin.display());
        std::process::exit(1);
    }
    let daemon_abs = daemon_bin.canonicalize().unwrap_or(daemon_bin);

    // 确保目录存在
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::create_dir_all(paths::agent_aspect_dir()).ok();

    let log_stdout = paths::daemon_stdout_log_path();
    let log_stderr = paths::daemon_stderr_log_path();

    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
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
        label = PLIST_LABEL,
        bin = daemon_abs.display(),
        stdout = log_stdout.display(),
        stderr = log_stderr.display(),
    );

    if let Err(e) = std::fs::write(&plist_path, &plist) {
        eprintln!("FAIL: write plist: {e}");
        std::process::exit(1);
    }
    println!("wrote {}", plist_path.display());

    // bootstrap
    let target = format!("gui/{}", unsafe { libc::getuid() });
    let out = run_launchctl("bootstrap", &[&target, plist_path.to_str().unwrap()]);
    match out {
        Ok(msg) => {
            if msg.is_empty() {
                println!("service loaded (bootstrap OK)");
            } else {
                println!("bootstrap: {msg}");
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

/// 卸载 launchd 服务。
///
/// 先 bootout（失败不阻断——service 可能没在运行），再删除 plist 文件。
/// bootout 失败只打 WARN，因为服务未运行是合法状态。
fn launchd_uninstall() {
    let plist_path = paths::launchd_plist_path();

    // bootout
    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, PLIST_LABEL);
    let out = run_launchctl("bootout", &[&target]);
    match out {
        Ok(msg) => {
            if msg.is_empty() {
                println!("service unloaded (bootout OK)");
            } else {
                println!("bootout: {msg}");
            }
        }
        Err(e) => {
            // bootout 可能因为 service 未运行而失败，不阻断
            eprintln!("WARN: bootout: {e}");
        }
    }

    // 删除 plist
    if plist_path.exists() {
        if let Err(e) = std::fs::remove_file(&plist_path) {
            eprintln!("FAIL: remove plist: {e}");
            std::process::exit(1);
        }
        println!("removed {}", plist_path.display());
    } else {
        println!("plist not found (already removed)");
    }
}

/// 显示 launchd 服务当前状态。
///
/// 三维度检查：
/// 1. plist 文件是否存在
/// 2. launchctl print 是否能查到服务（提取 state/pid/exit status 行）
/// 3. state.json 中记录的 pid 是否存活
fn launchd_status() {
    let plist_path = paths::launchd_plist_path();

    // plist 文件
    if plist_path.exists() {
        println!("plist: {} (exists)", plist_path.display());
    } else {
        println!("plist: not found");
    }

    // launchctl print
    let target = format!("gui/{}/{}", unsafe { libc::getuid() }, PLIST_LABEL);
    match run_launchctl("print", &[&target]) {
        Ok(output) => {
            // 提取关键信息
            for line in output.lines() {
                let l = line.trim();
                if l.starts_with("state") || l.starts_with("pid") || l.contains("exit status") {
                    println!("  {l}");
                }
            }
            println!("service: loaded in launchd");
        }
        Err(_) => {
            println!("service: not loaded in launchd");
        }
    }

    // 进程检查
    let state_path = paths::state_path();
    if state_path.exists() {
        if let Some(state) = std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if let Some(pid) = state["pid"].as_u64() {
                let running = unsafe { libc::kill(pid as i32, 0) == 0 };
                println!(
                    "daemon: pid={pid} {}",
                    if running { "running" } else { "not running" }
                );
            }
        }
    } else {
        println!("daemon: state.json not found");
    }
}
