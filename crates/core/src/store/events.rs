//! 事件 DAO — 工具使用事件的插入、查询、批量获取会话信息、过期清理。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};

/// 事件行 — 对应 events 表。
#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: String,
    pub phase: String,
    pub event_type: String,
    pub agent: String,
    pub tool_name: String,
    pub file_path: Option<String>,
    pub timestamp: String,
    pub raw_payload: Option<String>,
}

impl AuditStore {
    /// 插入事件并自动：
    /// 1. 从 raw_payload 提取 conversation_id / project_path
    /// 2. upsert 对应会话记录
    /// 3. 增量更新会话 event_count
    pub fn insert_event(
        &self,
        id: &str,
        phase: &str,
        event_type: &str,
        agent: &str,
        tool_name: &str,
        file_path: Option<&str>,
        timestamp: &str,
        raw_payload: &str,
    ) -> AgentAspectResult<()> {
        use crate::conversation;

        let conversation_id = conversation::extract_conversation_id(agent, raw_payload);
        let project_path = conversation::extract_project_path(agent, raw_payload).or_else(|| {
            file_path.and_then(|p| {
                std::path::Path::new(p)
                    .parent()
                    .and_then(|parent| parent.to_str())
                    .map(|s| s.to_string())
            })
        });
        let transcript_path = conversation::extract_transcript_path(agent, raw_payload);

        self.conn
            .execute(
                "INSERT INTO events (id, phase, type, agent, tool_name, file_path, timestamp, raw_payload, conversation_id, project_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![id, phase, event_type, agent, tool_name, file_path, timestamp, raw_payload, conversation_id.as_ref(), project_path.as_ref()],
            )
            .map_err(AgentAspectError::InsertEvent)?;

        // Update conversation index if conversation_id is present
        if let Some(ref cid) = conversation_id {
            let db_id = conversation::conversation_db_id(agent, cid);
            let title =
                conversation::generate_title(agent, project_path.as_deref(), Some(raw_payload));
            self.upsert_conversation(
                &db_id,
                agent,
                cid,
                &title,
                project_path.as_deref(),
                timestamp,
                timestamp,
                transcript_path.as_deref(),
            )?;
            if let Err(e) = self.update_conversation_counts(&db_id, 1, 0, 0, 0) {
                eprintln!("agent-aspect-audit: update conversation counts for {db_id}: {e}");
            }
            if let Some(permission_mode) = conversation::extract_permission_mode(raw_payload) {
                let _ = self.update_runtime_permission_mode(&db_id, &permission_mode, Some(agent));
            }
        }

        Ok(())
    }

    pub fn event_count(&self) -> AgentAspectResult<usize> {
        self.conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| {
                row.get::<_, usize>(0)
            })
            .map_err(AgentAspectError::CountEvents)
    }

    pub fn event_exists(&self, event_id: &str) -> AgentAspectResult<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM events WHERE id = ?1 LIMIT 1",
                rusqlite::params![event_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|_| true)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                _ => Err(AgentAspectError::QueryFeedback(e)),
            })
    }

    /// 批量获取多个 event_id 的会话信息（用于 API 一次返回）。
    pub fn event_conversation_info(
        &self,
        event_ids: &[String],
    ) -> AgentAspectResult<
        std::collections::HashMap<String, crate::store::conversations::ConversationInfo>,
    > {
        use std::collections::HashMap;

        if event_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<String> = event_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT e.id, e.conversation_id, e.agent, e.project_path,
                    c.id as c_db_id, c.title, c.title_source
             FROM events e
             LEFT JOIN conversations c ON c.conversation_id = e.conversation_id AND c.agent = e.agent
             WHERE e.id IN ({})",
            placeholders.join(", ")
        );
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = event_ids
            .iter()
            .map(|id| Box::new(id.clone()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let event_id: String = row.get(0)?;
                let conv_id: Option<String> = row.get(1)?;
                let agent: String = row.get(2)?;
                let project_path: Option<String> = row.get(3)?;
                let c_db_id: Option<String> = row.get(4)?;
                let title: Option<String> = row.get(5)?;
                let title_source: Option<String> = row.get(6)?;
                Ok((
                    event_id,
                    crate::store::conversations::ConversationInfo {
                        conversation_id: conv_id,
                        conversation_db_id: c_db_id,
                        agent,
                        project_path,
                        title,
                        title_source,
                    },
                ))
            })
            .map_err(AgentAspectError::QueryFilteredDecisions)?;

        let mut map = HashMap::new();
        for row in rows {
            let (event_id, info) = row.map_err(AgentAspectError::QueryFilteredDecisions)?;
            map.insert(event_id, info);
        }
        Ok(map)
    }

    /// 删除指定时间戳之前的旧事件、关联决策、孤立反馈。
    /// 返回 (events_deleted, decisions_deleted)。
    pub fn purge_before(&self, before_timestamp: &str) -> AgentAspectResult<(usize, usize)> {
        // Delete decisions referencing events about to be purged
        self.conn
            .execute(
                "DELETE FROM decisions WHERE event_id IN (SELECT id FROM events WHERE timestamp < ?1)",
                rusqlite::params![before_timestamp],
            )
            .map_err(AgentAspectError::PurgeOldRecords)?;

        let events_deleted = self
            .conn
            .execute(
                "DELETE FROM events WHERE timestamp < ?1",
                rusqlite::params![before_timestamp],
            )
            .map_err(AgentAspectError::PurgeOldRecords)?;

        let decisions_deleted = self
            .conn
            .execute(
                "DELETE FROM decisions WHERE timestamp < ?1",
                rusqlite::params![before_timestamp],
            )
            .map_err(AgentAspectError::PurgeOldRecords)?;

        // Purge feedback for deleted events
        self.conn
            .execute(
                "DELETE FROM event_feedback WHERE event_id NOT IN (SELECT id FROM events)",
                [],
            )
            .map_err(AgentAspectError::PurgeOldRecords)?;

        Ok((events_deleted, decisions_deleted))
    }
}
