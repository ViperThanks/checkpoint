//! Bridge 用户密码生命周期管理 — reset / set / init。
//!
//! 提供 CLI 和 Bridge 共用的密码管理函数。
//! 底层依赖 `store::users` DAO 和 `password` 哈希模块。
//!
//! 不变式：bridge.password 文件和 SQLite 必须保持一致。
//! 所有密码变更操作遵循"先写文件，后改 DB，失败回滚文件"的两阶段流程。

use crate::audit::AuditStore;
use crate::paths;
use crate::store::users::UserRow;
use std::io::Write;

/// 生成 64 字符随机密码（32 字节 hex = 256 bit 熵）。
pub fn generate_random_password() -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS random");
    buf.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 将密码写入文件（0600 权限），仅在文件不存在时创建。
pub fn write_password_file(path: &std::path::Path, password: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(password.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, password)?;
    }
    Ok(())
}

/// 覆盖密码文件（0600 权限），用于 reset/set 操作。
pub fn overwrite_password_file(path: &std::path::Path, password: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut f = std::fs::File::create(path)?;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        f.write_all(password.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, password)?;
    }
    Ok(())
}

/// 查找 admin 用户。若不存在根据 sys_user 是否为空返回不同错误。
pub fn resolve_admin_user(store: &AuditStore) -> Result<UserRow, String> {
    match store.get_user_by_username("admin") {
        Ok(Some(u)) => Ok(u),
        Ok(None) => {
            let count = store
                .count_users()
                .map_err(|e| format!("count users: {e}"))?;
            if count == 0 {
                Err("sys_user 表为空，请先启动 bridge 初始化".to_string())
            } else {
                Err("admin 用户不存在，请检查数据库".to_string())
            }
        }
        Err(e) => Err(format!("query admin: {e}")),
    }
}

/// 两阶段密码更新：先写文件，后改 DB，DB 失败则回滚文件。
///
/// 保证：返回 Ok 时文件和 DB 一致；返回 Err 时两者都保持原状（尽力回滚）。
fn atomic_password_update(
    store: &AuditStore,
    user_id: &str,
    new_password: &str,
) -> Result<(), String> {
    let path = paths::bridge_password_path();

    // Phase 1: 先写文件（安全：新密码在文件里，但 DB 仍是旧密码，旧密码仍可登录）
    let old_file = std::fs::read_to_string(&path).ok();
    overwrite_password_file(&path, new_password)
        .map_err(|e| format!("write password file: {e}"))?;

    // Phase 2: 更新 DB
    match set_password_for_user(store, user_id, new_password) {
        Ok(()) => Ok(()),
        Err(db_err) => {
            // 回滚文件
            if let Some(old) = old_file {
                let _ = overwrite_password_file(&path, &old);
            } else {
                let _ = std::fs::remove_file(&path);
            }
            Err(format!("update DB failed (file restored): {db_err}"))
        }
    }
}

/// 重置 admin 密码：生成新随机密码，更新 SQLite 和 bridge.password 文件。
/// 返回明文新密码（CLI 可以打印到 stdout）。
pub fn reset_admin_password(store: &AuditStore) -> Result<String, String> {
    let user = resolve_admin_user(store)?;
    let new_password = generate_random_password();
    atomic_password_update(store, &user.id, &new_password)?;
    Ok(new_password)
}

/// 设置 admin 密码：用传入的新密码更新 SQLite 和 bridge.password 文件。
pub fn set_admin_password(store: &AuditStore, new_password: &str) -> Result<(), String> {
    let user = resolve_admin_user(store)?;
    atomic_password_update(store, &user.id, new_password)
}

/// 仅当 sys_user 为空时初始化 admin 用户；已有用户则拒绝。
pub fn init_admin_user(store: &AuditStore) -> Result<(), String> {
    match store.count_users() {
        Ok(0) => {}
        Ok(_) => return Err("sys_user 已有用户，请用 password reset 或 password set".to_string()),
        Err(e) => return Err(format!("count users: {e}")),
    }
    bootstrap_owner_user(store)
}

