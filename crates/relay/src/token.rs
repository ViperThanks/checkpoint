//! 自验证 Relay 令牌，基于 HMAC-SHA256 签名。
//!
//! 职责：签发和验证 Relay 内部令牌（mac_token / client_token）。
//!
//! 架构角色：Relay 认证体系的核心。所有 WebSocket 连接和 HTTP 代理请求
//! 的身份验证都依赖此模块。
//!
//! 令牌格式：`cp_rt1.<payload_b64url>.<sig_b64url>`
//!
//! Payload（base64url 编码的 JSON）：
//! ```json
//! {"ver":1,"sid":"session-uuid","role":"mac|client","iat":unix_ts,"exp":unix_ts,"jti":"unique-id"}
//! ```
//!
//! 不变量：
//! - 无状态验证：不需要数据库查询，签名密钥即可验证真伪。
//! - 版本号固定为 1，未来格式变更需升级 ver 字段。
//! - BadSignature 和 InvalidFormat 对外统一显示 "invalid_token"，不泄露内部细节。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// 令牌前缀，标识版本和类型。
const PREFIX: &str = "cp_rt1.";

/// 令牌负载：包含会话标识、角色、签发/过期时间、唯一 ID。
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TokenPayload {
    pub ver: u8,
    /// 会话 ID，mac_token 和 client_token 共享同一 sid。
    pub sid: String,
    /// 角色："mac"（WebSocket 连接用）或 "client"（HTTP 代理用）。
    pub role: String,
    /// 签发时间（Unix 秒）。
    pub iat: i64,
    /// 过期时间（Unix 秒）。
    pub exp: i64,
    /// 令牌唯一 ID，防止重放。
    pub jti: String,
    /// client token 轮换代数。旧 token 缺省为 0，仅用于迁移兼容。
    #[serde(default)]
    pub generation: u64,
}

/// 验证通过的令牌，包含解码后的 payload 和原始字符串。
#[derive(Debug)]
pub struct VerifiedToken {
    pub payload: TokenPayload,
    pub raw: String,
}

/// 令牌验证错误类型。
#[derive(Debug, PartialEq)]
pub enum TokenError {
    InvalidFormat,
    BadSignature,
    Expired,
    UnsupportedVersion,
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // BadSignature 和 InvalidFormat 统一显示，不泄露区别
            TokenError::InvalidFormat => write!(f, "invalid_token"),
            TokenError::BadSignature => write!(f, "invalid_token"),
            TokenError::Expired => write!(f, "token_expired"),
            TokenError::UnsupportedVersion => write!(f, "invalid_token"),
        }
    }
}

/// 使用 HMAC-SHA256 签发令牌。
///
/// 返回格式为 `cp_rt1.<base64url(payload_json)>.<base64url(hmac_signature)>`。
pub fn sign_token(secret: &[u8], payload: &TokenPayload) -> String {
    let payload_json = serde_json::to_string(payload).expect("serialize payload");
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());

    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key length");
    mac.update(payload_b64.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = URL_SAFE_NO_PAD.encode(sig);

    format!("{PREFIX}{payload_b64}.{sig_b64}")
}

/// 验证令牌的签名、格式和有效期。
///
/// 验证流程：前缀检查 → 拆分 payload/sig → HMAC 验签 → 解码 payload →
/// 版本检查 → 过期检查。
pub fn verify_token(secret: &[u8], token: &str) -> Result<VerifiedToken, TokenError> {
    let rest = token
        .strip_prefix(PREFIX)
        .ok_or(TokenError::InvalidFormat)?;

    // 用 rfind 定位最后一个点，分离 payload 和签名
    let dot_pos = rest.rfind('.').ok_or(TokenError::InvalidFormat)?;
    let payload_b64 = &rest[..dot_pos];
    let sig_b64 = &rest[dot_pos + 1..];

    // HMAC 常量时间比较，防止时序攻击
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC key length");
    mac.update(payload_b64.as_bytes());
    let sig_bytes = URL_SAFE_NO_PAD
        .decode(sig_b64)
        .map_err(|_| TokenError::InvalidFormat)?;
    mac.verify_slice(&sig_bytes)
        .map_err(|_| TokenError::BadSignature)?;

    // 解码 payload JSON
    let payload_json = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| TokenError::InvalidFormat)?;
    let payload: TokenPayload =
        serde_json::from_slice(&payload_json).map_err(|_| TokenError::InvalidFormat)?;

    // 只支持版本 1
    if payload.ver != 1 {
        return Err(TokenError::UnsupportedVersion);
    }

    // 检查是否过期
    let now = chrono::Utc::now().timestamp();
    if payload.exp < now {
        return Err(TokenError::Expired);
    }

    Ok(VerifiedToken {
        payload,
        raw: token.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_secret() -> Vec<u8> {
        b"test-secret-key-32-bytes-long!!!!".to_vec()
    }

    fn test_payload(role: &str, exp_offset_secs: i64) -> TokenPayload {
        let now = chrono::Utc::now().timestamp();
        TokenPayload {
            ver: 1,
            sid: uuid::Uuid::now_v7().to_string(),
            role: role.to_string(),
            iat: now,
            exp: now + exp_offset_secs,
            jti: uuid::Uuid::now_v7().to_string(),
            generation: 1,
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let secret = test_secret();
        let payload = test_payload("mac", 3600);
        let token = sign_token(&secret, &payload);

        let verified = verify_token(&secret, &token).unwrap();
        assert_eq!(verified.payload.sid, payload.sid);
        assert_eq!(verified.payload.role, "mac");
        assert_eq!(verified.payload.ver, 1);
        assert!(token.starts_with("cp_rt1."));
    }

    #[test]
    fn tampered_signature_fails() {
        let secret = test_secret();
        let payload = test_payload("client", 3600);
        let token = sign_token(&secret, &payload);

        // Tamper with the signature
        let mut chars: Vec<char> = token.chars().collect();
        let last_dot = token.rfind('.').unwrap();
        if chars[last_dot + 1] == 'A' {
            chars[last_dot + 1] = 'B';
        } else {
            chars[last_dot + 1] = 'A';
        }
        let tampered: String = chars.into_iter().collect();

        assert_eq!(
            verify_token(&secret, &tampered).unwrap_err(),
            TokenError::BadSignature
        );
    }

    #[test]
    fn expired_token_fails() {
        let secret = test_secret();
        let payload = test_payload("mac", -100); // expired 100s ago
        let token = sign_token(&secret, &payload);

        assert_eq!(
            verify_token(&secret, &token).unwrap_err(),
            TokenError::Expired
        );
    }

    #[test]
    fn wrong_secret_fails() {
        let secret = test_secret();
        let wrong_secret = b"wrong-secret-key-32-bytes-long!".to_vec();
        let payload = test_payload("client", 3600);
        let token = sign_token(&secret, &payload);

        assert_eq!(
            verify_token(&wrong_secret, &token).unwrap_err(),
            TokenError::BadSignature
        );
    }

    #[test]
    fn bad_format_fails() {
        let secret = test_secret();
        assert_eq!(
            verify_token(&secret, "garbage").unwrap_err(),
            TokenError::InvalidFormat
        );
        assert_eq!(
            verify_token(&secret, "cp_rt1.").unwrap_err(),
            TokenError::InvalidFormat
        );
        assert_eq!(
            verify_token(&secret, "not_cp_rt1.abc.def").unwrap_err(),
            TokenError::InvalidFormat
        );
    }
}
