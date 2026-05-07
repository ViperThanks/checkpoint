//! CLI crate 根模块。
//!
//! 仅做子模块声明，所有命令实现都在 `commands/` 下。
//! main.rs 通过 `agent_aspect_cli::commands` 导入各命令入口函数。

pub mod commands;
