//! DAO 层 — 按领域拆分的 SQL 和行映射器。
//!
//! 每个子模块为 `AuditStore` 实现对应领域的方法。
//! `AuditStore` 保持单一 facade；调用方统一从 `crate::audit` 导入。

pub mod conversations;
pub mod decisions;
pub mod devices;
pub mod events;
pub mod feedback;
pub mod jobs;
pub mod messages;
pub mod suggestions;
pub mod users;
