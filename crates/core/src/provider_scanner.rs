//! Provider 可用性扫描 — 探测本机 provider CLI 的真实状态。
//!
//! 架构角色：
//! - 基于 ProviderRegistry + ProviderResolver 逐个探测 enabled provider
//! - 输出 ProviderScanResult（status / confidence / warnings），不做任何修改
//! - 供 bridge / doctor UI 消费

use crate::provider_registry::ProviderRegistry;
use crate::provider_resolver::ProviderResolver;
use crate::runtime_profile::{self, RuntimeIdentity};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 扫描结果的聚合输出。
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub results: Vec<ProviderScanResult>,
}

/// 单个 provider 的探测结果。
#[derive(Debug, Clone, Serialize)]
pub struct ProviderScanResult {
    pub provider: String,
    pub status: ProviderStatus,
    pub binary_path: Option<PathBuf>,
    pub command_version: Option<String>,
    pub transcript_status: TranscriptStatus,
    /// 0.0–1.0，反映探测置信度。
    pub confidence: f32,
    pub warnings: Vec<String>,
    /// 当前运行时身份（model / profile / workspace / config_hash）
    pub runtime_identity: RuntimeIdentity,
}

/// Provider 可用性等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ProviderStatus {
    /// 二进制存在且 probe 成功。
    Supported,
    /// 二进制存在但 probe 失败或超时。
    Partial,
    /// 二进制不存在。
    Missing,
    /// 在 registry 中被禁用。
    Disabled,
}

/// Transcript 解析状态（当前作为 provider 能力探测的预留字段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TranscriptStatus {
    Unknown,
}

/// Probe 函数类型：接收 binary path + timeout，返回 Ok(version_string) 或 Err(reason)。
/// 超时应返回 Err("timeout".to_string())。
/// 测试可注入自定义实现，避免执行真实 CLI。
type ProbeFn = std::sync::Arc<dyn Fn(&Path, Duration) -> Result<String, String> + Send + Sync>;

/// Provider 扫描器。
pub struct ProviderScanner {
    registry: ProviderRegistry,
    resolver: ProviderResolver,
    probe_timeout: Duration,
    probe_fn: ProbeFn,
}

/// 等待子进程完成，超时则 kill + wait。
/// 返回 Ok(output) 或 Err("timeout")。
fn wait_child_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() >= timeout {
            // 超时：kill 进程组
            kill_process(child);
            let _ = child.wait(); // 回收 zombie
            return Err("timeout".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // 进程已退出
                let stdout = child
                    .stdout
                    .take()
                    .map(|mut r| {
                        let mut s = String::new();
                        use std::io::Read;
                        let _ = r.read_to_string(&mut s);
                        s
                    })
                    .unwrap_or_default();
                let stderr = child
                    .stderr
                    .take()
                    .map(|mut r| {
                        let mut s = String::new();
                        use std::io::Read;
                        let _ = r.read_to_string(&mut s);
                        s
                    })
                    .unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout: stdout.into_bytes(),
                    stderr: stderr.into_bytes(),
                });
            }
            Ok(None) => {
                // 还在跑，sleep 一小段再查
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("wait error: {e}")),
        }
    }
}

/// Kill 子进程（Unix 上 kill 进程组）。
#[cfg(unix)]
fn kill_process(child: &mut std::process::Child) {
    // 尝试 kill 整个进程组（负 pid）
    let pid = child.id() as i32;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// 在新进程组中 spawn 子进程（Unix setsid）。
#[cfg(unix)]
fn spawn_in_new_group(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

#[cfg(not(unix))]
fn spawn_in_new_group(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// 默认 probe：执行 `binary --version`，受 timeout 约束。
/// 超时后 kill 子进程（含进程组），返回 Err("timeout")。
fn default_probe(binary: &Path, timeout: Duration) -> Result<String, String> {
    use std::process::Command;
    let mut child = spawn_in_new_group(Command::new(binary).arg("--version"))
        .map_err(|e| format!("exec failed: {e}"))?;

    let output = wait_child_with_timeout(&mut child, timeout)?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first_line = stdout.lines().next().unwrap_or("").trim().to_string();
        if first_line.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let first_err = stderr.lines().next().unwrap_or("").trim().to_string();
            if first_err.is_empty() {
                Ok("(no version output)".to_string())
            } else {
                Ok(first_err)
            }
        } else {
            Ok(first_line)
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "exit code {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.lines().next().unwrap_or("").trim()
        ))
    }
}

