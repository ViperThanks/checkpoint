//! Runtime Profile Resolver — 探测当前 provider 的运行时身份 + drift 检测。
//!
//! 架构角色：
//! - 探测当前环境的 model_id / profile_name / permission_mode / toolchain_fingerprint
//! - 提供 identity 比较逻辑（resume 前校验 model/profile 是否一致）
//! - 计算 RuntimeHealth（ok / warning / critical）供 API 和 UI 消费
//! - identity 持久化到 conversations 表（DAO 在 conversations.rs）
//!
//! 探测策略（按可靠性递减）：
//! 1. 环境变量（ANTHROPIC_MODEL / OPENAI_MODEL / CODEX_* 等）
//! 2. provider CLI config 文件（~/.claude.json / ~/.codex/config.toml 等）
//! 3. ccswitch profile（如果 ~/.ccswitch/ 存在）
//! 4. 回退到 "unknown"

use serde::Serialize;
use std::path::{Path, PathBuf};

/// 当前运行时的 provider 身份快照。
///
/// 在 agent_prompt 执行时采集，存入 conversation 记录。
/// resume 时重新采集并与记录值比较，不一致则要求确认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeIdentity {
    /// 模型标识（如 "sonnet", "opus", "gpt-5.4", "unknown"）
    pub model_id: String,
    /// 运行时 profile 名称（如 ccswitch profile 名、"default"、"unknown"）
    pub profile_name: String,
    /// 工作区路径（通常是 project_path）
    pub workspace_path: Option<String>,
    /// config 文件内容的 SHA-256 前 16 位，用于检测配置变更
    pub config_hash: Option<String>,
    /// 权限模式（"bypassPermissions" / "default" / "unknown"）
    pub permission_mode: String,
    /// provider binary 绝对路径（用于检测 entrypoint 变更）
    pub entrypoint: Option<String>,
    /// 工具链指纹 — which cargo/git/command 输出的 SHA-256 前 16 位
    pub toolchain_fingerprint: Option<String>,
}

/// identity 比较结果。
#[derive(Debug, Clone, Serialize)]
pub struct IdentityMismatch {
    pub field: String,
    pub recorded: String,
    pub current: String,
}

/// 运行时健康状态 — resume 前校验结果。
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeHealth {
    /// 整体状态
    pub status: RuntimeHealthStatus,
    /// 所有检测到的不匹配项
    pub warnings: Vec<IdentityMismatch>,
    /// 标记为 critical 的字段名（如 ["model_id", "permission_mode"]）
    pub critical_fields: Vec<String>,
}

/// 健康等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHealthStatus {
    Ok,
    Warning,
    Critical,
}

/// 比较两个 identity 是否兼容（用于 resume 前校验）。
///
/// 返回空 Vec 表示完全兼容；非空表示有不匹配，需要用户确认。
/// "unknown" 值不触发 mismatch（兼容老数据）。
pub fn compare_identities(
    recorded: &RuntimeIdentity,
    current: &RuntimeIdentity,
) -> Vec<IdentityMismatch> {
    let mut mismatches = Vec::new();

    if recorded.model_id != "unknown"
        && current.model_id != "unknown"
        && recorded.model_id != current.model_id
    {
        mismatches.push(IdentityMismatch {
            field: "model_id".to_string(),
            recorded: recorded.model_id.clone(),
            current: current.model_id.clone(),
        });
    }

    if recorded.profile_name != "unknown"
        && current.profile_name != "unknown"
        && recorded.profile_name != current.profile_name
    {
        mismatches.push(IdentityMismatch {
            field: "profile_name".to_string(),
            recorded: recorded.profile_name.clone(),
            current: current.profile_name.clone(),
        });
    }

    // workspace_path 只在两者都非 None 且不同时报告
    match (&recorded.workspace_path, &current.workspace_path) {
        (Some(r), Some(c)) if r != c => {
            mismatches.push(IdentityMismatch {
                field: "workspace_path".to_string(),
                recorded: r.clone(),
                current: c.clone(),
            });
        }
        _ => {}
    }

    if recorded.permission_mode != "unknown"
        && current.permission_mode != "unknown"
        && recorded.permission_mode != current.permission_mode
    {
        mismatches.push(IdentityMismatch {
            field: "permission_mode".to_string(),
            recorded: recorded.permission_mode.clone(),
            current: current.permission_mode.clone(),
        });
    }

    if let (Some(r), Some(c)) = (&recorded.config_hash, &current.config_hash) {
        if r != c {
            mismatches.push(IdentityMismatch {
                field: "config_hash".to_string(),
                recorded: r.clone(),
                current: c.clone(),
            });
        }
    }

    if let (Some(r), Some(c)) = (
        &recorded.toolchain_fingerprint,
        &current.toolchain_fingerprint,
    ) {
        if r != c {
            mismatches.push(IdentityMismatch {
                field: "toolchain_fingerprint".to_string(),
                recorded: r.clone(),
                current: c.clone(),
            });
        }
    }

    mismatches
}

