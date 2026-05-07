//! 决策 DAO — 决策记录的插入、查询、过滤、pending asks。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};

/// 决策行 — 关联 event、device 信息后的扁平视图。
#[derive(Debug)]
pub struct DecisionRow {
    pub event_id: String,
    pub action: String,
    pub rule_id: Option<String>,
    pub note: String,
    pub timestamp: String,
    pub tool_name: String,
    pub file_path: Option<String>,
    pub agent: String,
    pub phase: String,
    pub raw_payload: Option<String>,
    pub device_id: Option<String>,
    pub device_label: Option<String>,
}

/// 动态 WHERE 子句构建器 — 链式拼接过滤条件。
struct FilterClauses {
    sql: String,
    params: Vec<Box<dyn rusqlite::types::ToSql>>,
}

impl FilterClauses {
    fn new() -> Self {
        FilterClauses {
            sql: String::from(" WHERE 1=1"),
            params: Vec::new(),
        }
    }

    fn apply(
        &mut self,
        action_filter: Option<&str>,
        tool_filter: Option<&str>,
        since: Option<&str>,
        agent_filter: Option<&str>,
    ) {
        if let Some(a) = action_filter {
            self.sql.push_str(" AND d.action = ?");
            self.params.push(Box::new(a.to_string()));
        }
        if let Some(t) = tool_filter {
            self.sql.push_str(" AND e.tool_name = ?");
            self.params.push(Box::new(t.to_string()));
        }
        if let Some(s) = since {
            self.sql.push_str(" AND d.timestamp >= ?");
            self.params.push(Box::new(s.to_string()));
        }
        if let Some(a) = agent_filter {
            self.sql.push_str(" AND e.agent = ?");
            self.params.push(Box::new(a.to_string()));
        }
    }

    fn apply_latest_only(&mut self) {
        self.sql
            .push_str(" AND d.id IN (SELECT MAX(id) FROM decisions GROUP BY event_id)");
    }
}

impl AuditStore {
    pub(crate) const DECISION_COLUMNS: &'static str = "d.event_id, d.action, d.rule_id, d.note, d.timestamp, \
         e.tool_name, e.file_path, e.agent, e.phase, e.raw_payload, d.device_id, dv.label";

    pub(crate) fn map_decision_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DecisionRow> {
        Ok(DecisionRow {
            event_id: row.get(0)?,
            action: row.get(1)?,
            rule_id: row.get(2)?,
            note: row.get(3)?,
            timestamp: row.get(4)?,
            tool_name: row.get(5).unwrap_or_default(),
            file_path: row.get(6).unwrap_or_default(),
            agent: row.get(7).unwrap_or_default(),
            phase: row.get(8).unwrap_or_default(),
            raw_payload: row.get(9).unwrap_or_default(),
            device_id: row.get(10).unwrap_or_default(),
            device_label: row.get(11).unwrap_or_default(),
        })
    }

    /// 插入决策记录（无 device_id）。
    pub fn insert_decision(
        &self,
        event_id: &str,
        action: &str,
        rule_id: Option<&str>,
        note: &str,
        timestamp: &str,
    ) -> AgentAspectResult<()> {
        self.insert_decision_for_device(event_id, action, rule_id, note, timestamp, None)
    }

    /// 插入决策记录并同步更新所属会话的 ask/deny 计数。
    /// 当 action 为 ask 或 deny 时，自动查找对应会话并增量更新。
    pub fn insert_decision_for_device(
        &self,
        event_id: &str,
        action: &str,
        rule_id: Option<&str>,
        note: &str,
        timestamp: &str,
        device_id: Option<&str>,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "INSERT INTO decisions (event_id, action, rule_id, note, timestamp, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![event_id, action, rule_id, note, timestamp, device_id],
            )
            .map_err(AgentAspectError::InsertDecision)?;

        // Update conversation counts if this event belongs to a conversation
        if action == "ask" || action == "deny" {
            if let Ok(Some(conv_id)) = self.conn.query_row(
                "SELECT conversation_id, agent FROM events WHERE id = ?1",
                rusqlite::params![event_id],
                |row| {
                    let conv_id: Option<String> = row.get(0)?;
                    let agent: String = row.get(1)?;
                    Ok(conv_id.map(|c| (c, agent)))
                },
            ) {
                let (cid, agent) = conv_id;
                let db_id = crate::conversation::conversation_db_id(&agent, &cid);
                let ask_delta = if action == "ask" { 1 } else { 0 };
                let deny_delta = if action == "deny" { 1 } else { 0 };
                if let Err(e) = self.update_conversation_counts(&db_id, 0, ask_delta, deny_delta, 0)
                {
                    eprintln!("agent-aspect-audit: update conversation counts for {db_id}: {e}");
                }
            }
        }

