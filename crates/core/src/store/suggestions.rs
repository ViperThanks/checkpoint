//! 建议规则 DAO — 学习引擎生成的自动允许建议的 CRUD。
//!
//! 建议状态机：pending → accepted / rejected / expired。
//! accepted 的建议可被规则引擎查询为 learned allow。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};

/// 建议行 — 对应 suggestions 表。
#[derive(Debug, Clone)]
pub struct SuggestionRow {
    pub id: String,
    pub title: String,
    pub reason: String,
    pub confidence: f64,
    pub agent: String,
    pub tool_name: String,
    pub project_path: Option<String>,
    pub pattern: String,
    pub sample_event_ids: Vec<String>,
    pub sample_count: usize,
    pub suggested_action: String,
    pub status: String,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

impl AuditStore {
    pub fn insert_suggestion(&self, s: &SuggestionRow) -> AgentAspectResult<()> {
        let sample_json =
            serde_json::to_string(&s.sample_event_ids).unwrap_or_else(|_| "[]".to_string());
        self.conn
            .execute(
                "INSERT OR IGNORE INTO suggestions
                 (id, title, reason, confidence, agent, tool_name, project_path, pattern,
                  sample_event_ids, sample_count, suggested_action, status, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                rusqlite::params![
                    s.id,
                    s.title,
                    s.reason,
                    s.confidence,
                    s.agent,
                    s.tool_name,
                    s.project_path,
                    s.pattern,
                    sample_json,
                    s.sample_count as i64,
                    s.suggested_action,
                    s.status,
                    s.created_at,
                    s.resolved_at,
                ],
            )
            .map_err(AgentAspectError::InsertSuggestion)?;
        Ok(())
    }

    pub fn suggestion_exists(
        &self,
        agent: &str,
        tool_name: &str,
        pattern: &str,
    ) -> AgentAspectResult<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM suggestions WHERE agent = ?1 AND tool_name = ?2 AND pattern = ?3 LIMIT 1",
                rusqlite::params![agent, tool_name, pattern],
                |row| row.get::<_, i64>(0),
            )
            .map(|_| true)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                _ => Err(AgentAspectError::QuerySuggestion(e)),
            })
    }

    pub fn latest_suggestion_created_at(&self) -> AgentAspectResult<Option<String>> {
        self.conn
            .query_row("SELECT MAX(created_at) FROM suggestions", [], |row| {
                row.get(0)
            })
            .map_err(AgentAspectError::QuerySuggestion)
    }

    pub fn list_pending_suggestions(&self, limit: usize) -> AgentAspectResult<Vec<SuggestionRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, reason, confidence, agent, tool_name, project_path, pattern,
                        sample_event_ids, sample_count, suggested_action, status, created_at, resolved_at
                 FROM suggestions WHERE status = 'pending'
                 ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(AgentAspectError::QuerySuggestion)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], Self::map_suggestion_row)
            .map_err(AgentAspectError::QuerySuggestion)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QuerySuggestion)
    }

    /// 查询是否存在已接受的 learned allow 规则匹配 (agent, tool_name)。
    /// 用于规则引擎评估时快速跳过已知安全模式。
    pub fn has_learned_allow(&self, agent: &str, tool_name: &str) -> AgentAspectResult<bool> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM suggestions
                 WHERE status = 'accepted' AND suggested_action = 'allow'
                   AND agent = ?1 AND tool_name = ?2",
                rusqlite::params![agent, tool_name],
                |row| row.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .map_err(AgentAspectError::QuerySuggestion)
    }

    pub fn list_accepted_suggestions(&self) -> AgentAspectResult<Vec<SuggestionRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, title, reason, confidence, agent, tool_name, project_path, pattern,
                        sample_event_ids, sample_count, suggested_action, status, created_at, resolved_at
                 FROM suggestions WHERE status = 'accepted'
                 ORDER BY resolved_at DESC",
            )
            .map_err(AgentAspectError::QuerySuggestion)?;
        let rows = stmt
            .query_map([], Self::map_suggestion_row)
            .map_err(AgentAspectError::QuerySuggestion)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QuerySuggestion)
    }

    pub fn get_suggestion(&self, id: &str) -> AgentAspectResult<Option<SuggestionRow>> {
        self.conn
            .query_row(
                "SELECT id, title, reason, confidence, agent, tool_name, project_path, pattern,
                        sample_event_ids, sample_count, suggested_action, status, created_at, resolved_at
                 FROM suggestions WHERE id = ?1",
                rusqlite::params![id],
                Self::map_suggestion_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(AgentAspectError::QuerySuggestion(e)),
            })
    }

    pub fn update_suggestion_status(
        &self,
        id: &str,
        status: &str,
        resolved_at: &str,
    ) -> AgentAspectResult<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE suggestions SET status = ?2, resolved_at = ?3 WHERE id = ?1",
                rusqlite::params![id, status, resolved_at],
            )
            .map_err(AgentAspectError::UpdateSuggestion)?;
        Ok(rows > 0)
    }

    pub(crate) fn map_suggestion_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SuggestionRow> {
        let sample_json: String = row.get(8)?;
        let sample_ids: Vec<String> = serde_json::from_str(&sample_json).unwrap_or_default();
        Ok(SuggestionRow {
            id: row.get(0)?,
            title: row.get(1)?,
            reason: row.get(2)?,
            confidence: row.get(3)?,
            agent: row.get(4)?,
            tool_name: row.get(5)?,
            project_path: row.get(6)?,
            pattern: row.get(7)?,
            sample_event_ids: sample_ids,
            sample_count: row.get::<_, i64>(9)? as usize,
            suggested_action: row.get(10)?,
            status: row.get(11)?,
            created_at: row.get(12)?,
            resolved_at: row.get(13)?,
        })
    }
}
