//! Job 编排 — 后台任务的排队、执行、监控和日志流。
//!
//! 架构角色：将用户提交的 job（git_status, cargo_test, agent_prompt 等）
//! 排队执行，结果写入 DB，实时日志通过 SSE 推送到前端。
//!
//! 核心不变量：
//! - 同一时刻只运行一个 job（running 字段为 Option<RunningJob>）
//! - 子进程在独立进程组中运行（setpgid），空闲超时或取消时整组 kill
//! - 输出大小受 max_output_bytes 限制，超限截断并标记 truncated
//! - 崩溃恢复：启动时将 stale active job 标记为失败
//!
//! agent_prompt 的请求语义：
//! - 前端（手机/桌面）构造 body，通过 relay 代理或直接 POST /jobs
//! - handle_post_jobs 将 body 顶层字段（provider, prompt, conversation_id）
//!   promote 到 input 对象中，供 build_command 读取
//! - conversation_id：有 → 继续会话（provider 加 resume 参数）；
//!   无 → 新建会话。这里不做 provider 级别的静默丢弃

use checkpoint_core::audit::AuditStore;
use checkpoint_core::provider_registry::ProviderRegistry;
use checkpoint_core::provider_resolver::ProviderResolver;
use checkpoint_core::runtime_profile::{
    RuntimeHealth, RuntimeHealthStatus, compute_runtime_health, probe_identity,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::routes::json_response;
use crate::sse::{self, SharedBroadcaster};

const ALLOWED_KINDS: &[&str] = &[
    "agent_aspect_status",
    "git_status",
    "cargo_test",
    "smoke_test",
    "agent_aspect_mode",
    // 兼容旧客户端提交的历史 kind。
    "checkpoint_status",
    "checkpoint_mode",
    "custom_prompt",
    "agent_prompt",
];

const AGENT_ASPECT_PROJECT_DIR: &str = "Coding/Personal/agent-aspect";

/// 从 HTTP 请求头中提取指定 header 的值。
fn header_value<'a>(request: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

/// 提取设备标识：(device_id, user_agent, remote_addr)。
/// 优先使用 X-Device-Id 头，否则根据 remote_addr + user_agent 哈希生成 fallback ID。
fn request_device(request: &tiny_http::Request) -> (String, Option<String>, Option<String>) {
    let user_agent = header_value(request, "User-Agent").map(|s| s.to_string());
    let remote_addr = request.remote_addr().map(|a| a.to_string());
    if let Some(id) = header_value(request, "X-Device-Id").map(str::trim) {
        if !id.is_empty() && id.len() <= 128 {
            return (id.to_string(), user_agent, remote_addr);
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(remote_addr.as_deref().unwrap_or("unknown"));
    hasher.update(b"\0");
    hasher.update(user_agent.as_deref().unwrap_or("unknown"));
    let digest = hasher.finalize();
    (format!("fallback:{:x}", digest), user_agent, remote_addr)
}

/// 正在运行的 job：持有子进程句柄，用于空闲超时 kill 和取消操作。
struct RunningJob {
    job_id: String,
    child: std::process::Child,
}

/// job 提交错误类型。
///
/// 区分普通错误和 runtime drift 阻止，后者需返回 409 + health 详情。
#[derive(Debug)]
pub enum SubmitError {
    /// 普通错误（并发、校验失败等）
    Fatal(String),
    /// runtime drift 检测到严重不匹配，需用户确认
    DriftBlocked {
        message: String,
        health: RuntimeHealth,
    },
    /// 继续会话成本过高，需用户确认
    CostBlocked {
        message: String,
        stats: serde_json::Value,
    },
}

impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubmitError::Fatal(msg) => write!(f, "{}", msg),
            SubmitError::DriftBlocked { message, .. } => write!(f, "{}", message),
            SubmitError::CostBlocked { message, .. } => write!(f, "{}", message),
        }
    }
}

/// Job 运行器：管理 job 生命周期（排队 → 运行 → 完成/空闲超时/取消）。
/// 全局单例，同一时刻最多运行一个 job。
pub struct JobRunner {
    running: Arc<Mutex<Option<RunningJob>>>,
    db_path: PathBuf,
    runner_id: String,
    timeout_secs: u64,
    agent_prompt_timeout_secs: u64,
    max_output_bytes: usize,
    broadcaster: SharedBroadcaster,
    resolver: ProviderResolver,
    registry: ProviderRegistry,
}

impl JobRunner {
    /// 创建 JobRunner。启动时自动恢复 stale active jobs 和未绑定的 provider conversations。
    pub fn new(
        db_path: PathBuf,
        timeout_secs: u64,
        agent_prompt_timeout_secs: u64,
        max_output_kb: usize,
        broadcaster: SharedBroadcaster,
        resolver: ProviderResolver,
        registry: ProviderRegistry,
    ) -> Self {
        let runner_id = uuid::Uuid::now_v7().to_string();
        if let Ok(store) = AuditStore::open(&db_path) {
            let now = chrono::Utc::now().to_rfc3339();
            match store.recover_stale_active_jobs(&runner_id, &now) {
                Ok(count) if count > 0 => {
                    eprintln!("agent-aspect-bridge: recovered {count} stale active job(s)");
                }
                Ok(_) => {}
                Err(e) => eprintln!("agent-aspect-bridge: stale job recovery failed: {e}"),
            }
        }
        bind_recent_unbound_provider_conversations(&db_path);

        Self {
            running: Arc::new(Mutex::new(None)),
            db_path,
            runner_id,
            timeout_secs,
            agent_prompt_timeout_secs,
            max_output_bytes: max_output_kb * 1024,
            broadcaster,
            resolver,
            registry,
        }
    }

    pub fn validate_kind(kind: &str) -> bool {
        ALLOWED_KINDS.contains(&kind)
    }

    pub fn available_kinds() -> Vec<serde_json::Value> {
        const LABELS: &[(&str, &str)] = &[
            ("agent_aspect_status", "状态检查"),
            ("git_status", "Git 状态"),
            ("cargo_test", "Cargo 测试"),
            ("smoke_test", "冒烟测试"),
            ("agent_aspect_mode", "模式设置"),
            ("agent_prompt", "代理提示词"),
        ];
        LABELS
            .iter()
            .map(|(k, l)| serde_json::json!({"id": k, "label": l}))
            .collect()
    }

