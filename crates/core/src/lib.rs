//! agent-aspect-core — AI Agent 安全护栏的共享核心库。
//!
//! 为 CLI 和 bridge HTTP 服务器提供：审计存储、规则引擎、会话管理、
//! provider 适配器、transcript 解析、配置管理。
//!
//! 模块依赖关系（从底层到高层）：
//! error / constants / paths / utils → config / event / decision / wire →
//! adapter / normalize / provider_resolver → rule / learn →
//! conversation / transcript / title_import / transcript_sync → audit / store

pub mod adapter;
pub mod audit;
pub mod config;
pub mod constants;
pub mod conversation;
pub mod decision;
pub mod env_compat;
pub mod error;
pub mod event;
pub mod hook_status;
pub mod learn;
pub mod normalize;
pub mod password;
pub mod paths;
pub mod process_guard;
pub mod provider_registry;
pub mod provider_resolver;
pub mod provider_scanner;
pub mod rule;
pub mod runtime_profile;
pub mod store;
pub mod user_password;
pub mod utils;

use crate::rule::{Mode, RuleSource};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Autonomous => "autonomous",
            Self::Guard => "guard",
            Self::Paranoid => "paranoid",
        }
    }
}

impl Display for Mode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Mode {
    type Err = crate::error::AgentAspectError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "observer" => Ok(Self::Observer),
            "autonomous" => Ok(Self::Autonomous),
            "guard" => Ok(Self::Guard),
            "paranoid" => Ok(Self::Paranoid),
            other => Err(crate::error::AgentAspectError::InvalidMode(
                other.to_string(),
            )),
        }
    }
}

impl RuleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Learned => "learned",
            Self::User => "user",
            Self::Community => "community",
        }
    }
}

impl Display for RuleSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
pub mod title_import;
pub mod transcript;
pub mod transcript_sync;
pub mod wire;

/// 测试共享工具。
#[cfg(test)]
pub(crate) mod test_util {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// PATH 是进程全局状态，测试并行修改会互相踩。用 mutex 串行化。
    pub static PATH_MUTEX: Mutex<()> = Mutex::new(());
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// 获取 PATH 测试锁；即使前一个失败测试污染了锁，也允许后续测试继续清理现场。
    pub fn path_guard() -> MutexGuard<'static, ()> {
        PATH_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 生成单测专用临时目录，避免并行测试复用同一个进程级路径。
    pub fn unique_temp_dir(prefix: &str) -> PathBuf {
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("{prefix}-{}-{ts}-{n}", std::process::id()))
    }

    /// 设置 PATH 为指定值，返回旧值以便恢复。
    pub fn set_path(new_path: &str) -> String {
        let old = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", new_path);
        }
        old
    }
}
