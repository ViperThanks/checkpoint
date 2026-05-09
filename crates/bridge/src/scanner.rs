//! Transcript Scanner — 后台线程定时扫描活跃 observer 的 transcript 增量。
//!
//! Scanner 通过 DAO 方法更新 observer 状态（cursor / idle / timed_out）。
//! job 终态由 JobRunner 的 CompletionSink 写入，scanner 负责提供独立观测证据。
//!
//! M47.7 起增加 orphan/stale 收口能力：当 observer hard_deadline 超时且
//! 关联 job 的 runner heartbeat 明显 stale 时，scanner 可直接写入 job 终态。
//!
//! 关键不变量：
//! - ScannerIdle 永远不等于 completed
//! - idle_deadline_at 基于 last_activity_at 滚动
//! - hard_deadline_at 基于 started_at 固定，不因 transcript delta 延长
//! - cursor 必须能处理文件截断/重写（size < cursor → reset）
//! - scanner 处理 running / maybe_idle observer；maybe_idle 必须继续等待 hard deadline
//! - scanner 只对 heartbeat stale 的 orphan job 写入终态，不 kill 活跃进程
//! - JobRunner 对正常 running job 拥有主控权（kill / heartbeat 更新）

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::constants::SCANNER_STALE_HEARTBEAT_MIN_SECS;
use agent_aspect_core::lifecycle::{
    CompletionAuthority, CompletionOutcome, CompletionSignal, CompletionSignalKind,
};
use agent_aspect_core::store::completion::CompletionObserverRow;
use agent_aspect_core::store::jobs::JobRow;
use agent_aspect_core::utils::truncate_str;

use sha2::{Digest, Sha256};

use crate::completion::CompletionSink;

/// scanner 主循环入口 — 在独立线程中运行。
///
/// 每次 tick：
/// 1. 查询 status in ('running', 'maybe_idle') 的 observers
/// 2. 对每个 observer 执行 deadline 检查 + transcript 增量扫描
/// 3. 根据结果更新 observer 状态
/// 4. 当 observer hard deadline 超时且 job heartbeat stale 时，写入 job 终态
///
/// 打开独立 DB 连接用于读查询；终态写入通过 CompletionSink（共享 store + SSE）。
pub fn start_scanner_loop(
    db_path: std::path::PathBuf,
    poll_interval_secs: u64,
    sink: CompletionSink,
) {
    let store = match AuditStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[aspect-scanner] failed to open DB: {e}");
            return;
        }
    };

    let interval = Duration::from_secs(poll_interval_secs.max(1));
    let stale_threshold_secs = compute_stale_threshold(poll_interval_secs);
    eprintln!(
        "[aspect-scanner] started (poll_interval={}s, stale_threshold={}s)",
        poll_interval_secs, stale_threshold_secs
    );

    loop {
        std::thread::sleep(interval);

        let observers = match store.get_active_observers() {
            Ok(obs) => obs,
            Err(e) => {
                eprintln!("[aspect-scanner] query active observers failed: {e}");
                continue;
            }
        };

        if !observers.is_empty() {
            eprintln!(
                "[aspect-scanner] tick: {} active observers",
                observers.len()
            );
        }

        for observer in &observers {
            if let Err(e) = process_observer(&store, observer, &sink, stale_threshold_secs) {
                eprintln!("[aspect-scanner] observer {} error: {e}", observer.id);
            }
        }
    }
}

