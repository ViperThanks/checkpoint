//! 工作流 DAO — 本地编排引擎的持久化层。
//!
//! workflows 表存储工作流定义，workflow_steps 存储每一步的执行参数和状态。
//! 状态机：draft → running → succeeded / failed / cancelled。
//! 每一步关联一个 job_id，通过 context_strategy 控制日志传递。

use crate::audit::AuditStore;
use crate::error::{CheckpointError, CheckpointResult};

/// 工作流行 — 对应 workflows 表所有列。
#[derive(Debug, Clone)]
pub struct WorkflowRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

/// 工作流步骤行 — 对应 workflow_steps 表所有列。
#[derive(Debug, Clone)]
pub struct WorkflowStepRow {
    pub id: String,
    pub workflow_id: String,
    pub step_order: i64,
    pub kind: String,
    pub provider: Option<String>,
    pub project_path: Option<String>,
    pub prompt: String,
    pub context_strategy: String,
    pub context_from_step: Option<i64>,
    pub status: String,
    pub job_id: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

impl AuditStore {
    pub(crate) fn map_workflow_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowRow> {
        Ok(WorkflowRow {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            status: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }

    pub(crate) fn map_workflow_step_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkflowStepRow> {
        Ok(WorkflowStepRow {
            id: row.get(0)?,
            workflow_id: row.get(1)?,
            step_order: row.get(2)?,
            kind: row.get(3)?,
            provider: row.get(4)?,
            project_path: row.get(5)?,
            prompt: row.get(6)?,
            context_strategy: row.get(7)?,
            context_from_step: row.get(8)?,
            status: row.get(9)?,
            job_id: row.get(10)?,
            created_at: row.get(11)?,
            finished_at: row.get(12)?,
        })
    }

    /// 插入新工作流。
    pub fn insert_workflow(
        &self,
        id: &str,
        name: &str,
        description: &str,
        created_at: &str,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "INSERT INTO workflows (id, name, description, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'draft', ?4, ?4)",
                rusqlite::params![id, name, description, created_at],
            )
            .map_err(CheckpointError::InsertWorkflow)?;
        Ok(())
    }

    /// 获取单个工作流。
    pub fn get_workflow(&self, id: &str) -> CheckpointResult<Option<WorkflowRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, description, status, created_at, updated_at FROM workflows WHERE id = ?1")
            .map_err(CheckpointError::QueryWorkflow)?;
        let mut rows = stmt
            .query_map(rusqlite::params![id], Self::map_workflow_row)
            .map_err(CheckpointError::QueryWorkflow)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(CheckpointError::QueryWorkflow)?)),
            None => Ok(None),
        }
    }

    /// 列出所有工作流，按创建时间倒序。
    pub fn list_workflows(&self, limit: i64, offset: i64) -> CheckpointResult<Vec<WorkflowRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, name, description, status, created_at, updated_at
                 FROM workflows ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
            )
            .map_err(CheckpointError::QueryWorkflow)?;
        let rows = stmt
            .query_map(rusqlite::params![limit, offset], Self::map_workflow_row)
            .map_err(CheckpointError::QueryWorkflow)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryWorkflow)
    }

    /// 更新工作流状态。允许从任何状态转移到新状态。
    pub fn update_workflow_status(
        &self,
        id: &str,
        status: &str,
        updated_at: &str,
    ) -> CheckpointResult<usize> {
        let rows = self
            .conn
            .execute(
                "UPDATE workflows SET status = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id, status, updated_at],
            )
            .map_err(CheckpointError::UpdateWorkflow)?;
        Ok(rows)
    }

    /// 更新工作流名称和描述。只允许 draft/failed/cancelled 状态的工作流。
    pub fn update_workflow(
        &self,
        id: &str,
        name: &str,
        description: &str,
        updated_at: &str,
    ) -> CheckpointResult<usize> {
        let rows = self
            .conn
            .execute(
                "UPDATE workflows SET name = ?2, description = ?3, updated_at = ?4
                 WHERE id = ?1 AND status IN ('draft', 'failed', 'cancelled')",
                rusqlite::params![id, name, description, updated_at],
            )
            .map_err(CheckpointError::UpdateWorkflow)?;
        Ok(rows)
    }

    /// 删除工作流及其所有步骤。只允许 draft/failed/cancelled 状态。
    /// 返回：Ok(true) = 已删除，Ok(false) = not found，Err = running。
    pub fn delete_workflow(&self, id: &str) -> CheckpointResult<bool> {
        let wf = self.get_workflow(id)?;
        match wf {
            Some(w) if w.status == "running" => Err(CheckpointError::WorkflowNotRunning),
            Some(_) => {
                let tx = self.conn.unchecked_transaction()
                    .map_err(CheckpointError::UpdateWorkflow)?;
                tx.execute(
                    "DELETE FROM workflow_steps WHERE workflow_id = ?1",
                    rusqlite::params![id],
                ).map_err(CheckpointError::UpdateWorkflowStep)?;
                tx.execute(
                    "DELETE FROM workflows WHERE id = ?1",
                    rusqlite::params![id],
                ).map_err(CheckpointError::UpdateWorkflow)?;
                tx.commit().map_err(CheckpointError::UpdateWorkflow)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 重新排序工作流步骤。step_orders 是 (step_id, new_order) 的列表。
    pub fn reorder_workflow_steps(
        &self,
        step_orders: &[(String, i64)],
        updated_at: &str,
    ) -> CheckpointResult<()> {
        let tx = self.conn.unchecked_transaction()
            .map_err(CheckpointError::UpdateWorkflowStep)?;
        for (step_id, new_order) in step_orders {
            tx.execute(
                "UPDATE workflow_steps SET step_order = ?2 WHERE id = ?1",
                rusqlite::params![step_id, new_order],
            ).map_err(CheckpointError::UpdateWorkflowStep)?;
        }
        tx.commit().map_err(CheckpointError::UpdateWorkflowStep)?;
        let _ = updated_at; // reserved for future use
        Ok(())
    }

    /// 插入工作流步骤。
    pub fn insert_workflow_step(
        &self,
        id: &str,
        workflow_id: &str,
        step_order: i64,
        kind: &str,
        provider: Option<&str>,
        project_path: Option<&str>,
        prompt: &str,
        context_strategy: &str,
        context_from_step: Option<i64>,
        created_at: &str,
    ) -> CheckpointResult<()> {
        self.conn
            .execute(
                "INSERT INTO workflow_steps
                 (id, workflow_id, step_order, kind, provider, project_path, prompt,
                  context_strategy, context_from_step, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10)",
                rusqlite::params![
                    id, workflow_id, step_order, kind, provider, project_path, prompt,
                    context_strategy, context_from_step, created_at
                ],
            )
            .map_err(CheckpointError::InsertWorkflowStep)?;
        Ok(())
    }

    /// 获取单个步骤。
    pub fn get_workflow_step(&self, id: &str) -> CheckpointResult<Option<WorkflowStepRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, workflow_id, step_order, kind, provider, project_path, prompt,
                        context_strategy, context_from_step, status, job_id, created_at, finished_at
                 FROM workflow_steps WHERE id = ?1",
            )
            .map_err(CheckpointError::QueryWorkflowStep)?;
        let mut rows = stmt
            .query_map(rusqlite::params![id], Self::map_workflow_step_row)
            .map_err(CheckpointError::QueryWorkflowStep)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(CheckpointError::QueryWorkflowStep)?)),
            None => Ok(None),
        }
    }

    /// 获取工作流的所有步骤，按 step_order 排序。
    pub fn get_workflow_steps(&self, workflow_id: &str) -> CheckpointResult<Vec<WorkflowStepRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, workflow_id, step_order, kind, provider, project_path, prompt,
                        context_strategy, context_from_step, status, job_id, created_at, finished_at
                 FROM workflow_steps WHERE workflow_id = ?1 ORDER BY step_order ASC",
            )
            .map_err(CheckpointError::QueryWorkflowStep)?;
        let rows = stmt
            .query_map(rusqlite::params![workflow_id], Self::map_workflow_step_row)
            .map_err(CheckpointError::QueryWorkflowStep)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::QueryWorkflowStep)
    }

    /// 更新步骤状态。
    pub fn update_workflow_step_status(
        &self,
        id: &str,
        status: &str,
        finished_at: Option<&str>,
    ) -> CheckpointResult<usize> {
        let rows = self
            .conn
            .execute(
                "UPDATE workflow_steps SET status = ?2, finished_at = COALESCE(?3, finished_at) WHERE id = ?1",
                rusqlite::params![id, status, finished_at],
            )
            .map_err(CheckpointError::UpdateWorkflowStep)?;
        Ok(rows)
    }

    /// 绑定步骤的 job_id（仅当当前值为空时更新）。
    pub fn update_workflow_step_job(
        &self,
        id: &str,
        job_id: &str,
    ) -> CheckpointResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE workflow_steps SET job_id = ?2 WHERE id = ?1 AND (job_id IS NULL OR job_id = '')",
                rusqlite::params![id, job_id],
            )
            .map_err(CheckpointError::UpdateWorkflowStep)?;
        Ok(rows > 0)
    }

    /// 获取工作流中指定 step_order 的步骤。
    pub fn get_workflow_step_by_order(
        &self,
        workflow_id: &str,
        step_order: i64,
    ) -> CheckpointResult<Option<WorkflowStepRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, workflow_id, step_order, kind, provider, project_path, prompt,
                        context_strategy, context_from_step, status, job_id, created_at, finished_at
                 FROM workflow_steps WHERE workflow_id = ?1 AND step_order = ?2",
            )
            .map_err(CheckpointError::QueryWorkflowStep)?;
        let mut rows = stmt
            .query_map(rusqlite::params![workflow_id, step_order], Self::map_workflow_step_row)
            .map_err(CheckpointError::QueryWorkflowStep)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(CheckpointError::QueryWorkflowStep)?)),
            None => Ok(None),
        }
    }

    /// 获取工作流当前待执行的下一步（第一个 pending 状态的步骤）。
    pub fn get_next_pending_step(&self, workflow_id: &str) -> CheckpointResult<Option<WorkflowStepRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, workflow_id, step_order, kind, provider, project_path, prompt,
                        context_strategy, context_from_step, status, job_id, created_at, finished_at
                 FROM workflow_steps WHERE workflow_id = ?1 AND status = 'pending'
                 ORDER BY step_order ASC LIMIT 1",
            )
            .map_err(CheckpointError::QueryWorkflowStep)?;
        let mut rows = stmt
            .query_map(rusqlite::params![workflow_id], Self::map_workflow_step_row)
            .map_err(CheckpointError::QueryWorkflowStep)?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(CheckpointError::QueryWorkflowStep)?)),
            None => Ok(None),
        }
    }

    /// 取消工作流中所有未完成的步骤。
    pub fn cancel_workflow_steps(&self, workflow_id: &str) -> CheckpointResult<usize> {
        let rows = self
            .conn
            .execute(
                "UPDATE workflow_steps SET status = 'cancelled'
                 WHERE workflow_id = ?1 AND status IN ('pending', 'running')",
                rusqlite::params![workflow_id],
            )
            .map_err(CheckpointError::UpdateWorkflowStep)?;
        Ok(rows)
    }

    /// 统计工作流中各状态的步骤数。(total, succeeded, failed, pending, skipped)
    pub fn workflow_step_counts(&self, workflow_id: &str) -> CheckpointResult<(i64, i64, i64, i64, i64)> {
        let (total, succeeded, failed, pending, skipped): (i64, i64, i64, i64, i64) = self.conn.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(CASE WHEN status = 'succeeded' THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status IN ('failed', 'cancelled') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status IN ('pending', 'running') THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'skipped' THEN 1 ELSE 0 END), 0)
             FROM workflow_steps WHERE workflow_id = ?1",
            rusqlite::params![workflow_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).map_err(CheckpointError::QueryWorkflowStep)?;
        Ok((total, succeeded, failed, pending, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_crud_lifecycle() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test Workflow", "A test workflow", now).unwrap();

        let wf = store.get_workflow("wf1").unwrap().unwrap();
        assert_eq!(wf.name, "Test Workflow");
        assert_eq!(wf.status, "draft");

        store.update_workflow_status("wf1", "running", now).unwrap();
        let wf = store.get_workflow("wf1").unwrap().unwrap();
        assert_eq!(wf.status, "running");
    }

    #[test]
    fn workflow_steps_crud() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        store.insert_workflow_step("s1", "wf1", 0, "agent_prompt", Some("claude_code"), Some("/tmp/proj"), "step 1", "none", None, now).unwrap();
        store.insert_workflow_step("s2", "wf1", 1, "agent_prompt", Some("claude_code"), Some("/tmp/proj"), "step 2", "last_50_lines", Some(0), now).unwrap();

        let steps = store.get_workflow_steps("wf1").unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].step_order, 0);
        assert_eq!(steps[1].step_order, 1);
        assert_eq!(steps[1].context_strategy, "last_50_lines");
        assert_eq!(steps[1].context_from_step, Some(0));
    }

    #[test]
    fn next_pending_step_returns_first_pending() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        store.insert_workflow_step("s1", "wf1", 0, "agent_prompt", None, None, "step 1", "none", None, now).unwrap();
        store.insert_workflow_step("s2", "wf1", 1, "agent_prompt", None, None, "step 2", "none", None, now).unwrap();

        let next = store.get_next_pending_step("wf1").unwrap().unwrap();
        assert_eq!(next.id, "s1");

        store.update_workflow_step_status("s1", "succeeded", Some(now)).unwrap();
        let next = store.get_next_pending_step("wf1").unwrap().unwrap();
        assert_eq!(next.id, "s2");

        store.update_workflow_step_status("s2", "succeeded", Some(now)).unwrap();
        let next = store.get_next_pending_step("wf1").unwrap();
        assert!(next.is_none());
    }

    #[test]
    fn cancel_workflow_steps_skips_completed() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        store.insert_workflow_step("s1", "wf1", 0, "agent_prompt", None, None, "step 1", "none", None, now).unwrap();
        store.insert_workflow_step("s2", "wf1", 1, "agent_prompt", None, None, "step 2", "none", None, now).unwrap();
        store.insert_workflow_step("s3", "wf1", 2, "agent_prompt", None, None, "step 3", "none", None, now).unwrap();

        store.update_workflow_step_status("s1", "succeeded", Some(now)).unwrap();
        let cancelled = store.cancel_workflow_steps("wf1").unwrap();
        assert_eq!(cancelled, 2);

        let s1 = store.get_workflow_step("s1").unwrap().unwrap();
        assert_eq!(s1.status, "succeeded");
        let s2 = store.get_workflow_step("s2").unwrap().unwrap();
        assert_eq!(s2.status, "cancelled");
        let s3 = store.get_workflow_step("s3").unwrap().unwrap();
        assert_eq!(s3.status, "cancelled");
    }

    #[test]
    fn step_counts() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        store.insert_workflow_step("s1", "wf1", 0, "agent_prompt", None, None, "p1", "none", None, now).unwrap();
        store.insert_workflow_step("s2", "wf1", 1, "agent_prompt", None, None, "p2", "none", None, now).unwrap();
        store.insert_workflow_step("s3", "wf1", 2, "agent_prompt", None, None, "p3", "none", None, now).unwrap();

        store.update_workflow_step_status("s1", "succeeded", Some(now)).unwrap();

        let (total, succeeded, failed, pending, skipped) = store.workflow_step_counts("wf1").unwrap();
        assert_eq!(total, 3);
        assert_eq!(succeeded, 1);
        assert_eq!(failed, 0);
        assert_eq!(pending, 2);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn step_counts_empty_workflow() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf-empty", "Empty", "", now).unwrap();
        let (total, succeeded, failed, pending, skipped) = store.workflow_step_counts("wf-empty").unwrap();
        assert_eq!(total, 0);
        assert_eq!(succeeded, 0);
        assert_eq!(failed, 0);
        assert_eq!(pending, 0);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn get_workflow_nonexistent() {
        let store = AuditStore::open_in_memory().unwrap();
        let result = store.get_workflow("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_workflow_step_nonexistent() {
        let store = AuditStore::open_in_memory().unwrap();
        let result = store.get_workflow_step("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_workflows_pagination() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        for i in 0..5 {
            store.insert_workflow(&format!("wf{i}"), &format!("WF {i}"), "", now).unwrap();
        }

        let page1 = store.list_workflows(2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = store.list_workflows(2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = store.list_workflows(2, 4).unwrap();
        assert_eq!(page3.len(), 1);

        let page4 = store.list_workflows(2, 6).unwrap();
        assert_eq!(page4.len(), 0);
    }

    #[test]
    fn update_workflow_step_job_only_when_empty() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        store.insert_workflow_step("s1", "wf1", 0, "agent_prompt", None, None, "p", "none", None, now).unwrap();

        // First bind succeeds
        assert!(store.update_workflow_step_job("s1", "job-1").unwrap());
        let step = store.get_workflow_step("s1").unwrap().unwrap();
        assert_eq!(step.job_id.as_deref(), Some("job-1"));

        // Second bind is no-op (already bound)
        assert!(!store.update_workflow_step_job("s1", "job-2").unwrap());
        let step = store.get_workflow_step("s1").unwrap().unwrap();
        assert_eq!(step.job_id.as_deref(), Some("job-1"));
    }

    #[test]
    fn cancel_workflow_steps_mixed_statuses() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        store.insert_workflow_step("s1", "wf1", 0, "agent_prompt", None, None, "p1", "none", None, now).unwrap();
        store.insert_workflow_step("s2", "wf1", 1, "agent_prompt", None, None, "p2", "none", None, now).unwrap();
        store.insert_workflow_step("s3", "wf1", 2, "agent_prompt", None, None, "p3", "none", None, now).unwrap();
        store.insert_workflow_step("s4", "wf1", 3, "agent_prompt", None, None, "p4", "none", None, now).unwrap();

        store.update_workflow_step_status("s1", "succeeded", Some(now)).unwrap();
        store.update_workflow_step_status("s2", "failed", Some(now)).unwrap();
        // s3, s4 are pending

        let cancelled = store.cancel_workflow_steps("wf1").unwrap();
        // Only pending and running are cancelled (s3, s4)
        assert_eq!(cancelled, 2);

        assert_eq!(store.get_workflow_step("s1").unwrap().unwrap().status, "succeeded");
        assert_eq!(store.get_workflow_step("s2").unwrap().unwrap().status, "failed");
        assert_eq!(store.get_workflow_step("s3").unwrap().unwrap().status, "cancelled");
        assert_eq!(store.get_workflow_step("s4").unwrap().unwrap().status, "cancelled");
    }

    #[test]
    fn get_next_pending_respects_step_order() {
        let store = AuditStore::open_in_memory().unwrap();
        let now = "2026-05-04T10:00:00Z";

        store.insert_workflow("wf1", "Test", "", now).unwrap();
        // Insert out of order
        store.insert_workflow_step("s2", "wf1", 2, "agent_prompt", None, None, "step 2", "none", None, now).unwrap();
        store.insert_workflow_step("s1", "wf1", 1, "agent_prompt", None, None, "step 1", "none", None, now).unwrap();
        store.insert_workflow_step("s0", "wf1", 0, "agent_prompt", None, None, "step 0", "none", None, now).unwrap();

        // Should return step_order=0 first
        let next = store.get_next_pending_step("wf1").unwrap().unwrap();
        assert_eq!(next.id, "s0");
        assert_eq!(next.step_order, 0);
    }
}
