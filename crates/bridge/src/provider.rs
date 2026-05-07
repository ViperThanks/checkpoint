//! Provider 命令构建 — 为每个 AI provider 构造 CLI 命令。
//!
//! 架构角色：被 jobs.rs 的 build_command 调用，将 job 参数转换为子进程命令。
//!
//! 核心不变量：
//! - conversation_id 是「继续」与「新建」的唯一判据：
//!   Some → 加 resume 参数；None → 不加。
//! - 所有 provider 对 conversation_id 的处理方式相同，
//!   不允许根据 provider 静默丢弃 conversation_id。
//! - 命令参数通过 argv 传递，不做 shell 拼接。

use agent_aspect_core::provider_registry::ProviderRegistry;
use std::process::Command;

/// 校验 provider 名称是否在 registry 中已知且启用。
pub fn validate_provider(provider: &str, registry: &ProviderRegistry) -> bool {
    registry.is_known_provider(provider)
}

/// 构建非交互式 Command，向 agent 发送 prompt。
/// `binary_path` 必须是由 ProviderResolver 解析的绝对路径。
/// 所有参数通过 argv 传递，不做 shell 拼接。
pub fn build_agent_command(
    binary_path: &str,
    provider: &str,
    project_path: &str,
    conversation_id: Option<&str>,
    prompt: &str,
    runtime_permission_mode: Option<&str>,
    registry: &ProviderRegistry,
) -> Result<std::process::Command, String> {
    if prompt.is_empty() {
        return Err("prompt must not be empty".to_string());
    }

    // 通用权限透传：从 provider 配置中读取 CLI 参数和环境变量。
    let permission_injection =
        if runtime_permission_mode == Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS) {
            registry
                .get(provider)
                .filter(|c| c.supports_permission_passthrough)
        } else {
            None
        };

    match provider {
        "claude_code" => {
            let mut cmd = Command::new(binary_path);
            cmd.arg("--print")
                .arg("--output-format")
                .arg("text")
                .current_dir(project_path)
                .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1");

            if let Some(sid) = conversation_id {
                cmd.arg("--resume").arg(sid);
            }

            apply_permission_injection(&mut cmd, permission_injection);
            cmd.arg("-p").arg(prompt);
            Ok(cmd)
        }
        "kimi_code" => {
            let mut cmd = Command::new(binary_path);
            cmd.arg("--print").current_dir(project_path);

            if let Some(sid) = conversation_id {
                cmd.arg("--resume").arg(sid);
            }

            apply_permission_injection(&mut cmd, permission_injection);
            cmd.arg("-p").arg(prompt);
            Ok(cmd)
        }
        "codex_cli" => {
            let mut cmd = Command::new(binary_path);
            cmd.arg("exec")
                .arg("-C")
                .arg(project_path)
                .arg("--skip-git-repo-check")
                .current_dir(project_path);

            if let Some(sid) = conversation_id {
                cmd.arg("resume").arg(sid);
            }

            apply_permission_injection(&mut cmd, permission_injection);
            cmd.arg(prompt);
            Ok(cmd)
        }
        _ => Err(format!("unsupported provider: {provider}")),
    }
}

/// 将 provider 声明的权限透传参数写入命令。
/// 调用点必须放在最终 prompt 参数之前，避免 provider 将 prompt 之后的内容当作正文。
fn apply_permission_injection(
    cmd: &mut Command,
    cfg: Option<&agent_aspect_core::provider_registry::ProviderConfig>,
) {
    if let Some(cfg) = cfg {
        if let Some(ref arg) = cfg.permission_mode_cli_arg {
            cmd.arg(arg);
        }
        for (k, v) in &cfg.permission_mode_env_vars {
            cmd.env(k, v);
        }
    }
}

