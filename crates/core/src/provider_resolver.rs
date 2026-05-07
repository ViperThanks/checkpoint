//! Provider 二进制发现 — 在系统中查找和验证 AI provider CLI。
//!
//! bridge 使用此模块判断哪些 provider 可用于 agent-prompt 任务，
//! 并在 dashboard 中展示 provider 可用性。
//!
//! 查找优先级：显式配置 > PATH > macOS 常见目录。

use crate::provider_registry::ProviderRegistry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 三级查找 provider CLI 二进制路径：
/// 1. 显式配置 (provider_binaries.kimi_code = "/abs/path")
/// 2. PATH 环境变量
/// 3. macOS 常见目录
#[derive(Debug, Clone)]
pub struct ProviderResolver {
    explicit_paths: HashMap<String, String>,
    fallback_dirs: Vec<PathBuf>,
    /// provider key → CLI 二进制名，从 ProviderRegistry 构建
    binary_names: HashMap<String, String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderAvailability {
    pub provider: String,
    pub available: bool,
    pub binary_path: Option<String>,
    pub error: Option<String>,
}

impl ProviderResolver {
    /// 从 Config + ProviderRegistry 构建 resolver。
    /// registry 提供 provider key → binary name 映射。
    pub fn from_config(config: &crate::config::Config, registry: &ProviderRegistry) -> Self {
        let mut explicit = HashMap::new();
        for (k, v) in &config.provider_binaries {
            explicit.insert(k.clone(), v.clone());
        }
        let mut binary_names = HashMap::new();
        for (key, pc) in &registry.providers {
            binary_names.insert(key.clone(), pc.command.clone());
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let fallback = Self::default_fallback_dirs(&home);
        Self {
            explicit_paths: explicit,
            fallback_dirs: fallback,
            binary_names,
        }
    }

    /// 替换 fallback 搜索目录（测试用）。
    pub fn with_custom_fallback(mut self, dirs: Vec<PathBuf>) -> Self {
        self.fallback_dirs = dirs;
        self
    }

    /// macOS 常见二进制安装目录。
    pub fn default_fallback_dirs(home: &str) -> Vec<PathBuf> {
        vec![
            PathBuf::from(format!("{}/.local/bin", home)),
            PathBuf::from(format!("{}/.bun/bin", home)),
            PathBuf::from(format!("{}/.npm-global/bin", home)),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ]
    }

    /// 按三级优先级查找 provider CLI 的绝对路径。
    pub fn resolve(&self, provider: &str) -> Result<PathBuf, String> {
        let binary_name = self.binary_name(provider);

        // 1. Explicit config
        if let Some(path) = self.explicit_paths.get(provider) {
            let p = Path::new(path);
            if p.is_file() {
                return Ok(p.to_path_buf());
            }
            return Err(format!(
                "configured binary not found: {} (provider_binaries.{})",
                path, provider
            ));
        }

        // 2. PATH
        if let Ok(path) = which_in_path(&binary_name) {
            return Ok(path);
        }

        // 3. Fallback dirs
        for dir in &self.fallback_dirs {
            let candidate = dir.join(&binary_name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }

        let searched: Vec<String> = self
            .fallback_dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect();

        Err(format!(
            "binary '{}' not found in PATH or fallback directories. \
             Searched fallback: {}. \
             Hint: configure provider_binaries.{} in ~/.agent-aspect/config.toml \
             or ensure the binary directory is visible to agent-aspect-bridge",
            binary_name,
            searched.join(", "),
            provider
        ))
    }

    /// 返回 provider 可用性结构（供 dashboard API 使用）。
    pub fn availability(&self, provider: &str) -> ProviderAvailability {
        match self.resolve(provider) {
            Ok(path) => ProviderAvailability {
                provider: provider.to_string(),
                available: true,
                binary_path: Some(path.display().to_string()),
                error: None,
            },
            Err(e) => ProviderAvailability {
                provider: provider.to_string(),
                available: false,
                binary_path: None,
                error: Some(e),
            },
        }
    }

    /// provider key → CLI 二进制名（从 registry 查表，fallback 为 key 本身）。
    fn binary_name(&self, provider: &str) -> String {
        self.binary_names
            .get(provider)
            .cloned()
            .unwrap_or_else(|| provider.to_string())
    }
}

/// Search for a binary name in the current PATH environment variable.
fn which_in_path(binary: &str) -> Result<PathBuf, String> {
    let path_env = std::env::var("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(format!("{} not found in PATH", binary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_registry::ProviderRegistry;
    use crate::test_util::{path_guard, unique_temp_dir};
    use std::io::Write;

    fn tmp_dir() -> PathBuf {
        let dir = unique_temp_dir("cp-test");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn make_fake_binary(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"#!/bin/sh\necho fake\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    fn make_registry() -> ProviderRegistry {
        ProviderRegistry::from_config(&crate::config::Config::default_config())
    }

    #[test]
    fn explicit_config_takes_priority() {
        let tmp = tmp_dir();
        let fake = make_fake_binary(&tmp, "claude");

        let mut config = crate::config::Config::default_config();
        config
            .provider_binaries
            .insert("claude_code".to_string(), fake.display().to_string());

        let registry = make_registry();
        let resolver = ProviderResolver::from_config(&config, &registry);
        let resolved = resolver.resolve("claude_code").unwrap();
        assert_eq!(resolved, fake);
        cleanup(&tmp);
    }

    #[test]
    fn path_search_finds_binary() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        make_fake_binary(&tmp, "kimi");

        let old_path = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", tmp.display().to_string());
        }

        let config = crate::config::Config::default_config();
        let registry = make_registry();
        let resolver =
            ProviderResolver::from_config(&config, &registry).with_custom_fallback(vec![]);
        let resolved = resolver.resolve("kimi_code").unwrap();
        assert_eq!(resolved, tmp.join("kimi"));

        unsafe {
            std::env::set_var("PATH", old_path);
        }
        cleanup(&tmp);
    }

    #[test]
    fn fallback_dirs_used_when_path_missing() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        make_fake_binary(&tmp, "codex");

        let old_path = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", "/usr/bin:/bin");
        }

        let config = crate::config::Config::default_config();
        let registry = make_registry();
        let resolver = ProviderResolver::from_config(&config, &registry)
            .with_custom_fallback(vec![tmp.clone()]);

        let resolved = resolver.resolve("codex_cli").unwrap();
        assert_eq!(resolved, tmp.join("codex"));

        unsafe {
            std::env::set_var("PATH", old_path);
        }
        cleanup(&tmp);
    }

    #[test]
    fn missing_binary_returns_error() {
        let config = crate::config::Config::default_config();
        let registry = make_registry();
        let resolver =
            ProviderResolver::from_config(&config, &registry).with_custom_fallback(vec![]);

        let err = resolver.resolve("nonexistent_provider").unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn availability_struct_reflects_state() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        make_fake_binary(&tmp, "kimi");

        let old_path = std::env::var("PATH").unwrap_or_default();
        unsafe {
            std::env::set_var("PATH", format!("{}:{}", tmp.display(), old_path));
        }

        let config = crate::config::Config::default_config();
        let registry = make_registry();
        let resolver = ProviderResolver::from_config(&config, &registry);

        let avail = resolver.availability("kimi_code");
        assert!(avail.available);
        assert!(avail.binary_path.is_some());
        assert!(avail.error.is_none());

        let missing = resolver.availability("nonexistent_provider");
        assert!(!missing.available);
        assert!(missing.binary_path.is_none());
        assert!(missing.error.is_some());

        unsafe {
            std::env::set_var("PATH", old_path);
        }
        cleanup(&tmp);
    }
}