/// 计算 runtime health：根据 identity mismatch 分类严重程度。
///
/// 分类规则：
/// - model_id 不匹配 → Critical
/// - permission_mode 降级（bypassPermissions → default）→ Critical
/// - 其他 mismatch（profile, config_hash, toolchain）→ Warning
/// - 无 mismatch → Ok
pub fn compute_runtime_health(
    recorded: &RuntimeIdentity,
    current: &RuntimeIdentity,
) -> RuntimeHealth {
    let mismatches = compare_identities(recorded, current);
    let mut critical_fields = Vec::new();

    for m in &mismatches {
        let is_critical = match m.field.as_str() {
            "model_id" => true,
            "permission_mode" => {
                // 降级检测：bypass → default 是降级
                m.recorded == "bypassPermissions" && m.current != "bypassPermissions"
            }
            _ => false,
        };
        if is_critical {
            critical_fields.push(m.field.clone());
        }
    }

    let status = if !critical_fields.is_empty() {
        RuntimeHealthStatus::Critical
    } else if !mismatches.is_empty() {
        RuntimeHealthStatus::Warning
    } else {
        RuntimeHealthStatus::Ok
    };

    RuntimeHealth {
        status,
        warnings: mismatches,
        critical_fields,
    }
}

/// 探测当前环境的运行时身份。
///
/// `provider`: provider key（如 "claude_code"）
/// `workspace_path`: 当前项目路径（可选，用于填充 identity）
pub fn probe_identity(provider: &str, workspace_path: Option<&str>) -> RuntimeIdentity {
    match provider {
        "claude_code" => probe_claude_code(workspace_path),
        "codex_cli" => probe_codex_cli(workspace_path),
        "kimi_code" => probe_kimi_code(workspace_path),
        _ => RuntimeIdentity {
            model_id: "unknown".to_string(),
            profile_name: "unknown".to_string(),
            workspace_path: workspace_path.map(|s| s.to_string()),
            config_hash: None,
            permission_mode: "unknown".to_string(),
            entrypoint: None,
            toolchain_fingerprint: None,
        },
    }
}

/// 探测 Claude Code 的当前 model、profile 和 permission_mode。
fn probe_claude_code(workspace_path: Option<&str>) -> RuntimeIdentity {
    let model_id = detect_claude_model();
    let profile_name = detect_ccswitch_profile().unwrap_or_else(|| "default".to_string());
    let config_hash = hash_claude_config();
    let permission_mode = detect_claude_permission_mode();
    let entrypoint = which_binary("claude");
    let toolchain_fingerprint = compute_toolchain_fingerprint("claude");

    RuntimeIdentity {
        model_id,
        profile_name,
        workspace_path: workspace_path.map(|s| s.to_string()),
        config_hash,
        permission_mode,
        entrypoint,
        toolchain_fingerprint,
    }
}

