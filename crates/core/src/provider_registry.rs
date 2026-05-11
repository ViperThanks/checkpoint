//! Provider 注册表 — provider 能力的 single source of truth。
//!
//! 架构角色：
//! - 内置默认 provider 配置（claude_code, codex_cli, kimi_code）
//! - 用户 config.toml [providers.*] 节可覆盖或新增
//! - 所有 provider 特性查询（能否 resume、CLI 命令名、显示名、hook/transcript 能力）必须经过本 registry
//!
//! 关键不变量：
//! - 默认配置硬编码在 `builtin_defaults()`，不是 config.toml
//! - 用户 config 只覆盖（override），不删除默认 provider
//! - `provider_binaries`（旧格式）兼容：合成到对应 provider 的 binary_path

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Provider 对外能力视图。
///
/// 这是 UI、runner、hook 配置页读取 provider 能力的稳定结构，
/// 避免各层散落 `match agent` 或重复读取配置字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// 是否支持 before-tool 阻断/观察事件。
    pub supports_pretooluse: bool,
    /// 是否支持 after-tool 事件。
    pub supports_posttooluse: bool,
    /// 是否支持 turn/session stop 事件。
    pub supports_stop: bool,
    /// 是否有可读取的 transcript。
    pub supports_transcript: bool,
    /// 是否支持继续既有会话。
    pub supports_resume: bool,
    /// 是否支持启动新会话。
    pub supports_new: bool,
    /// 是否支持 provider 自带 timeout 控制。
    pub supports_native_timeout: bool,
    /// 是否支持运行时权限透传（Full Access）。
    pub supports_permission_passthrough: bool,
}

/// 单个 provider 的静态配置。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// CLI 二进制名（e.g. "claude"），用于 PATH / fallback 搜索
    pub command: String,
    /// UI 显示名（e.g. "Claude Code"）
    pub display_name: String,
    #[serde(default)]
    pub supports_resume: bool,
    #[serde(default = "default_true")]
    pub supports_new: bool,
    #[serde(default)]
    pub supports_stop_observer: bool,
    #[serde(default)]
    pub supports_pretooluse: bool,
    #[serde(default)]
    pub supports_posttooluse: bool,
    #[serde(default)]
    pub supports_stop: bool,
    #[serde(default)]
    pub supports_transcript: bool,
    #[serde(default)]
    pub supports_native_timeout: bool,
    /// 该 provider 是否支持运行时权限模式透传（如 bypassPermissions）。
    #[serde(default)]
    pub supports_permission_passthrough: bool,
    /// bypass 模式激活时注入的 CLI 参数（如 "--dangerously-skip-permissions"）。
    #[serde(default)]
    pub permission_mode_cli_arg: Option<String>,
    /// bypass 模式激活时设置的环境变量（如 [("VIBE_ISLAND_SKIP", "1")]）。
    #[serde(default)]
    pub permission_mode_env_vars: Vec<(String, String)>,
}

/// 用户 config 中的部分覆盖。所有字段可选，未填的继承默认值。
///
/// 用户只需写关心的字段：
/// ```toml
/// [providers.claude_code]
/// command = "/opt/bin/claude"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfigOverride {
    pub enabled: Option<bool>,
    pub command: Option<String>,
    pub display_name: Option<String>,
    pub supports_resume: Option<bool>,
    pub supports_new: Option<bool>,
    pub supports_stop_observer: Option<bool>,
    pub supports_pretooluse: Option<bool>,
    pub supports_posttooluse: Option<bool>,
    pub supports_stop: Option<bool>,
    pub supports_transcript: Option<bool>,
    pub supports_native_timeout: Option<bool>,
    /// 对应 ProviderConfig 同名字段。
    pub supports_permission_passthrough: Option<bool>,
    /// 对应 ProviderConfig 同名字段。
    pub permission_mode_cli_arg: Option<String>,
    /// 对应 ProviderConfig 同名字段。
    pub permission_mode_env_vars: Option<Vec<(String, String)>>,
}

