//! 设备 DAO — relay 设备的注册、查询、标签管理。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};

/// 设备行 — 对应 devices 表。
#[derive(Debug, Clone)]
pub struct DeviceRow {
    pub device_id: String,
    pub label: String,
    pub user_agent: Option<String>,
    pub remote_addr: Option<String>,
    pub first_seen: String,
    pub last_seen: String,
}

impl AuditStore {
    /// 注册或更新设备。首次插入时设置 first_seen 和 last_seen，
    /// 后续更新只刷新 user_agent / remote_addr / last_seen。
    pub fn register_device(
        &self,
        device_id: &str,
        user_agent: Option<&str>,
        remote_addr: Option<&str>,
        timestamp: &str,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "INSERT INTO devices (device_id, label, user_agent, remote_addr, first_seen, last_seen)
                 VALUES (?1, '', ?2, ?3, ?4, ?4)
                 ON CONFLICT(device_id) DO UPDATE SET
                    user_agent = COALESCE(excluded.user_agent, devices.user_agent),
                    remote_addr = COALESCE(excluded.remote_addr, devices.remote_addr),
                    last_seen = excluded.last_seen",
                rusqlite::params![device_id, user_agent, remote_addr, timestamp],
            )
            .map_err(AgentAspectError::UpsertDevice)?;
        Ok(())
    }

    pub fn list_devices(&self) -> AgentAspectResult<Vec<DeviceRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT device_id, label, user_agent, remote_addr, first_seen, last_seen
                 FROM devices
                 ORDER BY last_seen DESC",
            )
            .map_err(AgentAspectError::QueryDevice)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(DeviceRow {
                    device_id: row.get(0)?,
                    label: row.get(1)?,
                    user_agent: row.get(2)?,
                    remote_addr: row.get(3)?,
                    first_seen: row.get(4)?,
                    last_seen: row.get(5)?,
                })
            })
            .map_err(AgentAspectError::QueryDevice)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QueryDevice)
    }

    /// 更新设备显示标签。返回是否实际更新了行。
    pub fn update_device_label(&self, device_id: &str, label: &str) -> AgentAspectResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE devices SET label = ?2 WHERE device_id = ?1",
                rusqlite::params![device_id, label],
            )
            .map_err(AgentAspectError::UpdateDevice)?;
        Ok(rows > 0)
    }
}