/// 增量扫描单个 observer — 检查 deadline + 读 transcript。
///
/// 处理顺序（优先级从高到低）：
/// 1. hard_deadline_at 已超 → TimedOut（Authoritative 终态）
///    → 若关联 job heartbeat stale，scanner 收口写入 job 终态
/// 2. idle_deadline_at 已超 → MaybeIdle（Inferred 中间态）
/// 3. transcript 有增量 → 更新 cursor + 刷新 last_activity_at
/// 4. 无 delta 且未超 idle → 静默（不动 DB）
fn process_observer(
    store: &AuditStore,
    observer: &CompletionObserverRow,
    sink: &CompletionSink,
    stale_threshold_secs: i64,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let now_ts = parse_iso8601(&now).ok_or("failed to parse current time")?;

    // 1. hard deadline 检查（最高优先级）
    if let Some(hard_ts) = parse_iso8601(&observer.hard_deadline_at) {
        if now_ts >= hard_ts {
            let reason = "[aspect-timeout] hard deadline exceeded";
            store
                .mark_observer_timed_out(&observer.id, reason, &now)
                .map_err(|e| format!("mark_timed_out: {e}"))?;
            eprintln!(
                "[aspect-scanner] observer {} → timed_out (hard deadline)",
                observer.id
            );

            try_close_orphan_job(store, observer, sink, &now, now_ts, stale_threshold_secs);
            return Ok(());
        }
    }

    // 2. idle deadline 检查
    let over_idle = parse_iso8601(&observer.idle_deadline_at)
        .map(|idle_ts| now_ts >= idle_ts)
        .unwrap_or(false);

    // 3. transcript 增量扫描
    let scan_result = scan_transcript(observer);

    match scan_result {
        ScanResult::NoTranscriptPath => {
            if over_idle {
                store
                    .mark_observer_idle(&observer.id, &now)
                    .map_err(|e| format!("mark_idle: {e}"))?;
                eprintln!(
                    "[aspect-scanner] observer {} → maybe_idle (no transcript path, deadline fallback)",
                    observer.id
                );
            }
        }
        ScanResult::NoDelta => {
            if over_idle {
                store
                    .mark_observer_idle(&observer.id, &now)
                    .map_err(|e| format!("mark_idle: {e}"))?;
                eprintln!("[aspect-scanner] observer {} → maybe_idle", observer.id);
            }
        }
        ScanResult::Truncated {
            new_offset,
            last_line_no,
            last_line_hash,
            last_line_preview,
        } => {
            // 文件被截断/重写 → cursor 归零，刷新 activity
            store
                .update_observer_cursor(
                    &observer.id,
                    new_offset,
                    last_line_no,
                    last_line_hash.as_deref(),
                    last_line_preview.as_deref(),
                    None,
                    Some(new_offset),
                    &now,
                    &now,
                )
                .map_err(|e| format!("update_cursor (truncated): {e}"))?;
            eprintln!(
                "[aspect-scanner] observer {} → truncated reset, cursor={}",
                observer.id, new_offset
            );
        }
        ScanResult::Delta {
            new_offset,
            last_line_no,
            last_line_hash,
            last_line_preview,
            file_size,
        } => {
            // 有增量 → 更新 cursor + 刷新 last_activity_at
            store
                .update_observer_cursor(
                    &observer.id,
                    new_offset,
                    last_line_no,
                    last_line_hash.as_deref(),
                    last_line_preview.as_deref(),
                    None,
                    Some(file_size),
                    &now,
                    &now,
                )
                .map_err(|e| format!("update_cursor: {e}"))?;
            eprintln!(
                "[aspect-scanner] observer {} → delta, cursor={} line={}",
                observer.id, new_offset, last_line_no
            );
        }
    }

    Ok(())
}

/// 增量扫描 transcript 的结果。
enum ScanResult {
    /// observer 无 transcript_path 字段。
    NoTranscriptPath,
    /// 文件存在但无新增内容（size == cursor 或文件不存在）。
    NoDelta,
    /// 文件被截断/重写（size < cursor），cursor 需归零。
    Truncated {
        new_offset: i64,
        last_line_no: i64,
        last_line_hash: Option<String>,
        last_line_preview: Option<String>,
    },
    /// 有新内容（size > cursor），返回新的 cursor 位置和最后一行信息。
    Delta {
        new_offset: i64,
        last_line_no: i64,
        last_line_hash: Option<String>,
        last_line_preview: Option<String>,
        file_size: i64,
    },
}

