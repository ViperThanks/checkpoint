//! 会话 DAO — 会话的 CRUD、标题优先级管理、统计缓存。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};

/// 事件关联的会话摘要信息（用于 API 响应）。
#[derive(Debug, Clone)]
pub struct ConversationInfo {
    pub conversation_id: Option<String>,
    pub conversation_db_id: Option<String>,
    pub agent: String,
    pub project_path: Option<String>,
    pub title: Option<String>,
    pub title_source: Option<String>,
}

/// 会话完整行 — 对应 conversations 表所有列（含运行时身份字段）。
#[derive(Debug, Clone)]
pub struct ConversationRow {
    pub id: String,
    pub agent: String,
    pub conversation_id: String,
    pub title: String,
    pub project_path: Option<String>,
    pub started_at: String,
    pub last_seen_at: String,
    pub event_count: i64,
    pub ask_count: i64,
    pub deny_count: i64,
    pub job_count: i64,
    pub title_source: String,
    pub transcript_path: Option<String>,
    pub token_count: i64,
    pub file_size_bytes: i64,
    pub cached_token_count: Option<i64>,
    pub cached_file_size_bytes: Option<i64>,
    pub stats_computed_at: Option<String>,
    // 运行时身份
    pub model_id: String,
    pub runtime_profile: String,
    pub runtime_profile_hash: Option<String>,
    pub permission_mode: String,
    pub entrypoint: Option<String>,
    pub toolchain_fingerprint: Option<String>,
    pub last_runtime_check_at: Option<String>,
    pub last_runtime_warning: Option<String>,
    pub resume_cost_mode: Option<String>,
    pub identity_version: i64,
}

/// 项目上下文 — 聚合某项目路径下的 agents 和会话数。
#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub path: String,
    pub agents: Vec<String>,
    pub conversation_count: usize,
}

/// 运行上下文 — 所有项目聚合 + 最近会话，供 dashboard 使用。
#[derive(Debug, Clone)]
pub struct RunContext {
    pub projects: Vec<ProjectContext>,
    pub recent_conversations: Vec<ConversationRow>,
}

impl AuditStore {
    /// conversations 表完整列列表（28 列），保证所有 SELECT 使用相同列顺序。
    const CONV_COLUMNS: &'static str = "id, agent, conversation_id, title, project_path, \
        started_at, last_seen_at, event_count, ask_count, deny_count, job_count, \
        title_source, transcript_path, token_count, file_size_bytes, \
        cached_token_count, cached_file_size_bytes, stats_computed_at, \
        model_id, runtime_profile, runtime_profile_hash, permission_mode, \
        entrypoint, toolchain_fingerprint, last_runtime_check_at, \
        last_runtime_warning, resume_cost_mode, identity_version";
    pub(crate) fn map_conversation_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ConversationRow> {
        Ok(ConversationRow {
            id: row.get(0)?,
            agent: row.get(1)?,
            conversation_id: row.get(2)?,
            title: row.get(3)?,
            project_path: row.get(4)?,
            started_at: row.get(5)?,
            last_seen_at: row.get(6)?,
            event_count: row.get(7)?,
            ask_count: row.get(8)?,
            deny_count: row.get(9)?,
            job_count: row.get(10)?,
            title_source: row.get(11)?,
            transcript_path: row.get(12)?,
            token_count: row.get(13)?,
            file_size_bytes: row.get(14)?,
            cached_token_count: row.get(15)?,
            cached_file_size_bytes: row.get(16)?,
            stats_computed_at: row.get(17)?,
            // 运行时身份
            model_id: row.get(18)?,
            runtime_profile: row.get(19)?,
            runtime_profile_hash: row.get(20)?,
            permission_mode: row.get(21)?,
            entrypoint: row.get(22)?,
            toolchain_fingerprint: row.get(23)?,
            last_runtime_check_at: row.get(24)?,
            last_runtime_warning: row.get(25)?,
            resume_cost_mode: row.get(26)?,
            identity_version: row.get(27)?,
        })
    }