/// 探测 Codex CLI 的当前 model。
fn probe_codex_cli(workspace_path: Option<&str>) -> RuntimeIdentity {
    let model_id = std::env::var("OPENAI_MODEL")
        .or_else(|_| std::env::var("CODEX_MODEL"))
        .unwrap_or_else(|_| "unknown".to_string());

    let config_hash = codex_config_paths().iter().find_map(|path| hash_file(path));
    let permission_mode = detect_codex_permission_mode(workspace_path);
    let entrypoint = which_binary("codex");
    let toolchain_fingerprint = compute_toolchain_fingerprint("codex");

    RuntimeIdentity {
        model_id,
        profile_name: "default".to_string(),
        workspace_path: workspace_path.map(|s| s.to_string()),
        config_hash,
        permission_mode,
        entrypoint,
        toolchain_fingerprint,
    }
}

/// 探测 Kimi Code 的当前 model。
fn probe_kimi_code(workspace_path: Option<&str>) -> RuntimeIdentity {
    let model_id = std::env::var("KIMI_MODEL").unwrap_or_else(|_| "unknown".to_string());
    let entrypoint = which_binary("kimi");
    let toolchain_fingerprint = compute_toolchain_fingerprint("kimi");

    RuntimeIdentity {
        model_id,
        profile_name: "default".to_string(),
        workspace_path: workspace_path.map(|s| s.to_string()),
        config_hash: None,
        permission_mode: "unknown".to_string(),
        entrypoint,
        toolchain_fingerprint,
    }
}

/// 检测 Claude Code 的当前 model。
///
/// 优先级：
/// 1. ANTHROPIC_MODEL 环境变量
/// 2. CLAUDE_MODEL 环境变量（旧名）
/// 3. ~/.claude.json 中的 model 字段
/// 4. "unknown"
fn detect_claude_model() -> String {
    // 环境变量优先
    if let Ok(m) = std::env::var("ANTHROPIC_MODEL") {
        return normalize_model_id(&m);
    }
    if let Ok(m) = std::env::var("CLAUDE_MODEL") {
        return normalize_model_id(&m);
    }

    // 尝试读 ~/.claude.json
    if let Some(model) = read_claude_json_model() {
        return normalize_model_id(&model);
    }

    "unknown".to_string()
}

/// 检测 Claude Code 的 permission_mode。
///
/// 读 ~/.claude.json 中的 permissions 字段。
/// 如果 allow 列表包含 "*" → "bypassPermissions"，否则 "default"。
fn detect_claude_permission_mode() -> String {
    let path = home_path(".claude.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return "unknown".to_string(),
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return "unknown".to_string(),
    };

    // 检查 permissions.allow 是否包含 "*"
    let has_wildcard = json
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some("*")))
        .unwrap_or(false);

    if has_wildcard {
        "bypassPermissions".to_string()
    } else {
        "default".to_string()
    }
}

/// 检测 Codex CLI 的 permission_mode。
///
/// 优先读显式环境变量；其次解析 Codex config 中的 sandbox_mode 或项目 trust_level。
/// Codex trusted project 在当前桌面环境等价于 full access，统一落到内部 canonical
/// `bypassPermissions`，让 runtime drift 逻辑与 Claude Code 共用同一套判定。
fn detect_codex_permission_mode(workspace_path: Option<&str>) -> String {
    let env_keys = [
        "CODEX_PERMISSION_MODE",
        "CODEX_SANDBOX_MODE",
        "CODEX_SANDBOX",
        "CODEX_FULL_ACCESS",
    ];
    for key in env_keys {
        if let Ok(value) = std::env::var(key) {
            if let Some(mode) = normalize_permission_mode(&value) {
                return mode;
            }
        }
    }

    for path in codex_config_paths() {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(mode) = codex_permission_mode_from_config(&content, workspace_path) {
            return mode;
        }
    }

    "unknown".to_string()
}

