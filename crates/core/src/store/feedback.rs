//! 反馈 DAO — 用户对事件裁决的反馈（useful/noisy/wrong/unsure）。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};
use std::collections::HashMap;

/// 反馈行 — 对应 event_feedback 表。每个 event_id 最多一条，INSERT OR REPLACE 语义。
#[derive(Debug, Clone)]
pub struct FeedbackRow {
    pub event_id: String,
    pub verdict: String,
    pub note: String,
    pub created_at: String,
}

impl AuditStore {
    pub fn insert_feedback(
        &self,
        event_id: &str,
        verdict: &str,
        note: &str,
        created_at: &str,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO event_feedback (event_id, verdict, note, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![event_id, verdict, note, created_at],
            )
            .map_err(AgentAspectError::InsertFeedback)?;
        Ok(())
    }

    pub fn feedback_for_events(
        &self,
        event_ids: &[String],
    ) -> AgentAspectResult<HashMap<String, FeedbackRow>> {
        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<String> = event_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT event_id, verdict, note, created_at FROM event_feedback WHERE event_id IN ({})",
            placeholders.join(",")
        );
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = event_ids
            .iter()
            .map(|id| Box::new(id.clone()) as _)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryFeedback)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(FeedbackRow {
                    event_id: row.get(0)?,
                    verdict: row.get(1)?,
                    note: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(AgentAspectError::QueryFeedback)?;

        let mut map = HashMap::new();
        for row in rows {
            let fb = row.map_err(AgentAspectError::QueryFeedback)?;
            map.insert(fb.event_id.clone(), fb);
        }
        Ok(map)
    }
}