    /// Upsert 会话：插入新记录或更新已有记录。
    /// 标题更新遵循优先级：provider > first_prompt > fallback，低优先级不会覆盖高优先级。
    pub fn upsert_conversation(
        &self,
        id: &str,
        agent: &str,
        conversation_id: &str,
        title: &str,
        project_path: Option<&str>,
        started_at: &str,
        last_seen_at: &str,
        transcript_path: Option<&str>,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "INSERT INTO conversations (id, agent, conversation_id, title, title_source, project_path, started_at, last_seen_at, transcript_path)
                 VALUES (?1, ?2, ?3, ?4, 'fallback', ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                     title = CASE
                         WHEN conversations.title_source = 'provider' THEN conversations.title
                         WHEN conversations.title_source = 'first_prompt' THEN conversations.title
                         WHEN conversations.title != '' THEN conversations.title
                         ELSE excluded.title
                     END,
                     title_source = CASE
                         WHEN conversations.title_source IN ('provider', 'first_prompt') THEN conversations.title_source
                         ELSE 'fallback'
                     END,
                     project_path = COALESCE(excluded.project_path, conversations.project_path),
                     transcript_path = COALESCE(excluded.transcript_path, conversations.transcript_path)",
                rusqlite::params![id, agent, conversation_id, title, project_path, started_at, last_seen_at, transcript_path],
            )
            .map_err(AgentAspectError::InsertConversation)?;
        Ok(())
    }

