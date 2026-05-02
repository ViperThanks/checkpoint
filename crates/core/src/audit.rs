//! SQLite 审计存储 — 决策、事件、会话、任务的持久化核心。
//!
//! 所有持久化操作通过 `AuditStore` 进行，它管理 schema 迁移，
//! 并为 bridge HTTP API 提供查询方法。
//!
//! SQL 和行映射器按领域拆分在 `store/` 子模块中。
//!
//! 核心不变量：
//! - WAL 模式允许写期间并发读（job runner 写日志时 HTTP handler 可读）
//! - busy_timeout=5000ms 防止 SQLITE_BUSY 即时失败
//! - decisions 表允许同一 event_id 多行（覆盖场景）

use crate::error::{CheckpointError, CheckpointResult};
use rusqlite::Connection;
use std::path::Path;

// Re-export row types so external code can keep using `use checkpoint_core::audit::*`.
pub use crate::store::conversations::{
    ConversationInfo, ConversationRow, ProjectContext, RunContext,
};
pub use crate::store::decisions::DecisionRow;
pub use crate::store::devices::DeviceRow;
pub use crate::store::events::EventRow;
pub use crate::store::feedback::FeedbackRow;
pub use crate::store::jobs::{JobLogRow, JobRow};
pub use crate::store::messages::SyncStateRow;
pub use crate::store::suggestions::SuggestionRow;

/// 审计存储 facade — 持有 SQLite 连接，所有 DAO 方法通过 split impl 分布在各 store/ 子模块。
pub struct AuditStore {
    pub(crate) conn: Connection,
}

impl AuditStore {
    /// 打开磁盘数据库并启用 WAL + busy_timeout，然后执行 schema 初始化和迁移。
    pub fn open(path: &Path) -> CheckpointResult<Self> {
        let conn = Connection::open(path).map_err(CheckpointError::OpenDb)?;
        // WAL 模式允许写期间并发读（job runner 写日志时 HTTP handler 可读）。
        // busy_timeout 防止 SQLITE_BUSY 即时失败。
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;")
            .map_err(CheckpointError::InitTables)?;
        let store = Self { conn };
        store.init_tables()?;
        Ok(store)
    }

    /// 打开内存数据库（测试用）。
    pub fn open_in_memory() -> CheckpointResult<Self> {
        let conn = Connection::open_in_memory().map_err(CheckpointError::OpenInMemoryDb)?;
        let store = Self { conn };
        store.init_tables()?;
        Ok(store)
    }