/// 增量读取 transcript — 从 cursor_byte_offset 开始读新 bytes。
///
/// 1. stat 文件 → 不存在则 NoDelta
/// 2. size < cursor → 文件被重写/截断 → cursor 归零（Truncated）
/// 3. size == cursor → 无新增 → NoDelta
/// 4. size > cursor → 读新增部分，提取最后一行 → Delta
///
/// 只统计行数和 hash，不解析 JSONL 内容。
fn scan_transcript(observer: &CompletionObserverRow) -> ScanResult {
    let path_str = match observer.transcript_path.as_deref() {
        Some(p) => p,
        None => return ScanResult::NoTranscriptPath,
    };
    let path = std::path::Path::new(path_str);

    // 1. stat 文件
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return ScanResult::NoDelta,
    };
    let file_size = metadata.len() as i64;

    // 2. 文件截断检测
    if file_size < observer.cursor_byte_offset {
        return ScanResult::Truncated {
            new_offset: 0,
            last_line_no: 0,
            last_line_hash: None,
            last_line_preview: None,
        };
    }

    // 3. 无新增
    if file_size == observer.cursor_byte_offset {
        return ScanResult::NoDelta;
    }

    // 4. 有增量 → 读取新增部分
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return ScanResult::NoDelta,
    };

    let bytes_to_read = (file_size - observer.cursor_byte_offset) as usize;
    file.seek(SeekFrom::Start(observer.cursor_byte_offset as u64))
        .ok();

    let to_read = bytes_to_read.min(READ_CHUNK_LIMIT);
    let mut chunk = vec![0u8; to_read];
    let n = match file.read(&mut chunk) {
        Ok(n) if n > 0 => n,
        _ => return ScanResult::NoDelta,
    };

    // 提取最后一行
    let content = String::from_utf8_lossy(&chunk[..n]);
    let (last_line_no, last_line_hash, last_line_preview) =
        extract_last_line(&content, observer.last_line_no);

    ScanResult::Delta {
        new_offset: file_size,
        last_line_no,
        last_line_hash,
        last_line_preview,
        file_size,
    }
}

/// 从增量内容中提取最后一行信息。
///
/// 返回 (新的总行数, 最后一行 SHA256 hash, 最后一行预览≤240字符)。
fn extract_last_line(content: &str, prev_line_no: i64) -> (i64, Option<String>, Option<String>) {
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return (prev_line_no, None, None);
    }

    let new_line_count = lines.len() as i64;
    let total_line_no = prev_line_no + new_line_count;

    let last_line = lines.last().unwrap();
    let hash = format!("{:x}", Sha256::digest(last_line.as_bytes()));
    let preview = truncate_str(last_line, LAST_LINE_PREVIEW_LEN);

    (total_line_no, Some(hash), Some(preview))
}

/// 解析 ISO 8601 字符串为 Unix 时间戳（秒），用于比较 deadline。
fn parse_iso8601(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp())
}

/// 计算 stale heartbeat 判定阈值：max(SCANNER_STALE_HEARTBEAT_MIN_SECS, poll_interval * 3)。
fn compute_stale_threshold(poll_interval_secs: u64) -> i64 {
    let from_poll = (poll_interval_secs as i64) * 3;
    SCANNER_STALE_HEARTBEAT_MIN_SECS.max(from_poll)
}

/// 判定 job 是否属于 orphan/stale — heartbeat 已过期，JobRunner 不再活跃。
fn is_job_stale(job: &JobRow, now_ts: i64, stale_threshold_secs: i64) -> bool {
    match job.heartbeat_at.as_deref() {
        Some(hb) => {
            let hb_ts = match parse_iso8601(hb) {
                Some(t) => t,
                None => return true,
            };
            now_ts - hb_ts > stale_threshold_secs
        }
        None => {
            // 无 heartbeat 但 job 已开始 — 用 started_at 判定
            job.started_at
                .as_deref()
                .and_then(parse_iso8601)
                .map_or(true, |started| now_ts - started > stale_threshold_secs)
        }
    }
}

