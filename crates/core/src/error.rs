//! 统一错误类型 — 覆盖 DB、配置、协议解析、任务管理全链路。
//!
//! 所有变体都用 `#[source]` 保留底层错误链，方便调试。
//! `CheckpointResult<T>` 是贯穿整个 crate 的标准返回类型。

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CheckpointError {
    #[error("open db failed: {0}")]
    OpenDb(#[source] rusqlite::Error),

    #[error("open in-memory db failed: {0}")]
    OpenInMemoryDb(#[source] rusqlite::Error),

    #[error("init tables failed: {0}")]
    InitTables(#[source] rusqlite::Error),

    #[error("inspect decisions schema failed: {0}")]
    InspectDecisionsSchema(#[source] rusqlite::Error),

    #[error("read decisions schema failed: {0}")]
    ReadDecisionsSchema(#[source] rusqlite::Error),

    #[error("collect decisions schema failed: {0}")]
    CollectDecisionsSchema(#[source] rusqlite::Error),

    #[error("migrate legacy decisions schema failed: {0}")]
    MigrateLegacyDecisionsSchema(#[source] rusqlite::Error),

    #[error("insert event failed: {0}")]
    InsertEvent(#[source] rusqlite::Error),

    #[error("insert decision failed: {0}")]
    InsertDecision(#[source] rusqlite::Error),

    #[error("count events failed: {0}")]
    CountEvents(#[source] rusqlite::Error),

    #[error("count decisions failed: {0}")]
    CountDecisions(#[source] rusqlite::Error),

    #[error("prepare recent decisions failed: {0}")]
    PrepareRecentDecisions(#[source] rusqlite::Error),

    #[error("query recent decisions failed: {0}")]
    QueryRecentDecisions(#[source] rusqlite::Error),

    #[error("collect decisions failed: {0}")]
    CollectDecisions(#[source] rusqlite::Error),

    #[error("query filtered decisions failed: {0}")]
    QueryFilteredDecisions(#[source] rusqlite::Error),

    #[error("count filtered decisions failed: {0}")]
    CountFilteredDecisions(#[source] rusqlite::Error),

    #[error("purge old records failed: {0}")]
    PurgeOldRecords(#[source] rusqlite::Error),

    #[error("insert feedback failed: {0}")]
    InsertFeedback(#[source] rusqlite::Error),

    #[error("query feedback failed: {0}")]
    QueryFeedback(#[source] rusqlite::Error),

    #[error("read config failed: {0}")]
    ReadConfig(#[source] std::io::Error),

    #[error("write config failed: {0}")]
    WriteConfig(#[source] std::io::Error),

    #[error("create config dir failed: {0}")]
    CreateConfigDir(#[source] std::io::Error),

    #[error("parse config failed: {0}")]
    ParseConfig(#[source] toml::de::Error),

    #[error("serialize config failed: {0}")]
    SerializeConfig(#[source] toml::ser::Error),

    #[error("parse payload failed: {0}")]
    ParsePayload(#[source] serde_json::Error),

    #[error("unsupported hook event: {0}")]
    UnsupportedHookEvent(String),

    #[error("unknown mode: {0}")]
    InvalidMode(String),

    #[error("submit job failed: {0}")]
    SubmitJob(#[source] rusqlite::Error),

    #[error("query job failed: {0}")]
    QueryJob(#[source] rusqlite::Error),

    #[error("update job failed: {0}")]
    UpdateJob(#[source] rusqlite::Error),

    #[error("job log failed: {0}")]
    JobLog(#[source] rusqlite::Error),

    #[error("job already running")]
    JobConcurrency,

    #[error("job timed out")]
    JobTimeout,

    #[error("invalid job kind: {0}")]
    InvalidJobKind(String),

    #[error("insert conversation failed: {0}")]
    InsertConversation(#[source] rusqlite::Error),

    #[error("query conversation failed: {0}")]
    QueryConversation(#[source] rusqlite::Error),

    #[error("update conversation failed: {0}")]
    UpdateConversation(#[source] rusqlite::Error),

    #[error("update conversation title: {0}")]
    UpdateConversationTitle(#[source] rusqlite::Error),

    #[error("backfill conversations failed: {0}")]
    BackfillConversations(#[source] rusqlite::Error),

    #[error("migrate conversation schema failed: {0}")]
    MigrateConversationSchema(#[source] rusqlite::Error),

    #[error("migrate device schema failed: {0}")]
    MigrateDeviceSchema(#[source] rusqlite::Error),

    #[error("upsert device failed: {0}")]
    UpsertDevice(#[source] rusqlite::Error),

    #[error("query device failed: {0}")]
    QueryDevice(#[source] rusqlite::Error),

    #[error("update device failed: {0}")]
    UpdateDevice(#[source] rusqlite::Error),

    #[error("insert suggestion failed: {0}")]
    InsertSuggestion(#[source] rusqlite::Error),

    #[error("query suggestion failed: {0}")]
    QuerySuggestion(#[source] rusqlite::Error),

    #[error("update suggestion failed: {0}")]
    UpdateSuggestion(#[source] rusqlite::Error),

    #[error("update conv stats failed: {0}")]
    UpdateConvStats(#[source] rusqlite::Error),

    #[error("insert conversation message failed: {0}")]
    InsertConversationMessage(#[source] rusqlite::Error),

    #[error("query conversation messages failed: {0}")]
    QueryConversationMessages(#[source] rusqlite::Error),

    #[error("upsert sync state failed: {0}")]
    UpsertSyncState(#[source] rusqlite::Error),

    #[error("query sync state failed: {0}")]
    QuerySyncState(#[source] rusqlite::Error),

    #[error("clear conversation messages failed: {0}")]
    ClearConversationMessages(#[source] rusqlite::Error),

    #[error("migrate conversation messages schema failed: {0}")]
    MigrateConversationMessagesSchema(#[source] rusqlite::Error),

    #[error("transcript sync io: {0}")]
    TranscriptSyncIo(#[source] std::io::Error),

    #[error("update runtime identity failed: {0}")]
    UpdateRuntimeIdentity(#[source] rusqlite::Error),

    #[error("create user failed: {0}")]
    CreateUser(#[source] rusqlite::Error),

    #[error("query user failed: {0}")]
    QueryUser(#[source] rusqlite::Error),

    #[error("update user failed: {0}")]
    UpdateUser(#[source] rusqlite::Error),
}

pub type CheckpointResult<T> = Result<T, CheckpointError>;