/// 启动时若 sys_user 为空，自动创建 admin/owner 用户。
///
/// 密码来源优先级：
/// 1. `AGENT_ASPECT_BRIDGE_PASSWORD` 环境变量
/// 2. `~/.agent-aspect/bridge.password` 文件
/// 3. 生成随机密码并写入 `bridge.password`（0600 权限）
pub fn bootstrap_owner_user(store: &AuditStore) -> Result<(), String> {
    match store.count_users() {
        Ok(n) if n > 0 => return Ok(()),
        Ok(_) => {}
        Err(e) => return Err(format!("count users: {e}")),
    }

    let password_path = paths::bridge_password_path();

    let password = if let Some(pwd) = std::env::var("AGENT_ASPECT_BRIDGE_PASSWORD").ok() {
        pwd
    } else if password_path.exists() {
        std::fs::read_to_string(&password_path)
            .map(|p| p.trim().to_string())
            .map_err(|e| format!("read password file: {e}"))?
    } else {
        let pwd = generate_random_password();
        write_password_file(&password_path, &pwd)
            .map_err(|e| format!("write password file: {e}"))?;
        pwd
    };

    let (hash, salt) =
        crate::password::hash_password(&password).map_err(|e| format!("hash password: {e}"))?;

    let now = chrono::Utc::now().to_rfc3339();
    let id = uuid::Uuid::now_v7().to_string();

    store
        .create_user(&id, "admin", &hash, &salt, "owner", &now, &now)
        .map_err(|e| format!("create user: {e}"))?;

    eprintln!(
        "agent-aspect-bridge: bootstrapped admin user (password at {})",
        password_path.display()
    );
    Ok(())
}

