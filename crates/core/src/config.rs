//! 配置管理 — TOML 配置的加载、保存和默认值。
//!
//! 配置文件位于 `~/.agent-aspect/config.toml`（旧安装可能沿用 `~/.checkpoint/`）。
//! 所有字段都有 serde default，保证向后兼容新增字段。

use crate::error::CheckpointResult;
use crate::paths;
use crate::provider_registry::ProviderConfigOverride;
use crate::rule::Mode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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
        }
    }

    pub fn config_path() -> std::path::PathBuf {
        paths::config_path()
    }

    /// 从 TOML 文件加载配置。文件不存在或格式错误时返回 CheckpointError。
    pub fn load(path: &Path) -> CheckpointResult<Self> {
        let content =
            std::fs::read_to_string(path).map_err(crate::error::CheckpointError::ReadConfig)?;
        toml::from_str(&content).map_err(crate::error::CheckpointError::ParseConfig)
    }

    /// 将配置序列化为 TOML 并写入文件，自动创建父目录。
    pub fn save(&self, path: &Path) -> CheckpointResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(crate::error::CheckpointError::CreateConfigDir)?;
        }
        let content =
            toml::to_string_pretty(self).map_err(crate::error::CheckpointError::SerializeConfig)?;
        std::fs::write(path, content).map_err(crate::error::CheckpointError::WriteConfig)?;
        Ok(())
    }

    /// 加载已有配置；加载失败或文件不存在时生成默认配置并持久化。
    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match Self::load(&path) {
                Ok(c) => return c,
                Err(e) => eprintln!("checkpoint: config load error: {e}, using defaults"),
            }
        }
        let cfg = Self::default_config();
        if let Err(e) = cfg.save(&path) {
            eprintln!("checkpoint: config save error: {e}");
        }
        cfg
    }
}
