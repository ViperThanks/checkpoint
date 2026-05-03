//! 任务 DAO — agent-prompt 任务的完整生命周期管理。
//!
//! 任务状态机：queued → running → succeeded / failed / cancelled。
//! 支持进程监控（pid / heartbeat）和 bridge 重启后的 stale job 恢复。

use crate::audit::AuditStore;
use crate::error::{CheckpointError, CheckpointResult};

/// 任务行 — 对应 jobs 表所有列。
#[derive(Debug, Clone)]
pub struct JobRow {
    pub id: String,
    pub kind: String,
    pub input: String,
    pub status: String,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub provider: Option<String>,
    pub project_path: Option<String>,
    pub conversation_id: Option<String>,
    pub prompt: Option<String>,
    pub pid: Option<i64>,
    pub process_group_id: Option<i64>,
    pub runner_id: Option<String>,
    pub heartbeat_at: Option<String>,
    pub timeout_secs: Option<i64>,
    pub failure_reason: Option<String>,
    pub last_log_at: Option<String>,
    pub completed_reason: Option<String>,
    pub stop_requested_at: Option<String>,
}

/// 任务日志行 — stdout/stderr/system 流的逐块记录。
#[derive(Debug, Clone)]
pub struct JobLogRow {
    pub id: i64,
    pub job_id: String,
    pub stream: String,
    pub chunk: String,
    pub seq: i64,
    pub timestamp: String,
}