    /// 提交并执行一个 job。
    /// concurrency check → DB 持久化 → runtime drift 检测 → 构建命令 → 后台线程执行 → SSE 广播。
    /// custom_prompt 类型特殊处理：只存储不执行。
    /// agent_prompt 类型使用 agent_prompt_timeout_secs 超时。
    /// force_confirm: true 时跳过 runtime drift 阻止，写 audit "dangerous_override"。
    pub fn submit(
        &self,
        kind: &str,
        input: &serde_json::Value,
        provider: Option<&str>,
        project_path: Option<&str>,
        conversation_id: Option<&str>,
        prompt: Option<&str>,
        device_id: Option<&str>,
        force_confirm: bool,
    ) -> Result<String, SubmitError> {
        // Check concurrency
        {
            let guard = self.running.lock().unwrap();
            if guard.is_some() {
                return Err(SubmitError::Fatal("a job is already running".to_string()));
            }
        }

        // Also check DB for active jobs (queued or running) — crash recovery
        let store = AuditStore::open(&self.db_path)
            .map_err(|e| SubmitError::Fatal(format!("open db: {e}")))?;
        let active_count = store
            .count_active_jobs()
            .map_err(|e| SubmitError::Fatal(format!("check active: {e}")))?;
        if active_count > 0 {
            return Err(SubmitError::Fatal(
                "a job is already active (stale DB state)".to_string(),
            ));
        }

        // Validate project_path against observed conversations
        if let Some(pp) = project_path {
            if !pp.is_empty() && !store.is_known_project_path(pp).unwrap_or(false) {
                return Err(SubmitError::Fatal(format!("unknown project_path: {pp}")));
            }
        }

        let mut command_input = input.clone();

        // Runtime drift guard: agent_prompt 继续会话前检测 identity drift
        if kind == "agent_prompt" {
            let agent = provider.unwrap_or("unknown");
            let current_identity = probe_identity(agent, project_path);

            if let Some(cid) = conversation_id {
                // 继续会话：比较当前 identity 与存储值
                let db_id = checkpoint_core::conversation::conversation_db_id(agent, cid);
                if let Ok(Some(conv)) = store.get_conversation(&db_id) {
                    if let Some(obj) = command_input.as_object_mut() {
                        obj.insert(
                            "runtime_permission_mode".to_string(),
                            serde_json::json!(conv.permission_mode.clone()),
                        );
                    }
                    let stored_identity = checkpoint_core::runtime_profile::RuntimeIdentity {
                        model_id: conv.model_id.clone(),
                        profile_name: conv.runtime_profile.clone(),
                        workspace_path: conv.project_path.clone(),
                        config_hash: conv.runtime_profile_hash.clone(),
                        permission_mode: conv.permission_mode.clone(),
                        entrypoint: conv.entrypoint.clone(),
                        toolchain_fingerprint: conv.toolchain_fingerprint.clone(),
                    };

                    let health = compute_runtime_health(&stored_identity, &current_identity);

                    match health.status {
                        RuntimeHealthStatus::Critical if !force_confirm => {
                            // 阻止静默继续
                            let fields: Vec<String> = health
                                .warnings
                                .iter()
                                .map(|m| format!("{}: {} → {}", m.field, m.recorded, m.current))
                                .collect();
                            return Err(SubmitError::DriftBlocked {
                                message: format!("runtime drift detected: {}", fields.join("; ")),
                                health,
                            });
                        }
                        RuntimeHealthStatus::Critical => {
                            // force_confirm: 允许但记录 dangerous_override
                            let warning_text = health
                                .warnings
                                .iter()
                                .map(|m| format!("{}: {} → {}", m.field, m.recorded, m.current))
                                .collect::<Vec<_>>()
                                .join("; ");
                            let _ = store.update_runtime_warning(
                                &db_id,
                                Some(&format!("dangerous_override: {warning_text}")),
                                None,
                            );
                        }
                        RuntimeHealthStatus::Warning => {
                            // 警告但允许继续
                            let warning_text = health
                                .warnings
                                .iter()
                                .map(|m| format!("{}: {} → {}", m.field, m.recorded, m.current))
                                .collect::<Vec<_>>()
                                .join("; ");
                            let _ = store.update_runtime_warning(&db_id, Some(&warning_text), None);
                        }
                        RuntimeHealthStatus::Ok => {}
                    }
                }
            } else if let Some(_cid) = provider {
                // 新建会话：agent 已知但 conversation_id 为 None → 稍后写入 identity
                // identity 在 job 完成后由 bind_provider_conversation 写入
            }
        }

        // Resume cost guard: agent_prompt 继续会话前检查累积成本
        if kind == "agent_prompt" {
            if let Some(cid) = conversation_id {
                let agent = provider.unwrap_or("unknown");
                let db_id = checkpoint_core::conversation::conversation_db_id(agent, cid);
                if let Ok(Some(conv)) = store.get_conversation(&db_id) {
                    let tokens = conv.cached_token_count.unwrap_or(conv.token_count);
                    let file_size = conv.cached_file_size_bytes.unwrap_or(conv.file_size_bytes);
                    let messages = conv.event_count;
                    use checkpoint_core::constants::*;
                    let is_critical = tokens >= RESUME_TOKEN_CRITICAL
                        || file_size >= RESUME_FILE_SIZE_CRITICAL
                        || messages >= RESUME_MESSAGE_CRITICAL;
                    let is_warning = !is_critical
                        && (tokens >= RESUME_TOKEN_WARNING
                            || file_size >= RESUME_FILE_SIZE_WARNING
                            || messages >= RESUME_MESSAGE_WARNING);

                    let cost_status = if is_critical {
                        "critical"
                    } else if is_warning {
                        "warning"
                    } else {
                        "ok"
                    };

                    let stats = serde_json::json!({
                        "token_count": tokens,
                        "file_size_bytes": file_size,
                        "event_count": messages,
                        "thresholds": {
                            "token_warning": RESUME_TOKEN_WARNING,
                            "token_critical": RESUME_TOKEN_CRITICAL,
                            "file_size_warning": RESUME_FILE_SIZE_WARNING,
                            "file_size_critical": RESUME_FILE_SIZE_CRITICAL,
                            "message_warning": RESUME_MESSAGE_WARNING,
                            "message_critical": RESUME_MESSAGE_CRITICAL,
                        }
                    });

                    match cost_status {
                        "critical" if !force_confirm => {
                            return Err(SubmitError::CostBlocked {
                                message: format!(
                                    "resume cost too high: ~{}k tokens, {}MB transcript",
                                    tokens / 1000,
                                    file_size / (1024 * 1024),
                                ),
                                stats,
                            });
                        }
                        "critical" => {
                            let _ = store.update_runtime_warning(
                                &db_id,
                                Some(&format!("cost_override: ~{}k tokens", tokens / 1000)),
                                Some("critical"),
                            );
                        }
                        "warning" => {
                            let _ = store.update_runtime_warning(
                                &db_id,
                                Some(&format!("cost_warning: ~{}k tokens", tokens / 1000)),
                                Some("warning"),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }

        // custom_prompt does not execute — immediately mark as succeeded with a message
        if kind == "custom_prompt" {
            if prompt.is_none() || prompt.map(|p| p.trim().is_empty()).unwrap_or(true) {
                return Err(SubmitError::Fatal(
                    "custom_prompt requires a non-empty 'prompt' field".to_string(),
                ));
            }
            let job_id = uuid::Uuid::now_v7().to_string();
            let now = chrono::Utc::now().to_rfc3339();

            store
                .insert_job(
                    &job_id,
                    kind,
                    &input.to_string(),
                    &now,
                    provider,
                    project_path,
                    conversation_id,
                    prompt,
                )
                .map_err(|e| SubmitError::Fatal(format!("insert job: {e}")))?;

            // Immediately mark as succeeded — prompt stored but not executed
            store
                .update_job_started(&job_id, &now)
                .map_err(|e| SubmitError::Fatal(format!("mark started: {e}")))?;
            let now2 = chrono::Utc::now().to_rfc3339();
            store
                .insert_job_log(&job_id, "system", "Prompt recorded. Remote execution not yet implemented — stored for future use.", 0, &now2)
                .map_err(|e| SubmitError::Fatal(format!("log: {e}")))?;
            store
                .update_job_finished_with_completed_reason(&job_id, "succeeded", &now2, Some(0), None, Some("process_exit"))
                .map_err(|e| SubmitError::Fatal(format!("mark finished: {e}")))?;

            return Ok(job_id);
        }

        let job_id = uuid::Uuid::now_v7().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        store
            .insert_job(
                &job_id,
                kind,
                &command_input.to_string(),
                &now,
                provider,
                project_path,
                conversation_id,
                prompt,
            )
            .map_err(|e| SubmitError::Fatal(format!("insert job: {e}")))?;

        if kind == "agent_prompt" {
            if let (Some(agent), Some(cid)) = (provider, conversation_id) {
                let _ = store.touch_conversation_by_agent_cid(agent, cid, &now);
                let db_id = checkpoint_core::conversation::conversation_db_id(agent, cid);
                let _ = store.update_conversation_counts(&db_id, 0, 0, 0, 1);
            }
        }

        // Audit
        let audit_note = format!("job submitted: {kind}");
        let event_id = uuid::Uuid::now_v7().to_string();
        let _ = store.insert_event(
            &event_id,
            "before",
            "job.submit",
            "bridge",
            "remote_job",
            None,
            &now,
            &format!("{{\"job_id\":\"{job_id}\",\"kind\":\"{kind}\"}}"),
        );
        let _ = store.insert_decision_for_device(
            &event_id,
            "allow",
            None,
            &audit_note,
            &now,
            device_id,
        );

        // Build command
        let cmd = build_command(
            kind,
            &command_input,
            project_path,
            &self.resolver,
            &self.registry,
        )
        .map_err(SubmitError::Fatal)?;

        // Broadcast job_status on submit
        self.broadcaster.lock().unwrap().broadcast(sse::SseEvent {
            event_type: "job_status".to_string(),
            data: serde_json::json!({
                "job_id": job_id,
                "status": "queued",
                "failure_reason": null,
            })
            .to_string(),
        });

        // Spawn execution in background
        let db_path = self.db_path.clone();
        let timeout_secs = if kind == "agent_prompt" {
            self.agent_prompt_timeout_secs
        } else {
            self.timeout_secs
        };
        let max_output_bytes = self.max_output_bytes;
        let running = self.running.clone();
        let job_id_clone = job_id.clone();
        let broadcaster = self.broadcaster.clone();
        let runner_id = self.runner_id.clone();

        std::thread::spawn(move || {
            exec_job(
                &job_id_clone,
                &db_path,
                cmd,
                timeout_secs,
                max_output_bytes,
                &running,
                &broadcaster,
                &runner_id,
            )
        });

        Ok(job_id)
    }

    pub fn get_job(&self, job_id: &str) -> Result<Option<checkpoint_core::audit::JobRow>, String> {
        let store = AuditStore::open(&self.db_path).map_err(|e| format!("open db: {e}"))?;
        store.get_job(job_id).map_err(|e| format!("query job: {e}"))
    }

    pub fn list_jobs(
        &self,
        limit: usize,
        offset: usize,
        status: Option<&str>,
    ) -> Result<(Vec<checkpoint_core::audit::JobRow>, usize), String> {
        let store = AuditStore::open(&self.db_path).map_err(|e| format!("open db: {e}"))?;
        let total = store
            .count_jobs(status)
            .map_err(|e| format!("count jobs: {e}"))?;
        let jobs = store
            .list_jobs(limit, offset, status)
            .map_err(|e| format!("list jobs: {e}"))?;
        Ok((jobs, total))
    }

    pub fn get_job_logs(
        &self,
        job_id: &str,
    ) -> Result<Vec<checkpoint_core::audit::JobLogRow>, String> {
        let store = AuditStore::open(&self.db_path).map_err(|e| format!("open db: {e}"))?;
        store
            .get_job_logs(job_id)
            .map_err(|e| format!("query logs: {e}"))
    }

    pub fn get_job_logs_after(
        &self,
        job_id: &str,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<checkpoint_core::audit::JobLogRow>, String> {
        let store = AuditStore::open(&self.db_path).map_err(|e| format!("open db: {e}"))?;
        store
            .get_job_logs_after(job_id, after_id, limit)
            .map_err(|e| format!("query log delta: {e}"))
    }

    /// 取消运行中的 job。kill 子进程组 → DB 标记 cancelled → SSE 广播。
    pub fn cancel(&self, job_id: &str) -> Result<(), String> {
        let store = AuditStore::open(&self.db_path).map_err(|e| format!("open db: {e}"))?;
        let job = store
            .get_job(job_id)
            .map_err(|e| format!("query job: {e}"))?;

        match job {
            None => Err("job not found".to_string()),
            Some(j) if j.status == "queued" || j.status == "running" => {
                // Kill child process if running
                if j.status == "running" {
                    let mut guard = self.running.lock().unwrap();
                    let mut killed = false;
                    if let Some(ref mut rj) = *guard {
                        if rj.job_id == job_id {
                            kill_process_group_or_child(j.process_group_id, j.pid, &mut rj.child);
                            let _ = rj.child.wait();
                            *guard = None;
                            killed = true;
                        }
                    }
                    if !killed {
                        kill_stale_process(j.process_group_id, j.pid);
                    }
                }
                // DB-level cancel with status guard
                store
                    .cancel_job_with_reason(job_id, Some("cancelled by user"))
                    .map_err(|e| format!("cancel: {e}"))?;
                Ok(())
            }
            Some(j) => Err(format!("job is already {}", j.status)),
        }
    }
}

/// 从 job 的 provider + conversation_id 构造 DB 中的会话 ID。
fn job_conversation_db_id(j: &checkpoint_core::audit::JobRow) -> Option<String> {
    let provider = j.provider.as_deref()?;
    let conversation_id = j.conversation_id.as_deref()?;
    if conversation_id.is_empty() {
        None
    } else {
        Some(checkpoint_core::conversation::conversation_db_id(
            provider,
            conversation_id,
        ))
    }
}

/// 根据 job kind 构建子进程命令。
/// project_path 支持相对路径（相对 HOME）和绝对路径。
/// agent_prompt 从 input 读取 provider/prompt/conversation_id 并委托 provider 模块构建。
fn build_command(
    kind: &str,
    input: &serde_json::Value,
    project_path: Option<&str>,
    resolver: &ProviderResolver,
    registry: &ProviderRegistry,
) -> Result<std::process::Command, String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let default_project_dir = format!("{home}/{AGENT_ASPECT_PROJECT_DIR}");
    let project_dir = project_path
        .filter(|p| !p.is_empty())
        .map(|p| {
            if p.starts_with('/') {
                p.to_string()
            } else {
                format!("{home}/{p}")
            }
        })
        .unwrap_or(default_project_dir);

    match kind {
        "agent_aspect_status" | "checkpoint_status" => {
            let mut cmd = std::process::Command::new("agent-aspect");
            cmd.arg("status");
            cmd.current_dir(&home);
            Ok(cmd)
        }
        "git_status" => {
            let mut cmd = std::process::Command::new("git");
            cmd.args(["status", "--porcelain=v1"]);
            cmd.current_dir(&project_dir);
            Ok(cmd)
        }
        "cargo_test" => {
            let mut cmd = std::process::Command::new("cargo");
            cmd.arg("test");
            cmd.current_dir(&project_dir);
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            Ok(cmd)
        }
        "smoke_test" => {
            // smoke_test only allowed for this project
            if project_path.is_some() && project_path != Some(AGENT_ASPECT_PROJECT_DIR) {
                return Err("smoke_test is only available for the Agent Aspect project".to_string());
            }
            let mut cmd = std::process::Command::new("bash");
            cmd.arg(format!("{project_dir}/scripts/smoke_test.sh"));
            cmd.current_dir(&project_dir);
            Ok(cmd)
        }
        "agent_aspect_mode" | "checkpoint_mode" => {
            let mode = input
                .get("mode")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "agent_aspect_mode requires 'mode' in input".to_string())?;
            let mut cmd = std::process::Command::new("agent-aspect");
            cmd.args(["mode", mode]);
            cmd.current_dir(&home);
            Ok(cmd)
        }
        "agent_prompt" => {
            // 从 input 读取 provider/prompt/conversation_id，
            // 交给 provider command builder 构造命令。
            // conversation_id: Some → 继续（resume），None → 新建。
            // 不做 provider 级别过滤，所有 provider 统一处理。
            let provider = input
                .get("provider")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "agent_prompt requires 'provider' in input".to_string())?;
            if !crate::provider::validate_provider(provider, registry) {
                return Err(format!("unsupported provider: {provider}"));
            }
            let conversation_id = input.get("conversation_id").and_then(|v| v.as_str());
            if conversation_id.is_some() && !registry.supports_resume(provider) {
                return Err(format!("provider '{provider}' does not support resume"));
            }
            let runtime_permission_mode = input
                .get("runtime_permission_mode")
                .and_then(|v| v.as_str());
            let prompt = input
                .get("prompt")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "agent_prompt requires 'prompt' in input".to_string())?;
            crate::provider::resolve_and_build(
                resolver,
                registry,
                provider,
                &project_dir,
                conversation_id,
                prompt,
                runtime_permission_mode,
            )
        }
        _ => Err(format!("unknown job kind: {kind}")),
    }
}

/// 共享日志写入器：reader 线程通过它将 stdout/stderr 行写入 DB 并广播 SSE。
/// 跟踪总输出字节数，超限时截断。
struct LogWriter {
    db_path: PathBuf,
    job_id: String,
    seq: i64,
    total_bytes: usize,
    max_bytes: usize,
    truncated: bool,
    last_activity: Instant,
    broadcaster: SharedBroadcaster,
}

impl LogWriter {
    /// 写入一行日志。如果总大小超过 max_bytes，标记截断并不再写入。
    /// 每次写入后广播 SSE job_log 事件。
    fn write(&mut self, stream: &str, chunk: &str) {
        if self.truncated {
            return;
        }
        self.last_activity = Instant::now();
        let line_bytes = chunk.len();
        if self.total_bytes + line_bytes > self.max_bytes {
            let now = chrono::Utc::now().to_rfc3339();
            if let Ok(store) = AuditStore::open(&self.db_path) {
                let _ = store.insert_job_log(
                    &self.job_id,
                    "system",
                    "[output truncated — size limit reached]",
                    self.seq,
                    &now,
                );
                self.seq += 1;
            }
            self.truncated = true;
            // Broadcast truncation
            self.broadcaster.lock().unwrap().broadcast(sse::SseEvent {
                event_type: "job_log".to_string(),
                data: self.job_id.clone(),
            });
            return;
        }
        self.total_bytes += line_bytes;
        let now = chrono::Utc::now().to_rfc3339();
        if let Ok(store) = AuditStore::open(&self.db_path) {
            if let Err(e) = store.insert_job_log(&self.job_id, stream, chunk, self.seq, &now) {
                eprintln!("agent-aspect-bridge: job {}: log write: {e}", self.job_id);
            }
        }
        self.seq += 1;
        // Broadcast new log line
        self.broadcaster.lock().unwrap().broadcast(sse::SseEvent {
            event_type: "job_log".to_string(),
            data: self.job_id.clone(),
        });
    }

    /// 写入系统标记日志，不续期 watchdog。
    /// 用于 observing 进入/退出/超时等系统状态标记，
    /// 这些不是真实 agent 活动，不应延长空闲超时。
    fn write_system_no_activity(&mut self, chunk: &str) {
        if self.truncated {
            return;
        }
        let line_bytes = chunk.len();
        if self.total_bytes + line_bytes > self.max_bytes {
            let now = chrono::Utc::now().to_rfc3339();
            if let Ok(store) = AuditStore::open(&self.db_path) {
                let _ = store.insert_job_log(
                    &self.job_id,
                    "system",
                    "[output truncated — size limit reached]",
                    self.seq,
                    &now,
                );
                self.seq += 1;
            }
            self.truncated = true;
            self.broadcaster.lock().unwrap().broadcast(sse::SseEvent {
                event_type: "job_log".to_string(),
                data: self.job_id.clone(),
            });
            return;
        }
        self.total_bytes += line_bytes;
        let now = chrono::Utc::now().to_rfc3339();
        if let Ok(store) = AuditStore::open(&self.db_path) {
            if let Err(e) = store.insert_job_log(&self.job_id, "system", chunk, self.seq, &now) {
                eprintln!("agent-aspect-bridge: job {}: log write: {e}", self.job_id);
            }
        }
        self.seq += 1;
        self.broadcaster.lock().unwrap().broadcast(sse::SseEvent {
            event_type: "job_log".to_string(),
            data: self.job_id.clone(),
        });
    }
}

/// 杀掉指定进程组或进程（SIGTERM → 200ms → SIGKILL）。
/// 用于 kill 无法通过 RunningJob 句柄访问的 stale 进程（崩溃恢复场景）。
fn kill_stale_process(process_group_id: Option<i64>, pid: Option<i64>) {
    #[cfg(unix)]
    {
        if let Some(pgid) = process_group_id.filter(|v| *v > 0) {
            unsafe {
                libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            unsafe {
                libc::kill(-(pgid as libc::pid_t), libc::SIGKILL);
            }
            return;
        }
        if let Some(pid) = pid.filter(|v| *v > 0) {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            std::thread::sleep(Duration::from_millis(200));
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (process_group_id, pid);
    }
}

/// 优先 kill 进程组；如果没有 pgid/pid 信息，fallback 到 child.kill()。
fn kill_process_group_or_child(
    process_group_id: Option<i64>,
    pid: Option<i64>,
    child: &mut std::process::Child,
) {
    #[cfg(unix)]
    {
        if process_group_id.is_some() || pid.is_some() {
            kill_stale_process(process_group_id, pid);
            return;
        }
    }
    let _ = child.kill();
}

/// 扫描项目目录最近一次文件活动时间，用于长任务 idle watchdog 续期。
///
/// 只作为“进程是否仍在干活”的辅助信号：stdout/stderr 仍是主信号；
/// 这里跳过常见大目录，并设置扫描上限，避免 watchdog 自己变成负担。
fn latest_project_activity(root: &Path) -> Option<SystemTime> {
    const MAX_VISITED: usize = 4_000;
    const SKIP_DIRS: &[&str] = &[
        ".git",
        "target",
        "node_modules",
        ".next",
        "dist",
        "build",
        ".turbo",
        ".cache",
    ];

    fn walk(path: &Path, visited: &mut usize, latest: &mut Option<SystemTime>) {
        if *visited >= MAX_VISITED {
            return;
        }
        *visited += 1;

        let Ok(meta) = fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() {
            return;
        }
        if let Ok(modified) = meta.modified() {
            if latest.map(|old| modified > old).unwrap_or(true) {
                *latest = Some(modified);
            }
        }

        if !meta.is_dir() {
            return;
        }

        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let child = entry.path();
            if child
                .file_name()
                .and_then(|s| s.to_str())
                .map(|name| SKIP_DIRS.contains(&name))
                .unwrap_or(false)
            {
                continue;
            }
            walk(&child, visited, latest);
            if *visited >= MAX_VISITED {
                break;
            }
        }
    }

    let mut visited = 0usize;
    let mut latest = None;
    walk(root, &mut visited, &mut latest);
    latest
}

/// Job 执行主函数（在后台线程运行）。
/// 流程：标记 started → spawn 子进程 → 并行读取 stdout/stderr → 等待完成/空闲超时
/// → 清理 → 更新 DB 状态 → 绑定 provider conversation → SSE 广播完成。
fn exec_job(
    job_id: &str,
    db_path: &PathBuf,
    mut cmd: std::process::Command,
    timeout_secs: u64,
    max_output_bytes: usize,
    running: &Arc<Mutex<Option<RunningJob>>>,
    broadcaster: &SharedBroadcaster,
    runner_id: &str,
) {
    let store = match AuditStore::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agent-aspect-bridge: job {job_id}: open db: {e}");
            return;
        }
    };

    let now = chrono::Utc::now().to_rfc3339();
    match store.update_job_started_supervised(
        job_id,
        &now,
        None,
        None,
        Some(runner_id),
        Some(timeout_secs),
    ) {
        Ok(0) => {
            // Job was cancelled or terminal before worker got to it — do not execute
            eprintln!(
                "agent-aspect-bridge: job {job_id}: already cancelled/terminal, skipping execution"
            );
            return;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("agent-aspect-bridge: job {job_id}: mark started: {e}");
            return;
        }
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }

    let program = cmd.get_program().to_string_lossy().into_owned();
    let args = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    let cwd_path = cmd
        .get_current_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let cwd = cwd_path.display().to_string();

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let now = chrono::Utc::now().to_rfc3339();
            let path_env = std::env::var("PATH").unwrap_or_default();
            let err_msg = format!(
                "failed to spawn: {e}\n\
                 provider binary: {program}\n\
                 PATH: {path_env}\n\
                 hint: Configure provider_binaries.<name> in ~/.agent-aspect/config.toml \
                 or ensure the binary directory is visible to agent-aspect-bridge"
            );
            let _ = store.update_job_finished_with_completed_reason(
                job_id,
                "failed",
                &now,
                None,
                Some("spawn failed"),
                Some("process_exit_nonzero"),
            );
            let _ = store.insert_job_log(job_id, "system", &err_msg, 0, &now);
            // Broadcast job_status on spawn failure
            broadcaster.lock().unwrap().broadcast(sse::SseEvent {
                event_type: "job_status".to_string(),
                data: serde_json::json!({
                    "job_id": job_id,
                    "status": "failed",
                    "failure_reason": "spawn failed",
                })
                .to_string(),
            });
            return;
        }
    };

    let pid = child.id() as i64;
    let process_group_id = pid;
    let now = chrono::Utc::now().to_rfc3339();
    let _ = store.update_job_process(job_id, runner_id, pid, process_group_id, &now);
    let _ = store.insert_job_log(
        job_id,
        "system",
        &format!(
            "started pid={pid} pgid={process_group_id} idle_timeout={timeout_secs}s cwd={cwd} command={program} {args}"
        ),
        0,
        &now,
    );

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    // Register running job
    {
        let mut guard = running.lock().unwrap();
        *guard = Some(RunningJob {
            job_id: job_id.to_string(),
            child,
        });
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    let log_writer = Arc::new(Mutex::new(LogWriter {
        db_path: db_path.clone(),
        job_id: job_id.to_string(),
        seq: 1,
        total_bytes: 0,
        max_bytes: max_output_bytes,
        truncated: false,
        last_activity: Instant::now(),
        broadcaster: broadcaster.clone(),
    }));

    // Spawn concurrent reader threads
    let stdout_cancel = cancel_flag.clone();
    let stdout_writer = log_writer.clone();
    let stdout_handle = std::thread::spawn(move || {
        read_stream(
            BufReader::new(stdout),
            "stdout",
            &stdout_writer,
            &stdout_cancel,
        );
    });

    let stderr_cancel = cancel_flag.clone();
    let stderr_writer = log_writer.clone();
    let stderr_handle = std::thread::spawn(move || {
        read_stream(
            BufReader::new(stderr),
            "stderr",
            &stderr_writer,
            &stderr_cancel,
        );
    });

    // Wait for child with idle watchdog. The timer renews on stdout/stderr activity
    // and project file changes; we only kill a process that is alive but silent
    // and not changing files for timeout_secs.
    let soft_timeout = Duration::from_secs((timeout_secs as f64 * 0.8) as u64);
    let hard_timeout = Duration::from_secs(timeout_secs);
    let mut last_heartbeat = Instant::now();
    let mut last_fs_check = Instant::now();
    let mut last_fs_activity = latest_project_activity(&cwd_path);
    let mut timed_out = false;
    let mut observing = false;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    let mut termination_source = "process_exit";

    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            termination_source = "cancel";
            break;
        }

        let try_result = {
            let mut guard = running.lock().unwrap();
            match guard.as_mut() {
                Some(rj) => rj.child.try_wait(),
                None => break,
            }
        };

        match try_result {
            Ok(Some(status)) => {
                exit_status = Some(status);
                if observing {
                    termination_source = "process_exit_during_observe";
                }
                break;
            }
            Ok(None) => {
                let now_instant = Instant::now();
                if now_instant.duration_since(last_heartbeat) >= Duration::from_secs(2) {
                    let ts = chrono::Utc::now().to_rfc3339();
                    let _ = store.update_job_heartbeat(job_id, runner_id, &ts);
                    last_heartbeat = now_instant;
                }
                if now_instant.duration_since(last_fs_check) >= Duration::from_secs(5) {
                    if let Some(activity) = latest_project_activity(&cwd_path) {
                        if last_fs_activity.map(|old| activity > old).unwrap_or(true) {
                            last_fs_activity = Some(activity);
                            log_writer.lock().unwrap().last_activity = now_instant;
                            if observing {
                                observing = false;
                                let ts = chrono::Utc::now().to_rfc3339();
                                let _ = store.update_job_observing_revert(job_id, &ts);
                                let mut lw = log_writer.lock().unwrap();
                                lw.write_system_no_activity(
                                    "[activity detected, resuming from observe mode]",
                                );
                                broadcaster.lock().unwrap().broadcast(sse::SseEvent {
                                    event_type: "job_status".to_string(),
                                    data: serde_json::json!({
                                        "job_id": job_id,
                                        "status": "running",
                                    })
                                    .to_string(),
                                });
                            }
                        }
                    }
                    last_fs_check = now_instant;
                }

                let last_activity = log_writer.lock().unwrap().last_activity;

                // Soft timeout: enter observing state
                if !observing && now_instant.duration_since(last_activity) >= soft_timeout {
                    observing = true;
                    let ts = chrono::Utc::now().to_rfc3339();
                    let _ = store.update_job_observing(job_id, &ts);
                    let mut lw = log_writer.lock().unwrap();
                    lw.write_system_no_activity(&format!(
                        "[soft idle timeout, entering observe mode \u{2014} hard kill in {}s]",
                        timeout_secs.saturating_sub((timeout_secs as f64 * 0.8) as u64)
                    ));
                    broadcaster.lock().unwrap().broadcast(sse::SseEvent {
                        event_type: "job_status".to_string(),
                        data: serde_json::json!({
                            "job_id": job_id,
                            "status": "observing",
                            "message": "waiting for agent response",
                        })
                        .to_string(),
                    });
                }

                // Hard timeout: kill
                if now_instant.duration_since(last_activity) >= hard_timeout {
                    timed_out = true;
                    termination_source = "hard_idle_timeout";
                    break;
                }

                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("agent-aspect-bridge: job {job_id}: wait: {e}");
                break;
            }
        }
    }

    // Kill child on idle timeout or external cancel
    if timed_out || cancel_flag.load(Ordering::Relaxed) {
        let mut guard = running.lock().unwrap();
        if let Some(ref mut rj) = *guard {
            kill_process_group_or_child(Some(process_group_id), Some(pid), &mut rj.child);
            // Reap to avoid zombies — capture exit if we can
            match rj.child.wait() {
                Ok(s) if exit_status.is_none() => exit_status = Some(s),
                _ => {}
            }
        }
    }

    // Signal reader threads to stop
    cancel_flag.store(true, Ordering::Relaxed);
    let _ = stdout_handle.join();
    let _ = stderr_handle.join();

    // Write idle timeout log entry (system marker, no watchdog renewal)
    if timed_out {
        let mut lw = log_writer.lock().unwrap();
        lw.write_system_no_activity(&format!(
            "[job 空闲超时（{timeout_secs}s 无输出）— {termination_source}]"
        ));
    }

    // Clear running state
    {
        let mut guard = running.lock().unwrap();
        *guard = None;
    }

    // Check if cancel already wrote a terminal state — status guard prevents overwrite
    let now = chrono::Utc::now().to_rfc3339();
    if timed_out {
        let _ = store.update_job_finished_with_completed_reason(
            job_id,
            "failed",
            &now,
            None,
            Some(&format!(
                "idle timeout after {timeout_secs}s without output (source: {termination_source})"
            )),
            Some("timeout_killed"),
        );
    } else if let Some(status) = exit_status {
        let code = status.code();
        let final_status = if status.success() {
            "succeeded"
        } else {
            "failed"
        };
        let reason = if status.success() {
            None
        } else {
            Some("process exited with non-zero status")
        };
        let completed_reason = if status.success() {
            "process_exit"
        } else {
            "process_exit_nonzero"
        };
        let _ = store.update_job_finished_with_completed_reason(job_id, final_status, &now, code, reason, Some(completed_reason));
    } else {
        // Child exited without status (e.g. killed by cancel or signal)
        let reason = if cancel_flag.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            "timeout_killed"
        };
        let _ = store.update_job_finished_with_completed_reason(
            job_id,
            "failed",
            &now,
            None,
            Some("process ended without exit status"),
            Some(reason),
        );
    }

    bind_provider_conversation(job_id, db_path);

    // Broadcast job_status on completion with full status
    let final_job = store.get_job(job_id).ok().flatten();
    let final_status = final_job
        .as_ref()
        .map(|j| j.status.clone())
        .unwrap_or_else(|| "failed".to_string());
    let final_reason = final_job.as_ref().and_then(|j| j.failure_reason.clone());
    broadcaster.lock().unwrap().broadcast(sse::SseEvent {
        event_type: "job_status".to_string(),
        data: serde_json::json!({
            "job_id": job_id,
            "status": final_status,
            "failure_reason": final_reason,
        })
        .to_string(),
    });

    // Broadcast conversation_update so conv list refreshes
    broadcaster.lock().unwrap().broadcast(sse::SseEvent {
        event_type: "conversation_update".to_string(),
        data: serde_json::json!({"type": "job_complete"}).to_string(),
    });

    crate::routes::invalidate_overview_cache();
}