/// 组合方法：先通过 resolver 解析二进制路径，再构建 Command。
/// resolver 从 config.toml 和 PATH 查找实际的可执行文件路径。
pub fn resolve_and_build(
    resolver: &agent_aspect_core::provider_resolver::ProviderResolver,
    registry: &ProviderRegistry,
    provider: &str,
    project_path: &str,
    conversation_id: Option<&str>,
    prompt: &str,
    runtime_permission_mode: Option<&str>,
) -> Result<std::process::Command, String> {
    let binary_path = resolver.resolve(provider)?;
    build_agent_command(
        binary_path.to_str().unwrap_or(provider),
        provider,
        project_path,
        conversation_id,
        prompt,
        runtime_permission_mode,
        registry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_aspect_core::provider_registry::ProviderConfigOverride;
    use std::collections::HashMap;

    fn make_registry() -> ProviderRegistry {
        ProviderRegistry::from_config(&agent_aspect_core::config::Config::default_config())
    }

    fn make_registry_with_permission_passthrough(
        provider: &str,
        cli_arg: &str,
        env_key: &str,
        env_value: &str,
    ) -> ProviderRegistry {
        let mut config = agent_aspect_core::config::Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            provider.to_string(),
            ProviderConfigOverride {
                supports_permission_passthrough: Some(true),
                permission_mode_cli_arg: Some(cli_arg.to_string()),
                permission_mode_env_vars: Some(vec![(env_key.to_string(), env_value.to_string())]),
                ..Default::default()
            },
        );
        config.providers = providers;
        ProviderRegistry::from_config(&config)
    }

    #[test]
    fn validates_known_providers() {
        let r = make_registry();
        assert!(validate_provider("claude_code", &r));
        assert!(validate_provider("kimi_code", &r));
        assert!(validate_provider("codex_cli", &r));
        assert!(!validate_provider("unknown", &r));
    }

    #[test]
    fn claude_command_no_resume() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/claude",
            "claude_code",
            "/tmp/proj",
            None,
            "hello",
            None,
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"hello".to_string()));
        assert!(!args.contains(&"--resume".to_string()));
        assert_eq!(
            cmd.get_current_dir().unwrap().to_str().unwrap(),
            "/tmp/proj"
        );
    }

    #[test]
    fn claude_command_with_resume() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/claude",
            "claude_code",
            "/tmp/proj",
            Some("sess-123"),
            "continue",
            None,
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess-123".to_string()));
    }

    #[test]
    fn kimi_command() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/kimi",
            "kimi_code",
            "/tmp/proj",
            Some("sid"),
            "test",
            None,
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--print".to_string()));
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sid".to_string()));
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"test".to_string()));
    }

    #[test]
    fn codex_command_no_resume() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/codex",
            "codex_cli",
            "/tmp/proj",
            None,
            "fix bug",
            None,
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"-C".to_string()));
        assert!(args.contains(&"/tmp/proj".to_string()));
        assert!(args.contains(&"--skip-git-repo-check".to_string()));
        assert!(args.contains(&"fix bug".to_string()));
        assert!(!args.contains(&"resume".to_string()));
    }

    #[test]
    fn codex_command_with_resume() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/codex",
            "codex_cli",
            "/tmp/proj",
            Some("tid"),
            "fix",
            None,
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"exec".to_string()));
        assert!(args.contains(&"-C".to_string()));
        assert!(args.contains(&"/tmp/proj".to_string()));
        assert!(args.contains(&"--skip-git-repo-check".to_string()));
        assert!(args.contains(&"resume".to_string()));
        assert!(args.contains(&"tid".to_string()));
        assert!(args.contains(&"fix".to_string()));
    }

    #[test]
    fn empty_prompt_rejected() {
        let registry = make_registry();
        assert!(
            build_agent_command(
                "/usr/bin/claude",
                "claude_code",
                "/tmp",
                None,
                "",
                None,
                &registry
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_provider_rejected() {
        let registry = make_registry();
        assert!(
            build_agent_command(
                "/usr/bin/unknown",
                "unknown",
                "/tmp",
                None,
                "hi",
                None,
                &registry
            )
            .is_err()
        );
    }

    #[test]
    fn claude_bypass_permission_mode_adds_skip_permissions_flag() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/claude",
            "claude_code",
            "/tmp/proj",
            Some("sess-123"),
            "continue",
            Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS),
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn claude_bypass_permission_mode_sets_vibe_island_skip_env() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/claude",
            "claude_code",
            "/tmp/proj",
            Some("sess-123"),
            "continue",
            Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS),
            &registry,
        )
        .unwrap();
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        assert_eq!(
            envs.get(std::ffi::OsStr::new("VIBE_ISLAND_SKIP"))
                .map(|v| v.unwrap()),
            Some(std::ffi::OsStr::new("1"))
        );
    }

    #[test]
    fn claude_non_bypass_does_not_set_vibe_island_skip() {
        let registry = make_registry();
        let cmd = build_agent_command(
            "/usr/bin/claude",
            "claude_code",
            "/tmp/proj",
            None,
            "hello",
            None,
            &registry,
        )
        .unwrap();
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        assert!(!envs.contains_key(std::ffi::OsStr::new("VIBE_ISLAND_SKIP")));
    }

    #[test]
    fn codex_bypass_permission_mode_uses_provider_config() {
        let registry = make_registry_with_permission_passthrough(
            "codex_cli",
            "--full-auto",
            "CODEX_UNSAFE_MODE",
            "1",
        );
        let cmd = build_agent_command(
            "/usr/bin/codex",
            "codex_cli",
            "/tmp/proj",
            Some("tid"),
            "fix",
            Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS),
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();

        assert!(args.contains(&"--full-auto".to_string()));
        assert_eq!(
            envs.get(std::ffi::OsStr::new("CODEX_UNSAFE_MODE"))
                .map(|v| v.unwrap()),
            Some(std::ffi::OsStr::new("1"))
        );
    }

    #[test]
    fn kimi_bypass_permission_mode_uses_provider_config() {
        let registry =
            make_registry_with_permission_passthrough("kimi_code", "--unsafe", "KIMI_UNSAFE", "1");
        let cmd = build_agent_command(
            "/usr/bin/kimi",
            "kimi_code",
            "/tmp/proj",
            Some("sid"),
            "fix",
            Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS),
            &registry,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();

        assert!(args.contains(&"--unsafe".to_string()));
        assert_eq!(
            envs.get(std::ffi::OsStr::new("KIMI_UNSAFE"))
                .map(|v| v.unwrap()),
            Some(std::ffi::OsStr::new("1"))
        );
    }

    #[test]
    fn disabled_providers_ignore_runtime_permission_mode() {
        let registry = make_registry();
        let codex = build_agent_command(
            "/usr/bin/codex",
            "codex_cli",
            "/tmp/proj",
            None,
            "fix",
            Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS),
            &registry,
        )
        .unwrap();
        let kimi = build_agent_command(
            "/usr/bin/kimi",
            "kimi_code",
            "/tmp/proj",
            None,
            "fix",
            Some(agent_aspect_core::constants::PERMISSION_MODE_BYPASS),
            &registry,
        )
        .unwrap();

        assert!(
            !codex
                .get_args()
                .any(|a| a == std::ffi::OsStr::new("--dangerously-skip-permissions"))
        );
        assert!(
            !kimi
                .get_args()
                .any(|a| a == std::ffi::OsStr::new("--dangerously-skip-permissions"))
        );
        assert!(
            !codex
                .get_envs()
                .any(|(k, _)| k == std::ffi::OsStr::new("VIBE_ISLAND_SKIP"))
        );
        assert!(
            !kimi
                .get_envs()
                .any(|(k, _)| k == std::ffi::OsStr::new("VIBE_ISLAND_SKIP"))
        );
    }
}