impl ProviderScanner {
    pub fn new(registry: ProviderRegistry, resolver: ProviderResolver) -> Self {
        Self {
            registry,
            resolver,
            probe_timeout: Duration::from_secs(3),
            probe_fn: std::sync::Arc::new(default_probe),
        }
    }

    /// 替换 probe 函数（测试用）。probe 会在线程中执行，受 probe_timeout 约束。
    ///
    /// **Timeout 限制**：线程级 `recv_timeout` 只能防止调用方永久阻塞——
    /// 如果注入的 probe 内部执行阻塞 I/O（如 `wait()` 无超时），该线程本身不会被杀死。
    /// 注入 probe 应自行管理超时（例如对子进程设置 deadline），否则超时后线程会泄漏
    /// 直到 probe 自然返回。默认 `default_probe` 已通过 `wait_child_with_timeout` 处理。
    pub fn with_probe<F>(mut self, f: F) -> Self
    where
        F: Fn(&Path, Duration) -> Result<String, String> + Send + Sync + 'static,
    {
        self.probe_fn = std::sync::Arc::new(f);
        self
    }

    /// 替换 probe 超时。
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.probe_timeout = d;
        self
    }

    /// 扫描所有 enabled provider。
    pub fn scan(&self) -> ScanReport {
        let all_keys: Vec<String> = self
            .registry
            .enabled_providers()
            .into_iter()
            .map(|s| s.to_string())
            .collect();

        let results = all_keys.iter().map(|key| self.scan_one(key)).collect();

        ScanReport { results }
    }

    fn scan_one(&self, provider: &str) -> ProviderScanResult {
        let mut warnings = Vec::new();
        let identity = runtime_profile::probe_identity(provider, None);

        // 1. Resolve binary
        match self.resolver.resolve(provider) {
            Ok(binary_path) => {
                // 2. Executable check
                if !is_executable(&binary_path) {
                    warnings.push(format!(
                        "binary exists but is not executable: {}",
                        binary_path.display()
                    ));
                    return ProviderScanResult {
                        provider: provider.to_string(),
                        status: ProviderStatus::Partial,
                        binary_path: Some(binary_path),
                        command_version: None,
                        transcript_status: TranscriptStatus::Unknown,
                        confidence: 0.3,
                        warnings,
                        runtime_identity: identity,
                    };
                }

                // 3. Probe（在线程中执行，受 probe_timeout 约束）
                let probe_result = {
                    let probe_fn = self.probe_fn.clone();
                    let path = binary_path.clone();
                    let timeout = self.probe_timeout;
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(probe_fn(&path, timeout));
                    });
                    rx.recv_timeout(self.probe_timeout)
                        .unwrap_or(Err("timeout".to_string()))
                };
                match probe_result {
                    Ok(version) => ProviderScanResult {
                        provider: provider.to_string(),
                        status: ProviderStatus::Supported,
                        binary_path: Some(binary_path),
                        command_version: Some(version),
                        transcript_status: TranscriptStatus::Unknown,
                        confidence: 1.0,
                        warnings,
                        runtime_identity: identity,
                    },
                    Err(reason) => {
                        warnings.push(format!("probe failed: {reason}"));
                        ProviderScanResult {
                            provider: provider.to_string(),
                            status: ProviderStatus::Partial,
                            binary_path: Some(binary_path),
                            command_version: None,
                            transcript_status: TranscriptStatus::Unknown,
                            confidence: 0.5,
                            warnings,
                            runtime_identity: identity,
                        }
                    }
                }
            }
            Err(e) => {
                warnings.push(format!("binary not found: {e}"));
                ProviderScanResult {
                    provider: provider.to_string(),
                    status: ProviderStatus::Missing,
                    binary_path: None,
                    command_version: None,
                    transcript_status: TranscriptStatus::Unknown,
                    confidence: 0.0,
                    warnings,
                    runtime_identity: identity,
                }
            }
        }
    }
}