/// 内部 helper：hash 新密码并更新 SQLite。
fn set_password_for_user(
    store: &AuditStore,
    user_id: &str,
    new_password: &str,
) -> Result<(), String> {
    let (hash, salt) =
        crate::password::hash_password(new_password).map_err(|e| format!("hash: {e}"))?;
    let now = chrono::Utc::now().to_rfc3339();
    store
        .update_user_password(user_id, &hash, &salt, &now)
        .map_err(|e| format!("update password: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditStore;
    use std::sync::Mutex;

    /// 串行化 HOME 修改，防止并行测试互相污染。
    static HOME_MUTEX: Mutex<()> = Mutex::new(());

    /// 创建唯一临时目录（进程 ID + 时间戳 + 计数器）。
    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("{prefix}-{}-{ts}-{n}", std::process::id()))
    }

    /// 创建带 admin 用户的内存 DB。
    fn setup_store_and_user() -> AuditStore {
        let store = AuditStore::open_in_memory().expect("open db");
        let (hash, salt) = crate::password::hash_password("initial-password").unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let id = uuid::Uuid::now_v7().to_string();
        store
            .create_user(&id, "admin", &hash, &salt, "owner", &now, &now)
            .unwrap();
        store
    }

    #[test]
    fn generate_random_password_length() {
        let pwd = generate_random_password();
        assert_eq!(pwd.len(), 64, "32 bytes hex = 64 chars");
        assert!(pwd.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn overwrite_password_file_roundtrip() {
        let dir = unique_temp_dir("agent-aspect-test-pwd-file");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test.password");

        write_password_file(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");

        // create_new should NOT overwrite
        assert!(write_password_file(&path, "second").is_err());

        // overwrite should work
        overwrite_password_file(&path, "third").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "third");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- 以下测试修改 HOME，必须持锁串行 ---

    #[test]
    fn reset_admin_password_changes_db_and_file() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let tmp_home = unique_temp_dir("agent-aspect-test-reset");
        let _ = std::fs::create_dir_all(&tmp_home);
        let old_home = unsafe {
            let old = std::env::var("HOME").unwrap_or_default();
            std::env::set_var("HOME", &tmp_home);
            old
        };

        let store = setup_store_and_user();
        let new_password = reset_admin_password(&store).unwrap();
        assert_eq!(new_password.len(), 64);

        let user = store.get_user_by_username("admin").unwrap().unwrap();
        assert!(crate::password::verify_password(
            &new_password,
            &user.password_hash,
            &user.password_salt
        ));
        assert!(!crate::password::verify_password(
            "initial-password",
            &user.password_hash,
            &user.password_salt
        ));

        // 文件内容应与 DB 一致
        let file_content =
            std::fs::read_to_string(tmp_home.join(".agent-aspect/bridge.password")).unwrap();
        assert_eq!(file_content, new_password);

        unsafe { std::env::set_var("HOME", &old_home) };
        let _ = std::fs::remove_dir_all(&tmp_home);
    }

    #[test]
    fn set_admin_password_updates_db_and_file() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let tmp_home = unique_temp_dir("agent-aspect-test-set");
        let _ = std::fs::create_dir_all(&tmp_home);
        let old_home = unsafe {
            let old = std::env::var("HOME").unwrap_or_default();
            std::env::set_var("HOME", &tmp_home);
            old
        };

        let store = setup_store_and_user();
        set_admin_password(&store, "new-password-12chars").unwrap();

        let user = store.get_user_by_username("admin").unwrap().unwrap();
        assert!(crate::password::verify_password(
            "new-password-12chars",
            &user.password_hash,
            &user.password_salt
        ));

        let file_content =
            std::fs::read_to_string(tmp_home.join(".agent-aspect/bridge.password")).unwrap();
        assert_eq!(file_content, "new-password-12chars");

        unsafe { std::env::set_var("HOME", &old_home) };
        let _ = std::fs::remove_dir_all(&tmp_home);
    }

    #[test]
    fn init_rejects_when_users_exist() {
        let store = setup_store_and_user();
        let result = init_admin_user(&store);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("已有用户"));
    }

    #[test]
    fn bootstrap_creates_admin_in_empty_db() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let tmp_home = unique_temp_dir("agent-aspect-test-bootstrap");
        let _ = std::fs::create_dir_all(&tmp_home);
        let old_home = unsafe {
            let old = std::env::var("HOME").unwrap_or_default();
            std::env::set_var("HOME", &tmp_home);
            old
        };

        let store = AuditStore::open_in_memory().expect("open db");
        unsafe {
            std::env::set_var("AGENT_ASPECT_BRIDGE_PASSWORD", "env-password-12");
        }
        let result = bootstrap_owner_user(&store);
        unsafe {
            std::env::remove_var("AGENT_ASPECT_BRIDGE_PASSWORD");
        }
        assert!(result.is_ok());

        let user = store.get_user_by_username("admin").unwrap().unwrap();
        assert_eq!(user.role, "owner");
        assert!(crate::password::verify_password(
            "env-password-12",
            &user.password_hash,
            &user.password_salt
        ));

        unsafe { std::env::set_var("HOME", &old_home) };
        let _ = std::fs::remove_dir_all(&tmp_home);
    }

    #[test]
    fn bootstrap_idempotent() {
        let _guard = HOME_MUTEX.lock().unwrap();
        let tmp_home = unique_temp_dir("agent-aspect-test-idempotent");
        let _ = std::fs::create_dir_all(&tmp_home);
        let old_home = unsafe {
            let old = std::env::var("HOME").unwrap_or_default();
            std::env::set_var("HOME", &tmp_home);
            old
        };

        let store = AuditStore::open_in_memory().expect("open db");
        unsafe {
            std::env::set_var("AGENT_ASPECT_BRIDGE_PASSWORD", "first-password-12");
        }
        bootstrap_owner_user(&store).unwrap();
        unsafe {
            std::env::remove_var("AGENT_ASPECT_BRIDGE_PASSWORD");
        }

        // Second call should be no-op
        unsafe {
            std::env::set_var("AGENT_ASPECT_BRIDGE_PASSWORD", "second-password-12");
        }
        bootstrap_owner_user(&store).unwrap();
        unsafe {
            std::env::remove_var("AGENT_ASPECT_BRIDGE_PASSWORD");
        }

        let user = store.get_user_by_username("admin").unwrap().unwrap();
        assert!(crate::password::verify_password(
            "first-password-12",
            &user.password_hash,
            &user.password_salt
        ));

        unsafe { std::env::set_var("HOME", &old_home) };
        let _ = std::fs::remove_dir_all(&tmp_home);
    }

    #[test]
    fn resolve_admin_user_errors_on_empty_db() {
        let store = AuditStore::open_in_memory().expect("open db");
        let result = resolve_admin_user(&store);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("为空"));
    }
}