/// job 完成后，如果是 agent_prompt 且 provider 输出了 conversation_id，
/// 从日志中提取并绑定到 DB 的 conversations 表。
fn bind_provider_conversation(job_id: &str, db_path: &PathBuf) {
    let store = match AuditStore::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("agent-aspect-bridge: job {job_id}: bind conversation open db: {e}");
            return;
        }
    };
    let job = match store.get_job(job_id) {
        Ok(Some(j)) => j,
        Ok(None) => return,
        Err(e) => {
            eprintln!("agent-aspect-bridge: job {job_id}: bind conversation query job: {e}");
            return;
        }
    };
    if job.kind != "agent_prompt"
        || job
            .conversation_id
            .as_deref()
            .is_some_and(|v| !v.is_empty())
    {
        return;
    }
    let Some(provider) = job.provider.as_deref() else {
        return;
    };
    let Ok(logs) = store.get_job_logs(job_id) else {
        return;
    };
    let Some(conversation_id) = extract_provider_conversation_id(provider, &logs) else {
        return;
    };

    let timestamp = job
        .finished_at
        .as_deref()
        .or(job.started_at.as_deref())
        .unwrap_or(&job.created_at)
        .to_string();
    let title = job
        .prompt
        .as_deref()
        .map(short_title)
        .unwrap_or_else(|| format!("{} conversation", provider));
    let db_id = checkpoint_core::conversation::conversation_db_id(provider, &conversation_id);

    if let Err(e) = store.upsert_conversation_from_metadata(
        &db_id,
        provider,
        &conversation_id,
        &title,
        job.project_path.as_deref(),
        &timestamp,
        None,
        Some(&title),
        Some("first_prompt"),
    ) {
        eprintln!("agent-aspect-bridge: job {job_id}: upsert provider conversation: {e}");
        return;
    }
    let mut identity = probe_identity(provider, job.project_path.as_deref());
    if let Ok(input) = serde_json::from_str::<serde_json::Value>(&job.input) {
        if let Some(mode) = input
            .get("runtime_permission_mode")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            identity.permission_mode = mode.to_string();
        }
    }
    let _ = store.update_runtime_identity(
        &db_id,
        &identity.model_id,
        &identity.profile_name,
        identity.config_hash.as_deref(),
        &identity.permission_mode,
        identity.entrypoint.as_deref(),
        identity.toolchain_fingerprint.as_deref(),
    );
    let _ = store.update_job_conversation_id(job_id, &conversation_id);
    let _ = store.update_conversation_counts(&db_id, 0, 0, 0, 1);
}

