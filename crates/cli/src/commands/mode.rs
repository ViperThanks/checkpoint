//! `agent-aspect mode` — 查看或设置 daemon 运行模式。
//!
//! 读/写 config.toml 中的 mode 字段。daemon 在每次处理请求时
//! 会重新加载 config.toml，所以修改后无需重启 daemon。
//!
//! 合法值：observer / autonomous / guard / paranoid。

use agent_aspect_core::config::Config;
use agent_aspect_core::rule::Mode;

/// 查看或设置运行模式。
///
/// - `new_mode` 为 `None` 时打印当前模式
/// - 为合法模式名时写入 config.toml 并提示 daemon 会在下次请求时自动热加载
/// - 为其他值时报错退出
pub fn cmd_mode(new_mode: Option<&str>) {
    let config_path = Config::config_path();

    match new_mode {
        // 无参数：读并打印当前模式
        None => {
            let cfg = if config_path.exists() {
                match Config::load(&config_path) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("config load error: {e}");
                        return;
                    }
                }
            } else {
                Config::default_config()
            };
            println!("{}", cfg.mode);
        }
        // 四个合法模式名
        Some("observer") | Some("autonomous") | Some("guard") | Some("paranoid") => {
            let mode = new_mode.unwrap().parse::<Mode>().expect("validated mode");
            let mut cfg = if config_path.exists() {
                Config::load(&config_path).unwrap_or_else(|_| Config::default_config())
            } else {
                Config::default_config()
            };
            cfg.mode = mode;
            if let Err(e) = cfg.save(&config_path) {
                eprintln!("config save error: {e}");
                std::process::exit(1);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600));
            }
            println!("mode set to {}", mode);
            println!("daemon will pick up on next request");
        }
        _ => {
            eprintln!("unknown mode: {}", new_mode.unwrap());
            eprintln!("valid modes: observer, autonomous, guard, paranoid");
            std::process::exit(1);
        }
    }
}