/// 检查文件是否可执行。
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::provider_registry::ProviderConfigOverride;
    use crate::test_util::{path_guard, set_path, unique_temp_dir};
    use std::collections::HashMap;
    use std::io::Write;

    fn tmp_dir() -> PathBuf {
        let dir = unique_temp_dir("cp-scanner");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn make_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"#!/bin/sh\necho fake").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn make_scanner_with_fallback<F>(fallback: Vec<PathBuf>, probe: F) -> ProviderScanner
    where
        F: Fn(&Path, Duration) -> Result<String, String> + Send + Sync + 'static,
    {
        let config = Config::default_config();
        let registry = ProviderRegistry::from_config(&config);
        let resolver =
            ProviderResolver::from_config(&config, &registry).with_custom_fallback(fallback);
        ProviderScanner::new(registry, resolver).with_probe(probe)
    }

    /// 不存在的目录，用于让 PATH 搜索必定失败（不影响 resolver 测试自己的 PATH 设置）。
    const VOID_PATH: &str = "/nonexistent/agent-aspect-test-void";

    #[test]
    fn binary_found_probe_success() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        make_executable(&tmp, "claude");
        let old_path = set_path(tmp.to_str().unwrap());

        let scanner = make_scanner_with_fallback(vec![], |_, _| Ok("claude 1.2.3".to_string()));
        let report = scanner.scan();
        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();

        assert_eq!(claude.status, ProviderStatus::Supported);
        assert_eq!(claude.command_version.as_deref(), Some("claude 1.2.3"));
        assert_eq!(claude.confidence, 1.0);
        assert!(claude.warnings.is_empty());

        set_path(&old_path);
        cleanup(&tmp);
    }

    #[test]
    fn binary_missing_reports_missing() {
        let _guard = path_guard();
        let old_path = set_path(VOID_PATH);
        let scanner = make_scanner_with_fallback(vec![], |_, _| panic!("should not be called"));
        let report = scanner.scan();
        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();

        assert_eq!(claude.status, ProviderStatus::Missing);
        assert_eq!(claude.binary_path, None);
        assert_eq!(claude.confidence, 0.0);
        assert!(!claude.warnings.is_empty());

        set_path(&old_path);
    }

    #[test]
    fn disabled_provider_skipped() {
        let _guard = path_guard();
        let old_path = set_path(VOID_PATH);
        let mut config = Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "claude_code".into(),
            ProviderConfigOverride {
                enabled: Some(false),
                ..Default::default()
            },
        );
        config.providers = providers;

        let registry = ProviderRegistry::from_config(&config);
        let resolver =
            ProviderResolver::from_config(&config, &registry).with_custom_fallback(vec![]);
        let scanner = ProviderScanner::new(registry, resolver)
            .with_probe(|_, _| panic!("should not be called"));

        let report = scanner.scan();
        assert!(report.results.iter().all(|r| r.provider != "claude_code"));

        set_path(&old_path);
    }

    #[test]
    fn custom_command_override_is_scanned() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        make_executable(&tmp, "my-claude");
        let old_path = set_path(tmp.to_str().unwrap());

        let mut config = Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "claude_code".into(),
            ProviderConfigOverride {
                command: Some("my-claude".into()),
                ..Default::default()
            },
        );
        config.providers = providers;

        let registry = ProviderRegistry::from_config(&config);
        let resolver =
            ProviderResolver::from_config(&config, &registry).with_custom_fallback(vec![]);
        let scanner = ProviderScanner::new(registry, resolver)
            .with_probe(|_, _| Ok("my-claude 0.1".to_string()));

        let report = scanner.scan();
        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();

        assert_eq!(claude.status, ProviderStatus::Supported);
        assert_eq!(
            claude.binary_path.as_ref().unwrap().file_name().unwrap(),
            "my-claude"
        );

        set_path(&old_path);
        cleanup(&tmp);
    }

    #[test]
    fn probe_failure_goes_partial() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        make_executable(&tmp, "claude");
        let old_path = set_path(tmp.to_str().unwrap());

        let scanner = make_scanner_with_fallback(vec![], |_, _| Err("segfault".to_string()));
        let report = scanner.scan();
        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();

        assert_eq!(claude.status, ProviderStatus::Partial);
        assert_eq!(claude.confidence, 0.5);
        assert!(claude.warnings.iter().any(|w| w.contains("segfault")));

        set_path(&old_path);
        cleanup(&tmp);
    }

    #[test]
    fn non_executable_binary_goes_partial() {
        let _guard = path_guard();
        let tmp = tmp_dir();
        let path = tmp.join("claude");
        std::fs::write(&path, "not executable").unwrap();
        let old_path = set_path(tmp.to_str().unwrap());

        let scanner = make_scanner_with_fallback(vec![], |_, _| panic!("should not be called"));
        let report = scanner.scan();
        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();

        assert_eq!(claude.status, ProviderStatus::Partial);
        assert_eq!(claude.confidence, 0.3);
        assert!(claude.warnings.iter().any(|w| w.contains("not executable")));

        set_path(&old_path);
        cleanup(&tmp);
    }

    #[test]
    fn scan_report_serializes() {
        let report = ScanReport {
            results: vec![ProviderScanResult {
                provider: "test".to_string(),
                status: ProviderStatus::Supported,
                binary_path: Some(PathBuf::from("/usr/bin/test")),
                command_version: Some("1.0".to_string()),
                transcript_status: TranscriptStatus::Unknown,
                confidence: 1.0,
                warnings: vec![],
                runtime_identity: RuntimeIdentity {
                    model_id: "sonnet".to_string(),
                    profile_name: "default".to_string(),
                    workspace_path: None,
                    config_hash: None,
                    permission_mode: "unknown".to_string(),
                    entrypoint: None,
                    toolchain_fingerprint: None,
                },
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"Supported\""));
        assert!(json.contains("\"Unknown\""));
        assert!(json.contains("\"sonnet\""));
    }

    #[test]
    fn injected_probe_timeout_goes_partial() {
        // 注入一个 sleep 超过 timeout 的 probe
        let scanner = make_scanner_with_fallback(vec![], |_, timeout| {
            std::thread::sleep(timeout + Duration::from_millis(500));
            Ok("should not reach".to_string())
        })
        .with_timeout(Duration::from_millis(200));

        let _guard = path_guard();
        let tmp = tmp_dir();
        make_executable(&tmp, "claude");
        let old_path = set_path(tmp.to_str().unwrap());

        let start = std::time::Instant::now();
        let report = scanner.scan();
        let elapsed = start.elapsed();

        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();
        assert_eq!(claude.status, ProviderStatus::Partial);
        assert_eq!(claude.confidence, 0.5);
        assert!(claude.warnings.iter().any(|w| w.contains("timeout")));
        // 必须在 ~200ms 内返回，不能等 probe sleep 完
        assert!(
            elapsed < Duration::from_secs(2),
            "should return quickly, took {elapsed:?}"
        );

        set_path(&old_path);
        cleanup(&tmp);
    }

    #[test]
    fn default_probe_timeout_kills_child() {
        // 假 binary 是一个 sleep 很久的脚本
        let _guard = path_guard();
        let tmp = tmp_dir();
        let script = tmp.join("claude");
        let mut f = std::fs::File::create(&script).unwrap();
        f.write_all(b"#!/bin/sh\n/bin/sleep 60\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let old_path = set_path(tmp.to_str().unwrap());

        let scanner = ProviderScanner::new(
            ProviderRegistry::from_config(&Config::default_config()),
            ProviderResolver::from_config(
                &Config::default_config(),
                &ProviderRegistry::from_config(&Config::default_config()),
            )
            .with_custom_fallback(vec![]),
        )
        .with_timeout(Duration::from_millis(300));

        let start = std::time::Instant::now();
        let report = scanner.scan();
        let elapsed = start.elapsed();

        let claude = report
            .results
            .iter()
            .find(|r| r.provider == "claude_code")
            .unwrap();
        assert_eq!(claude.status, ProviderStatus::Partial);
        assert!(
            claude.warnings.iter().any(|w| w.contains("timeout")),
            "expected timeout in warnings, got: {:?}",
            claude.warnings
        );
        // 必须在 ~300ms 内返回，不能等 sleep 60s
        assert!(
            elapsed < Duration::from_secs(3),
            "should return quickly, took {elapsed:?}"
        );

        set_path(&old_path);
        cleanup(&tmp);
    }
}
