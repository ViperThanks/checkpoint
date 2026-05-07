//! 共享应用上下文 — AuditStore、ProviderResolver 和 ProviderRegistry 的容器。
//!
//! 架构角色：在 main 中创建一次，传给所有 HTTP handler。
//! store 用 Arc<Mutex<>> 包裹是因为 SQLite 连接不是线程安全的，
//! 同一时刻只有一个 handler 可以访问 DB。

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::provider_registry::ProviderRegistry;
use agent_aspect_core::provider_resolver::ProviderResolver;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 全局共享状态。store 的 Mutex 保证同一时刻只有一个请求操作 DB，
/// resolver 和 registry 是只读的（创建后不变），不需要同步。
/// Clone 派生用于并发请求线程共享（所有 clone 共享同一 Arc<Mutex<Store>>）。
#[derive(Clone)]
pub struct AppContext {
    pub store: Arc<Mutex<AuditStore>>,
    pub db_path: PathBuf,
    pub resolver: ProviderResolver,
    pub registry: ProviderRegistry,
}

impl AppContext {
    /// 打开 audit DB 并创建共享上下文。DB 路径通常来自 paths::audit_db_path()。
    pub fn new(
        db_path: &std::path::Path,
        resolver: ProviderResolver,
        registry: ProviderRegistry,
    ) -> Result<Self, String> {
        let store = AuditStore::open(db_path).map_err(|e| format!("open audit db: {e}"))?;
        Ok(Self {
            store: Arc::new(Mutex::new(store)),
            db_path: db_path.to_path_buf(),
            resolver,
            registry,
        })
    }
}