impl AuditStore {
    pub(crate) fn map_job_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
        Ok(JobRow {
            id: row.get(0)?,
            kind: row.get(1)?,
            input: row.get(2)?,
            status: row.get(3)?,
            created_at: row.get(4)?,
            started_at: row.get(5)?,
            finished_at: row.get(6)?,
            exit_code: row.get(7)?,
            provider: row.get(8)?,
            project_path: row.get(9)?,
            conversation_id: row.get(10)?,
            prompt: row.get(11)?,
            pid: row.get(12)?,
            process_group_id: row.get(13)?,
            runner_id: row.get(14)?,
            heartbeat_at: row.get(15)?,
            timeout_secs: row.get(16)?,
            failure_reason: row.get(17)?,
            last_log_at: row.get(18)?,
            completed_reason: row.get(19)?,
            stop_requested_at: row.get(20)?,
        })
    }

    pub(crate) fn map_job_log_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobLogRow> {
        Ok(JobLogRow {
            id: row.get(0)?,
            job_id: row.get(1)?,
            stream: row.get(2)?,
            chunk: row.get(3)?,
            seq: row.get(4)?,
            timestamp: row.get(5)?,
        })
    }

    pub fn insert_job(
        &self,
        id: &str,
        kind: &str,
        input: &str,
        created_at: &str,
        provider: Option<&str>,
        project_path: Option<&str>,
        conversation_id: Option<&str>,
        prompt: Option<&str>,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "INSERT INTO jobs (id, kind, input, status, created_at, provider, project_path, conversation_id, prompt) VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![id, kind, input, created_at, provider, project_path, conversation_id, prompt],
            )
            .map_err(CheckpointError::SubmitJob)?;
        Ok(())
    }

    /// 标记任务开始运行（无进程监控）。只更新 status='queued' 的行。
    /// 返回受影响行数（1 = 成功，0 = 状态已变更）。
    pub fn update_job_started(&self, id: &str, started_at: &str) -> CheckpointResult<usize> {
        self.update_job_started_supervised(id, started_at, None, None, None, None)
    }

    /// 带进程监控的任务启动：记录 pid / process_group_id / runner_id / timeout。
    /// 同样只更新 status='queued' 的行。
    pub fn update_job_started_supervised(
        &self,
        id: &str,
        started_at: &str,
        pid: Option<i64>,
        process_group_id: Option<i64>,
        runner_id: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> CheckpointResult<usize> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET
                     status = 'running',
                     started_at = ?1,
                     pid = ?3,
                     process_group_id = ?4,
                     runner_id = ?5,
                     heartbeat_at = ?1,
                     timeout_secs = ?6,
                     failure_reason = NULL
                 WHERE id = ?2 AND status = 'queued'",
                rusqlite::params![
                    started_at,
                    id,
                    pid,
                    process_group_id,
                    runner_id,
                    timeout_secs.map(|v| v as i64)
                ],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows)
    }

    pub fn update_job_finished(
        &self,
        id: &str,
        status: &str,
        finished_at: &str,
        exit_code: Option<i32>,
    ) -> CheckpointResult<()> {
        self.update_job_finished_with_reason(id, status, finished_at, exit_code, None)
    }

    pub fn update_job_finished_with_reason(
        &self,
        id: &str,
        status: &str,
        finished_at: &str,
        exit_code: Option<i32>,
        failure_reason: Option<&str>,
    ) -> CheckpointResult<()> {
        self.update_job_finished_with_completed_reason(
            id,
            status,
            finished_at,
            exit_code,
            failure_reason,
            None,
        )
    }

    /// 写入 job 终态，包含结构化 completed_reason（process_exit / timeout_killed / cancelled / bridge_restart）。
    pub fn update_job_finished_with_completed_reason(
        &self,
        id: &str,
        status: &str,
        finished_at: &str,
        exit_code: Option<i32>,
        failure_reason: Option<&str>,
        completed_reason: Option<&str>,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "UPDATE jobs SET
                     status = ?1,
                     finished_at = ?2,
                     exit_code = ?3,
                     failure_reason = COALESCE(?5, failure_reason),
                     completed_reason = COALESCE(?6, completed_reason),
                     heartbeat_at = ?2
                 WHERE id = ?4 AND status IN ('running', 'queued', 'observing')",
                rusqlite::params![
                    status,
                    finished_at,
                    exit_code,
                    id,
                    failure_reason,
                    completed_reason
                ],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(())
    }

    /// 设置任务的 conversation_id（仅当当前值为空时更新）。
    pub fn update_job_conversation_id(
        &self,
        id: &str,
        conversation_id: &str,
    ) -> CheckpointResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs
                 SET conversation_id = ?2
                 WHERE id = ?1
                   AND (conversation_id IS NULL OR conversation_id = '')",
                rusqlite::params![id, conversation_id],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows > 0)
    }

    pub fn cancel_job(&self, id: &str) -> CheckpointResult<bool> {
        self.cancel_job_with_reason(id, None)
    }

    pub fn cancel_job_with_reason(&self, id: &str, reason: Option<&str>) -> CheckpointResult<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET
                     status = 'cancelled',
                     finished_at = ?2,
                     failure_reason = COALESCE(?3, failure_reason),
                     completed_reason = 'cancelled',
                     heartbeat_at = ?2
                 WHERE id = ?1 AND status IN ('running', 'queued', 'observing')",
                rusqlite::params![id, now, reason],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows > 0)
    }

    pub fn update_job_heartbeat(
        &self,
        id: &str,
        runner_id: &str,
        timestamp: &str,
    ) -> CheckpointResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET heartbeat_at = ?3 WHERE id = ?1 AND runner_id = ?2 AND status IN ('running', 'observing')",
                rusqlite::params![id, runner_id, timestamp],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows > 0)
    }

    /// 将 job 状态改为 observing（soft timeout 后进入观察期）。
    /// 不设 finished_at，job 仍在进行中。
    pub fn update_job_observing(&self, id: &str, timestamp: &str) -> CheckpointResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET status = 'observing', heartbeat_at = ?2 WHERE id = ?1 AND status = 'running'",
                rusqlite::params![id, timestamp],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows > 0)
    }

    /// 将 job 从 observing 状态恢复为 running（观察到新活动）。
    pub fn update_job_observing_revert(&self, id: &str, timestamp: &str) -> CheckpointResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET status = 'running', heartbeat_at = ?2 WHERE id = ?1 AND status = 'observing'",
                rusqlite::params![id, timestamp],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows > 0)
    }

    pub fn update_job_process(
        &self,
        id: &str,
        runner_id: &str,
        pid: i64,
        process_group_id: i64,
        timestamp: &str,
    ) -> CheckpointResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET
                     pid = ?3,
                     process_group_id = ?4,
                     heartbeat_at = ?5
                 WHERE id = ?1 AND runner_id = ?2 AND status = 'running'",
                rusqlite::params![id, runner_id, pid, process_group_id, timestamp],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows > 0)
    }

    /// 恢复 stale 任务 — bridge 重启后，将不属于当前 runner 的 queued/running 任务标记为 failed。
    pub fn recover_stale_active_jobs(
        &self,
        runner_id: &str,
        timestamp: &str,
    ) -> CheckpointResult<usize> {
        let stale_jobs = self.list_active_jobs_not_owned_by(runner_id)?;
        for job in &stale_jobs {
            let reason = "bridge restarted before job completed";
            self.conn
                .execute(
                    "UPDATE jobs SET
                         status = 'failed',
                         finished_at = ?2,
                         failure_reason = ?3,
                         completed_reason = 'bridge_restart',
                         heartbeat_at = ?2
                     WHERE id = ?1 AND status IN ('queued', 'running', 'observing')",
                    rusqlite::params![job.id, timestamp, reason],
                )
                .map_err(CheckpointError::UpdateJob)?;
            let _ = self.insert_job_log(
                &job.id,
                "system",
                "[recovered stale job after bridge restart]",
                0,
                timestamp,
            );
        }
        Ok(stale_jobs.len())
    }

    pub fn list_active_jobs_not_owned_by(&self, runner_id: &str) -> CheckpointResult<Vec<JobRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, input, status, created_at, started_at, finished_at, exit_code,
                    provider, project_path, conversation_id, prompt, pid, process_group_id,
                    runner_id, heartbeat_at, timeout_secs, failure_reason, last_log_at,
                    completed_reason, stop_requested_at
             FROM jobs
             WHERE status IN ('queued', 'running', 'observing') AND (runner_id IS NULL OR runner_id != ?1)
             ORDER BY created_at ASC",
            )
            .map_err(CheckpointError::QueryJob)?;
        let rows = stmt
            .query_map(rusqlite::params![runner_id], Self::map_job_row)
            .map_err(CheckpointError::QueryJob)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryJob)
    }

    /// 追加一条任务日志并更新 jobs.last_log_at。
    pub fn insert_job_log(
        &self,
        job_id: &str,
        stream: &str,
        chunk: &str,
        seq: i64,
        timestamp: &str,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "INSERT INTO job_logs (job_id, stream, chunk, seq, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![job_id, stream, chunk, seq, timestamp],
            )
            .map_err(CheckpointError::JobLog)?;
        self.conn
            .execute(
                "UPDATE jobs SET last_log_at = ?2 WHERE id = ?1",
                rusqlite::params![job_id, timestamp],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(())
    }

    pub fn get_job(&self, id: &str) -> CheckpointResult<Option<JobRow>> {
        self.conn
            .query_row(
                "SELECT id, kind, input, status, created_at, started_at, finished_at, exit_code,
                        provider, project_path, conversation_id, prompt, pid, process_group_id,
                        runner_id, heartbeat_at, timeout_secs, failure_reason, last_log_at,
                        completed_reason, stop_requested_at
                 FROM jobs WHERE id = ?1",
                rusqlite::params![id],
                Self::map_job_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(CheckpointError::QueryJob(e)),
            })
    }

    pub fn list_jobs(
        &self,
        limit: usize,
        offset: usize,
        status_filter: Option<&str>,
    ) -> CheckpointResult<Vec<JobRow>> {
        let mut sql = String::from(
            "SELECT id, kind, input, status, created_at, started_at, finished_at, exit_code,
                    provider, project_path, conversation_id, prompt, pid, process_group_id,
                    runner_id, heartbeat_at, timeout_secs, failure_reason, last_log_at,
                    completed_reason, stop_requested_at FROM jobs",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(s) = status_filter {
            sql.push_str(" WHERE status = ?");
            params.push(Box::new(s.to_string()));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql).map_err(CheckpointError::QueryJob)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::map_job_row)
            .map_err(CheckpointError::QueryJob)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryJob)
    }

    pub fn get_job_logs(&self, job_id: &str) -> CheckpointResult<Vec<JobLogRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, job_id, stream, chunk, seq, timestamp FROM job_logs WHERE job_id = ?1 ORDER BY seq ASC",
            )
            .map_err(CheckpointError::QueryJob)?;
        let rows = stmt
            .query_map(rusqlite::params![job_id], Self::map_job_log_row)
            .map_err(CheckpointError::QueryJob)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryJob)
    }

    /// 增量日志查询 — 返回 id > after_id 的日志，用于 SSE 流式推送。
    /// 上限 500 条防止一次拉取过多。
    pub fn get_job_logs_after(
        &self,
        job_id: &str,
        after_id: i64,
        limit: usize,
    ) -> CheckpointResult<Vec<JobLogRow>> {
        let limit = limit.min(500) as i64;
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, job_id, stream, chunk, seq, timestamp
                 FROM job_logs
                 WHERE job_id = ?1 AND id > ?2
                 ORDER BY id ASC
                 LIMIT ?3",
            )
            .map_err(CheckpointError::QueryJob)?;
        let rows = stmt
            .query_map(
                rusqlite::params![job_id, after_id, limit],
                Self::map_job_log_row,
            )
            .map_err(CheckpointError::QueryJob)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryJob)
    }

    pub fn count_running_jobs(&self) -> CheckpointResult<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE status = 'running'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .map_err(CheckpointError::QueryJob)
    }

    pub fn count_active_jobs(&self) -> CheckpointResult<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE status IN ('queued', 'running', 'observing')",
                [],
                |row| row.get::<_, usize>(0),
            )
            .map_err(CheckpointError::QueryJob)
    }

    pub fn count_jobs(&self, status_filter: Option<&str>) -> CheckpointResult<usize> {
        match status_filter {
            Some(s) => self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM jobs WHERE status = ?1",
                    rusqlite::params![s],
                    |row| row.get::<_, usize>(0),
                )
                .map_err(CheckpointError::QueryJob),
            None => self
                .conn
                .query_row("SELECT COUNT(*) FROM jobs", [], |row| {
                    row.get::<_, usize>(0)
                })
                .map_err(CheckpointError::QueryJob),
        }
    }

    /// Set stop_requested_at on a running/observing job. Returns affected rows.
    pub fn set_stop_requested_at(&self, job_id: &str, timestamp: &str) -> CheckpointResult<usize> {
        let rows = self
            .conn
            .execute(
                "UPDATE jobs SET stop_requested_at = ?2
                 WHERE id = ?1 AND status IN ('running', 'observing')
                   AND stop_requested_at IS NULL",
                rusqlite::params![job_id, timestamp],
            )
            .map_err(CheckpointError::UpdateJob)?;
        Ok(rows)
    }

    /// Find a running job matching the stop signal by provider + conversation_id.
    pub fn find_running_job_by_conversation(
        &self,
        provider: &str,
        conversation_id: &str,
    ) -> CheckpointResult<Option<JobRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, input, status, created_at, started_at, finished_at, exit_code,
                        provider, project_path, conversation_id, prompt, pid, process_group_id,
                        runner_id, heartbeat_at, timeout_secs, failure_reason, last_log_at,
                        completed_reason, stop_requested_at
                 FROM jobs
                 WHERE status IN ('running', 'observing')
                   AND provider = ?1 AND conversation_id = ?2
                 ORDER BY started_at DESC LIMIT 1",
            )
            .map_err(CheckpointError::QueryJob)?;
        let mut rows = stmt
            .query_map(
                rusqlite::params![provider, conversation_id],
                Self::map_job_row,
            )
            .map_err(CheckpointError::QueryJob)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(CheckpointError::QueryJob)?)),
            None => Ok(None),
        }
    }

    /// Find a running job matching by provider + project_path (fallback when no conversation_id).
    pub fn find_running_job_by_project(
        &self,
        provider: &str,
        project_path: &str,
    ) -> CheckpointResult<Option<JobRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, input, status, created_at, started_at, finished_at, exit_code,
                        provider, project_path, conversation_id, prompt, pid, process_group_id,
                        runner_id, heartbeat_at, timeout_secs, failure_reason, last_log_at,
                        completed_reason, stop_requested_at
                 FROM jobs
                 WHERE status IN ('running', 'observing')
                   AND provider = ?1 AND project_path = ?2
                 ORDER BY started_at DESC LIMIT 1",
            )
            .map_err(CheckpointError::QueryJob)?;
        let mut rows = stmt
            .query_map(rusqlite::params![provider, project_path], Self::map_job_row)
            .map_err(CheckpointError::QueryJob)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(CheckpointError::QueryJob)?)),
            None => Ok(None),
        }
    }

    /// Get all running/observing jobs that have stop_requested_at set.
    /// Used by bridge tick to converge stopped jobs.
    pub fn get_jobs_with_stop_requested(&self) -> CheckpointResult<Vec<JobRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, kind, input, status, created_at, started_at, finished_at, exit_code,
                        provider, project_path, conversation_id, prompt, pid, process_group_id,
                        runner_id, heartbeat_at, timeout_secs, failure_reason, last_log_at,
                        completed_reason, stop_requested_at
                 FROM jobs
                 WHERE status IN ('running', 'observing')
                   AND stop_requested_at IS NOT NULL",
            )
            .map_err(CheckpointError::QueryJob)?;
        let rows = stmt
            .query_map([], Self::map_job_row)
            .map_err(CheckpointError::QueryJob)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryJob)
    }
}