/// 启动时扫描最近 100 个 agent_prompt job，为未绑定 conversation_id 的成功 job 执行绑定。
fn bind_recent_unbound_provider_conversations(db_path: &PathBuf) {
    let store = match AuditStore::open(db_path) {
        Ok(s) => s,
        Err(_) => return,
    };
    let jobs = match store.list_jobs(100, 0, None) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("agent-aspect-bridge: list unbound provider jobs failed: {e}");
            return;
        }
    };
    for job in jobs {
        if job.kind == "agent_prompt"
            && job.status == "succeeded"
            && job.conversation_id.as_deref().unwrap_or("").is_empty()
            && job.provider.as_deref().is_some()
        {
            bind_provider_conversation(&job.id, db_path);
        }
    }
}

/// 从 provider 的日志输出中提取 conversation_id（用于 "继续会话" 的标识）。
/// 不同 provider 的日志格式不同，需要分别处理。
fn extract_provider_conversation_id(
    provider: &str,
    logs: &[checkpoint_core::audit::JobLogRow],
) -> Option<String> {
    match provider {
        "kimi_code" => logs
            .iter()
            .rev()
            .filter(|l| l.stream == "stderr" || l.stream == "stdout")
            .find_map(|l| extract_after_marker(&l.chunk, "kimi -r ")),
        "claude_code" => logs
            .iter()
            .rev()
            .filter(|l| l.stream == "stderr" || l.stream == "stdout")
            .find_map(|l| {
                extract_after_marker(&l.chunk, "claude --resume ")
                    .or_else(|| extract_after_marker(&l.chunk, "claude -r "))
            }),
        _ => None,
    }
}