impl ProviderConfigOverride {
    /// 将覆盖合并到默认配置上，返回完整的 ProviderConfig。
    pub fn merge_onto(&self, defaults: &ProviderConfig) -> ProviderConfig {
        ProviderConfig {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            command: self
                .command
                .clone()
                .unwrap_or_else(|| defaults.command.clone()),
            display_name: self
                .display_name
                .clone()
                .unwrap_or_else(|| defaults.display_name.clone()),
            supports_resume: self.supports_resume.unwrap_or(defaults.supports_resume),
            supports_new: self.supports_new.unwrap_or(defaults.supports_new),
            supports_stop_observer: self
                .supports_stop_observer
                .unwrap_or(defaults.supports_stop_observer),
            supports_pretooluse: self
                .supports_pretooluse
                .unwrap_or(defaults.supports_pretooluse),
            supports_posttooluse: self
                .supports_posttooluse
                .unwrap_or(defaults.supports_posttooluse),
            supports_stop: self.supports_stop.unwrap_or(defaults.supports_stop),
            supports_transcript: self
                .supports_transcript
                .unwrap_or(defaults.supports_transcript),
            supports_native_timeout: self
                .supports_native_timeout
                .unwrap_or(defaults.supports_native_timeout),
            supports_permission_passthrough: self
                .supports_permission_passthrough
                .unwrap_or(defaults.supports_permission_passthrough),
            permission_mode_cli_arg: self
                .permission_mode_cli_arg
                .clone()
                .or_else(|| defaults.permission_mode_cli_arg.clone()),
            permission_mode_env_vars: self
                .permission_mode_env_vars
                .clone()
                .unwrap_or_else(|| defaults.permission_mode_env_vars.clone()),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Provider 注册表：默认 + 用户覆盖。
#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    /// provider key → config。pub(crate) 供 provider_resolver 构建时读取。
    pub(crate) providers: HashMap<String, ProviderConfig>,
}

impl ProviderRegistry {
    /// 内置默认 provider。用户 config 可以覆盖任意字段。
    pub fn builtin_defaults() -> HashMap<String, ProviderConfig> {
        let mut m = HashMap::new();
        m.insert(
            "claude_code".into(),
            ProviderConfig {
                enabled: true,
                command: "claude".into(),
                display_name: "Claude Code".into(),
                supports_resume: true,
                supports_new: true,
                supports_stop_observer: true,
                supports_pretooluse: true,
                supports_posttooluse: false,
                supports_stop: true,
                supports_transcript: true,
                supports_native_timeout: false,
                supports_permission_passthrough: true,
                permission_mode_cli_arg: Some("--dangerously-skip-permissions".into()),
                permission_mode_env_vars: vec![("VIBE_ISLAND_SKIP".into(), "1".into())],
            },
        );
        m.insert(
            "codex_cli".into(),
            ProviderConfig {
                enabled: true,
                command: "codex".into(),
                display_name: "Codex CLI".into(),
                supports_resume: true,
                supports_new: true,
                supports_stop_observer: false,
                supports_pretooluse: true,
                supports_posttooluse: true,
                supports_stop: true,
                supports_transcript: true,
                supports_native_timeout: false,
                supports_permission_passthrough: true,
                permission_mode_cli_arg: Some("--dangerously-bypass-approvals-and-sandbox".into()),
                permission_mode_env_vars: vec![],
            },
        );
        m.insert(
            "kimi_code".into(),
            ProviderConfig {
                enabled: true,
                command: "kimi".into(),
                display_name: "Kimi Code".into(),
                supports_resume: true,
                supports_new: true,
                supports_stop_observer: false,
                supports_pretooluse: true,
                supports_posttooluse: false,
                supports_stop: true,
                supports_transcript: true,
                supports_native_timeout: false,
                supports_permission_passthrough: false,
                permission_mode_cli_arg: None,
                permission_mode_env_vars: vec![],
            },
        );
        m
    }

    /// 从 config 构建注册表：builtin → config.providers 覆盖 → provider_binaries 兼容。
    pub fn from_config(config: &crate::config::Config) -> Self {
        let mut providers = Self::builtin_defaults();

        // 用户 [providers.*] 部分覆盖：只替换用户显式写的字段
        for (key, over) in &config.providers {
            let merged = if let Some(existing) = providers.get(key) {
                over.merge_onto(existing)
            } else {
                // 新 provider：覆盖 apply 到空白默认
                over.merge_onto(&ProviderConfig {
                    enabled: true,
                    command: key.clone(),
                    display_name: key.clone(),
                    supports_resume: false,
                    supports_new: true,
                    supports_stop_observer: false,
                    supports_pretooluse: false,
                    supports_posttooluse: false,
                    supports_stop: false,
                    supports_transcript: false,
                    supports_native_timeout: false,
                    supports_permission_passthrough: false,
                    permission_mode_cli_arg: None,
                    permission_mode_env_vars: vec![],
                })
            };
            providers.insert(key.clone(), merged);
        }

        // 旧格式 provider_binaries 兼容：未知 provider 自动合成一条
        for key in config.provider_binaries.keys() {
            if !providers.contains_key(key) {
                providers.insert(
                    key.clone(),
                    ProviderConfig {
                        enabled: true,
                        command: key.clone(),
                        display_name: key.clone(),
                        supports_resume: false,
                        supports_new: true,
                        supports_stop_observer: false,
                        supports_pretooluse: false,
                        supports_posttooluse: false,
                        supports_stop: false,
                        supports_transcript: false,
                        supports_native_timeout: false,
                        supports_permission_passthrough: false,
                        permission_mode_cli_arg: None,
                        permission_mode_env_vars: vec![],
                    },
                );
            }
        }

        Self { providers }
    }

    /// 获取单个 provider 配置。
    pub fn get(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.get(name)
    }

    /// 所有启用的 provider key。
    pub fn enabled_providers(&self) -> Vec<&str> {
        self.providers
            .iter()
            .filter(|(_, c)| c.enabled)
            .map(|(k, _)| k.as_str())
            .collect()
    }

    /// provider 是否已知且启用。
    pub fn is_known_provider(&self, name: &str) -> bool {
        self.providers.get(name).map(|c| c.enabled).unwrap_or(false)
    }

    /// provider key → CLI 二进制名。
    pub fn binary_name(&self, provider: &str) -> Option<&str> {
        self.providers.get(provider).map(|c| c.command.as_str())
    }

    /// UI 显示名，未知 provider 回退到 key 本身。
    pub fn display_name(&self, provider: &str) -> String {
        self.providers
            .get(provider)
            .map(|c| c.display_name.as_str())
            .unwrap_or(provider)
            .to_string()
    }

    /// provider 是否支持 resume。
    pub fn supports_resume(&self, provider: &str) -> bool {
        self.providers
            .get(provider)
            .map(|c| c.supports_resume)
            .unwrap_or(false)
    }

    /// provider 的能力视图。未知 provider 返回 None。
    pub fn capabilities(&self, provider: &str) -> Option<ProviderCapabilities> {
        self.providers.get(provider).map(|c| ProviderCapabilities {
            supports_pretooluse: c.supports_pretooluse,
            supports_posttooluse: c.supports_posttooluse,
            supports_stop: c.supports_stop,
            supports_transcript: c.supports_transcript,
            supports_resume: c.supports_resume,
            supports_new: c.supports_new,
            supports_native_timeout: c.supports_native_timeout,
            supports_permission_passthrough: c.supports_permission_passthrough,
        })
    }

    /// provider 是否可 resume（必须同时 enabled 且 supports_resume）。
    pub fn can_resume(&self, provider: &str) -> bool {
        self.providers
            .get(provider)
            .map(|c| c.enabled && c.supports_resume)
            .unwrap_or(false)
    }

    /// provider 是否可启动新任务（必须同时 enabled 且 supports_new）。
    pub fn can_start_new(&self, provider: &str) -> bool {
        self.providers
            .get(provider)
            .map(|c| c.enabled && c.supports_new)
            .unwrap_or(false)
    }

    /// provider 是否支持权限透传（e.g. bypassPermissions → CLI flag + env）。
    pub fn supports_permission_passthrough(&self, provider: &str) -> bool {
        self.providers
            .get(provider)
            .map(|c| c.supports_permission_passthrough)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_registry() -> ProviderRegistry {
        ProviderRegistry::from_config(&crate::config::Config::default_config())
    }

    #[test]
    fn builtin_defaults_contain_three_providers() {
        let r = make_registry();
        assert!(r.is_known_provider("claude_code"));
        assert!(r.is_known_provider("codex_cli"));
        assert!(r.is_known_provider("kimi_code"));
        assert_eq!(r.enabled_providers().len(), 3);
    }

    #[test]
    fn binary_name_mapping() {
        let r = make_registry();
        assert_eq!(r.binary_name("claude_code"), Some("claude"));
        assert_eq!(r.binary_name("codex_cli"), Some("codex"));
        assert_eq!(r.binary_name("kimi_code"), Some("kimi"));
        assert_eq!(r.binary_name("unknown"), None);
    }

    #[test]
    fn display_name_fallback() {
        let r = make_registry();
        assert_eq!(r.display_name("claude_code"), "Claude Code");
        assert_eq!(r.display_name("unknown_provider"), "unknown_provider");
    }

    #[test]
    fn supports_resume_defaults() {
        let r = make_registry();
        assert!(r.supports_resume("claude_code"));
        assert!(r.supports_resume("codex_cli"));
        assert!(!r.supports_resume("nonexistent"));
    }

    #[test]
    fn capability_defaults_are_explicit() {
        let r = make_registry();

        let claude = r.capabilities("claude_code").unwrap();
        assert!(claude.supports_pretooluse);
        assert!(!claude.supports_posttooluse);
        assert!(claude.supports_stop);
        assert!(claude.supports_transcript);
        assert!(claude.supports_resume);
        assert!(claude.supports_new);
        assert!(!claude.supports_native_timeout);

        let codex = r.capabilities("codex_cli").unwrap();
        assert!(codex.supports_pretooluse);
        assert!(codex.supports_posttooluse);
        assert!(codex.supports_stop);
        assert!(codex.supports_transcript);
        assert!(codex.supports_resume);
        assert!(codex.supports_new);
        assert!(codex.supports_permission_passthrough);
        assert!(r.can_start_new("codex_cli"));
        assert!(!r.can_start_new("nonexistent"));
    }

    #[test]
    fn config_override_disables_provider() {
        let mut config = crate::config::Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "codex_cli".into(),
            ProviderConfigOverride {
                enabled: Some(false),
                ..Default::default()
            },
        );
        config.providers = providers;
        let r = ProviderRegistry::from_config(&config);
        assert!(!r.is_known_provider("codex_cli"));
        assert!(r.is_known_provider("claude_code"));
        assert_eq!(r.enabled_providers().len(), 2);
    }

    #[test]
    fn partial_override_preserves_unset_fields() {
        let mut config = crate::config::Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "claude_code".into(),
            ProviderConfigOverride {
                command: Some("/opt/bin/claude".into()),
                ..Default::default()
            },
        );
        config.providers = providers;
        let r = ProviderRegistry::from_config(&config);
        // command 被覆盖
        assert_eq!(r.binary_name("claude_code"), Some("/opt/bin/claude"));
        // 其他字段继承默认
        assert!(r.supports_resume("claude_code"));
        assert_eq!(r.display_name("claude_code"), "Claude Code");
    }

    #[test]
    fn permission_passthrough_override_merges() {
        let mut config = crate::config::Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "codex_cli".into(),
            ProviderConfigOverride {
                supports_permission_passthrough: Some(true),
                permission_mode_cli_arg: Some("--full-auto".into()),
                permission_mode_env_vars: Some(vec![("CODEX_UNSAFE_MODE".into(), "1".into())]),
                ..Default::default()
            },
        );
        config.providers = providers;

        let r = ProviderRegistry::from_config(&config);
        let cfg = r.get("codex_cli").unwrap();
        assert!(cfg.supports_permission_passthrough);
        assert_eq!(cfg.permission_mode_cli_arg.as_deref(), Some("--full-auto"));
        assert_eq!(
            cfg.permission_mode_env_vars,
            vec![("CODEX_UNSAFE_MODE".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn capability_override_merges() {
        let mut config = crate::config::Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "custom_agent".into(),
            ProviderConfigOverride {
                display_name: Some("Custom Agent".into()),
                supports_resume: Some(true),
                supports_pretooluse: Some(true),
                supports_transcript: Some(true),
                supports_native_timeout: Some(true),
                ..Default::default()
            },
        );
        config.providers = providers;

        let r = ProviderRegistry::from_config(&config);
        let caps = r.capabilities("custom_agent").unwrap();
        assert_eq!(r.display_name("custom_agent"), "Custom Agent");
        assert!(caps.supports_resume);
        assert!(caps.supports_pretooluse);
        assert!(caps.supports_transcript);
        assert!(caps.supports_native_timeout);
        assert!(!caps.supports_posttooluse);
        assert!(!caps.supports_stop);
    }

    #[test]
    fn capability_toml_example_parses() {
        let raw = r#"
mode = "guard"

[providers.codex_cli]
supports_pretooluse = true
supports_posttooluse = true
supports_stop = true
supports_transcript = true
supports_resume = true
supports_native_timeout = false
"#;
        let config: crate::config::Config = toml::from_str(raw).unwrap();
        let over = config.providers.get("codex_cli").unwrap();

        assert_eq!(over.supports_pretooluse, Some(true));
        assert_eq!(over.supports_posttooluse, Some(true));
        assert_eq!(over.supports_stop, Some(true));
        assert_eq!(over.supports_transcript, Some(true));
        assert_eq!(over.supports_native_timeout, Some(false));
    }

    #[test]
    fn permission_passthrough_toml_example_parses() {
        let raw = r#"
mode = "guard"

[providers.claude_code]
supports_permission_passthrough = true
permission_mode_cli_arg = "--dangerously-skip-permissions"
permission_mode_env_vars = [["VIBE_ISLAND_SKIP", "1"]]
"#;
        let config: crate::config::Config = toml::from_str(raw).unwrap();
        let over = config.providers.get("claude_code").unwrap();

        assert_eq!(over.supports_permission_passthrough, Some(true));
        assert_eq!(
            over.permission_mode_cli_arg.as_deref(),
            Some("--dangerously-skip-permissions")
        );
        assert_eq!(
            over.permission_mode_env_vars.as_ref().unwrap(),
            &vec![("VIBE_ISLAND_SKIP".to_string(), "1".to_string())]
        );
    }

    #[test]
    fn legacy_provider_binaries_creates_entry() {
        let mut config = crate::config::Config::default_config();
        config
            .provider_binaries
            .insert("custom_tool".into(), "/usr/local/bin/custom".into());
        let r = ProviderRegistry::from_config(&config);
        assert!(r.is_known_provider("custom_tool"));
        assert_eq!(r.binary_name("custom_tool"), Some("custom_tool"));
    }

    #[test]
    fn builtin_defaults_static() {
        let defaults = ProviderRegistry::builtin_defaults();
        assert_eq!(defaults.len(), 3);
        assert!(defaults.contains_key("claude_code"));
        assert!(defaults.contains_key("codex_cli"));
        assert!(defaults.contains_key("kimi_code"));
    }
}
