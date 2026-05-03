//! 密码哈希与校验 — argon2id 实现。
//!
//! 提供 `hash_password` 和 `verify_password` 两个函数，
//! 所有密码存储都走此模块，不在业务代码中散落哈希逻辑。

use argon2::password_hash::{Encoding, SaltString};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

/// 用 argon2id 哈希密码，返回 (hash, salt) 两个字符串。
/// salt 由 getrandom 生成随机字节，hash 以 PHC 格式存储。
pub fn hash_password(password: &str) -> Result<(String, String), String> {
    let mut salt_bytes = [0u8; 32];
    getrandom::fill(&mut salt_bytes).map_err(|e| format!("random salt: {e}"))?;
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| format!("encode salt: {e}"))?;
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| format!("argon2 hash failed: {e}"))?;
    Ok((hash.to_string(), salt.to_string()))
}

/// 校验密码是否匹配已存储的 hash + salt。
/// 使用 argon2 库的恒定时间比较，不做字符串裸比对。
pub fn verify_password(password: &str, stored_hash: &str, _stored_salt: &str) -> bool {
    let parsed = match PasswordHash::parse(stored_hash, Encoding::B64) {
        Ok(p) => p,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_and_verify_correct_password() {
        let (hash, salt) = hash_password("correct-horse-battery-staple").unwrap();
        assert!(verify_password(
            "correct-horse-battery-staple",
            &hash,
            &salt
        ));
    }

    #[test]
    fn wrong_password_fails() {
        let (hash, salt) = hash_password("my-secret").unwrap();
        assert!(!verify_password("wrong-password", &hash, &salt));
    }

    #[test]
    fn each_hash_is_unique() {
        let (h1, s1) = hash_password("same-input").unwrap();
        let (h2, s2) = hash_password("same-input").unwrap();
        assert_ne!(h1, h2, "same password should produce different hashes");
        assert_ne!(s1, s2, "each call should generate a new salt");
    }

    #[test]
    fn invalid_hash_returns_false() {
        assert!(!verify_password("anything", "not-a-valid-hash", "somesalt"));
    }
}
