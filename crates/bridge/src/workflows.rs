//! 工作流编排 — 链式任务执行引擎和 HTTP API。
//!
//! 架构角色：管理多步骤工作流的创建、执行和状态追踪。
//! WorkflowRunner 在后台线程中串行执行 steps，前一步的 job logs
//! 按 context_strategy 传递给下一步的 prompt。
//!
//! 核心不变量：
//! - 同一时刻只有一个 workflow 在执行（与 JobRunner 单 job 约束一致）
//! - step 按 step_order 严格串行，前一步 succeeded 才执行下一步
//! - 取消传播：workflow cancel 会取消当前运行的 job 和所有 pending steps
//! - context_strategy 控制日志截断：none / last_50_lines / last_100_lines / full_log

use checkpoint_core::audit::AuditStore;
use checkpoint_core::store::workflows::WorkflowStepRow;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::jobs::JobRunner;
use crate::routes::{json_response, read_json_body};
use crate::sse::SharedBroadcaster;

/// 工作流运行状态。
struct RunningWorkflow {
    workflow_id: String,
    cancel_flag: Arc<AtomicBool>,
}

/// 工作流运行器：管理 workflow 生命周期（draft → running → succeeded/failed/cancelled）。
/// 全局单例，同一时刻最多运行一个 workflow。
pub struct WorkflowRunner {
    running: Arc<Mutex<Option<RunningWorkflow>>>,
    db_path: PathBuf,
    broadcaster: SharedBroadcaster,
}

impl WorkflowRunner {
    pub fn new(db_path: PathBuf, broadcaster: SharedBroadcaster) -> Self {
        Self {
            running: Arc::new(Mutex::new(None)),
            db_path,
            broadcaster,
        }
    }

    /// 启动工作流执行。在后台线程中串行执行所有 steps。
    pub fn start_workflow(
        &self,
        workflow_id: &str,
        job_runner: &Arc<JobRunner>,
    ) -> Result<(), String> {
        {
            let guard = self.running.lock().unwrap();
            if guard.is_some() {
                return Err("a workflow is already running".to_string());
            }
        }

        let store = AuditStore::open(&self.db_path)
            .map_err(|e| format!("open db: {e}"))?;

        // 验证 workflow 存在且状态为 draft 或 failed（允许重试）
        let wf = store.get_workflow(workflow_id)
            .map_err(|e| format!("query workflow: {e}"))?
            .ok_or("workflow not found")?;

        if wf.status != "draft" && wf.status != "failed" && wf.status != "cancelled" {
            return Err(format!("workflow status '{}' cannot be started", wf.status));
        }

        // 验证至少有一个 step
        let steps = store.get_workflow_steps(workflow_id)
            .map_err(|e| format!("query steps: {e}"))?;
        if steps.is_empty() {
            return Err("workflow has no steps".to_string());
        }

        // 重置所有非 succeeded 的步骤为 pending（支持重试）
        for step in &steps {
            if step.status != "succeeded" {
                let _ = store.update_workflow_step_status(&step.id, "pending", None);
            }
        }

        let now = chrono::Utc::now().to_rfc3339();
        store.update_workflow_status(workflow_id, "running", &now)
            .map_err(|e| format!("update workflow status: {e}"))?;

        let cancel_flag = Arc::new(AtomicBool::new(false));
        let running = RunningWorkflow {
            workflow_id: workflow_id.to_string(),
            cancel_flag: cancel_flag.clone(),
        };
        *self.running.lock().unwrap() = Some(running);

        // 广播 workflow 状态变更
        self.broadcaster.lock().unwrap().broadcast(crate::sse::SseEvent {
            event_type: "workflow_status".to_string(),
            data: workflow_id.to_string(),
        });

        // 启动后台执行线程
        let wf_id = workflow_id.to_string();
        let db_path = self.db_path.clone();
        let running_ref = self.running.clone();
        let broadcaster = self.broadcaster.clone();
        let job_runner = job_runner.clone();

        std::thread::spawn(move || {
            execute_workflow(
                &wf_id,
                &db_path,
                &running_ref,
                &broadcaster,
                &job_runner,
                &cancel_flag,
            );
        });

        Ok(())
    }