/// 尝试关闭 observer 关联的 orphan job。
///
/// 仅当 job 仍是 active status（queued/running/observing）且 heartbeat stale 时，
/// 才通过 CompletionSink 写入 `scanner_timeout` 终态。
/// 不 kill 进程 — 那是 JobRunner 的职责。
fn try_close_orphan_job(
    store: &AuditStore,
    observer: &CompletionObserverRow,
    sink: &CompletionSink,
    now: &str,
    now_ts: i64,
    stale_threshold_secs: i64,
) {
    let job_id = match observer.job_id.as_deref() {
        Some(id) => id,
        None => return,
    };

    let job = match store.get_job(job_id) {
        Ok(Some(j)) => j,
        _ => return,
    };

    let is_active = matches!(job.status.as_str(), "queued" | "running" | "observing");
    if !is_active {
        return;
    }

    if !is_job_stale(&job, now_ts, stale_threshold_secs) {
        eprintln!(
            "[aspect-scanner] observer {} → job {} heartbeat fresh, deferring to JobRunner",
            observer.id, job_id
        );
        return;
    }

    let signal = CompletionSignal {
        kind: CompletionSignalKind::ScannerTimeout,
        authority: CompletionAuthority::Authoritative,
        outcome: CompletionOutcome::TimedOut,
        agent: agent_aspect_core::event::AgentId::ClaudeCode,
        job_id: Some(job_id.to_string()),
        workflow_id: None,
        workflow_step_id: None,
        conversation_id: None,
        reason: "[aspect-scanner] orphan job: hard deadline exceeded, heartbeat stale".to_string(),
        observed_at: now.to_string(),
    };

    match sink.apply(&signal) {
        Ok(()) => {
            eprintln!(
                "[aspect-scanner] observer {} → closed orphan job {} (scanner_timeout)",
                observer.id, job_id
            );
        }
        Err(e) => {
            eprintln!(
                "[aspect-scanner] observer {} → failed to close orphan job {}: {e}",
                observer.id, job_id
            );
        }
    }
}

/// 单次读取的最大字节数（1MB）— 防止单次扫描读取过多数据。
const READ_CHUNK_LIMIT: usize = 1024 * 1024;

/// 最后一行预览的最大字符数。
const LAST_LINE_PREVIEW_LEN: usize = 240;

#[cfg(test)]
mod tests {
    use super::*;
    use agent_aspect_core::audit::AuditStore;
    use agent_aspect_core::store::completion::CompletionObserverRow;
    use agent_aspect_core::store::jobs::JobRow;
    use std::sync::{Arc, Mutex};

    fn make_observer(job_id: &str) -> CompletionObserverRow {
        CompletionObserverRow {
            id: "obs-1".to_string(),
            job_id: Some(job_id.to_string()),
            workflow_id: None,
            workflow_step_id: None,
            conversation_id: None,
            agent: "claude_code".to_string(),
            transcript_path: None,
            file_fingerprint: None,
            cursor_byte_offset: 0,
            last_line_no: 0,
            last_line_hash: None,
            last_line_preview: None,
            last_observed_mtime: None,
            last_observed_size: None,
            started_at: "2026-05-10T00:00:00Z".to_string(),
            last_activity_at: "2026-05-10T00:00:00Z".to_string(),
            idle_deadline_at: "2026-05-10T00:01:00Z".to_string(),
            hard_deadline_at: "2026-05-10T00:10:00Z".to_string(),
            attempt: 1,
            max_attempts: 1,
            status: "running".to_string(),
            completion_signal: None,
            completion_authority: None,
            completion_reason: None,
            created_at: "2026-05-10T00:00:00Z".to_string(),
            updated_at: "2026-05-10T00:00:00Z".to_string(),
        }
    }

