//! 命令子模块注册与统一导出。
//!
//! 每个子命令一个文件，通过 `pub use` 把 `cmd_*` 入口函数
//! 重新导出到这个命名空间，让 main.rs 可以一行 import 全部命令。

pub mod audit;
pub mod bridge;
pub mod conversations;
pub mod daemon;
pub mod doctor;
pub mod helpers;
pub mod hooks;
pub mod init;
pub mod launchd;
pub mod mode;
pub mod rules;
pub mod status;

// Re-export command entry points for convenience
pub use audit::cmd_audit;
pub use bridge::cmd_bridge;
pub use conversations::cmd_conversations;
pub use daemon::cmd_daemon;
pub use doctor::cmd_doctor;
pub use hooks::cmd_hooks;
pub use init::cmd_init;
pub use launchd::cmd_launchd;
pub use mode::cmd_mode;
pub use rules::cmd_rules;
pub use status::cmd_status;
