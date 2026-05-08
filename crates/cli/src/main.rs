//! `agent-aspect` CLI 入口二进制。
//!
//! 极简的手工参数解析——没有引入 clap/structopt，
//! 因为一共只有十来个平级子命令，没必要拉依赖。
//! 每个子命令直接转发到 `commands::cmd_*` 函数。

use agent_aspect_cli::commands::{
    cmd_audit, cmd_bridge, cmd_conversations, cmd_daemon, cmd_doctor, cmd_hooks, cmd_init,
    cmd_launchd, cmd_mode, cmd_rules, cmd_status,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // 第一个位置参数是子命令名；缺失时显示帮助
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match subcommand {
        "status" => cmd_status(),
        "rules" => cmd_rules(),
        "audit" => cmd_audit(),
        // mode/daemon/launchd/conversations 需要第二个参数（子操作或值）
        "mode" => cmd_mode(args.get(2).map(|s| s.as_str())),
        "doctor" => cmd_doctor(),
        "init" => cmd_init(args.get(2).map(|s| s.as_str())),
        "daemon" => cmd_daemon(args.get(2).map(|s| s.as_str())),
        "launchd" => cmd_launchd(args.get(2).map(|s| s.as_str())),
        // bridge 比较特殊：第二个参数是子命令，剩余参数作为该子命令的选项
        "bridge" => cmd_bridge(
            args.get(2).map(|s| s.as_str()),
            args.get(3..).unwrap_or(&[]),
        ),
        "conversations" => cmd_conversations(args.get(2).map(|s| s.as_str())),
        "hooks" => cmd_hooks(args.get(2..).map(|s| s.join(" ")).as_deref()),
        "help" | "--help" | "-h" => {
            println!("agent-aspect — AI agent behavior monitor");
            println!();
            println!("Usage: agent-aspect <command>");
            println!();
            println!("Commands:");
            println!("  status    Show daemon status and event counts");
            println!("  rules     List active default rules");
            println!("  audit     Show recent audit entries");
            println!("  mode      Show or set daemon mode (observer|autonomous|guard|paranoid)");
            println!("  doctor    Run health checks on Agent Aspect installation");
            println!("  init      Install verified agent hook configuration");
            println!("  daemon    Manage daemon process (start|stop|restart|status)");
            println!("  launchd   Manage macOS launchd integration (install|uninstall|status)");
            println!("  bridge    Manage bridge HTTP server and relay access");
            println!(
                "  conversations  Import real titles from provider transcripts (import-titles)"
            );
            println!("  hooks      Manage hook configuration (status|enable|disable|reconcile)");
        }
        _ => {
            eprintln!("unknown command: {subcommand}");
            eprintln!("run 'agent-aspect help' for usage");
            std::process::exit(1);
        }
    }
}