/// 在文本中查找 marker 后面的连续标识符（字母、数字、连字符、下划线）。
fn extract_after_marker(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let tail = text[start..].trim_start();
    let id: String = tail
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if id.is_empty() { None } else { Some(id) }
}

/// 截断文本到 80 字符作为会话标题。
fn short_title(text: &str) -> String {
    let trimmed = text.trim();
    let mut title: String = trimmed.chars().take(80).collect();
    if trimmed.chars().count() > 80 {
        title.push_str("...");
    }
    if title.is_empty() {
        "新会话".to_string()
    } else {
        title
    }
}

/// 从 stdout/stderr 流逐行读取并写入共享 LogWriter。
/// cancel_flag 用于在 job 取消时通知 reader 线程退出。
fn read_stream<R: Read>(
    reader: BufReader<R>,
    stream: &'static str,
    writer: &Arc<Mutex<LogWriter>>,
    cancel_flag: &AtomicBool,
) {
    let mut lines = reader.lines();
    loop {
        if cancel_flag.load(Ordering::Relaxed) {
            break;
        }
        match lines.next() {
            Some(Ok(line)) => {
                let mut lw = writer.lock().unwrap();
                lw.write(stream, &line);
            }
            Some(Err(_)) | None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log(stream: &str, chunk: &str) -> checkpoint_core::audit::JobLogRow {
        checkpoint_core::audit::JobLogRow {
            id: 1,
            job_id: "job-1".to_string(),
            stream: stream.to_string(),
            chunk: chunk.to_string(),
            seq: 1,
            timestamp: "2026-04-27T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn extracts_kimi_resume_id_from_logs() {
        let logs = vec![
            log("stdout", "回复 1"),
            log(
                "stderr",
                "To resume this session: kimi -r 250190b6-fcbe-497f-abf5-161f3b4e05c6",
            ),
        ];
        assert_eq!(
            extract_provider_conversation_id("kimi_code", &logs),
            Some("250190b6-fcbe-497f-abf5-161f3b4e05c6".to_string())
        );
    }

    #[test]
    fn ignores_unknown_provider_resume_text() {
        let logs = vec![log("stderr", "To resume this session: kimi -r abc")];
        assert_eq!(extract_provider_conversation_id("codex_cli", &logs), None);
    }

    fn make_registry() -> ProviderRegistry {
        ProviderRegistry::from_config(&checkpoint_core::config::Config::default_config())
    }

    fn make_resolver_with_binary(provider: &str, path: &str) -> ProviderResolver {
        let mut config = checkpoint_core::config::Config::default_config();
        config
            .provider_binaries
            .insert(provider.to_string(), path.to_string());
        let registry = make_registry();
        ProviderResolver::from_config(&config, &registry).with_custom_fallback(vec![])
    }

    fn make_fake_binary(name: &str) -> tempfile::NamedTempFile {
        let f = tempfile::NamedTempFile::with_prefix(name).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        f
    }

    #[test]
    fn agent_prompt_codex_with_conversation_id_succeeds() {
        let tmp = make_fake_binary("codex");
        let resolver = make_resolver_with_binary("codex_cli", tmp.path().to_str().unwrap());
        let input = serde_json::json!({
            "provider": "codex_cli",
            "prompt": "fix bug",
            "conversation_id": "sess-abc-123"
        });
        let registry = make_registry();
        let result = build_command(
            "agent_prompt",
            &input,
            Some("/tmp/proj"),
            &resolver,
            &registry,
        );
        assert!(
            result.is_ok(),
            "codex_cli with conversation_id should not fail"
        );
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.contains(&"resume".to_string()),
            "must contain 'resume'"
        );
        assert!(
            args.contains(&"sess-abc-123".to_string()),
            "must contain conversation_id"
        );
    }

    #[test]
    fn agent_prompt_codex_without_conversation_id_no_resume() {
        let tmp = make_fake_binary("codex");
        let resolver = make_resolver_with_binary("codex_cli", tmp.path().to_str().unwrap());
        let input = serde_json::json!({
            "provider": "codex_cli",
            "prompt": "new task"
        });
        let registry = make_registry();
        let result = build_command(
            "agent_prompt",
            &input,
            Some("/tmp/proj"),
            &resolver,
            &registry,
        );
        assert!(result.is_ok());
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            !args.contains(&"resume".to_string()),
            "must NOT contain 'resume'"
        );
    }

    #[test]
    fn agent_prompt_claude_with_conversation_id_includes_resume() {
        let tmp = make_fake_binary("claude");
        let resolver = make_resolver_with_binary("claude_code", tmp.path().to_str().unwrap());
        let input = serde_json::json!({
            "provider": "claude_code",
            "prompt": "continue",
            "conversation_id": "sess-xyz"
        });
        let registry = make_registry();
        let result = build_command(
            "agent_prompt",
            &input,
            Some("/tmp/proj"),
            &resolver,
            &registry,
        );
        assert!(result.is_ok());
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sess-xyz".to_string()));
    }

    #[test]
    fn agent_prompt_claude_with_runtime_bypass_includes_permission_flags() {
        let tmp = make_fake_binary("claude");
        let resolver = make_resolver_with_binary("claude_code", tmp.path().to_str().unwrap());
        let input = serde_json::json!({
            "provider": "claude_code",
            "prompt": "continue",
            "conversation_id": "sess-xyz",
            "runtime_permission_mode": checkpoint_core::constants::PERMISSION_MODE_BYPASS,
        });
        let registry = make_registry();
        let result = build_command(
            "agent_prompt",
            &input,
            Some("/tmp/proj"),
            &resolver,
            &registry,
        );
        assert!(result.is_ok());
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        let envs: std::collections::HashMap<_, _> = cmd.get_envs().collect();
        assert_eq!(
            envs.get(std::ffi::OsStr::new("VIBE_ISLAND_SKIP"))
                .map(|v| v.unwrap()),
            Some(std::ffi::OsStr::new("1"))
        );
    }

    #[test]
    fn agent_prompt_kimi_with_conversation_id_includes_resume() {
        let tmp = make_fake_binary("kimi");
        let resolver = make_resolver_with_binary("kimi_code", tmp.path().to_str().unwrap());
        let input = serde_json::json!({
            "provider": "kimi_code",
            "prompt": "test",
            "conversation_id": "sid-999"
        });
        let registry = make_registry();
        let result = build_command(
            "agent_prompt",
            &input,
            Some("/tmp/proj"),
            &resolver,
            &registry,
        );
        assert!(result.is_ok());
        let cmd = result.unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--resume".to_string()));
        assert!(args.contains(&"sid-999".to_string()));
    }

    #[test]
    fn resume_guard_rejects_non_resume_provider() {
        use checkpoint_core::provider_registry::ProviderConfigOverride;
        use std::collections::HashMap;
        let mut config = checkpoint_core::config::Config::default_config();
        let mut providers = HashMap::new();
        providers.insert(
            "no_resume_tool".into(),
            ProviderConfigOverride {
                enabled: Some(true),
                command: Some("no_resume_tool".into()),
                display_name: Some("No Resume".into()),
                supports_resume: Some(false),
                ..Default::default()
            },
        );
        config.providers = providers;
        let registry = ProviderRegistry::from_config(&config);
        let resolver = ProviderResolver::from_config(&config, &registry);
        let input = serde_json::json!({
            "provider": "no_resume_tool",
            "prompt": "test",
            "conversation_id": "sess-123"
        });
        let result = build_command(
            "agent_prompt",
            &input,
            Some("/tmp/proj"),
            &resolver,
            &registry,
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("does not support resume"),
            "expected resume guard error"
        );
    }

    #[test]
    fn write_system_no_activity_does_not_renew_watchdog() {
        use std::time::Duration;

        let tmp_db = tempfile::NamedTempFile::new().unwrap();
        let db_path = tmp_db.path().to_path_buf();
        {
            let store = AuditStore::open(&db_path).unwrap();
            store
                .insert_job(
                    "job-test",
                    "agent_prompt",
                    "{}",
                    "2026-05-01T00:00:00Z",
                    Some("claude_code"),
                    Some("/tmp"),
                    None,
                    Some("test"),
                )
                .unwrap();
        }
        let broadcaster =
            std::sync::Arc::new(std::sync::Mutex::new(crate::sse::SseBroadcaster::new()));
        let mut lw = LogWriter {
            db_path,
            job_id: "job-test".to_string(),
            seq: 1,
            total_bytes: 0,
            max_bytes: 1024 * 1024,
            truncated: false,
            last_activity: Instant::now() - Duration::from_secs(10),
            broadcaster,
        };

        let before = lw.last_activity;

        // write_system_no_activity must NOT renew last_activity
        lw.write_system_no_activity("[soft idle timeout, entering observe mode]");
        assert_eq!(
            lw.last_activity, before,
            "write_system_no_activity must not update last_activity"
        );

        // regular write must renew last_activity
        lw.write("stdout", "real output");
        assert!(lw.last_activity > before, "write must update last_activity");
    }
}

// ---- HTTP handlers ----

/// 读取并解析请求体为 JSON，失败时返回 400 错误响应。
fn read_body(
    request: &mut tiny_http::Request,
) -> Result<serde_json::Value, tiny_http::ResponseBox> {
    let mut body = String::new();
    if let Err(e) = request.as_reader().read_to_string(&mut body) {
        return Err(json_response(
            400,
            &serde_json::json!({"error": format!("read body: {e}")}),
        ));
    }
    match serde_json::from_str(&body) {
        Ok(v) => Ok(v),
        Err(e) => Err(json_response(
            400,
            &serde_json::json!({"error": format!("parse json: {e}")}),
        )),
    }
}

/// POST /jobs 处理器。
/// 读取 body → 校验 kind → promote 顶层字段到 input → 提交 job → 返回 job_id。
/// 已有 job 运行时返回 409。
pub fn handle_post_jobs(
    request: &mut tiny_http::Request,
    runner: &JobRunner,
) -> tiny_http::ResponseBox {
    let parsed = match read_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let kind = match parsed.get("kind").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => return json_response(400, &serde_json::json!({"error": "missing 'kind' field"})),
    };

    if !JobRunner::validate_kind(kind) {
        return json_response(
            400,
            &serde_json::json!({"error": format!("invalid job kind: {kind}")}),
        );
    }

    let mut input = parsed
        .get("input")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    // Promote: 将前端 body 顶层字段写入 input，供 build_command 读取。
    // 用 or_insert 保证不覆盖 input 中已有的值。
    // conversation_id 在这里不做过滤——由 provider command builder 决定如何使用。
    if let Some(obj) = input.as_object_mut() {
        if let Some(v) = parsed.get("provider").cloned() {
            obj.entry("provider").or_insert(v);
        }
        if let Some(v) = parsed.get("prompt").cloned() {
            obj.entry("prompt").or_insert(v);
        }
        if let Some(v) = parsed.get("conversation_id").cloned() {
            obj.entry("conversation_id").or_insert(v);
        }
    }

    // Validate Agent Aspect mode input（兼容旧 kind）。
    if kind == "agent_aspect_mode" || kind == "checkpoint_mode" {
        if input.get("mode").and_then(|v| v.as_str()).is_none() {
            return json_response(
                400,
                &serde_json::json!({"error": "agent_aspect_mode requires 'mode' in input"}),
            );
        }
    }

    let provider = parsed.get("provider").and_then(|v| v.as_str());
    let project_path = parsed.get("project_path").and_then(|v| v.as_str());
    let conversation_id = parsed.get("conversation_id").and_then(|v| v.as_str());
    let prompt = parsed.get("prompt").and_then(|v| v.as_str());
    let force_confirm = parsed
        .get("force_confirm")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let (device_id, user_agent, remote_addr) = request_device(request);
    if let Ok(store) = AuditStore::open(&runner.db_path) {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = store.register_device(
            &device_id,
            user_agent.as_deref(),
            remote_addr.as_deref(),
            &now,
        );
    }

    match runner.submit(
        kind,
        &input,
        provider,
        project_path,
        conversation_id,
        prompt,
        Some(&device_id),
        force_confirm,
    ) {
        Ok(job_id) => json_response(
            200,
            &serde_json::json!({"job_id": job_id, "status": "queued"}),
        ),
        Err(SubmitError::DriftBlocked { message, health }) => {
            // Runtime drift: 返回 409 + health 详情，前端弹出确认框
            json_response(
                409,
                &serde_json::json!({
                    "error": "runtime drift detected",
                    "message": message,
                    "runtime_health": health,
                }),
            )
        }
        Err(SubmitError::CostBlocked { message, stats }) => {
            // Resume cost: 返回 409 + 成本详情，前端弹出确认框
            json_response(
                409,
                &serde_json::json!({
                    "error": "resume cost too high",
                    "message": message,
                    "cost_stats": stats,
                }),
            )
        }
        Err(SubmitError::Fatal(e)) => {
            let status = if e.contains("already running") {
                409
            } else {
                500
            };
            json_response(status, &serde_json::json!({"error": e}))
        }
    }
}

