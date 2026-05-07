//! 消息 DAO — 会话消息的缓存读写、同步状态管理。
//!
//! 消息从 provider transcript 增量同步到 SQLite，按 (conversation_id, raw_hash) 去重。
//! 同步状态（line_offset / file_size / mtime）用于断点续传。

use crate::audit::AuditStore;
use crate::error::{AgentAspectError, AgentAspectResult};

/// 同步状态行 — 记录上次同步位置，用于增量续传。
#[derive(Debug, Clone)]
pub struct SyncStateRow {
    pub conversation_id: String,
    pub transcript_path: Option<String>,
    pub file_size_bytes: i64,
    pub file_mtime_ms: i64,
    pub line_offset: i64,
    pub line_count: i64,
    pub message_count: i64,
    pub last_synced_at: Option<String>,
    pub last_error: Option<String>,
}

impl AuditStore {
    /// Get the max seq for a conversation. Returns 0 if no messages exist.
    pub fn max_seq_for_conversation(&self, conversation_id: &str) -> AgentAspectResult<i64> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM conversation_messages WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .map_err(AgentAspectError::QueryConversationMessages)
    }

    /// Atomically insert messages and upsert sync state in one transaction.
    /// On failure the entire transaction rolls back — no partial state visible.
    /// Returns (inserted_count, total_message_count).
    pub fn sync_messages_and_state_txn(
        &self,
        messages: &[(
            String,         // 0  id
            String,         // 1  conversation_id
            i64,            // 2  seq
            String,         // 3  role
            Option<String>, // 4  timestamp
            String,         // 5  text
            String,         // 6  source
            Option<String>, // 7  turn_id
            Option<String>, // 8  tool_name
            Option<String>, // 9  tool_input_preview
            Option<String>, // 10 tool_input_full
            Option<String>, // 11 thinking
            String,         // 12 raw_hash
            String,         // 13 created_at
        )],
        state: &SyncStateRow,
    ) -> AgentAspectResult<(usize, i64)> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(AgentAspectError::InsertConversationMessage)?;

        let mut inserted = 0usize;
        let result: AgentAspectResult<i64> = (|| {
            for msg in messages {
                let n = self
                    .conn
                    .execute(
                        "INSERT OR IGNORE INTO conversation_messages
                     (id, conversation_id, seq, role, timestamp, text, source, turn_id,
                      tool_name, tool_input_preview, tool_input_full, thinking, raw_hash, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                        rusqlite::params![
                            msg.0, msg.1, msg.2, msg.3, msg.4, msg.5, msg.6, msg.7, msg.8, msg.9,
                            msg.10, msg.11, msg.12, msg.13
                        ],
                    )
                    .map_err(AgentAspectError::InsertConversationMessage)?;
                inserted += n;
            }
            // Count total messages after inserts (includes pre-existing + new)
            let total: i64 = self
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation_messages WHERE conversation_id = ?1",
                    rusqlite::params![state.conversation_id],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            // Upsert sync state in the same transaction
            self.conn
                .execute(
                    "INSERT INTO conversation_sync_state
                 (conversation_id, transcript_path, file_size_bytes, file_mtime_ms,
                  line_offset, line_count, message_count, last_synced_at, last_error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(conversation_id) DO UPDATE SET
                    transcript_path = excluded.transcript_path,
                    file_size_bytes = excluded.file_size_bytes,
                    file_mtime_ms = excluded.file_mtime_ms,
                    line_offset = excluded.line_offset,
                    line_count = excluded.line_count,
                    message_count = excluded.message_count,
                    last_synced_at = excluded.last_synced_at,
                    last_error = excluded.last_error",
                    rusqlite::params![
                        state.conversation_id,
                        state.transcript_path,
                        state.file_size_bytes,
                        state.file_mtime_ms,
                        state.line_offset,
                        state.line_count,
                        total,
                        state.last_synced_at,
                        state.last_error
                    ],
                )
                .map_err(AgentAspectError::UpsertSyncState)?;
            Ok(total)
        })();

        match result {
            Ok(total) => {
                self.conn
                    .execute_batch("COMMIT")
                    .map_err(AgentAspectError::InsertConversationMessage)?;
                Ok((inserted, total))
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Get paginated messages for a conversation (newest first, offset=0 is latest).
    pub fn get_conversation_messages(
        &self,
        conversation_id: &str,
        limit: usize,
        offset: usize,
    ) -> AgentAspectResult<Vec<serde_json::Value>> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_messages WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .map_err(AgentAspectError::QueryConversationMessages)?;

        // Reverse pagination: offset=0 returns the newest `limit` messages
        let end = total.saturating_sub(offset as i64);
        let start = end.saturating_sub(limit as i64);

        let mut stmt = self.conn.prepare(
            "SELECT role, timestamp, text, source, turn_id, tool_name, tool_input_preview, tool_input_full, thinking, seq
             FROM conversation_messages
             WHERE conversation_id = ?1
             ORDER BY seq ASC
             LIMIT ?2 OFFSET ?3"
        ).map_err(AgentAspectError::QueryConversationMessages)?;

        let rows = stmt
            .query_map(
                rusqlite::params![conversation_id, limit as i64, start.max(0)],
                |row| {
                    Ok(serde_json::json!({
                        "role": row.get::<_, String>(0)?,
                        "timestamp": row.get::<_, Option<String>>(1)?,
                        "text": row.get::<_, String>(2)?,
                        "source": row.get::<_, String>(3)?,
                        "turn_id": row.get::<_, Option<String>>(4)?,
                        "tool_name": row.get::<_, Option<String>>(5)?,
                        "tool_input_preview": row.get::<_, Option<String>>(6)?,
                        "tool_input_full": row.get::<_, Option<String>>(7)?,
                        "thinking": row.get::<_, Option<String>>(8)?,
                        "seq": row.get::<_, i64>(9)?,
                    }))
                },
            )
            .map_err(AgentAspectError::QueryConversationMessages)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QueryConversationMessages)
    }

    /// Get messages with seq > after_seq (for delta endpoint).
    pub fn get_conversation_messages_after_seq(
        &self,
        conversation_id: &str,
        after_seq: i64,
        limit: usize,
    ) -> AgentAspectResult<Vec<serde_json::Value>> {
        let mut stmt = self.conn.prepare(
            "SELECT role, timestamp, text, source, turn_id, tool_name, tool_input_preview, tool_input_full, thinking, seq
             FROM conversation_messages
             WHERE conversation_id = ?1 AND seq > ?2
             ORDER BY seq ASC
             LIMIT ?3"
        ).map_err(AgentAspectError::QueryConversationMessages)?;

        let rows = stmt
            .query_map(
                rusqlite::params![conversation_id, after_seq, limit as i64],
                |row| {
                    Ok(serde_json::json!({
                        "role": row.get::<_, String>(0)?,
                        "timestamp": row.get::<_, Option<String>>(1)?,
                        "text": row.get::<_, String>(2)?,
                        "source": row.get::<_, String>(3)?,
                        "turn_id": row.get::<_, Option<String>>(4)?,
                        "tool_name": row.get::<_, Option<String>>(5)?,
                        "tool_input_preview": row.get::<_, Option<String>>(6)?,
                        "tool_input_full": row.get::<_, Option<String>>(7)?,
                        "thinking": row.get::<_, Option<String>>(8)?,
                        "seq": row.get::<_, i64>(9)?,
                    }))
                },
            )
            .map_err(AgentAspectError::QueryConversationMessages)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(AgentAspectError::QueryConversationMessages)
    }

    /// Count cached messages for a conversation.
    pub fn count_conversation_cached_messages(
        &self,
        conversation_id: &str,
    ) -> AgentAspectResult<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_messages WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .map_err(AgentAspectError::QueryConversationMessages)
    }

    /// Get the total count of cached messages (for total field).
    pub fn total_conversation_messages(&self, conversation_id: &str) -> AgentAspectResult<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM conversation_messages WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
                |row| row.get(0),
            )
            .map_err(AgentAspectError::QueryConversationMessages)
    }

    // ---- Conversation Sync State ----

    /// Get sync state for a conversation.
    pub fn get_sync_state(&self, conversation_id: &str) -> AgentAspectResult<Option<SyncStateRow>> {
        self.conn
            .query_row(
                "SELECT conversation_id, transcript_path, file_size_bytes, file_mtime_ms,
                    line_offset, line_count, message_count, last_synced_at, last_error
             FROM conversation_sync_state WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
                |row| {
                    Ok(SyncStateRow {
                        conversation_id: row.get(0)?,
                        transcript_path: row.get(1)?,
                        file_size_bytes: row.get(2)?,
                        file_mtime_ms: row.get(3)?,
                        line_offset: row.get(4)?,
                        line_count: row.get(5)?,
                        message_count: row.get(6)?,
                        last_synced_at: row.get(7)?,
                        last_error: row.get(8)?,
                    })
                },
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(AgentAspectError::QuerySyncState(e)),
            })
    }

    /// Upsert sync state for a conversation.
    pub fn upsert_sync_state(&self, state: &SyncStateRow) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "INSERT INTO conversation_sync_state
             (conversation_id, transcript_path, file_size_bytes, file_mtime_ms, line_offset, line_count, message_count, last_synced_at, last_error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(conversation_id) DO UPDATE SET
                transcript_path = excluded.transcript_path,
                file_size_bytes = excluded.file_size_bytes,
                file_mtime_ms = excluded.file_mtime_ms,
                line_offset = excluded.line_offset,
                line_count = excluded.line_count,
                message_count = excluded.message_count,
                last_synced_at = excluded.last_synced_at,
                last_error = excluded.last_error",
                rusqlite::params![
                    state.conversation_id,
                    state.transcript_path,
                    state.file_size_bytes,
                    state.file_mtime_ms,
                    state.line_offset,
                    state.line_count,
                    state.message_count,
                    state.last_synced_at,
                    state.last_error
                ],
            )
            .map_err(AgentAspectError::UpsertSyncState)?;
        Ok(())
    }

    /// Clear all cached messages and sync state for a conversation.
    pub fn clear_conversation_cache(&self, conversation_id: &str) -> AgentAspectResult<()> {
        self.conn
            .execute(
                "DELETE FROM conversation_messages WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
            )
            .map_err(AgentAspectError::ClearConversationMessages)?;
        self.conn
            .execute(
                "DELETE FROM conversation_sync_state WHERE conversation_id = ?1",
                rusqlite::params![conversation_id],
            )
            .map_err(AgentAspectError::ClearConversationMessages)?;
        Ok(())
    }
}