/// 将 provider 原生权限名归一到 Agent Aspect 内部权限语义。
fn normalize_permission_mode(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let key = trimmed.to_ascii_lowercase().replace('_', "-");
    match key.as_str() {
        "1" | "true" | "yes" | "full" | "full-access" | "danger-full-access" | "bypass"
        | "bypasspermissions" | "bypass-permissions" => {
            Some(crate::constants::PERMISSION_MODE_BYPASS.to_string())
        }
        "0" | "false" | "no" | "default" | "on-request" | "ask" => Some("default".to_string()),
        "read-only" | "readonly" => Some("read-only".to_string()),
        "workspace-write" | "workspacewrite" => Some("workspace-write".to_string()),
        "unknown" => Some("unknown".to_string()),
        _ => None,
    }
}

/// 从 Codex config.toml 中解析权限模式。
fn codex_permission_mode_from_config(
    content: &str,
    workspace_path: Option<&str>,
) -> Option<String> {
    let value: toml::Value = content.parse().ok()?;

    if let Some(mode) = value.get("sandbox_mode").and_then(|v| v.as_str()) {
        if let Some(normalized) = normalize_permission_mode(mode) {
            return Some(normalized);
        }
    }

    let workspace_path = workspace_path?;
    let projects = value.get("projects").and_then(|v| v.as_table())?;
    let mut best: Option<(usize, String)> = None;

    for (project_path, project_config) in projects {
        if !path_contains_workspace(project_path, workspace_path) {
            continue;
        }
        let Some(trust_level) = project_config.get("trust_level").and_then(|v| v.as_str()) else {
            continue;
        };
        let mode = match trust_level {
            "trusted" => crate::constants::PERMISSION_MODE_BYPASS.to_string(),
            "untrusted" => "default".to_string(),
            other => match normalize_permission_mode(other) {
                Some(mode) => mode,
                None => continue,
            },
        };
        let score = Path::new(project_path).components().count();
        if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
            best = Some((score, mode));
        }
    }

    best.map(|(_, mode)| mode)
}

/// 判断 Codex project 配置是否覆盖当前 workspace。
fn path_contains_workspace(project_path: &str, workspace_path: &str) -> bool {
    let project = Path::new(project_path);
    let workspace = Path::new(workspace_path);
    workspace == project || workspace.starts_with(project)
}

/// Codex config.toml 的候选路径，CODEX_HOME 优先。
fn codex_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        if !codex_home.trim().is_empty() {
            paths.push(PathBuf::from(codex_home).join("config.toml"));
        }
    }
    paths.push(home_path(".codex/config.toml"));
    paths.push(home_path(".config/codex/config.toml"));
    paths.dedup();
    paths
}