/// GET /jobs 处理器。支持 limit/offset/status 分页参数。
pub fn handle_get_jobs(request: &tiny_http::Request, runner: &JobRunner) -> tiny_http::ResponseBox {
    let url = request.url();
    let limit = super::routes::query_param(url, "limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(super::routes::DEFAULT_PAGE_SIZE)
        .min(super::routes::MAX_PAGE_SIZE);
    let offset = super::routes::query_param(url, "offset")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let status = super::routes::query_param(url, "status");

    match runner.list_jobs(limit, offset, status) {
        Ok((jobs, total)) => {
            let jobs_json: Vec<serde_json::Value> = jobs
                .iter()
                .map(|j| {
                    serde_json::json!({
                        "id": j.id,
                        "kind": j.kind,
                        "input": j.input,
                        "status": j.status,
                        "created_at": j.created_at,
                        "started_at": j.started_at,
                        "finished_at": j.finished_at,
                        "exit_code": j.exit_code,
                        "provider": j.provider,
                        "project_path": j.project_path,
                        "conversation_id": j.conversation_id,
                        "conversation_db_id": job_conversation_db_id(j),
                        "prompt": j.prompt,
                        "pid": j.pid,
                        "process_group_id": j.process_group_id,
                        "runner_id": j.runner_id,
                        "heartbeat_at": j.heartbeat_at,
                        "timeout_secs": j.timeout_secs,
                        "failure_reason": j.failure_reason,
                        "last_log_at": j.last_log_at,
                    })
                })
                .collect();
            json_response(200, &serde_json::json!({"jobs": jobs_json, "total": total}))
        }
        Err(e) => json_response(500, &serde_json::json!({"error": e})),
    }
}

/// GET /jobs/:id 处理器。返回单个 job 的完整信息。
pub fn handle_get_job(job_id: &str, runner: &JobRunner) -> tiny_http::ResponseBox {
    match runner.get_job(job_id) {
        Ok(None) => json_response(404, &serde_json::json!({"error": "job not found"})),
        Ok(Some(j)) => json_response(
            200,
            &serde_json::json!({
                "id": j.id,
                "kind": j.kind,
                "input": j.input,
                "status": j.status,
                "created_at": j.created_at,
                "started_at": j.started_at,
                "finished_at": j.finished_at,
                "exit_code": j.exit_code,
                "provider": j.provider,
                "project_path": j.project_path,
                "conversation_id": j.conversation_id,
                "conversation_db_id": job_conversation_db_id(&j),
                "prompt": j.prompt,
                "pid": j.pid,
                "process_group_id": j.process_group_id,
                "runner_id": j.runner_id,
                "heartbeat_at": j.heartbeat_at,
                "timeout_secs": j.timeout_secs,
                "failure_reason": j.failure_reason,
                "last_log_at": j.last_log_at,
            }),
        ),
        Err(e) => json_response(500, &serde_json::json!({"error": e})),
    }
}

/// GET /jobs/:id/logs 处理器。返回 job 的所有日志行。
pub fn handle_get_job_logs(job_id: &str, runner: &JobRunner) -> tiny_http::ResponseBox {
    match runner.get_job_logs(job_id) {
        Ok(logs) => {
            let logs_json: Vec<serde_json::Value> = logs
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "stream": l.stream,
                        "chunk": l.chunk,
                        "seq": l.seq,
                        "timestamp": l.timestamp,
                    })
                })
                .collect();
            json_response(200, &serde_json::json!({"logs": logs_json}))
        }
        Err(e) => json_response(500, &serde_json::json!({"error": e})),
    }
}