    /// 建表 + 索引 + 执行所有迁移。幂等，已存在的表/索引自动跳过。
    fn init_tables(&self) -> CheckpointResult<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS events (
                    id TEXT PRIMARY KEY,
                    phase TEXT NOT NULL,
                    type TEXT NOT NULL,
                    agent TEXT NOT NULL,
                    tool_name TEXT NOT NULL,
                    file_path TEXT,
                    timestamp TEXT NOT NULL,
                    raw_payload TEXT
                );
                CREATE TABLE IF NOT EXISTS decisions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_id TEXT NOT NULL,
                    action TEXT NOT NULL,
                    rule_id TEXT,
                    note TEXT,
                    timestamp TEXT NOT NULL,
                    device_id TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_decisions_event_id ON decisions(event_id);
                CREATE TABLE IF NOT EXISTS devices (
                    device_id TEXT PRIMARY KEY,
                    label TEXT NOT NULL DEFAULT '',
                    user_agent TEXT,
                    remote_addr TEXT,
                    first_seen TEXT NOT NULL,
                    last_seen TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS event_feedback (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_id TEXT NOT NULL UNIQUE,
                    verdict TEXT NOT NULL CHECK(verdict IN ('useful','noisy','wrong','unsure')),
                    note TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS jobs (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    input TEXT NOT NULL DEFAULT '{}',
                    status TEXT NOT NULL DEFAULT 'queued'
                        CHECK(status IN ('queued','running','succeeded','failed','cancelled')),
                    created_at TEXT NOT NULL,
                    started_at TEXT,
                    finished_at TEXT,
                    exit_code INTEGER
                );
                CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
                CREATE TABLE IF NOT EXISTS job_logs (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    job_id TEXT NOT NULL,
                    stream TEXT NOT NULL CHECK(stream IN ('stdout','stderr','system')),
                    chunk TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    timestamp TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_job_logs_job_id ON job_logs(job_id);
                CREATE TABLE IF NOT EXISTS conversations (
                    id TEXT PRIMARY KEY,
                    agent TEXT NOT NULL,
                    conversation_id TEXT NOT NULL,
                    title TEXT NOT NULL DEFAULT '',
                    project_path TEXT,
                    started_at TEXT NOT NULL,
                    last_seen_at TEXT NOT NULL,
                    event_count INTEGER NOT NULL DEFAULT 0,
                    ask_count INTEGER NOT NULL DEFAULT 0,
                    deny_count INTEGER NOT NULL DEFAULT 0,
                    job_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE INDEX IF NOT EXISTS idx_conversations_agent ON conversations(agent);
                CREATE INDEX IF NOT EXISTS idx_conversations_last_seen ON conversations(last_seen_at DESC);
                CREATE INDEX IF NOT EXISTS idx_decisions_timestamp ON decisions(timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_events_timestamp ON events(timestamp DESC);
                CREATE INDEX IF NOT EXISTS idx_conversations_project ON conversations(project_path);
                CREATE TABLE IF NOT EXISTS suggestions (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    reason TEXT NOT NULL DEFAULT '',
                    confidence REAL NOT NULL DEFAULT 0.0,
                    agent TEXT NOT NULL,
                    tool_name TEXT NOT NULL,
                    project_path TEXT,
                    pattern TEXT NOT NULL DEFAULT '*',
                    sample_event_ids TEXT NOT NULL DEFAULT '[]',
                    sample_count INTEGER NOT NULL DEFAULT 0,
                    suggested_action TEXT NOT NULL DEFAULT 'allow',
                    status TEXT NOT NULL DEFAULT 'pending'
                        CHECK(status IN ('pending','accepted','rejected','expired')),
                    created_at TEXT NOT NULL,
                    resolved_at TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_suggestions_status ON suggestions(status);
                CREATE INDEX IF NOT EXISTS idx_suggestions_allow_lookup ON suggestions(status, suggested_action, agent, tool_name);
                CREATE TABLE IF NOT EXISTS conversation_messages (
                    id TEXT PRIMARY KEY,
                    conversation_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    role TEXT NOT NULL,
                    timestamp TEXT,
                    text TEXT NOT NULL DEFAULT '',
                    source TEXT NOT NULL,
                    turn_id TEXT,
                    tool_name TEXT,
                    tool_input_preview TEXT,
                    tool_input_full TEXT,
                    raw_hash TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_conversation_messages_conv_hash
                    ON conversation_messages(conversation_id, raw_hash);
                CREATE INDEX IF NOT EXISTS idx_conversation_messages_conv_seq
                    ON conversation_messages(conversation_id, seq);
                CREATE TABLE IF NOT EXISTS conversation_sync_state (
                    conversation_id TEXT PRIMARY KEY,
                    transcript_path TEXT,
                    file_size_bytes INTEGER NOT NULL DEFAULT 0,
                    file_mtime_ms INTEGER NOT NULL DEFAULT 0,
                    line_offset INTEGER NOT NULL DEFAULT 0,
                    line_count INTEGER NOT NULL DEFAULT 0,
                    message_count INTEGER NOT NULL DEFAULT 0,
                    last_synced_at TEXT,
                    last_error TEXT
                );",
            )
            .map_err(CheckpointError::InitTables)?;
        self.migrate_legacy_decisions_schema()?;
        self.migrate_v2_conversations()?;
        self.migrate_v3_title_import()?;
        self.migrate_v4_job_context()?;
        self.migrate_v5_conv_stats()?;
        self.migrate_v6_job_supervisor()?;
        self.migrate_v8_conv_stats_cache()?;
        self.migrate_v9_devices()?;
        self.migrate_v10_conversation_messages()?;
        self.migrate_v11_runtime_identity()?;
        self.migrate_v12_job_completed_reason()?;
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_events_conv_agent ON events(conversation_id, agent)",
                [],
            )
            .map_err(CheckpointError::InitTables)?;
        Ok(())
    }

    /// 检查表中是否存在指定列（用于安全迁移，避免 ALTER 重复列）。
    fn column_exists(&self, table: &str, column: &str) -> CheckpointResult<bool> {
        let sql = format!(
            "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = ?1",
            table
        );
        let count: i64 = self
            .conn
            .query_row(&sql, rusqlite::params![column], |row| row.get(0))
            .map_err(CheckpointError::MigrateConversationSchema)?;
        Ok(count > 0)
    }

    /// v2: events 表新增 conversation_id / project_path 列，jobs 新增 conversation_id。
    fn migrate_v2_conversations(&self) -> CheckpointResult<()> {
        if !self.column_exists("events", "conversation_id")? {
            self.conn
                .execute("ALTER TABLE events ADD COLUMN conversation_id TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("events", "project_path")? {
            self.conn
                .execute("ALTER TABLE events ADD COLUMN project_path TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if self.column_exists("events", "conversation_id")? {
            self.conn
                .execute(
                    "CREATE INDEX IF NOT EXISTS idx_events_conversation ON events(conversation_id)",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("jobs", "conversation_id")? {
            self.conn
                .execute("ALTER TABLE jobs ADD COLUMN conversation_id TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        Ok(())
    }

    /// v3: conversations 表新增 title_source / transcript_path 列。
    fn migrate_v3_title_import(&self) -> CheckpointResult<()> {
        if !self.column_exists("conversations", "title_source")? {
            self.conn
                .execute("ALTER TABLE conversations ADD COLUMN title_source TEXT NOT NULL DEFAULT 'fallback'", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("conversations", "transcript_path")? {
            self.conn
                .execute(
                    "ALTER TABLE conversations ADD COLUMN transcript_path TEXT",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        Ok(())
    }

    /// v4: jobs 表新增 provider / project_path / prompt 列。
    fn migrate_v4_job_context(&self) -> CheckpointResult<()> {
        if !self.column_exists("jobs", "provider")? {
            self.conn
                .execute("ALTER TABLE jobs ADD COLUMN provider TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("jobs", "project_path")? {
            self.conn
                .execute("ALTER TABLE jobs ADD COLUMN project_path TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("jobs", "prompt")? {
            self.conn
                .execute("ALTER TABLE jobs ADD COLUMN prompt TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        Ok(())
    }

    /// v5: conversations 表新增 token_count / file_size_bytes 列。
    fn migrate_v5_conv_stats(&self) -> CheckpointResult<()> {
        if !self.column_exists("conversations", "token_count")? {
            self.conn
                .execute(
                    "ALTER TABLE conversations ADD COLUMN token_count INTEGER NOT NULL DEFAULT 0",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("conversations", "file_size_bytes")? {
            self.conn
                .execute(
                    "ALTER TABLE conversations ADD COLUMN file_size_bytes INTEGER NOT NULL DEFAULT 0",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        Ok(())
    }

    /// v6: jobs 表新增进程监控字段（pid / runner_id / heartbeat 等）。
    fn migrate_v6_job_supervisor(&self) -> CheckpointResult<()> {
        let columns = [
            ("pid", "INTEGER"),
            ("process_group_id", "INTEGER"),
            ("runner_id", "TEXT"),
            ("heartbeat_at", "TEXT"),
            ("timeout_secs", "INTEGER"),
            ("failure_reason", "TEXT"),
            ("last_log_at", "TEXT"),
        ];
        for (name, ty) in columns {
            if !self.column_exists("jobs", name)? {
                self.conn
                    .execute(&format!("ALTER TABLE jobs ADD COLUMN {name} {ty}"), [])
                    .map_err(CheckpointError::MigrateConversationSchema)?;
            }
        }
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_jobs_runner_status ON jobs(runner_id, status)",
                [],
            )
            .map_err(CheckpointError::MigrateConversationSchema)?;
        Ok(())
    }

    /// v8: conversations 表新增缓存统计字段（cached_token_count 等）。
    fn migrate_v8_conv_stats_cache(&self) -> CheckpointResult<()> {
        if !self.column_exists("conversations", "cached_token_count")? {
            self.conn
                .execute(
                    "ALTER TABLE conversations ADD COLUMN cached_token_count INTEGER",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("conversations", "cached_file_size_bytes")? {
            self.conn
                .execute(
                    "ALTER TABLE conversations ADD COLUMN cached_file_size_bytes INTEGER",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        if !self.column_exists("conversations", "stats_computed_at")? {
            self.conn
                .execute(
                    "ALTER TABLE conversations ADD COLUMN stats_computed_at TEXT",
                    [],
                )
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        Ok(())
    }

    /// v9: 新增 devices 表；decisions 表新增 device_id 列。
    fn migrate_v9_devices(&self) -> CheckpointResult<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS devices (
                    device_id TEXT PRIMARY KEY,
                    label TEXT NOT NULL DEFAULT '',
                    user_agent TEXT,
                    remote_addr TEXT,
                    first_seen TEXT NOT NULL,
                    last_seen TEXT NOT NULL
                );",
            )
            .map_err(CheckpointError::MigrateDeviceSchema)?;
        if !self.column_exists("decisions", "device_id")? {
            self.conn
                .execute("ALTER TABLE decisions ADD COLUMN device_id TEXT", [])
                .map_err(CheckpointError::MigrateDeviceSchema)?;
        }
        self.conn
            .execute(
                "CREATE INDEX IF NOT EXISTS idx_decisions_device ON decisions(device_id)",
                [],
            )
            .map_err(CheckpointError::MigrateDeviceSchema)?;
        Ok(())
    }

    /// v10: 占位迁移（表已在 init_tables 中创建）。
    fn migrate_v10_conversation_messages(&self) -> CheckpointResult<()> {
        // 占位：表已在 init_tables batch 中创建，未来 schema 变更在此扩展。
        Ok(())
    }

    /// v11: conversations 表新增运行时身份字段（Runtime Drift Guard）。
    ///
    /// 老数据默认 "unknown" / NULL / 0，不影响查看，只在 resume 时警告。
    fn migrate_v11_runtime_identity(&self) -> CheckpointResult<()> {
        let columns = [
            ("model_id", "TEXT NOT NULL DEFAULT 'unknown'"),
            ("runtime_profile", "TEXT NOT NULL DEFAULT 'unknown'"),
            ("runtime_profile_hash", "TEXT"),
            ("permission_mode", "TEXT NOT NULL DEFAULT 'unknown'"),
            ("entrypoint", "TEXT"),
            ("toolchain_fingerprint", "TEXT"),
            ("last_runtime_check_at", "TEXT"),
            ("last_runtime_warning", "TEXT"),
            ("resume_cost_mode", "TEXT"),
            ("identity_version", "INTEGER NOT NULL DEFAULT 0"),
        ];
        for (name, ty) in columns {
            if !self.column_exists("conversations", name)? {
                self.conn
                    .execute(
                        &format!("ALTER TABLE conversations ADD COLUMN {name} {ty}"),
                        [],
                    )
                    .map_err(CheckpointError::MigrateConversationSchema)?;
            }
        }
        Ok(())
    }

    /// v12: jobs 表新增 completed_reason 结构化完成原因。
    ///
    /// 值：process_exit / process_exit_nonzero / timeout_killed / cancelled / bridge_restart / NULL。
    /// 与 failure_reason 并存：failure_reason 是自由文本（向后兼容），completed_reason 是枚举值（便于查询）。
    fn migrate_v12_job_completed_reason(&self) -> CheckpointResult<()> {
        if !self.column_exists("jobs", "completed_reason")? {
            self.conn
                .execute("ALTER TABLE jobs ADD COLUMN completed_reason TEXT", [])
                .map_err(CheckpointError::MigrateConversationSchema)?;
        }
        Ok(())
    }
    /// 遗留迁移：将 decisions 表从 event_id 主键改为自增 id 主键，
    /// 以支持同一 event 的多次决策（覆盖场景）。
    fn migrate_legacy_decisions_schema(&self) -> CheckpointResult<()> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(decisions)")
            .map_err(CheckpointError::InspectDecisionsSchema)?;

        let columns = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(CheckpointError::ReadDecisionsSchema)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(CheckpointError::CollectDecisionsSchema)?;

        let has_new_id = columns.iter().any(|(name, _, _)| name == "id");
        let old_event_pk = columns
            .iter()
            .any(|(name, _, pk)| name == "event_id" && *pk == 1);

        if has_new_id || !old_event_pk {
            return Ok(());
        }

        self.conn
            .execute_batch(
                "ALTER TABLE decisions RENAME TO decisions_legacy;
                 CREATE TABLE decisions (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     event_id TEXT NOT NULL,
                     action TEXT NOT NULL,
                     rule_id TEXT,
                     note TEXT,
                     timestamp TEXT NOT NULL
                 );
                 INSERT INTO decisions (event_id, action, rule_id, note, timestamp)
                 SELECT event_id, action, rule_id, note, timestamp
                 FROM decisions_legacy
                 ORDER BY timestamp ASC;
                 DROP TABLE decisions_legacy;
                 CREATE INDEX IF NOT EXISTS idx_decisions_event_id ON decisions(event_id);",
            )
            .map_err(CheckpointError::MigrateLegacyDecisionsSchema)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::AuditStore;
    use rusqlite::Connection;
    use std::fs;

    #[test]
    fn migrates_legacy_decisions_schema_and_allows_multiple_rows_per_event() {
        let db_path = std::env::temp_dir().join(format!(
            "checkpoint-audit-migration-{}.db",
            uuid::Uuid::now_v7()
        ));

        {
            let conn = Connection::open(&db_path).expect("create legacy db");
            conn.execute_batch(
                "CREATE TABLE decisions (
                    event_id TEXT PRIMARY KEY,
                    action TEXT NOT NULL,
                    rule_id TEXT,
                    note TEXT,
                    timestamp TEXT NOT NULL
                );
                 INSERT INTO decisions (event_id, action, rule_id, note, timestamp)
                 VALUES ('evt-1', 'ask', 'D001', 'legacy row', '2026-04-24T00:00:00Z');",
            )
            .expect("seed legacy schema");
        }

        let store = AuditStore::open(&db_path).expect("migrate legacy db");
        store
            .insert_decision(
                "evt-1",
                "deny",
                Some("user_override"),
                "user rejected: ask -> deny",
                "2026-04-24T00:00:01Z",
            )
            .expect("insert second decision");

        let conn = Connection::open(&db_path).expect("reopen migrated db");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM decisions WHERE event_id = 'evt-1'",
                [],
                |row| row.get(0),
            )
            .expect("count migrated decisions");
        let has_id_column: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('decisions') WHERE name = 'id'",
                [],
                |row| row.get(0),
            )
            .expect("check id column");

        assert_eq!(count, 2);
        assert_eq!(has_id_column, 1);

        fs::remove_file(&db_path).ok();
    }

    #[test]
    fn records_device_for_user_decision() {
        let store = AuditStore::open_in_memory().expect("open db");
        store
            .insert_event(
                "evt-device",
                "before",
                "tool_use",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                "{}",
            )
            .expect("insert event");
        store
            .register_device(
                "browser-1",
                Some("Safari"),
                Some("127.0.0.1:1234"),
                "2026-04-25T10:00:01Z",
            )
            .expect("register device");
        store
            .update_device_label("browser-1", "我的 iPhone")
            .expect("label");
        store
            .insert_decision_for_device(
                "evt-device",
                "allow",
                Some("user_override"),
                "ok",
                "2026-04-25T10:00:02Z",
                Some("browser-1"),
            )
            .expect("insert decision");

        let rows = store.recent_decisions(1).expect("query decisions");
        assert_eq!(rows[0].device_id.as_deref(), Some("browser-1"));
        assert_eq!(rows[0].device_label.as_deref(), Some("我的 iPhone"));

        let devices = store.list_devices().expect("devices");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].label, "我的 iPhone");
    }

    #[test]
    fn pending_asks_returns_only_events_whose_latest_decision_is_ask() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");

        store
            .insert_event(
                "e1",
                "p1",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("e1", "ask", Some("D001"), "test", "2026-04-25T10:00:01Z")
            .unwrap();
        store
            .insert_decision(
                "e1",
                "deny",
                Some("user_override"),
                "rejected",
                "2026-04-25T10:00:02Z",
            )
            .unwrap();

        store
            .insert_event(
                "e2",
                "p1",
                "tool.request",
                "claude_code",
                "Edit",
                Some("/tmp/a.rs"),
                "2026-04-25T10:01:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("e2", "ask", Some("D002"), "test", "2026-04-25T10:01:01Z")
            .unwrap();

        store
            .insert_event(
                "e3",
                "p1",
                "tool.request",
                "claude_code",
                "Write",
                None,
                "2026-04-25T10:02:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision(
                "e3",
                "ask",
                Some("D003"),
                "first ask",
                "2026-04-25T10:02:01Z",
            )
            .unwrap();
        store
            .insert_decision(
                "e3",
                "allow",
                Some("user_override"),
                "approved",
                "2026-04-25T10:02:02Z",
            )
            .unwrap();
        store
            .insert_decision(
                "e3",
                "ask",
                Some("D003"),
                "second ask",
                "2026-04-25T10:02:03Z",
            )
            .unwrap();

        let pending = store.pending_asks(10).expect("pending asks");
        assert_eq!(pending.len(), 2);
        let ids: Vec<&str> = pending.iter().map(|d| d.event_id.as_str()).collect();
        assert!(ids.contains(&"e2"));
        assert!(ids.contains(&"e3"));
        assert!(!ids.contains(&"e1"));
    }

    #[test]
    fn latest_decision_for_event_returns_none_for_missing() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let result = store
            .latest_decision_for_event("nonexistent")
            .expect("query");
        assert!(result.is_none());
    }

    #[test]
    fn latest_decision_for_event_returns_most_recent() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        store
            .insert_event(
                "e1",
                "p1",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("e1", "ask", Some("D001"), "first", "2026-04-25T10:00:01Z")
            .unwrap();
        store
            .insert_decision(
                "e1",
                "deny",
                Some("user_override"),
                "second",
                "2026-04-25T10:00:02Z",
            )
            .unwrap();

        let d = store
            .latest_decision_for_event("e1")
            .expect("query")
            .expect("some");
        assert_eq!(d.action, "deny");
        assert_eq!(d.rule_id.as_deref(), Some("user_override"));
    }

    #[test]
    fn insert_and_query_feedback() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        store
            .insert_event(
                "e1",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("e1", "deny", Some("D003"), "rm -rf", "2026-04-25T10:00:01Z")
            .unwrap();

        store
            .insert_feedback("e1", "useful", "good catch", "2026-04-25T11:00:00Z")
            .unwrap();
        let fb = store
            .feedback_for_events(&["e1".to_string()])
            .expect("query");
        assert_eq!(fb.len(), 1);
        assert_eq!(fb["e1"].verdict, "useful");
        assert_eq!(fb["e1"].note, "good catch");

        store
            .insert_feedback("e1", "noisy", "too many alerts", "2026-04-25T12:00:00Z")
            .unwrap();
        let fb2 = store
            .feedback_for_events(&["e1".to_string()])
            .expect("query2");
        assert_eq!(fb2["e1"].verdict, "noisy");
    }

    #[test]
    fn event_exists_check() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        store
            .insert_event(
                "e1",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                "{}",
            )
            .unwrap();
        assert!(store.event_exists("e1").unwrap());
        assert!(!store.event_exists("nonexistent").unwrap());
    }

    #[test]
    fn verdict_filter_in_query_decisions() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");

        store
            .insert_event(
                "e1",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("e1", "deny", Some("D003"), "rm -rf", "2026-04-25T10:00:01Z")
            .unwrap();

        store
            .insert_event(
                "e2",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:01:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("e2", "allow", None, "ok", "2026-04-25T10:01:01Z")
            .unwrap();

        store
            .insert_feedback("e1", "noisy", "false positive", "2026-04-25T11:00:00Z")
            .unwrap();

        let noisy = store
            .query_decisions(10, 0, None, None, None, None, Some("noisy"), false)
            .unwrap();
        assert_eq!(noisy.len(), 1);
        assert_eq!(noisy[0].event_id, "e1");

        let unlabeled = store
            .query_decisions(10, 0, None, None, None, None, Some("unlabeled"), false)
            .unwrap();
        assert_eq!(unlabeled.len(), 1);
        assert_eq!(unlabeled[0].event_id, "e2");

        let all = store
            .query_decisions(10, 0, None, None, None, None, None, false)
            .unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn update_job_started_has_status_guard() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let now = "2026-04-25T10:00:00Z";

        store
            .insert_job("j1", "git_status", "{}", now, None, None, None, None)
            .expect("insert job");
        let rows = store
            .update_job_started("j1", "2026-04-25T10:00:01Z")
            .expect("update started");
        assert_eq!(rows, 1);
        let job = store.get_job("j1").expect("get job").expect("job exists");
        assert_eq!(job.status, "running");

        store
            .insert_job("j2", "git_status", "{}", now, None, None, None, None)
            .expect("insert job j2");
        store.cancel_job("j2").expect("cancel j2");
        let rows = store
            .update_job_started("j2", "2026-04-25T10:00:02Z")
            .expect("update started on cancelled");
        assert_eq!(rows, 0);
        let job2 = store.get_job("j2").expect("get job2").expect("job2 exists");
        assert_eq!(job2.status, "cancelled");
    }

    #[test]
    fn touch_conversation_only_moves_forward() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let db_id = crate::conversation::conversation_db_id("codex_cli", "sess-touch");

        store
            .upsert_conversation(
                &db_id,
                "codex_cli",
                "sess-touch",
                "Codex CLI · checkpoint",
                Some("/tmp/checkpoint"),
                "2026-04-25T10:00:00Z",
                "2026-04-25T10:00:00Z",
                None,
            )
            .unwrap();

        store
            .touch_conversation(&db_id, "2026-04-25T09:00:00Z")
            .unwrap();
        let conv = store.get_conversation(&db_id).unwrap().unwrap();
        assert_eq!(conv.last_seen_at, "2026-04-25T10:00:00Z");

        store
            .touch_conversation(&db_id, "2026-04-25T11:00:00Z")
            .unwrap();
        let conv = store.get_conversation(&db_id).unwrap().unwrap();
        assert_eq!(conv.last_seen_at, "2026-04-25T11:00:00Z");
    }

    #[test]
    fn event_counts_do_not_reorder_conversation() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let db_id = crate::conversation::conversation_db_id("claude_code", "sess-count");

        store
            .upsert_conversation(
                &db_id,
                "claude_code",
                "sess-count",
                "Claude Code · checkpoint",
                Some("/tmp/checkpoint"),
                "2026-04-25T10:00:00Z",
                "2026-04-25T10:00:00Z",
                None,
            )
            .unwrap();
        store
            .update_conversation_counts(&db_id, 1, 1, 0, 0)
            .unwrap();

        let conv = store.get_conversation(&db_id).unwrap().unwrap();
        assert_eq!(conv.event_count, 1);
        assert_eq!(conv.ask_count, 1);
        assert_eq!(conv.last_seen_at, "2026-04-25T10:00:00Z");
    }

    #[test]
    fn count_active_jobs_includes_queued_and_running() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let now = "2026-04-25T10:00:00Z";

        assert_eq!(store.count_active_jobs().unwrap(), 0);

        store
            .insert_job("j1", "git_status", "{}", now, None, None, None, None)
            .unwrap();
        assert_eq!(store.count_active_jobs().unwrap(), 1);

        store.update_job_started("j1", now).unwrap();
        assert_eq!(store.count_active_jobs().unwrap(), 1);

        store
            .update_job_finished("j1", "succeeded", now, Some(0))
            .unwrap();
        assert_eq!(store.count_active_jobs().unwrap(), 0);

        store
            .insert_job("j2", "git_status", "{}", now, None, None, None, None)
            .unwrap();
        store.cancel_job("j2").unwrap();
        assert_eq!(store.count_active_jobs().unwrap(), 0);

        store
            .insert_job("j3", "git_status", "{}", now, None, None, None, None)
            .unwrap();
        store
            .insert_job("j4", "git_status", "{}", now, None, None, None, None)
            .unwrap();
        assert_eq!(store.count_active_jobs().unwrap(), 2);
    }

    #[test]
    fn supervised_job_start_records_owner_pid_and_heartbeat() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let now = "2026-04-25T10:00:00Z";
        store
            .insert_job("j1", "git_status", "{}", now, None, None, None, None)
            .unwrap();

        let rows = store
            .update_job_started_supervised(
                "j1",
                "2026-04-25T10:00:01Z",
                Some(123),
                Some(123),
                Some("runner-a"),
                Some(180),
            )
            .unwrap();
        assert_eq!(rows, 1);

        let job = store.get_job("j1").unwrap().unwrap();
        assert_eq!(job.status, "running");
        assert_eq!(job.pid, Some(123));
        assert_eq!(job.process_group_id, Some(123));
        assert_eq!(job.runner_id, Some("runner-a".to_string()));
        assert_eq!(job.heartbeat_at, Some("2026-04-25T10:00:01Z".to_string()));
        assert_eq!(job.timeout_secs, Some(180));

        assert!(
            store
                .update_job_heartbeat("j1", "runner-a", "2026-04-25T10:00:03Z")
                .unwrap()
        );
        let job = store.get_job("j1").unwrap().unwrap();
        assert_eq!(job.heartbeat_at, Some("2026-04-25T10:00:03Z".to_string()));
    }

    #[test]
    fn recover_stale_active_jobs_marks_old_runner_failed() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let now = "2026-04-25T10:00:00Z";
        store
            .insert_job("queued", "git_status", "{}", now, None, None, None, None)
            .unwrap();
        store
            .insert_job("running", "git_status", "{}", now, None, None, None, None)
            .unwrap();
        store
            .update_job_started_supervised(
                "running",
                "2026-04-25T10:00:01Z",
                Some(999),
                Some(999),
                Some("old-runner"),
                Some(300),
            )
            .unwrap();

        let recovered = store
            .recover_stale_active_jobs("new-runner", "2026-04-25T10:01:00Z")
            .unwrap();
        assert_eq!(recovered, 2);

        for id in ["queued", "running"] {
            let job = store.get_job(id).unwrap().unwrap();
            assert_eq!(job.status, "failed");
            assert_eq!(
                job.failure_reason,
                Some("bridge restarted before job completed".to_string())
            );
            assert_eq!(job.finished_at, Some("2026-04-25T10:01:00Z".to_string()));
            let logs = store.get_job_logs(id).unwrap();
            assert!(logs.iter().any(|l| l.chunk.contains("recovered stale job")));
        }
    }

    #[test]
    fn insert_event_creates_conversation_and_counts() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let payload = r#"{"session_id":"sess-abc","cwd":"/tmp/myproj","tool_name":"Bash","tool_input":{"command":"echo hi"}}"#;

        store
            .insert_event(
                "e1",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                payload,
            )
            .unwrap();

        let db_id = crate::conversation::conversation_db_id("claude_code", "sess-abc");
        let conv = store.get_conversation(&db_id).expect("query conv");
        assert!(conv.is_some());
        let conv = conv.unwrap();
        assert_eq!(conv.agent, "claude_code");
        assert_eq!(conv.conversation_id, "sess-abc");
        assert_eq!(conv.event_count, 1);
        assert_eq!(conv.project_path, Some("/tmp/myproj".to_string()));
    }

    #[test]
    fn insert_event_records_runtime_permission_mode() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let payload = r#"{"session_id":"sess-bypass","cwd":"/tmp/myproj","tool_name":"Bash","permission_mode":"bypassPermissions","tool_input":{"command":"echo hi"}}"#;

        store
            .insert_event(
                "e-runtime",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                payload,
            )
            .unwrap();

        let db_id = crate::conversation::conversation_db_id("claude_code", "sess-bypass");
        let conv = store.get_conversation(&db_id).unwrap().unwrap();
        assert_eq!(conv.permission_mode, "bypassPermissions");
        assert_eq!(conv.entrypoint, Some("claude_code".to_string()));
        assert!(conv.identity_version > 0);
    }

    #[test]
    fn insert_decision_updates_conversation_ask_deny_counts() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let payload = r#"{"session_id":"sess-def","cwd":"/tmp/proj","tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#;

        store
            .insert_event(
                "e2",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                payload,
            )
            .unwrap();
        store
            .insert_decision(
                "e2",
                "ask",
                Some("D001"),
                "main push",
                "2026-04-25T10:00:01Z",
            )
            .unwrap();

        let db_id = crate::conversation::conversation_db_id("claude_code", "sess-def");
        let conv = store.get_conversation(&db_id).expect("query conv").unwrap();
        assert_eq!(conv.event_count, 1);
        assert_eq!(conv.ask_count, 1);
        assert_eq!(conv.deny_count, 0);

        store
            .insert_decision(
                "e2",
                "deny",
                Some("user_override"),
                "rejected",
                "2026-04-25T10:00:02Z",
            )
            .unwrap();
        let conv = store.get_conversation(&db_id).expect("query conv").unwrap();
        assert_eq!(conv.ask_count, 1);
        assert_eq!(conv.deny_count, 1);
    }

    #[test]
    fn current_conversation_decision_counts_use_latest_decision() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let payload = r#"{"session_id":"sess-latest","cwd":"/tmp/proj","tool_name":"Bash","tool_input":{"command":"git push origin main"}}"#;

        store
            .insert_event(
                "e-latest",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                payload,
            )
            .unwrap();
        store
            .insert_decision(
                "e-latest",
                "ask",
                Some("D001"),
                "main push",
                "2026-04-25T10:00:01Z",
            )
            .unwrap();
        store
            .insert_decision(
                "e-latest",
                "allow",
                Some("user_override"),
                "approved",
                "2026-04-25T10:00:02Z",
            )
            .unwrap();

        let db_id = crate::conversation::conversation_db_id("claude_code", "sess-latest");
        let (ask_count, deny_count) = store
            .current_conversation_decision_counts(&db_id)
            .expect("current counts");
        assert_eq!((ask_count, deny_count), (0, 0));
    }

    #[test]
    fn list_conversations_respects_agent_filter() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        let p1 = r#"{"session_id":"s1","cwd":"/tmp/a","tool_name":"Bash"}"#;
        let p2 = r#"{"session_id":"s2","cwd":"/tmp/b","tool_name":"Shell"}"#;

        store
            .insert_event(
                "e1",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                None,
                "2026-04-25T10:00:00Z",
                p1,
            )
            .unwrap();
        store
            .insert_event(
                "e2",
                "before",
                "tool.request",
                "kimi_code",
                "Shell",
                None,
                "2026-04-25T10:01:00Z",
                p2,
            )
            .unwrap();

        let all = store.list_conversations(10, 0, None).unwrap();
        assert_eq!(all.len(), 2);

        let claude_only = store
            .list_conversations(10, 0, Some("claude_code"))
            .unwrap();
        assert_eq!(claude_only.len(), 1);
        assert_eq!(claude_only[0].agent, "claude_code");
    }

    #[test]
    fn backfill_conversations_populates_from_existing_events() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        store.conn.execute(
            "INSERT INTO events (id, phase, type, agent, tool_name, file_path, timestamp, raw_payload, conversation_id, project_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL)",
            rusqlite::params!["e1", "before", "tool.request", "claude_code", "Bash", None::<&str>, "2026-04-25T10:00:00Z",
                r#"{"session_id":"sess-x","cwd":"/tmp/backfill","tool_name":"Bash"}"#],
        ).unwrap();

        let count = store.backfill_conversations().unwrap();
        assert_eq!(count, 1);

        let db_id = crate::conversation::conversation_db_id("claude_code", "sess-x");
        let conv = store.get_conversation(&db_id).unwrap();
        assert!(conv.is_some());
    }

    #[test]
    fn upsert_conversation_preserves_imported_title() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation(
                "db1",
                "codex_cli",
                "cid1",
                "Codex CLI · proj",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
                None,
            )
            .unwrap();
        let conv = store.get_conversation("db1").unwrap().unwrap();
        assert_eq!(conv.title, "Codex CLI · proj");
        assert_eq!(conv.title_source, "fallback");

        store
            .update_conversation_title("db1", "My Real Title", "provider")
            .unwrap();

        store
            .upsert_conversation(
                "db1",
                "codex_cli",
                "cid1",
                "Codex CLI · proj",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:01:00Z",
                None,
            )
            .unwrap();
        let conv = store.get_conversation("db1").unwrap().unwrap();
        assert_eq!(conv.title, "My Real Title");
        assert_eq!(conv.title_source, "provider");
    }

    #[test]
    fn upsert_conversation_preserves_first_prompt_title() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation(
                "db2",
                "claude_code",
                "sess-1",
                "Claude Code · proj",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
                None,
            )
            .unwrap();
        store
            .update_conversation_title("db2", "fix the login bug", "first_prompt")
            .unwrap();

        store
            .upsert_conversation(
                "db2",
                "claude_code",
                "sess-1",
                "Claude Code · proj",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:02:00Z",
                None,
            )
            .unwrap();
        let conv = store.get_conversation("db2").unwrap().unwrap();
        assert_eq!(conv.title, "fix the login bug");
        assert_eq!(conv.title_source, "first_prompt");
    }

    #[test]
    fn backfill_counts_do_not_cross_contaminate_across_agents() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");
        store.conn.execute(
            "INSERT INTO events (id, phase, type, agent, tool_name, file_path, timestamp, raw_payload, conversation_id, project_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL)",
            rusqlite::params!["e-claude", "before", "tool.request", "claude_code", "Bash", None::<&str>, "2026-04-25T10:00:00Z",
                r#"{"session_id":"shared-sess","cwd":"/tmp/claude","tool_name":"Bash"}"#],
        ).unwrap();
        store.conn.execute(
            "INSERT INTO events (id, phase, type, agent, tool_name, file_path, timestamp, raw_payload, conversation_id, project_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL)",
            rusqlite::params!["e-kimi", "before", "tool.request", "kimi_code", "Shell", None::<&str>, "2026-04-25T10:01:00Z",
                r#"{"session_id":"shared-sess","cwd":"/tmp/kimi","tool_name":"Shell"}"#],
        ).unwrap();

        store.conn.execute("INSERT INTO decisions (event_id, action, rule_id, note, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["e-claude", "ask", "D001", "", "2026-04-25T10:00:01Z"]).unwrap();
        store.conn.execute("INSERT INTO decisions (event_id, action, rule_id, note, timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["e-kimi", "deny", "D002", "", "2026-04-25T10:01:01Z"]).unwrap();

        let count = store.backfill_conversations().unwrap();
        assert_eq!(count, 2);

        let db_id_claude = crate::conversation::conversation_db_id("claude_code", "shared-sess");
        let db_id_kimi = crate::conversation::conversation_db_id("kimi_code", "shared-sess");

        let conv_claude = store
            .get_conversation(&db_id_claude)
            .unwrap()
            .expect("claude conv");
        let conv_kimi = store
            .get_conversation(&db_id_kimi)
            .unwrap()
            .expect("kimi conv");

        assert_ne!(db_id_claude, db_id_kimi);
        assert_eq!(conv_claude.event_count, 1);
        assert_eq!(conv_claude.ask_count, 1);
        assert_eq!(conv_claude.deny_count, 0);
        assert_eq!(conv_kimi.event_count, 1);
        assert_eq!(conv_kimi.ask_count, 0);
        assert_eq!(conv_kimi.deny_count, 1);
    }

    #[test]
    fn title_priority_fallback_to_first_prompt() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp1",
                "claude_code",
                "cid-tp1",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                None,
                None,
            )
            .unwrap();
        let conv = store.get_conversation("tp1").unwrap().unwrap();
        assert_eq!(conv.title_source, "fallback");

        store
            .upsert_conversation_from_metadata(
                "tp1",
                "claude_code",
                "cid-tp1",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:01:00Z",
                None,
                Some("First Prompt Title"),
                Some("first_prompt"),
            )
            .unwrap();
        let conv = store.get_conversation("tp1").unwrap().unwrap();
        assert_eq!(conv.title, "First Prompt Title");
        assert_eq!(conv.title_source, "first_prompt");
    }

    #[test]
    fn title_priority_fallback_to_provider() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp2",
                "claude_code",
                "cid-tp2",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                None,
                None,
            )
            .unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp2",
                "claude_code",
                "cid-tp2",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:01:00Z",
                None,
                Some("Provider Title"),
                Some("provider"),
            )
            .unwrap();
        let conv = store.get_conversation("tp2").unwrap().unwrap();
        assert_eq!(conv.title, "Provider Title");
        assert_eq!(conv.title_source, "provider");
    }

    #[test]
    fn title_priority_first_prompt_to_provider() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp3",
                "claude_code",
                "cid-tp3",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                Some("First Prompt Title"),
                Some("first_prompt"),
            )
            .unwrap();
        let conv = store.get_conversation("tp3").unwrap().unwrap();
        assert_eq!(conv.title_source, "first_prompt");

        store
            .upsert_conversation_from_metadata(
                "tp3",
                "claude_code",
                "cid-tp3",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:01:00Z",
                None,
                Some("Provider Title"),
                Some("provider"),
            )
            .unwrap();
        let conv = store.get_conversation("tp3").unwrap().unwrap();
        assert_eq!(conv.title, "Provider Title");
        assert_eq!(conv.title_source, "provider");
    }

    #[test]
    fn title_priority_provider_not_overwritten_by_first_prompt() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp4",
                "claude_code",
                "cid-tp4",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                Some("Provider Title"),
                Some("provider"),
            )
            .unwrap();
        let conv = store.get_conversation("tp4").unwrap().unwrap();
        assert_eq!(conv.title_source, "provider");

        store
            .upsert_conversation_from_metadata(
                "tp4",
                "claude_code",
                "cid-tp4",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:01:00Z",
                None,
                Some("First Prompt Title"),
                Some("first_prompt"),
            )
            .unwrap();
        let conv = store.get_conversation("tp4").unwrap().unwrap();
        assert_eq!(conv.title, "Provider Title");
        assert_eq!(conv.title_source, "provider");
    }

    #[test]
    fn title_priority_provider_not_overwritten_by_fallback() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp5",
                "claude_code",
                "cid-tp5",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                Some("Provider Title"),
                Some("provider"),
            )
            .unwrap();