    /// 取消正在运行的工作流。设置 cancel_flag 让后台线程自行收敛。
    pub fn cancel_workflow(&self, workflow_id: &str) -> Result<bool, String> {
        {
            let guard = self.running.lock().unwrap();
            if let Some(ref running) = *guard {
                if running.workflow_id == workflow_id {
                    running.cancel_flag.store(true, Ordering::SeqCst);
                    return Ok(true);
                }
            }
        }

        // 不在运行器中（可能已完成或未启动），直接更新 DB 状态
        let store = AuditStore::open(&self.db_path)
            .map_err(|e| format!("open db: {e}"))?;
        let wf = store.get_workflow(workflow_id)
            .map_err(|e| format!("query workflow: {e}"))?
            .ok_or("workflow not found")?;

        if wf.status == "running" || wf.status == "draft" {
            let now = chrono::Utc::now().to_rfc3339();
            store.update_workflow_status(workflow_id, "cancelled", &now)
                .map_err(|e| format!("update status: {e}"))?;

            // 取消所有未完成步骤，并尝试 cancel 关联的 running job
            let steps = store.get_workflow_steps(workflow_id)
                .map_err(|e| format!("query steps: {e}"))?;
            for step in &steps {
                if step.status == "pending" || step.status == "running" {
                    let _ = store.update_workflow_step_status(&step.id, "cancelled", None);
                    // 如果步骤有关联的 running job，尝试取消它
                    if step.status == "running" {
                        if let Some(ref job_id) = step.job_id {
                            let _ = store.cancel_job(job_id);
                        }
                    }
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().unwrap().is_some()
    }

    pub fn running_workflow_id(&self) -> Option<String> {
        self.running.lock().unwrap().as_ref().map(|r| r.workflow_id.clone())
    }
}

/// 工作流执行主函数（在后台线程运行）。
/// 串行执行每个 step：提交 job → 等待完成 → 读取日志 → 传递上下文 → 提交下一步。
fn execute_workflow(
    workflow_id: &str,
    db_path: &PathBuf,
    running: &Arc<Mutex<Option<RunningWorkflow>>>,
    broadcaster: &SharedBroadcaster,
    job_runner: &Arc<JobRunner>,
    cancel_flag: &Arc<AtomicBool>,
) {
    let result = execute_workflow_inner(workflow_id, db_path, running, broadcaster, job_runner, cancel_flag);

    // 清理运行状态
    *running.lock().unwrap() = None;

    // 更新最终状态
    if let Ok(store) = AuditStore::open(db_path) {
        let now = chrono::Utc::now().to_rfc3339();
        let final_status = match &result {
            Ok(()) => "succeeded",
            Err(ExecuteError::Cancelled) => "cancelled",
            Err(_) => "failed",
        };
        let _ = store.update_workflow_status(workflow_id, final_status, &now);

        // 如果失败，将剩余 pending 步骤标记为 skipped
        if final_status == "failed" || final_status == "cancelled" {
            let _ = store.cancel_workflow_steps(workflow_id);
        }

        broadcaster.lock().unwrap().broadcast(crate::sse::SseEvent {
            event_type: "workflow_status".to_string(),
            data: workflow_id.to_string(),
        });
    }

    if let Err(e) = &result {
        eprintln!("agent-aspect-bridge: workflow {workflow_id}: {e}");
    }
}

#[derive(Debug)]
enum ExecuteError {
    Cancelled,
    Db(String),
    JobFailed(String),
    Timeout,
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::Cancelled => write!(f, "cancelled"),
            ExecuteError::Db(msg) => write!(f, "db error: {msg}"),
            ExecuteError::JobFailed(msg) => write!(f, "job failed: {msg}"),
            ExecuteError::Timeout => write!(f, "workflow step timeout"),
        }
    }
}

fn execute_workflow_inner(
    workflow_id: &str,
    db_path: &PathBuf,
    _running: &Arc<Mutex<Option<RunningWorkflow>>>,
    broadcaster: &SharedBroadcaster,
    job_runner: &Arc<JobRunner>,
    cancel_flag: &Arc<AtomicBool>,
) -> Result<(), ExecuteError> {
    let store = AuditStore::open(db_path).map_err(|e| ExecuteError::Db(e.to_string()))?;
    let steps = store.get_workflow_steps(workflow_id)
        .map_err(|e| ExecuteError::Db(e.to_string()))?;

    let mut previous_logs: Option<String> = None;

    for step in &steps {
        // 检查取消
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = store.update_workflow_step_status(&step.id, "cancelled", None);
            return Err(ExecuteError::Cancelled);
        }

        // 跳过已完成的步骤（重试场景）
        if step.status == "succeeded" {
            // 读取该步骤的日志作为下一步的上下文
            if let Some(ref job_id) = step.job_id {
                previous_logs = Some(read_job_logs_for_context(
                    &store, job_id, &step.context_strategy,
                ));
            }
            continue;
        }

        // 构建 prompt：如果有前一步的日志且当前步骤配置了 context_strategy
        let prompt = build_step_prompt(step, previous_logs.as_deref());

        // 更新步骤状态为 running
        store.update_workflow_step_status(&step.id, "running", None)
            .map_err(|e| ExecuteError::Db(e.to_string()))?;

        broadcaster.lock().unwrap().broadcast(crate::sse::SseEvent {
            event_type: "workflow_step_status".to_string(),
            data: serde_json::json!({
                "workflow_id": workflow_id,
                "step_id": step.id,
                "step_order": step.step_order,
                "status": "running"
            }).to_string(),
        });

        // 提交 job
        let job_id = uuid::Uuid::now_v7().to_string();
        let input = serde_json::json!({});
        let provider = step.provider.as_deref();
        let project_path = step.project_path.as_deref();

        store.insert_job(
            &job_id,
            &step.kind,
            &input.to_string(),
            &chrono::Utc::now().to_rfc3339(),
            provider,
            project_path,
            None,
            Some(&prompt),
        ).map_err(|e| ExecuteError::Db(e.to_string()))?;

        // 绑定 job_id 到步骤
        let _ = store.update_workflow_step_job(&step.id, &job_id);

        // 广播步骤 job 提交
        broadcaster.lock().unwrap().broadcast(crate::sse::SseEvent {
            event_type: "workflow_step_job".to_string(),
            data: serde_json::json!({
                "workflow_id": workflow_id,
                "step_id": step.id,
                "job_id": job_id,
            }).to_string(),
        });

        // 同步执行 job（阻塞直到完成）
        let job_result = execute_step_job_sync(
            &job_id,
            db_path,
            job_runner,
            cancel_flag,
            broadcaster,
        );

        match job_result {
            Ok(()) => {
                let now = chrono::Utc::now().to_rfc3339();
                store.update_workflow_step_status(&step.id, "succeeded", Some(&now))
                    .map_err(|e| ExecuteError::Db(e.to_string()))?;

                // 读取日志作为下一步上下文
                previous_logs = Some(read_job_logs_for_context(
                    &store, &job_id, &step.context_strategy,
                ));
            }
            Err(ExecuteError::Cancelled) => {
                let _ = store.update_workflow_step_status(&step.id, "cancelled", None);
                // 取消正在运行的 job
                let _ = job_runner.cancel(&job_id);
                return Err(ExecuteError::Cancelled);
            }
            Err(e) => {
                let now = chrono::Utc::now().to_rfc3339();
                let _ = store.update_workflow_step_status(&step.id, "failed", Some(&now));
                return Err(e);
            }
        }

        // 广播步骤完成
        broadcaster.lock().unwrap().broadcast(crate::sse::SseEvent {
            event_type: "workflow_step_status".to_string(),
            data: serde_json::json!({
                "workflow_id": workflow_id,
                "step_id": step.id,
                "step_order": step.step_order,
                "status": "succeeded"
            }).to_string(),
        });
    }