/// POST /jobs/:id/logs/delta 处理器。增量获取 after_id 之后的日志行。
/// 返回 next_after_id 和 has_more 供前端分页。
pub fn handle_post_job_logs_delta(
    job_id: &str,
    request: &mut tiny_http::Request,
    runner: &JobRunner,
) -> tiny_http::ResponseBox {
    let parsed = match read_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let after_id = parsed.get("after_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let limit = parsed
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(100)
        .clamp(1, 500) as usize;

    match runner.get_job_logs_after(job_id, after_id, limit + 1) {
        Ok(mut logs) => {
            let has_more = logs.len() > limit;
            if has_more {
                logs.truncate(limit);
            }
            let next_after_id = logs.last().map(|l| l.id).unwrap_or(after_id);
            let logs_json: Vec<serde_json::Value> = logs
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "id": l.id,
                        "stream": l.stream,
                        "chunk": l.chunk,
                        "seq": l.seq,
                        "timestamp": l.timestamp,
                    })
                })
                .collect();
            json_response(
                200,
                &serde_json::json!({
                    "logs": logs_json,
                    "after_id": after_id,
                    "next_after_id": next_after_id,
                    "has_more": has_more,
                    "limit": limit,
                }),
            )
        }
        Err(e) => json_response(500, &serde_json::json!({"error": e})),
    }
}

/// POST /jobs/:id/cancel 处理器。取消运行中或排队的 job。
pub fn handle_post_cancel(job_id: &str, runner: &JobRunner) -> tiny_http::ResponseBox {
    match runner.cancel(job_id) {
        Ok(()) => {
            // Broadcast job_status on cancel
            runner.broadcaster.lock().unwrap().broadcast(sse::SseEvent {
                event_type: "job_status".to_string(),
                data: serde_json::json!({
                    "job_id": job_id,
                    "status": "cancelled",
                    "failure_reason": "cancelled by user",
                })
                .to_string(),
            });
            crate::routes::invalidate_overview_cache();
            json_response(
                200,
                &serde_json::json!({"job_id": job_id, "status": "cancelled"}),
            )
        }
        Err(e) => {
            let status = if e.contains("not found") {
                404
            } else if e.contains("already") {
                409
            } else {
                500
            };
            json_response(status, &serde_json::json!({"error": e}))
        }
    }
}