    fn make_store_and_sink() -> (AuditStore, CompletionSink, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "agent-aspect-scanner-test-{}.db",
            uuid::Uuid::now_v7()
        ));
        let store = AuditStore::open(&path).expect("open db");
        let sink_store = AuditStore::open(&path).expect("open sink db");
        let broadcaster = crate::sse::SseBroadcaster::shared();
        let sink = CompletionSink::new(Arc::new(Mutex::new(sink_store)), broadcaster);
        (store, sink, path)
    }

    fn setup_job_and_observer(
        store: &AuditStore,
        job_id: &str,
        job_status: &str,
        heartbeat_at: Option<&str>,
    ) {
        store
            .insert_job(
                job_id,
                "agent_prompt",
                "{}",
                "2026-05-10T00:00:00Z",
                Some("claude_code"),
                Some("/tmp/project"),
                None,
                None,
                None,
            )
            .expect("insert job");

        if job_status != "queued" {
            store
                .update_job_started_supervised(
                    job_id,
                    "2026-05-10T00:00:00Z",
                    Some(12345),
                    Some(12345),
                    Some("runner-1"),
                    Some(600),
                )
                .expect("start job");
        }

        if let Some(hb) = heartbeat_at {
            store
                .update_job_heartbeat(job_id, "runner-1", hb)
                .expect("update heartbeat");
        }

        store
            .create_completion_observer(
                "obs-1",
                Some(job_id),
                None,
                None,
                None,
                "claude_code",
                None,
                None,
                "2026-05-10T00:00:00Z",
                "2026-05-10T00:00:00Z",
                "2026-05-10T00:01:00Z",
                "2026-05-10T00:10:00Z",
                1,
                "2026-05-10T00:00:00Z",
                "2026-05-10T00:00:00Z",
            )
            .expect("insert observer");
    }

    fn make_job_row(heartbeat_at: Option<&str>, started_at: Option<&str>) -> JobRow {
        JobRow {
            id: "j1".to_string(),
            kind: "agent_prompt".to_string(),
            input: "{}".to_string(),
            status: "running".to_string(),
            created_at: "2026-05-10T00:00:00Z".to_string(),
            started_at: started_at.map(str::to_string),
            finished_at: None,
            exit_code: None,
            provider: Some("claude_code".to_string()),
            project_path: None,
            conversation_id: None,
            prompt: None,
            pid: Some(123),
            process_group_id: Some(123),
            runner_id: Some("r1".to_string()),
            heartbeat_at: heartbeat_at.map(str::to_string),
            timeout_secs: Some(600),
            failure_reason: None,
            last_log_at: None,
            completed_reason: None,
            stop_requested_at: None,
            workflow_id: None,
        }
    }

    #[test]
    fn is_job_stale_with_fresh_heartbeat() {
        let job = make_job_row(Some("2026-05-10T00:00:50Z"), Some("2026-05-10T00:00:00Z"));
        // now=00:01:00, heartbeat=00:00:50, threshold=30s → 10s < 30s → not stale
        let now_ts = parse_iso8601("2026-05-10T00:01:00Z").unwrap();
        assert!(!is_job_stale(&job, now_ts, 30));
    }

    #[test]
    fn is_job_stale_with_old_heartbeat() {
        let job = make_job_row(Some("2026-05-10T00:00:10Z"), Some("2026-05-10T00:00:00Z"));
        // now=00:01:00, heartbeat=00:00:10, threshold=30s → 50s > 30s → stale
        let now_ts = parse_iso8601("2026-05-10T00:01:00Z").unwrap();
        assert!(is_job_stale(&job, now_ts, 30));
    }

    #[test]
    fn is_job_stale_no_heartbeat_with_old_started() {
        let job = make_job_row(None, Some("2026-05-10T00:00:00Z"));
        // now=00:01:00, started=00:00:00, threshold=30s → 60s > 30s → stale
        let now_ts = parse_iso8601("2026-05-10T00:01:00Z").unwrap();
        assert!(is_job_stale(&job, now_ts, 30));
    }

    #[test]
    fn is_job_stale_no_heartbeat_with_recent_started() {
        let job = make_job_row(None, Some("2026-05-10T00:00:50Z"));
        // now=00:01:00, started=00:00:50, threshold=30s → 10s < 30s → not stale
        let now_ts = parse_iso8601("2026-05-10T00:01:00Z").unwrap();
        assert!(!is_job_stale(&job, now_ts, 30));
    }

    #[test]
    fn compute_stale_threshold_uses_minimum() {
        // poll_interval=5 → 5*3=15 → max(30,15) = 30
        assert_eq!(compute_stale_threshold(5), 30);
    }

    #[test]
    fn compute_stale_threshold_uses_poll_based() {
        // poll_interval=30 → 30*3=90 → max(30,90) = 90
        assert_eq!(compute_stale_threshold(30), 90);
    }

    #[test]
    fn transcript_no_delta_marks_maybe_idle_before_hard_deadline() {
        let (store, sink, _path) = make_store_and_sink();
        let now = chrono::Utc::now();
        let started_at = (now - chrono::Duration::seconds(10)).to_rfc3339();
        let idle_deadline_at = (now - chrono::Duration::seconds(1)).to_rfc3339();
        let hard_deadline_at = (now + chrono::Duration::seconds(60)).to_rfc3339();
        store
            .create_completion_observer(
                "obs-idle",
                None,
                None,
                None,
                None,
                "claude_code",
                None,
                None,
                &started_at,
                &started_at,
                &idle_deadline_at,
                &hard_deadline_at,
                1,
                &started_at,
                &started_at,
            )
            .expect("insert observer");

        let observer = store
            .get_active_observers()
            .expect("active observers")
            .into_iter()
            .find(|o| o.id == "obs-idle")
            .expect("observer exists");
        process_observer(&store, &observer, &sink, 30).expect("process observer");

        let updated = store
            .get_observer_by_job_id("missing-job")
            .expect("query by job should not fail");
        assert!(updated.is_none());
        let active = store.get_active_observers().expect("active observers");
        let row = active
            .iter()
            .find(|o| o.id == "obs-idle")
            .expect("maybe idle stays active");
        assert_eq!(row.status, "maybe_idle");
        assert_eq!(row.completion_signal.as_deref(), Some("ScannerIdle"));
    }

    #[test]
    fn fresh_heartbeat_prevents_job_closure() {
        let (store, sink, _path) = make_store_and_sink();

        // Job with fresh heartbeat (5 seconds ago)
        setup_job_and_observer(&store, "job-1", "running", Some("2026-05-10T00:00:55Z"));

        let observer = make_observer("job-1");
        let now = "2026-05-10T00:01:00Z";
        let now_ts = parse_iso8601(now).unwrap();

        try_close_orphan_job(&store, &observer, &sink, now, now_ts, 30);

        let job = store
            .get_job("job-1")
            .expect("get job")
            .expect("job exists");
        assert_eq!(job.status, "running");
        assert!(job.completed_reason.is_none());
    }

    #[test]
    fn stale_heartbeat_triggers_job_closure() {
        let (store, sink, _path) = make_store_and_sink();

        // Job with stale heartbeat (60 seconds ago)
        setup_job_and_observer(&store, "job-2", "running", Some("2026-05-10T00:00:00Z"));

        let observer = make_observer("job-2");
        let now = "2026-05-10T00:01:00Z";
        let now_ts = parse_iso8601(now).unwrap();

        try_close_orphan_job(&store, &observer, &sink, now, now_ts, 30);

        let job = store
            .get_job("job-2")
            .expect("get job")
            .expect("job exists");
        assert_eq!(job.status, "failed");
        assert_eq!(job.completed_reason.as_deref(), Some("scanner_timeout"));
        assert!(job.failure_reason.is_some());
    }

    #[test]
    fn already_finished_job_is_not_closed() {
        let (store, sink, _path) = make_store_and_sink();

        setup_job_and_observer(&store, "job-3", "running", None);

        // Manually finish the job first
        store
            .update_job_finished_with_completed_reason(
                "job-3",
                "succeeded",
                "2026-05-10T00:00:30Z",
                Some(0),
                None,
                Some("stop_hook"),
            )
            .expect("finish job");

        let observer = make_observer("job-3");
        let now = "2026-05-10T00:01:00Z";
        let now_ts = parse_iso8601(now).unwrap();

        try_close_orphan_job(&store, &observer, &sink, now, now_ts, 30);

        let job = store
            .get_job("job-3")
            .expect("get job")
            .expect("job exists");
        assert_eq!(job.status, "succeeded");
        assert_eq!(job.completed_reason.as_deref(), Some("stop_hook"));
    }

    #[test]
    fn observer_without_job_id_is_skipped() {
        let (store, sink, _path) = make_store_and_sink();

        let mut observer = make_observer("nonexistent");
        observer.job_id = None;

        let now = "2026-05-10T00:01:00Z";
        let now_ts = parse_iso8601(now).unwrap();

        // Should not panic or fail
        try_close_orphan_job(&store, &observer, &sink, now, now_ts, 30);
    }
}