        store
            .upsert_conversation_from_metadata(
                "tp5",
                "claude_code",
                "cid-tp5",
                "New Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:01:00Z",
                None,
                None,
                None,
            )
            .unwrap();
        let conv = store.get_conversation("tp5").unwrap().unwrap();
        assert_eq!(conv.title, "Provider Title");
        assert_eq!(conv.title_source, "provider");
    }

    #[test]
    fn title_priority_first_prompt_not_overwritten_by_fallback() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "tp6",
                "claude_code",
                "cid-tp6",
                "Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                Some("First Prompt Title"),
                Some("first_prompt"),
            )
            .unwrap();

        store
            .upsert_conversation_from_metadata(
                "tp6",
                "claude_code",
                "cid-tp6",
                "New Fallback Title",
                Some("/tmp/proj"),
                "2026-01-01T00:01:00Z",
                None,
                None,
                None,
            )
            .unwrap();
        let conv = store.get_conversation("tp6").unwrap().unwrap();
        assert_eq!(conv.title, "First Prompt Title");
        assert_eq!(conv.title_source, "first_prompt");
    }

    #[test]
    fn backfill_runtime_permission_mode_only_updates_unknown() {
        let store = AuditStore::open_in_memory().unwrap();
        store
            .upsert_conversation_from_metadata(
                "runtime-backfill",
                "claude_code",
                "cid-runtime-backfill",
                "Claude Code",
                Some("/tmp/proj"),
                "2026-01-01T00:00:00Z",
                None,
                None,
                None,
            )
            .unwrap();

        store
            .backfill_runtime_permission_mode("runtime-backfill", "bypassPermissions", Some("cli"))
            .unwrap();
        let conv = store.get_conversation("runtime-backfill").unwrap().unwrap();
        assert_eq!(conv.permission_mode, "bypassPermissions");
        assert_eq!(conv.entrypoint, Some("cli".to_string()));
        assert_eq!(conv.identity_version, 1);

        store
            .backfill_runtime_permission_mode("runtime-backfill", "default", Some("sdk-cli"))
            .unwrap();
        let conv = store.get_conversation("runtime-backfill").unwrap().unwrap();
        assert_eq!(conv.permission_mode, "bypassPermissions");
        assert_eq!(conv.entrypoint, Some("cli".to_string()));
        assert_eq!(conv.identity_version, 1);
    }
}
