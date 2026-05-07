//! 环境变量读取 — Agent Aspect 只接受 `AGENT_ASPECT_*`。
//!
//! M44 后删除 `legacy env` 双轨，避免同一进程读到两套运行身份。

/// 读取环境变量，未设置时返回 None。
pub fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// 读取环境变量，未设置时返回 default。
pub fn env_var_or(name: &str, default: String) -> String {
    env_var(name).unwrap_or(default)
}

/// 检查环境变量是否设置。
pub fn env_var_is_set(name: &str) -> bool {
    std::env::var(name).is_ok()
}