/// 执行 `which <command>` 获取 binary 绝对路径。
fn which_binary(command: &str) -> Option<String> {
    std::process::Command::new("which")
        .arg(command)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 计算工具链指纹 — 拼接 which cargo; which git; which <command> 输出后 hash。
fn compute_toolchain_fingerprint(provider_command: &str) -> Option<String> {
    let mut combined = String::new();
    for cmd in &["cargo", "git", provider_command] {
        if let Some(path) = which_binary(cmd) {
            combined.push_str(&path);
            combined.push('\n');
        }
    }
    if combined.is_empty() {
        return None;
    }
    hash_bytes(combined.as_bytes())
}

/// 对字节内容做 SHA-256 并返回前 16 位 hex。
fn hash_bytes(data: &[u8]) -> Option<String> {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    Some(format!("{:x}", hash)[..16].to_string())
}

/// 将完整 model 名称标准化为短标识。
///
/// "claude-sonnet-4-20250514" → "sonnet"
/// "claude-opus-4-20250514" → "opus"
/// "claude-haiku-4-20250514" → "haiku"
/// "sonnet" → "sonnet"
/// 其他 → 原样返回
fn normalize_model_id(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("opus") {
        "opus".to_string()
    } else if lower.contains("sonnet") {
        "sonnet".to_string()
    } else if lower.contains("haiku") {
        "haiku".to_string()
    } else {
        raw.to_string()
    }
}

/// 读取 ~/.claude.json 中的 model 字段。
fn read_claude_json_model() -> Option<String> {
    let path = home_path(".claude.json");
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    json.get("model")
        .or_else(|| json.get("defaultModel"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// 检测 ccswitch profile。
///
/// ccswitch 是 Claude Code 的 profile 切换工具。
/// 检测策略：
/// 1. CCSWITCH_PROFILE 环境变量
/// 2. ~/.ccswitch/current 文件内容
/// 3. None（表示没有 ccswitch 或使用默认 profile）
fn detect_ccswitch_profile() -> Option<String> {
    if let Ok(p) = std::env::var("CCSWITCH_PROFILE") {
        return Some(p);
    }

    let current_path = home_path(".ccswitch/current");
    std::fs::read_to_string(&current_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 计算 Claude 配置文件的 hash。
fn hash_claude_config() -> Option<String> {
    hash_file(&home_path(".claude.json"))
}

/// 计算文件内容的 SHA-256 前 16 位。
fn hash_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let content = std::fs::read(path).ok()?;
    let hash = Sha256::digest(&content);
    Some(format!("{:x}", hash)[..16].to_string())
}

/// 构造 home 目录下的路径。
fn home_path(relative: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_identity(model: &str, profile: &str, ws: Option<&str>) -> RuntimeIdentity {
        RuntimeIdentity {
            model_id: model.to_string(),
            profile_name: profile.to_string(),
            workspace_path: ws.map(|s| s.to_string()),
            config_hash: None,
            permission_mode: "unknown".to_string(),
            entrypoint: None,
            toolchain_fingerprint: None,
        }
    }

    fn make_full_identity(
        model: &str,
        profile: &str,
        ws: Option<&str>,
        permission: &str,
    ) -> RuntimeIdentity {
        RuntimeIdentity {
            model_id: model.to_string(),
            profile_name: profile.to_string(),
            workspace_path: ws.map(|s| s.to_string()),
            config_hash: None,
            permission_mode: permission.to_string(),
            entrypoint: None,
            toolchain_fingerprint: None,
        }
    }

    #[test]
    fn compare_exact_match_no_mismatch() {
        let a = make_identity("sonnet", "default", Some("/proj"));
        let b = make_identity("sonnet", "default", Some("/proj"));
        assert!(compare_identities(&a, &b).is_empty());
    }

    #[test]
    fn compare_model_mismatch_detected() {
        let recorded = make_identity("sonnet", "default", None);
        let current = make_identity("opus", "default", None);
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].field, "model_id");
        assert_eq!(mismatches[0].recorded, "sonnet");
        assert_eq!(mismatches[0].current, "opus");
    }

    #[test]
    fn compare_profile_mismatch_detected() {
        let recorded = make_identity("sonnet", "work", None);
        let current = make_identity("sonnet", "personal", None);
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].field, "profile_name");
    }

    #[test]
    fn compare_workspace_mismatch_detected() {
        let recorded = make_identity("sonnet", "default", Some("/proj-a"));
        let current = make_identity("sonnet", "default", Some("/proj-b"));
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].field, "workspace_path");
    }

    #[test]
    fn compare_unknown_fields_are_tolerant() {
        let recorded = make_identity("unknown", "unknown", None);
        let current = make_identity("sonnet", "work", Some("/proj"));
        assert!(compare_identities(&recorded, &current).is_empty());
    }

    #[test]
    fn compare_multiple_mismatches() {
        let recorded = make_identity("sonnet", "work", Some("/a"));
        let current = make_identity("opus", "personal", Some("/b"));
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 3);
    }

    #[test]
    fn compare_permission_mode_mismatch() {
        let recorded = make_full_identity("sonnet", "default", None, "bypassPermissions");
        let current = make_full_identity("sonnet", "default", None, "default");
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].field, "permission_mode");
    }

    #[test]
    fn compare_config_hash_mismatch() {
        let mut recorded = make_identity("sonnet", "default", None);
        recorded.config_hash = Some("abc123".to_string());
        let mut current = make_identity("sonnet", "default", None);
        current.config_hash = Some("def456".to_string());
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].field, "config_hash");
    }

    #[test]
    fn compare_toolchain_fingerprint_mismatch() {
        let mut recorded = make_identity("sonnet", "default", None);
        recorded.toolchain_fingerprint = Some("fp1".to_string());
        let mut current = make_identity("sonnet", "default", None);
        current.toolchain_fingerprint = Some("fp2".to_string());
        let mismatches = compare_identities(&recorded, &current);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].field, "toolchain_fingerprint");
    }

    // --- RuntimeHealth tests ---

    #[test]
    fn health_exact_match_is_ok() {
        let a = make_full_identity("sonnet", "default", Some("/p"), "bypassPermissions");
        let health = compute_runtime_health(&a, &a);
        assert_eq!(health.status, RuntimeHealthStatus::Ok);
        assert!(health.warnings.is_empty());
    }

    #[test]
    fn health_model_mismatch_is_critical() {
        let recorded = make_full_identity("sonnet", "default", None, "bypassPermissions");
        let current = make_full_identity("opus", "default", None, "bypassPermissions");
        let health = compute_runtime_health(&recorded, &current);
        assert_eq!(health.status, RuntimeHealthStatus::Critical);
        assert!(health.critical_fields.contains(&"model_id".to_string()));
    }

    #[test]
    fn health_permission_downgrade_is_critical() {
        let recorded = make_full_identity("sonnet", "default", None, "bypassPermissions");
        let current = make_full_identity("sonnet", "default", None, "default");
        let health = compute_runtime_health(&recorded, &current);
        assert_eq!(health.status, RuntimeHealthStatus::Critical);
        assert!(
            health
                .critical_fields
                .contains(&"permission_mode".to_string())
        );
    }

    #[test]
    fn health_permission_upgrade_is_not_critical() {
        // default → bypassPermissions 是升级，不是 critical
        let recorded = make_full_identity("sonnet", "default", None, "default");
        let current = make_full_identity("sonnet", "default", None, "bypassPermissions");
        let health = compute_runtime_health(&recorded, &current);
        assert_eq!(health.status, RuntimeHealthStatus::Warning);
        assert!(health.critical_fields.is_empty());
    }

    #[test]
    fn health_profile_change_is_warning() {
        let recorded = make_full_identity("sonnet", "work", None, "default");
        let current = make_full_identity("sonnet", "personal", None, "default");
        let health = compute_runtime_health(&recorded, &current);
        assert_eq!(health.status, RuntimeHealthStatus::Warning);
        assert!(health.critical_fields.is_empty());
    }

    #[test]
    fn health_unknown_identity_is_ok() {
        let recorded = make_identity("unknown", "unknown", None);
        let current = make_full_identity("sonnet", "work", Some("/p"), "bypassPermissions");
        let health = compute_runtime_health(&recorded, &current);
        assert_eq!(health.status, RuntimeHealthStatus::Ok);
    }

    #[test]
    fn normalize_model_id_variants() {
        assert_eq!(normalize_model_id("claude-sonnet-4-20250514"), "sonnet");
        assert_eq!(normalize_model_id("claude-opus-4-20250514"), "opus");
        assert_eq!(normalize_model_id("claude-haiku-4-20250514"), "haiku");
        assert_eq!(normalize_model_id("sonnet"), "sonnet");
        assert_eq!(normalize_model_id("gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn probe_unknown_provider_returns_unknown() {
        let identity = probe_identity("nonexistent", None);
        assert_eq!(identity.model_id, "unknown");
        assert_eq!(identity.profile_name, "unknown");
        assert_eq!(identity.permission_mode, "unknown");
    }

    #[test]
    fn probe_preserves_workspace_path() {
        let identity = probe_identity("claude_code", Some("/my/project"));
        assert_eq!(identity.workspace_path.as_deref(), Some("/my/project"));
    }

    #[test]
    fn probe_codex_reads_env() {
        unsafe {
            std::env::set_var("OPENAI_MODEL", "gpt-5.4");
        }
        let identity = probe_identity("codex_cli", None);
        assert_eq!(identity.model_id, "gpt-5.4");
        unsafe {
            std::env::remove_var("OPENAI_MODEL");
        }
    }

    #[test]
    fn codex_permission_normalizes_full_access_variants() {
        assert_eq!(
            normalize_permission_mode("danger-full-access").as_deref(),
            Some(crate::constants::PERMISSION_MODE_BYPASS)
        );
        assert_eq!(
            normalize_permission_mode("bypassPermissions").as_deref(),
            Some(crate::constants::PERMISSION_MODE_BYPASS)
        );
        assert_eq!(
            normalize_permission_mode("workspace_write").as_deref(),
            Some("workspace-write")
        );
    }

    #[test]
    fn codex_permission_reads_sandbox_mode_from_config() {
        let config = r#"
sandbox_mode = "danger-full-access"
"#;
        assert_eq!(
            codex_permission_mode_from_config(config, Some("/tmp/project")).as_deref(),
            Some(crate::constants::PERMISSION_MODE_BYPASS)
        );
    }

    #[test]
    fn codex_permission_reads_trusted_project_from_config() {
        let config = r#"
[projects."/Users/dev/checkpoint"]
trust_level = "trusted"
"#;
        assert_eq!(
            codex_permission_mode_from_config(config, Some("/Users/dev/checkpoint/crates/core"))
                .as_deref(),
            Some(crate::constants::PERMISSION_MODE_BYPASS)
        );
    }

    #[test]
    fn codex_permission_uses_most_specific_project_config() {
        let config = r#"
[projects."/Users/dev"]
trust_level = "trusted"

[projects."/Users/dev/checkpoint"]
trust_level = "untrusted"
"#;
        assert_eq!(
            codex_permission_mode_from_config(config, Some("/Users/dev/checkpoint/crates/core"))
                .as_deref(),
            Some("default")
        );
    }

    #[test]
    fn probe_kimi_reads_env() {
        unsafe {
            std::env::set_var("KIMI_MODEL", "kimi-k2");
        }
        let identity = probe_identity("kimi_code", None);
        assert_eq!(identity.model_id, "kimi-k2");
        unsafe {
            std::env::remove_var("KIMI_MODEL");
        }
    }

    /// bb7a22a7 回测：Claude Code compaction/continue 后 permissionMode 从 bypassPermissions 降到 default。
    /// 这是 permission 与 model drift 的核心事故样本。
    #[test]
    fn regression_bb7a22a7_permission_downgrade_is_critical() {
        // 会话启动时记录的身份
        let recorded = make_full_identity("sonnet", "default", Some("/proj"), "bypassPermissions");
        // compaction/continue 后环境漂移：permission 降级到 default
        let current = make_full_identity("sonnet", "default", Some("/proj"), "default");
        let health = compute_runtime_health(&recorded, &current);

        assert_eq!(health.status, RuntimeHealthStatus::Critical);
        assert!(
            health
                .critical_fields
                .contains(&"permission_mode".to_string())
        );
        assert_eq!(health.warnings.len(), 1);
        assert_eq!(health.warnings[0].field, "permission_mode");
        assert_eq!(health.warnings[0].recorded, "bypassPermissions");
        assert_eq!(health.warnings[0].current, "default");
    }

    /// bb7a22a7 回测扩展：permission 降级 + model 同时变更 → 两个 critical
    #[test]
    fn regression_bb7a22a7_permission_and_model_drift() {
        let recorded = make_full_identity("sonnet", "default", Some("/proj"), "bypassPermissions");
        let current = make_full_identity("opus", "default", Some("/proj"), "default");
        let health = compute_runtime_health(&recorded, &current);

        assert_eq!(health.status, RuntimeHealthStatus::Critical);
        assert!(health.critical_fields.contains(&"model_id".to_string()));
        assert!(
            health
                .critical_fields
                .contains(&"permission_mode".to_string())
        );
    }
}