    /// 增量更新会话计数（event_count / ask_count / deny_count / job_count）。
    pub fn update_conversation_counts(
        &self,
        id: &str,
        event_delta: i64,
        ask_delta: i64,
        deny_delta: i64,
        job_delta: i64,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations SET
                    event_count = event_count + ?1,
                    ask_count = ask_count + ?2,
                    deny_count = deny_count + ?3,
                    job_count = job_count + ?4
                 WHERE id = ?5",
                rusqlite::params![event_delta, ask_delta, deny_delta, job_delta, id],
            )
            .map_err(AgentAspectError::UpdateConversation)?;
        Ok(())
    }

    /// 更新 last_seen_at，但只向前推进（不会回退）。
    pub fn touch_conversation(&self, id: &str, timestamp: &str) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations
                 SET last_seen_at = CASE
                     WHEN last_seen_at IS NULL OR last_seen_at = '' OR last_seen_at < ?1
                     THEN ?1
                     ELSE last_seen_at
                 END
                 WHERE id = ?2",
                rusqlite::params![timestamp, id],
            )
            .map_err(AgentAspectError::UpdateConversation)?;
        Ok(())
    }

    /// 按 (agent, conversation_id) 组合键 touch 会话。
    pub fn touch_conversation_by_agent_cid(
        &self,
        agent: &str,
        conversation_id: &str,
        timestamp: &str,
    ) -> AgentAspectResult<()> {
        let id = crate::conversation::conversation_db_id(agent, conversation_id);
        self.touch_conversation(&id, timestamp)
    }

    /// 更新缓存统计（token_count / file_size_bytes）及 stats_computed_at 时间戳。
    pub fn update_cached_stats(
        &self,
        id: &str,
        token_count: i64,
        file_size_bytes: i64,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations SET cached_token_count = ?1, cached_file_size_bytes = ?2,
                        stats_computed_at = datetime('now') WHERE id = ?3",
                rusqlite::params![token_count, file_size_bytes, id],
            )
            .map_err(AgentAspectError::UpdateConvStats)?;
        Ok(())
    }

    /// 从元数据 hook（SessionStart / UserPromptSubmit）upsert 会话。
    /// 只在当前 title_source 优先级低于新来源时升级标题。
    /// 优先级：provider > first_prompt > fallback。
    pub fn upsert_conversation_from_metadata(
        &self,
        id: &str,
        agent: &str,
        conversation_id: &str,
        fallback_title: &str,
        project_path: Option<&str>,
        timestamp: &str,
        transcript_path: Option<&str>,
        title: Option<&str>,
        title_source: Option<&str>,
    ) -> AgentAspectResult<()> {
        let current_source: Option<String> = self
            .conn
            .query_row(
                "SELECT title_source FROM conversations WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .ok();

        match current_source {
            None => {
                let t = title.unwrap_or(fallback_title);
                let ts = title_source.unwrap_or("fallback");
                self.conn
                    .execute(
                        "INSERT INTO conversations (id, agent, conversation_id, title, title_source, project_path, started_at, last_seen_at, transcript_path)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)",
                        rusqlite::params![id, agent, conversation_id, t, ts, project_path, timestamp, transcript_path],
                    )
                    .map_err(AgentAspectError::InsertConversation)?;
            }
            Some(src) => {
                let should_upgrade = match (title_source, src.as_str()) {
                    (_, "provider") => false,
                    (Some("provider"), _) => true,
                    (_, "first_prompt") => false,
                    (Some("first_prompt"), "fallback") => true,
                    _ => false,
                };

                if should_upgrade {
                    if let (Some(t), Some(ts)) = (title, title_source) {
                        self.conn
                            .execute(
                                "UPDATE conversations SET
                                     title = ?1, title_source = ?2,
                                     last_seen_at = ?3,
                                     project_path = COALESCE(?4, project_path),
                                     transcript_path = COALESCE(?5, transcript_path)
                                 WHERE id = ?6",
                                rusqlite::params![
                                    t,
                                    ts,
                                    timestamp,
                                    project_path,
                                    transcript_path,
                                    id
                                ],
                            )
                            .map_err(AgentAspectError::UpdateConversation)?;
                    }
                } else {
                    self.conn
                        .execute(
                            "UPDATE conversations SET
                                 last_seen_at = ?1,
                                 project_path = COALESCE(?2, project_path),
                                 transcript_path = COALESCE(?3, transcript_path)
                             WHERE id = ?4",
                            rusqlite::params![timestamp, project_path, transcript_path, id],
                        )
                        .map_err(AgentAspectError::UpdateConversation)?;
                }
            }
        }
        Ok(())
    }

    /// 分页查询会话列表，可按 agent 过滤，按 last_seen_at 降序。
    pub fn list_conversations(
        &self,
        limit: usize,
        offset: usize,
        agent_filter: Option<&str>,
    ) -> AgentAspectResult<Vec<ConversationRow>> {
        let mut sql = format!("SELECT {} FROM conversations", Self::CONV_COLUMNS);
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(a) = agent_filter {
            sql.push_str(" WHERE agent = ?");
            params.push(Box::new(a.to_string()));
        }
        sql.push_str(" ORDER BY last_seen_at DESC LIMIT ? OFFSET ?");
        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryConversation)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::map_conversation_row)
            .map_err(AgentAspectError::QueryConversation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QueryConversation)
    }

    /// 按 DB id 获取单个会话。
    pub fn get_conversation(&self, id: &str) -> AgentAspectResult<Option<ConversationRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {} FROM conversations WHERE id = ?1",
                    Self::CONV_COLUMNS
                ),
                rusqlite::params![id],
                Self::map_conversation_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(AgentAspectError::QueryConversation(e)),
            })
    }

    /// 基于最新决策（MAX(id)）统计会话当前的 ask/deny 计数。
    /// 注意：只看每个事件的最新决策，非历史累计。
    pub fn current_conversation_decision_counts(
        &self,
        conversation_db_id: &str,
    ) -> AgentAspectResult<(i64, i64)> {
        self.conn
            .query_row(
                "SELECT
                    COALESCE(SUM(CASE WHEN d.action = 'ask' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN d.action = 'deny' THEN 1 ELSE 0 END), 0)
                 FROM conversations c
                 JOIN events e ON e.conversation_id = c.conversation_id AND e.agent = c.agent
                 JOIN decisions d ON d.event_id = e.id
                 WHERE c.id = ?1
                   AND d.id IN (SELECT MAX(id) FROM decisions GROUP BY event_id)",
                rusqlite::params![conversation_db_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(AgentAspectError::QueryConversation)
    }

    /// 批量查询多个会话的 (ask_count, deny_count)。
    /// 单条 SQL，返回 HashMap<conversation_db_id, (ask_count, deny_count)>。
    /// 空 ids 则返回空 map。
    pub fn batch_conversation_decision_counts(
        &self,
        ids: &[&str],
    ) -> AgentAspectResult<std::collections::HashMap<String, (i64, i64)>> {
        use std::collections::HashMap;
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders: Vec<String> = ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect();
        let sql = format!(
            "SELECT c.id,
                    COALESCE(SUM(CASE WHEN d.action = 'ask' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN d.action = 'deny' THEN 1 ELSE 0 END), 0)
             FROM conversations c
             JOIN events e ON e.conversation_id = c.conversation_id AND e.agent = c.agent
             JOIN decisions d ON d.event_id = e.id
             WHERE c.id IN ({})
               AND d.id IN (SELECT MAX(id) FROM decisions GROUP BY event_id)
             GROUP BY c.id",
            placeholders.join(", ")
        );
        let params: Vec<Box<dyn rusqlite::types::ToSql>> = ids
            .iter()
            .map(|id| Box::new(id.to_string()) as Box<dyn rusqlite::types::ToSql>)
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryConversation)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(AgentAspectError::QueryConversation)?;
        let mut map = HashMap::new();
        for row in rows {
            let (id, ask, deny) = row.map_err(AgentAspectError::QueryConversation)?;
            map.insert(id, (ask, deny));
        }
        Ok(map)
    }

    pub fn get_conversation_events(
        &self,
        conversation_db_id: &str,
        limit: usize,
        offset: usize,
    ) -> AgentAspectResult<Vec<crate::store::decisions::DecisionRow>> {
        let sql = format!(
            "SELECT {} FROM decisions d
             LEFT JOIN events e ON d.event_id = e.id
             LEFT JOIN devices dv ON d.device_id = dv.device_id
             JOIN conversations c ON c.conversation_id = e.conversation_id AND c.agent = e.agent
             WHERE c.id = ?1
             ORDER BY d.timestamp DESC
             LIMIT ?2 OFFSET ?3",
            Self::DECISION_COLUMNS
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        let rows = stmt
            .query_map(
                rusqlite::params![conversation_db_id, limit as i64, offset as i64],
                Self::map_decision_row,
            )
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    pub fn count_conversation_events(&self, conversation_db_id: &str) -> AgentAspectResult<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM decisions d
                 LEFT JOIN events e ON d.event_id = e.id
                 JOIN conversations c ON c.conversation_id = e.conversation_id AND c.agent = e.agent
                 WHERE c.id = ?1",
                rusqlite::params![conversation_db_id],
                |row| row.get::<_, usize>(0),
            )
            .map_err(AgentAspectError::QueryFilteredDecisions)
    }

    pub fn count_conversations(&self, agent_filter: Option<&str>) -> AgentAspectResult<usize> {
        let mut sql = String::from("SELECT COUNT(*) FROM conversations");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(a) = agent_filter {
            sql.push_str(" WHERE agent = ?");
            params.push(Box::new(a.to_string()));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        self.conn
            .query_row(&sql, param_refs.as_slice(), |row| row.get::<_, usize>(0))
            .map_err(AgentAspectError::QueryConversation)
    }

    /// 回填：为 conversation_id 为 NULL 的旧事件重建会话记录，
    /// 并刷新所有会话的 event_count / ask_count / deny_count。
    pub fn backfill_conversations(&self) -> AgentAspectResult<usize> {
        use crate::conversation;

        let mut stmt = self
            .conn
            .prepare("SELECT id, agent, timestamp, raw_payload FROM events WHERE conversation_id IS NULL ORDER BY timestamp ASC")
            .map_err(AgentAspectError::BackfillConversations)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(AgentAspectError::BackfillConversations)?;

        let mut count = 0usize;
        for row in rows {
            let (event_id, agent, timestamp, raw_payload) =
                row.map_err(AgentAspectError::BackfillConversations)?;
            if let Some(cid) = conversation::extract_conversation_id(&agent, &raw_payload) {
                let db_id = conversation::conversation_db_id(&agent, &cid);
                let project_path = conversation::extract_project_path(&agent, &raw_payload);
                let title = conversation::generate_title(
                    &agent,
                    project_path.as_deref(),
                    Some(&raw_payload),
                );

                self.conn
                    .execute(
                        "UPDATE events SET conversation_id = ?1, project_path = ?2 WHERE id = ?3",
                        rusqlite::params![cid, project_path.as_ref(), event_id],
                    )
                    .map_err(AgentAspectError::BackfillConversations)?;

                self.upsert_conversation(
                    &db_id,
                    &agent,
                    &cid,
                    &title,
                    project_path.as_deref(),
                    &timestamp,
                    &timestamp,
                    None,
                )?;
                count += 1;
            }
        }

        self.conn
            .execute(
                "UPDATE conversations SET
                    event_count = (SELECT COUNT(*) FROM events e WHERE e.conversation_id = conversations.conversation_id AND e.agent = conversations.agent),
                    ask_count = (SELECT COUNT(*) FROM decisions d JOIN events e ON d.event_id = e.id WHERE e.conversation_id = conversations.conversation_id AND e.agent = conversations.agent AND d.action = 'ask'),
                    deny_count = (SELECT COUNT(*) FROM decisions d JOIN events e ON d.event_id = e.id WHERE e.conversation_id = conversations.conversation_id AND e.agent = conversations.agent AND d.action = 'deny')",
                [],
            )
            .map_err(AgentAspectError::BackfillConversations)?;

        Ok(count)
    }

    pub fn get_conversation_all_events(
        &self,
        conversation_db_id: &str,
    ) -> AgentAspectResult<Vec<crate::store::events::EventRow>> {
        let sql = "SELECT e.id, e.phase, e.type, e.agent, e.tool_name, e.file_path, e.timestamp, e.raw_payload
                   FROM events e
                   JOIN conversations c ON c.conversation_id = e.conversation_id AND c.agent = e.agent
                   WHERE c.id = ?1
                   ORDER BY e.timestamp ASC";
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        let rows = stmt
            .query_map(rusqlite::params![conversation_db_id], |row| {
                Ok(crate::store::events::EventRow {
                    id: row.get(0)?,
                    phase: row.get(1)?,
                    event_type: row.get(2)?,
                    agent: row.get(3)?,
                    tool_name: row.get(4)?,
                    file_path: row.get(5)?,
                    timestamp: row.get(6)?,
                    raw_payload: row.get(7)?,
                })
            })
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    pub fn get_conversation_raw_events(
        &self,
        conversation_db_id: &str,
        limit: usize,
        offset: usize,
    ) -> AgentAspectResult<Vec<crate::store::events::EventRow>> {
        let sql = "SELECT e.id, e.phase, e.type, e.agent, e.tool_name, e.file_path, e.timestamp, e.raw_payload
                   FROM events e
                   JOIN conversations c ON c.conversation_id = e.conversation_id AND c.agent = e.agent
                   WHERE c.id = ?1
                   ORDER BY e.timestamp ASC
                   LIMIT ?2 OFFSET ?3";
        let mut stmt = self
            .conn
            .prepare(sql)
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        let rows = stmt
            .query_map(
                rusqlite::params![conversation_db_id, limit as i64, offset as i64],
                |row| {
                    Ok(crate::store::events::EventRow {
                        id: row.get(0)?,
                        phase: row.get(1)?,
                        event_type: row.get(2)?,
                        agent: row.get(3)?,
                        tool_name: row.get(4)?,
                        file_path: row.get(5)?,
                        timestamp: row.get(6)?,
                        raw_payload: row.get(7)?,
                    })
                },
            )
            .map_err(AgentAspectError::QueryFilteredDecisions)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::CollectDecisions)
    }

    pub fn count_conversation_raw_events(
        &self,
        conversation_db_id: &str,
    ) -> AgentAspectResult<usize> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM events e
                 JOIN conversations c ON c.conversation_id = e.conversation_id AND c.agent = e.agent
                 WHERE c.id = ?1",
                rusqlite::params![conversation_db_id],
                |row| row.get::<_, usize>(0),
            )
            .map_err(AgentAspectError::QueryFilteredDecisions)
    }

    /// 强制更新会话标题和来源（不检查优先级）。
    pub fn update_conversation_title(
        &self,
        id: &str,
        title: &str,
        title_source: &str,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations SET title = ?1, title_source = ?2 WHERE id = ?3",
                rusqlite::params![title, title_source, id],
            )
            .map_err(AgentAspectError::UpdateConversationTitle)?;
        Ok(())
    }

    /// 查询 title_source='fallback' 的会话，供 title_import 批量导入真实标题。
    pub fn list_conversations_for_title_import(
        &self,
        limit: usize,
    ) -> AgentAspectResult<Vec<ConversationRow>> {
        let sql = format!(
            "SELECT {} FROM conversations WHERE title_source = 'fallback' ORDER BY last_seen_at DESC LIMIT ?1",
            Self::CONV_COLUMNS
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryConversation)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], Self::map_conversation_row)
            .map_err(AgentAspectError::QueryConversation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QueryConversation)
    }

    /// 返回 stats 未缓存的会话（cached_token_count 为 NULL），供后台 warming。
    pub fn list_conversations_for_stats_warming(
        &self,
        limit: usize,
    ) -> AgentAspectResult<Vec<ConversationRow>> {
        let sql = format!(
            "SELECT {} FROM conversations WHERE cached_token_count IS NULL ORDER BY last_seen_at DESC LIMIT ?1",
            Self::CONV_COLUMNS
        );
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(AgentAspectError::QueryConversation)?;
        let rows = stmt
            .query_map(rusqlite::params![limit], Self::map_conversation_row)
            .map_err(AgentAspectError::QueryConversation)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QueryConversation)
    }

    /// 获取运行上下文：所有项目聚合 + 最近 10 条会话，供 dashboard 首页。
    pub fn get_run_context(&self) -> AgentAspectResult<RunContext> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT project_path, GROUP_CONCAT(DISTINCT agent) as agents_str, COUNT(*) as cnt \
             FROM conversations \
             WHERE project_path IS NOT NULL AND project_path != '' \
             GROUP BY project_path \
             ORDER BY project_path",
            )
            .map_err(AgentAspectError::QueryConversation)?;
        let projects = stmt
            .query_map([], |row| {
                let path: String = row.get(0)?;
                let agents_str: String = row.get(1)?;
                let count: i64 = row.get(2)?;
                Ok(ProjectContext {
                    path,
                    agents: agents_str.split(',').map(String::from).collect(),
                    conversation_count: count as usize,
                })
            })
            .map_err(AgentAspectError::QueryConversation)?
            .filter_map(|r| r.ok())
            .collect();

        let recent = self.list_conversations(10, 0, None)?;

        Ok(RunContext {
            projects,
            recent_conversations: recent,
        })
    }

    /// 检查项目路径是否曾在会话中被观察到（用于 relay 设备验证）。
    pub fn is_known_project_path(&self, project_path: &str) -> AgentAspectResult<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM conversations WHERE project_path = ?1 LIMIT 1",
                rusqlite::params![project_path],
                |row| row.get::<_, i64>(0),
            )
            .map(|_| true)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                _ => Err(AgentAspectError::QueryConversation(e)),
            })
    }

    /// 写入/更新会话的运行时身份字段。
    /// identity_version 自增，last_runtime_check_at 自动更新。
    pub fn update_runtime_identity(
        &self,
        id: &str,
        model_id: &str,
        runtime_profile: &str,
        runtime_profile_hash: Option<&str>,
        permission_mode: &str,
        entrypoint: Option<&str>,
        toolchain_fingerprint: Option<&str>,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations SET
                    model_id = ?1, runtime_profile = ?2, runtime_profile_hash = ?3,
                    permission_mode = ?4, entrypoint = ?5, toolchain_fingerprint = ?6,
                    last_runtime_check_at = datetime('now'),
                    identity_version = identity_version + 1
                 WHERE id = ?7",
                rusqlite::params![
                    model_id,
                    runtime_profile,
                    runtime_profile_hash,
                    permission_mode,
                    entrypoint,
                    toolchain_fingerprint,
                    id
                ],
            )
            .map_err(AgentAspectError::UpdateRuntimeIdentity)?;
        Ok(())
    }

    /// 只更新会话权限模式，不覆盖 model/profile 等完整运行时身份字段。
    pub fn update_runtime_permission_mode(
        &self,
        id: &str,
        permission_mode: &str,
        entrypoint: Option<&str>,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations SET
                    permission_mode = ?1,
                    entrypoint = COALESCE(?2, entrypoint),
                    last_runtime_check_at = datetime('now'),
                    identity_version = identity_version + 1
                 WHERE id = ?3",
                rusqlite::params![permission_mode, entrypoint, id],
            )
            .map_err(AgentAspectError::UpdateRuntimeIdentity)?;
        Ok(())
    }

    /// 只在历史会话仍为 unknown 时回填运行权限模式。
    ///
    /// 用于 transcript auto-import 兜底：老会话可能没有 hook 事件落库，
    /// 但 Claude transcript 顶层元数据里保留了 permissionMode。
    pub fn backfill_runtime_permission_mode(
        &self,
        id: &str,
        permission_mode: &str,
        entrypoint: Option<&str>,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations
                 SET permission_mode = ?1,
                     entrypoint = COALESCE(?2, entrypoint),
                     last_runtime_check_at = datetime('now'),
                     identity_version = identity_version + 1
                 WHERE id = ?3
                   AND permission_mode = 'unknown'",
                rusqlite::params![permission_mode, entrypoint, id],
            )
            .map_err(AgentAspectError::UpdateRuntimeIdentity)?;
        Ok(())
    }

    /// 记录最近一次 runtime drift 检查的警告摘要。
    pub fn update_runtime_warning(
        &self,
        id: &str,
        warning: Option<&str>,
        resume_cost_mode: Option<&str>,
    ) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "UPDATE conversations SET
                    last_runtime_warning = ?1, resume_cost_mode = ?2,
                    last_runtime_check_at = datetime('now')
                 WHERE id = ?3",
                rusqlite::params![warning, resume_cost_mode, id],
            )
            .map_err(AgentAspectError::UpdateRuntimeIdentity)?;
        Ok(())
    }
}
