//! sys_user 表 DAO — 用户 CRUD 和登录查询。
//!
//! 支持用户名密码登录的基础用户表，密码哈希由 `crate::password` 模块提供。

use crate::audit::AuditStore;
use crate::error::{CheckpointError, CheckpointResult};

/// sys_user 行类型。
#[derive(Debug, Clone)]
pub struct UserRow {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub password_salt: String,
    pub role: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_login_at: Option<String>,
    pub disabled_at: Option<String>,
}

impl AuditStore {
    /// 创建用户。id 由调用方生成（UUID v7）。
    pub fn create_user(
        &self,
        id: &str,
        username: &str,
        password_hash: &str,
        password_salt: &str,
        role: &str,
        created_at: &str,
        updated_at: &str,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "INSERT INTO sys_user (id, username, password_hash, password_salt, role, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![id, username, password_hash, password_salt, role, created_at, updated_at],
            )
            .map_err(CheckpointError::CreateUser)?;
        Ok(())
    }

    /// 按用户名查询用户。
    pub fn get_user_by_username(&self, username: &str) -> CheckpointResult<Option<UserRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, username, password_hash, password_salt, role,
                        created_at, updated_at, last_login_at, disabled_at
                 FROM sys_user WHERE username = ?1",
            )
            .map_err(CheckpointError::QueryUser)?;
        let row = stmt.query_row(rusqlite::params![username], |row: &rusqlite::Row| {
            Ok(UserRow {
                id: row.get(0)?,
                username: row.get(1)?,
                password_hash: row.get(2)?,
                password_salt: row.get(3)?,
                role: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                last_login_at: row.get(7)?,
                disabled_at: row.get(8)?,
            })
        });
        match row {
            Ok(u) => Ok(Some(u)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(CheckpointError::QueryUser(e)),
        }
    }

    /// 更新最后登录时间。
    pub fn update_last_login(&self, user_id: &str, timestamp: &str) -> CheckpointResult<()> {
        self.conn
            .execute(
                "UPDATE sys_user SET last_login_at = ?1, updated_at = ?1 WHERE id = ?2",
                rusqlite::params![timestamp, user_id],
            )
            .map_err(CheckpointError::UpdateUser)?;
        Ok(())
    }

    /// 更新用户密码（hash + salt）。
    pub fn update_user_password(
        &self,
        user_id: &str,
        password_hash: &str,
        password_salt: &str,
        updated_at: &str,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "UPDATE sys_user SET password_hash = ?1, password_salt = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![password_hash, password_salt, updated_at, user_id],
            )
            .map_err(CheckpointError::UpdateUser)?;
        Ok(())
    }

    /// 统计用户数量。
    pub fn count_users(&self) -> CheckpointResult<i64> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sys_user",
                [],
                |row: &rusqlite::Row| row.get(0),
            )
            .map_err(CheckpointError::QueryUser)?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use crate::audit::AuditStore;

    #[test]
    fn create_and_query_user() {
        let store = AuditStore::open_in_memory().expect("open db");
        store
            .create_user(
                "u1",
                "admin",
                "fakehash",
                "fakesalt",
                "owner",
                "2026-05-03T00:00:00Z",
                "2026-05-03T00:00:00Z",
            )
            .expect("create user");

        let user = store
            .get_user_by_username("admin")
            .expect("query")
            .expect("found");
        assert_eq!(user.id, "u1");
        assert_eq!(user.role, "owner");
        assert!(user.last_login_at.is_none());
    }

    #[test]
    fn query_missing_user_returns_none() {
        let store = crate::audit::AuditStore::open_in_memory().expect("open db");
        let result = store.get_user_by_username("nobody").expect("query");
        assert!(result.is_none());
    }

    #[test]
    fn count_users_returns_correct_count() {
        let store = crate::audit::AuditStore::open_in_memory().expect("open db");
        assert_eq!(store.count_users().unwrap(), 0);

        store
            .create_user(
                "u1",
                "admin",
                "h",
                "s",
                "owner",
                "2026-05-03T00:00:00Z",
                "2026-05-03T00:00:00Z",
            )
            .unwrap();
        assert_eq!(store.count_users().unwrap(), 1);
    }

    #[test]
    fn update_last_login_sets_timestamp() {
        let store = crate::audit::AuditStore::open_in_memory().expect("open db");
        store
            .create_user(
                "u1",
                "admin",
                "h",
                "s",
                "owner",
                "2026-05-03T00:00:00Z",
                "2026-05-03T00:00:00Z",
            )
            .unwrap();

        store
            .update_last_login("u1", "2026-05-03T12:00:00Z")
            .unwrap();

        let user = store.get_user_by_username("admin").unwrap().unwrap();
        assert_eq!(user.last_login_at.as_deref(), Some("2026-05-03T12:00:00Z"));
    }

    #[test]
    fn update_user_password_replaces_hash() {
        let store = crate::audit::AuditStore::open_in_memory().expect("open db");
        store
            .create_user(
                "u1",
                "admin",
                "old-hash",
                "old-salt",
                "owner",
                "2026-05-03T00:00:00Z",
                "2026-05-03T00:00:00Z",
            )
            .unwrap();

        store
            .update_user_password("u1", "new-hash", "new-salt", "2026-05-04T00:00:00Z")
            .unwrap();

        let user = store.get_user_by_username("admin").unwrap().unwrap();
        assert_eq!(user.password_hash, "new-hash");
        assert_eq!(user.password_salt, "new-salt");
        assert_eq!(user.updated_at, "2026-05-04T00:00:00Z");
    }
}