    Ok(())
}

/// 同步执行单个 step job：轮询 job 状态直到 terminal。
fn execute_step_job_sync(
    job_id: &str,
    db_path: &PathBuf,
    _job_runner: &Arc<JobRunner>,
    cancel_flag: &Arc<AtomicBool>,
    _broadcaster: &SharedBroadcaster,
) -> Result<(), ExecuteError> {
    // 轮询 job 状态直到 terminal（succeeded/failed/cancelled）
    // 由于 JobRunner 只支持白名单 kind 且单 job 并发，
    // workflow step job 直接写 DB 后等待外部 JobRunner pickup 或超时。
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(600); // 10 分钟 per step

    loop {
        if cancel_flag.load(Ordering::SeqCst) {
            return Err(ExecuteError::Cancelled);
        }

        if start.elapsed() > timeout {
            return Err(ExecuteError::Timeout);
        }

        // 每次轮询打开独立连接，避免长期持锁
        let store = AuditStore::open(db_path).map_err(|e| ExecuteError::Db(e.to_string()))?;
        if let Ok(Some(job)) = store.get_job(job_id) {
            match job.status.as_str() {
                "succeeded" => return Ok(()),
                "failed" => {
                    let reason = job.failure_reason.unwrap_or_default();
                    return Err(ExecuteError::JobFailed(reason));
                }
                "cancelled" => return Err(ExecuteError::Cancelled),
                _ => {
                    // queued, running, observing — 继续等待
                }
            }
        }

        std::thread::sleep(Duration::from_secs(2));
    }
}

