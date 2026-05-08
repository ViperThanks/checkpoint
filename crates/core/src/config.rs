//! 配置管理 — TOML 配置的加载、保存和默认值。
//!
//! 配置文件位于 `~/.agent-aspect/config.toml`（旧安装可能沿用 `~/.agent-aspect/`）。
//! 所有字段都有 serde default，保证向后兼容新增字段。

use crate::error::AgentAspectResult;
use crate::paths;
use crate::provider_registry::ProviderConfigOverride;
use crate::rule::Mode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Per-agent hook 控制配置 — 控制该 agent 的 hook 安装和各类事件评估开关。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentHookConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub pretooluse_enabled: bool,
    #[serde(default = "default_true")]
    pub metadata_enabled: bool,
    #[serde(default = "default_true")]
    pub stop_enabled: bool,
}

impl Default for AgentHookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            pretooluse_enabled: true,
            metadata_enabled: true,
            stop_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub mode: Mode,
    #[serde(default = "default_bridge_addr")]
    pub bridge_addr: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_audit_retention_days")]
    pub audit_retention_days: u32,
    #[serde(default = "default_job_timeout_secs")]
    pub job_timeout_secs: u64,
    #[serde(default = "default_agent_prompt_timeout_secs")]
    pub agent_prompt_timeout_secs: u64,
    #[serde(default = "default_job_max_output_kb")]
    pub job_max_output_kb: usize,
    #[serde(default)]
    pub bridge_lan_enabled: bool,
    #[serde(default)]
    pub provider_binaries: HashMap<String, String>,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfigOverride>,
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub approval_review: ApprovalReviewConfig,
    #[serde(default = "default_true")]
    pub pretooluse_enabled: bool,
    #[serde(default)]
    pub agent_hooks: HashMap<String, AgentHookConfig>,
}

/// 审批 review payload 配置 — 控制 /pending 响应中 review 字段展示哪些内容。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalReviewConfig {
    #[serde(default = "default_approval_view")]
    pub default_view: String,
    #[serde(default = "default_true")]
    pub show_rule: bool,
    #[serde(default = "default_true")]
    pub show_agent: bool,
    #[serde(default = "default_true")]
    pub show_device: bool,
    #[serde(default = "default_true")]
    pub show_file_path: bool,
    #[serde(default = "default_true")]
    pub show_command: bool,
    #[serde(default)]
    pub show_payload_preview: bool,
    #[serde(default = "default_payload_preview_chars")]
    pub payload_preview_chars: usize,
}

impl Default for ApprovalReviewConfig {
    fn default() -> Self {
        Self {
            default_view: default_approval_view(),
            show_rule: true,
            show_agent: true,
            show_device: true,
            show_file_path: true,
            show_command: true,
            show_payload_preview: false,
            payload_preview_chars: default_payload_preview_chars(),
        }
    }
}

fn default_approval_view() -> String {
    "standard".to_string()
}

fn default_true() -> bool {
    true
}

fn default_payload_preview_chars() -> usize {
    800
}

impl ApprovalReviewConfig {
    /// 校验并修正无效配置值（不让配置错误导致 bridge 起不来）。
    pub fn sanitize(&mut self) {
        if !matches!(self.default_view.as_str(), "compact" | "standard" | "full") {
            self.default_view = "standard".to_string();
        }
        if self.payload_preview_chars > 4000 {
            self.payload_preview_chars = 4000;
        }
    }
}

fn default_bridge_addr() -> String {
    "127.0.0.1:7676".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_audit_retention_days() -> u32 {
    90
}

fn default_job_timeout_secs() -> u64 {
    300
}

fn default_agent_prompt_timeout_secs() -> u64 {
    600
}

fn default_job_max_output_kb() -> usize {
    512
}

impl Config {
    /// 以 Guard 模式为默认值构造配置。
    pub fn default_config() -> Self {
        Self {
            mode: Mode::Guard,
            bridge_addr: default_bridge_addr(),
            log_level: default_log_level(),
            audit_retention_days: default_audit_retention_days(),
            job_timeout_secs: default_job_timeout_secs(),
            agent_prompt_timeout_secs: default_agent_prompt_timeout_secs(),
            job_max_output_kb: default_job_max_output_kb(),
            bridge_lan_enabled: false,
            provider_binaries: HashMap::new(),
            providers: HashMap::new(),
            relay_url: None,
            approval_review: ApprovalReviewConfig::default(),
            pretooluse_enabled: true,
            agent_hooks: HashMap::new(),
        }
    }

    /// 获取指定 agent 的 hook 配置；config 中无该 agent 条目时返回全 true 默认值。
    pub fn agent_hook_config(&self, agent: &str) -> AgentHookConfig {
        self.agent_hooks.get(agent).cloned().unwrap_or_default()
    }

    pub fn config_path() -> std::path::PathBuf {
        paths::config_path()
    }

    /// 从 TOML 文件加载配置。文件不存在或格式错误时返回 AgentAspectError。
    pub fn load(path: &Path) -> AgentAspectResult<Self> {
        let content =
            std::fs::read_to_string(path).map_err(crate::error::AgentAspectError::ReadConfig)?;
        toml::from_str(&content).map_err(crate::error::AgentAspectError::ParseConfig)
    }

    /// 将配置序列化为 TOML 并写入文件，自动创建父目录。
    pub fn save(&self, path: &Path) -> AgentAspectResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(crate::error::AgentAspectError::CreateConfigDir)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(crate::error::AgentAspectError::SerializeConfig)?;
        std::fs::write(path, content).map_err(crate::error::AgentAspectError::WriteConfig)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// 加载已有配置；加载失败或文件不存在时生成默认配置并持久化。
    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match Self::load(&path) {
                Ok(c) => return c,
                Err(e) => eprintln!("agent-aspect: config load error: {e}, using defaults"),
            }
        }
        let cfg = Self::default_config();
        if let Err(e) = cfg.save(&path) {
            eprintln!("agent-aspect: config save error: {e}");
        }
        cfg
    }
}