        Ok(())
    }

    pub fn decision_count(&self) -> AgentAspectResult<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM decisions", [], |row| {
                row.get::<_, usize>(0)
            })
            .map_err(AgentAspectError::CountDecisions)
    }

    pub fn decision_count_since(&self, since: &str) -> AgentAspectResult<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM decisions WHERE timestamp > ?1",
                rusqlite::params![since],
                |row| row.get::<_, usize>(0),
            )
            .map_err(AgentAspectError::CountDecisions)
    }

    pub fn recent_decisions(&self, limit: usize) -> AgentAspectResult<Vec<DecisionRow>> {
        let sql = format!(
            "SELECT {} FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id
             LEFT JOIN devices dv ON d.device_id = dv.device_id
             ORDER BY d.timestamp DESC
             LIMIT ?1",
            Self::DECISION_COLUMNS
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::PrepareRecentDecisions)?;

        let rows = stmt
            .query_map(rusqlite::params![limit], Self::map_decision_row)
            .map_err(AgentAspectError::QueryRecentDecisions)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    /// 带多维度过滤的决策查询：action / tool / since / agent / verdict / latest_only。
    pub fn query_decisions(
        &self,
        limit: usize,
        offset: usize,
        action_filter: Option<&str>,
        tool_filter: Option<&str>,
        since: Option<&str>,
        agent_filter: Option<&str>,
        verdict_filter: Option<&str>,
        latest_only: bool,
    ) -> AgentAspectResult<Vec<DecisionRow>> {
        let mut filter = FilterClauses::new();
        filter.apply(action_filter, tool_filter, since, agent_filter);
        if latest_only {
            filter.apply_latest_only();
        }

        let mut sql = format!(
            "SELECT {} FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id
             LEFT JOIN devices dv ON d.device_id = dv.device_id",
            Self::DECISION_COLUMNS
        );
        sql.push_str(&filter.sql);

        // Verdict filter requires joining event_feedback
        let mut verdict_param: Option<Box<dyn rusqlite::types::ToSql>> = None;
        if let Some(v) = verdict_filter {
            if v == "unlabeled" {
                sql.push_str(" AND d.event_id NOT IN (SELECT event_id FROM event_feedback)");
            } else {
                sql.push_str(
                    " AND d.event_id IN (SELECT event_id FROM event_feedback WHERE verdict = ?)",
                );
                verdict_param = Some(Box::new(v.to_string()));
            }
        }

        sql.push_str(" ORDER BY d.timestamp DESC LIMIT ? OFFSET ?");

        let mut params = filter.params;
        if let Some(vp) = verdict_param {
            params.push(vp);
        }
        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;

        let rows = stmt
            .query_map(param_refs.as_slice(), Self::map_decision_row)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    pub fn count_decisions_filtered(
        &self,
        action_filter: Option<&str>,
        tool_filter: Option<&str>,
        since: Option<&str>,
        agent_filter: Option<&str>,
        verdict_filter: Option<&str>,
        latest_only: bool,
    ) -> AgentAspectResult<usize> {
        let mut filter = FilterClauses::new();
        filter.apply(action_filter, tool_filter, since, agent_filter);
        if latest_only {
            filter.apply_latest_only();
        }

        let mut sql = String::from(
            "SELECT COUNT(*) FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id",
        );
        sql.push_str(&filter.sql);

        let mut verdict_param: Option<Box<dyn rusqlite::types::ToSql>> = None;
        if let Some(v) = verdict_filter {
            if v == "unlabeled" {
                sql.push_str(" AND d.event_id NOT IN (SELECT event_id FROM event_feedback)");
            } else {
                sql.push_str(
                    " AND d.event_id IN (SELECT event_id FROM event_feedback WHERE verdict = ?)",
                );
                verdict_param = Some(Box::new(v.to_string()));
            }
        }

        let mut params = filter.params;
        if let Some(vp) = verdict_param {
            params.push(vp);
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        self.conn
            .query_row(&sql, param_refs.as_slice(), |row| row.get::<_, usize>(0))
            .map_err(AgentAspectError::CountFilteredDecisions)
    }

    pub fn get_decisions_for_events(
        &self,
        event_ids: &[String],
    ) -> AgentAspectResult<Vec<DecisionRow>> {
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<String> = event_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT {} FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id
             LEFT JOIN devices dv ON d.device_id = dv.device_id
             WHERE d.event_id IN ({})
             ORDER BY d.timestamp ASC",
            Self::DECISION_COLUMNS,
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
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::map_decision_row)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    /// Returns events where the latest decision is "ask" (pending user action).
    pub fn pending_asks(&self, limit: usize) -> AgentAspectResult<Vec<DecisionRow>> {
        let sql = format!(
            "SELECT {} FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id
             LEFT JOIN devices dv ON d.device_id = dv.device_id
             WHERE d.id IN (
                 SELECT MAX(id) FROM decisions GROUP BY event_id
             )
             AND d.action = 'ask'
             ORDER BY d.timestamp DESC
             LIMIT ?1",
            Self::DECISION_COLUMNS
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;

        let rows = stmt
            .query_map(rusqlite::params![limit], Self::map_decision_row)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    /// Returns the latest decision + feedback for a single event_id.
    pub fn get_decision_with_feedback(
        &self,
        event_id: &str,
    ) -> AgentAspectResult<Option<(DecisionRow, Option<crate::store::feedback::FeedbackRow>)>> {
        let decision = match self.latest_decision_for_event(event_id)? {
            Some(d) => d,
            None => return Ok(None),
        };
        let mut feedback = self.feedback_for_events(&[event_id.to_string()])?;
        Ok(Some((decision, feedback.remove(event_id))))
    }

    /// Returns the latest decision for a given event_id, or None if no decisions exist.
    pub fn latest_decision_for_event(
        &self,
        event_id: &str,
    ) -> AgentAspectResult<Option<DecisionRow>> {
        let sql = format!(
            "SELECT {} FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id
             LEFT JOIN devices dv ON d.device_id = dv.device_id
             WHERE d.event_id = ?1
             ORDER BY d.id DESC LIMIT 1",
            Self::DECISION_COLUMNS
        );
        self.conn
            .query_row(&sql, rusqlite::params![event_id], Self::map_decision_row)
            .map_err(AgentAspectError::QueryFilteredDecisions)
            .map(Some)
            .or_else(|e| {
                if let AgentAspectError::QueryFilteredDecisions(
                    rusqlite::Error::QueryReturnedNoRows,
                ) = e
                {
                    Ok(None)
                } else {
                    Err(e)
                }
            })
    }
}