/// 读取 job 日志并按 context_strategy 截断。
fn read_job_logs_for_context(
    store: &AuditStore,
    job_id: &str,
    strategy: &str,
) -> String {
    if strategy == "none" {
        return String::new();
    }

    let logs = match store.get_job_logs(job_id) {
        Ok(logs) => logs,
        Err(_) => return String::new(),
    };

    let all_output: Vec<&str> = logs
        .iter()
        .filter(|l| l.stream == "stdout" || l.stream == "stderr")
        .map(|l| l.chunk.as_str())
        .collect();

    match strategy {
        "last_50_lines" => {
            let start = all_output.len().saturating_sub(50);
            all_output[start..].join("\n")
        }
        "last_100_lines" => {
            let start = all_output.len().saturating_sub(100);
            all_output[start..].join("\n")
        }
        "full_log" => all_output.join("\n"),
        _ => String::new(),
    }
}

/// 构建 step prompt：将前一步的日志作为上下文注入。
fn build_step_prompt(step: &WorkflowStepRow, previous_logs: Option<&str>) -> String {
    match (step.context_strategy.as_str(), previous_logs) {
        ("none", _) | (_, None) | (_, Some("")) => step.prompt.clone(),
        (strategy, Some(logs)) => {
            format!(
                "[Previous step output ({}):]\n{}\n\n[Your task:]\n{}",
                strategy, logs, step.prompt
            )
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// HTTP Handlers
// ═══════════════════════════════════════════════════════════════════

/// POST /workflows — 创建新工作流。
///
/// Body: { "name": "...", "description": "...", "steps": [...] }
pub fn handle_post_workflows(
    ctx: &crate::context::AppContext,
    request: &mut tiny_http::Request,
    workflow_runner: &Arc<WorkflowRunner>,
) -> tiny_http::ResponseBox {
    let body: serde_json::Value = match read_json_body(request) {
        Ok(v) => v,
        Err(r) => return r,
    };

    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return json_response(400, &serde_json::json!({"error": "name is required"})),
    };
    let description = body.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let steps = match body.get("steps").and_then(|v| v.as_array()) {
        Some(s) => s,
        _ => return json_response(400, &serde_json::json!({"error": "steps array is required"})),
    };

    if steps.is_empty() {
        return json_response(400, &serde_json::json!({"error": "steps cannot be empty"}));
    }

    let store = ctx.store.lock().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let wf_id = uuid::Uuid::now_v7().to_string();

    if let Err(e) = store.insert_workflow(&wf_id, name, description, &now) {
        return json_response(500, &serde_json::json!({"error": e.to_string()}));
    }

    for (i, step_val) in steps.iter().enumerate() {
        let step_id = uuid::Uuid::now_v7().to_string();
        let kind = step_val.get("kind").and_then(|v| v.as_str()).unwrap_or("agent_prompt");
        let provider = step_val.get("provider").and_then(|v| v.as_str());
        let project_path = step_val.get("project_path").and_then(|v| v.as_str());
        let prompt = step_val.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let context_strategy = step_val.get("context_strategy").and_then(|v| v.as_str()).unwrap_or("none");
        let context_from_step = step_val.get("context_from_step").and_then(|v| v.as_i64());

        if let Err(e) = store.insert_workflow_step(
            &step_id, &wf_id, i as i64, kind, provider, project_path,
            prompt, context_strategy, context_from_step, &now,
        ) {
            return json_response(500, &serde_json::json!({"error": e.to_string()}));
        }
    }

    json_response(201, &serde_json::json!({"id": wf_id, "status": "draft"}))
}

/// GET /workflows — 列出所有工作流。
pub fn handle_get_workflows(
    ctx: &crate::context::AppContext,
    request: &tiny_http::Request,
) -> tiny_http::ResponseBox {
    let url = request.url();
    let limit = crate::routes::query_param(url, "limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(20)
        .min(100);
    let offset = crate::routes::query_param(url, "offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);

    let store = ctx.store.lock().unwrap();
    let workflows = match store.list_workflows(limit, offset) {
        Ok(w) => w,
        Err(e) => return json_response(500, &serde_json::json!({"error": e.to_string()})),
    };

    let items: Vec<serde_json::Value> = workflows.iter().map(|wf| {
        let (total, succeeded, failed, pending) = store.workflow_step_counts(&wf.id).unwrap_or((0, 0, 0, 0));
        serde_json::json!({
            "id": wf.id,
            "name": wf.name,
            "description": wf.description,
            "status": wf.status,
            "created_at": wf.created_at,
            "updated_at": wf.updated_at,
            "step_counts": {
                "total": total,
                "succeeded": succeeded,
                "failed": failed,
                "pending": pending,
            }
        })
    }).collect();

    json_response(200, &serde_json::json!({
        "workflows": items,
        "limit": limit,
        "offset": offset,
    }))
}

/// GET /workflows/:id — 获取工作流详情（含所有 steps）。
pub fn handle_get_workflow(
    ctx: &crate::context::AppContext,
    workflow_id: &str,
) -> tiny_http::ResponseBox {
    let store = ctx.store.lock().unwrap();
    let wf = match store.get_workflow(workflow_id) {
        Ok(Some(wf)) => wf,
        Ok(None) => return json_response(404, &serde_json::json!({"error": "workflow not found"})),
        Err(e) => return json_response(500, &serde_json::json!({"error": e.to_string()})),
    };

    let steps = match store.get_workflow_steps(workflow_id) {
        Ok(s) => s,
        Err(e) => return json_response(500, &serde_json::json!({"error": e.to_string()})),
    };

    let step_values: Vec<serde_json::Value> = steps.iter().map(|s| {
        serde_json::json!({
            "id": s.id,
            "step_order": s.step_order,
            "kind": s.kind,
            "provider": s.provider,
            "project_path": s.project_path,
            "prompt": s.prompt,
            "context_strategy": s.context_strategy,
            "context_from_step": s.context_from_step,
            "status": s.status,
            "job_id": s.job_id,
            "created_at": s.created_at,
            "finished_at": s.finished_at,
        })
    }).collect();

    let (total, succeeded, failed, pending) = store.workflow_step_counts(workflow_id).unwrap_or((0, 0, 0, 0));

    json_response(200, &serde_json::json!({
        "id": wf.id,
        "name": wf.name,
        "description": wf.description,
        "status": wf.status,
        "created_at": wf.created_at,
        "updated_at": wf.updated_at,
        "steps": step_values,
        "step_counts": {
            "total": total,
            "succeeded": succeeded,
            "failed": failed,
            "pending": pending,
        }
    }))
}

/// POST /workflows/:id/run — 触发工作流执行。
pub fn handle_post_workflow_run(
    _ctx: &crate::context::AppContext,
    workflow_id: &str,
    workflow_runner: &Arc<WorkflowRunner>,
    job_runner: &Arc<JobRunner>,
) -> tiny_http::ResponseBox {
    match workflow_runner.start_workflow(workflow_id, job_runner) {
        Ok(()) => json_response(200, &serde_json::json!({"status": "running"})),
        Err(e) => {
            if e.contains("already running") {
                json_response(409, &serde_json::json!({"error": e}))
            } else {
                json_response(400, &serde_json::json!({"error": e}))
            }
        }
    }
}

/// POST /workflows/:id/cancel — 取消工作流。
pub fn handle_post_workflow_cancel(
    workflow_id: &str,
    workflow_runner: &Arc<WorkflowRunner>,
) -> tiny_http::ResponseBox {
    match workflow_runner.cancel_workflow(workflow_id) {
        Ok(true) => json_response(200, &serde_json::json!({"status": "cancelled"})),
        Ok(false) => json_response(400, &serde_json::json!({"error": "workflow not in cancellable state"})),
        Err(e) => json_response(500, &serde_json::json!({"error": e})),
    }
}
