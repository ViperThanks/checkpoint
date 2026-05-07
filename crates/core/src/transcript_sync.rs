//! 增量 transcript 同步 — 将 JSONL transcript 解析后缓存到 SQLite。
//!
//! 使用 line-offset 游标实现高效增量更新：
//! - 首次同步：从第 0 行读取，插入所有消息
//! - 增量同步：跳过已处理行，只读新增行
//! - 文件截断/轮转：检测到文件变小时清空缓存重建
//! - Bridge 重启后从持久化的 line_offset 恢复，无需重读全文件
//!
//! 核心不变量：
//! - 消息按 (conversation_id, raw_hash) 去重，同一行不会重复插入
//! - 消息插入和同步状态更新在同一事务中完成

use crate::audit::{AuditStore, SyncStateRow};
use crate::transcript;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader};

/// Result of a sync operation.
pub struct SyncResult {
    pub messages_synced: usize,
    pub total_messages: i64,
    pub last_error: Option<String>,
}

/// Sync conversation messages from the transcript file into SQLite.
///
/// - `cache_id`: the DB hash ID (`conversations.id`) — used as the key for
///   `conversation_messages` and `conversation_sync_state`.
/// - `provider_session_id`: the provider's native session ID — used only to
///   resolve the transcript file path.
///
/// - First sync: reads from line 0, inserts all messages.
/// - Incremental: skips `line_offset` lines, reads only new lines.
/// - File truncated/rotated: clears cache and rebuilds from 0.
/// - Single bad JSON lines: skipped, error recorded in `last_error`.
/// - Duplicate messages: deduplicated by `(conversation_id, raw_hash)`.
pub fn sync_conversation_messages(
    store: &AuditStore,
    agent: &str,
    cache_id: &str,
    provider_session_id: &str,
    project_path: Option<&str>,
    transcript_path: Option<&str>,
) -> SyncResult {
    let file_path = match transcript::resolve_transcript_path(
        agent,
        provider_session_id,
        project_path,
        transcript_path,
    ) {
        Some(p) => p,
        None => {
            // Transcript not found — return existing state without clearing cache
            let total = store
                .count_conversation_cached_messages(cache_id)
                .unwrap_or(0);
            let existing = store.get_sync_state(cache_id).ok().flatten();
            let last_error = existing
                .as_ref()
                .and_then(|s| s.last_error.clone())
                .or_else(|| Some("transcript file not found".to_string()));
            return SyncResult {
                messages_synced: 0,
                total_messages: total,
                last_error,
            };
        }
    };

    // Get file metadata
    let metadata = match std::fs::metadata(&file_path) {
        Ok(m) => m,
        Err(e) => {
            let total = store
                .count_conversation_cached_messages(cache_id)
                .unwrap_or(0);
            return SyncResult {
                messages_synced: 0,
                total_messages: total,
                last_error: Some(format!("stat file: {e}")),
            };
        }
    };

    let file_size = metadata.len() as i64;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    // Load existing sync state
    let sync_state = store.get_sync_state(cache_id).ok().flatten();

    // Detect file truncation or rotation:
    // - file got smaller (truncated/rotated)
    // - mtime went backward with different size (rotated)
    let needs_rebuild = match &sync_state {
        Some(state) => {
            file_size < state.file_size_bytes
                || (mtime_ms < state.file_mtime_ms && file_size != state.file_size_bytes)
        }
        None => false,
    };

    if needs_rebuild {
        if let Err(e) = store.clear_conversation_cache(cache_id) {
            eprintln!("checkpoint-sync: clear cache for {cache_id}: {e}");
        }
    }

    let line_offset = if needs_rebuild || sync_state.is_none() {
        0i64
    } else {
        sync_state.as_ref().map(|s| s.line_offset).unwrap_or(0)
    };

    // Open file and skip already-processed lines
    let file = match std::fs::File::open(&file_path) {
        Ok(f) => f,
        Err(e) => {
            let total = store
                .count_conversation_cached_messages(cache_id)
                .unwrap_or(0);
            return SyncResult {
                messages_synced: 0,
                total_messages: total,
                last_error: Some(format!("open file: {e}")),
            };
        }
    };

    let mut reader = BufReader::new(file);
    let mut skipped: i64 = 0;
    let mut skip_buf = String::new();
    while skipped < line_offset {
        skip_buf.clear();
        match reader.read_line(&mut skip_buf) {
            Ok(0) => break, // EOF before reaching line_offset
            Ok(_) => skipped += 1,
            Err(e) => {
                eprintln!(
                    "checkpoint-sync: skip line {skipped}/{} for {cache_id}: {e}",
                    line_offset
                );
                break;
            }
        }
    }

    // P2a fix: if we couldn't skip the expected number of lines (file was
    // truncated/rewritten without changing size or mtime), rebuild from scratch.
    if skipped < line_offset {
        eprintln!(
            "checkpoint-sync: line_offset={line_offset} but only {skipped} lines in file for {cache_id}, rebuilding"
        );
        if let Err(e) = store.clear_conversation_cache(cache_id) {
            eprintln!("checkpoint-sync: clear cache for {cache_id}: {e}");
        }
        // Re-open and read from start
        let file = match std::fs::File::open(&file_path) {
            Ok(f) => f,
            Err(e) => {
                let total = store
                    .count_conversation_cached_messages(cache_id)
                    .unwrap_or(0);
                return SyncResult {
                    messages_synced: 0,
                    total_messages: total,
                    last_error: Some(format!("open file for rebuild: {e}")),
                };
            }
        };
        reader = BufReader::new(file);
        // Reset to scan from line 0
        return sync_from_reader(
            store, agent, cache_id, &file_path, file_size, mtime_ms, 0i64, reader,
        );
    }

    sync_from_reader(
        store,
        agent,
        cache_id,
        &file_path,
        file_size,
        mtime_ms,
        line_offset,
        reader,
    )
}

/// Core sync loop — reads from the current reader position, parses, inserts.
/// `start_line_offset` is the line_offset we began scanning from (for sync state).
fn sync_from_reader(
    store: &AuditStore,
    agent: &str,
    cache_id: &str,
    file_path: &std::path::Path,
    file_size: i64,
    mtime_ms: i64,
    start_line_offset: i64,
    mut reader: BufReader<std::fs::File>,
) -> SyncResult {
    let start_seq = store.max_seq_for_conversation(cache_id).unwrap_or(0);
    let now = chrono::Utc::now().to_rfc3339();

    let mut messages_to_insert = Vec::new();
    let mut lines_scanned: i64 = 0; // lines read in this pass
    let mut last_error: Option<String> = None;
    let mut msg_seq = start_seq;
    // P1a fix: track per-line message index for unique hash
    let mut msg_index_in_line: usize;

    let mut line_buf = String::new();
    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => break, // EOF
            Ok(_) => {
                lines_scanned += 1;
                msg_index_in_line = 0;

                let trimmed = line_buf.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let parsed = match transcript::parse_transcript_line(agent, trimmed) {
                    Ok(msgs) => msgs,
                    Err(e) => {
                        // P2b fix: bad JSON is a real error, record it
                        last_error = Some(e);
                        continue;
                    }
                };

                for msg in parsed {
                    msg_seq += 1;
                    // P1a fix: include message index within line for unique hash
                    let raw_hash =
                        compute_message_hash(agent, cache_id, trimmed, msg_index_in_line);
                    msg_index_in_line += 1;
                    let msg_id = format!("{}:{}", cache_id, msg_seq);

                    messages_to_insert.push((
                        msg_id,
                        cache_id.to_string(),
                        msg_seq,
                        msg.role,
                        msg.timestamp,
                        msg.text,
                        msg.source,
                        msg.turn_id,
                        msg.tool_name,
                        msg.tool_input_preview,
                        msg.tool_input_full,
                        msg.thinking,
                        raw_hash,
                        now.clone(),
                    ));
                }
            }
            Err(e) => {
                last_error = Some(format!("read line: {e}"));
                break;
            }
        }
    }

    // P1b fix: only advance line_offset if insert succeeds.
    // P2 fix: messages + sync_state in a single atomic transaction.
    if !messages_to_insert.is_empty() {
        let sync_state_out = SyncStateRow {
            conversation_id: cache_id.to_string(),
            transcript_path: Some(file_path.to_string_lossy().to_string()),
            file_size_bytes: file_size,
            file_mtime_ms: mtime_ms,
            line_offset: start_line_offset + lines_scanned,
            line_count: start_line_offset + lines_scanned,
            message_count: 0, // computed inside the transaction
            last_synced_at: Some(now),
            last_error: last_error.clone(),
        };
        match store.sync_messages_and_state_txn(&messages_to_insert, &sync_state_out) {
            Ok((_inserted, total)) => SyncResult {
                messages_synced: _inserted,
                total_messages: total,
                last_error,
            },
            Err(e) => {
                // Transaction rolled back — nothing changed
                let total = store
                    .count_conversation_cached_messages(cache_id)
                    .unwrap_or(0);
                SyncResult {
                    messages_synced: 0,
                    total_messages: total,
                    last_error: Some(format!("sync messages: {e}")),
                }
            }
        }
    } else {
        // No messages to insert — just persist sync state (offset past metadata/empty lines)
        let new_line_offset = start_line_offset + lines_scanned;
        let total_messages = store
            .count_conversation_cached_messages(cache_id)
            .unwrap_or(0);
        let sync_state_out = SyncStateRow {
            conversation_id: cache_id.to_string(),
            transcript_path: Some(file_path.to_string_lossy().to_string()),
            file_size_bytes: file_size,
            file_mtime_ms: mtime_ms,
            line_offset: new_line_offset,
            line_count: new_line_offset,
            message_count: total_messages,
            last_synced_at: Some(now),
            last_error: last_error.clone(),
        };
        if let Err(e) = store.upsert_sync_state(&sync_state_out) {
            eprintln!("checkpoint-sync: upsert sync state for {cache_id}: {e}");
        }
        SyncResult {
            messages_synced: 0,
            total_messages,
            last_error,
        }
    }
}

/// Compute a deterministic hash for deduplication.
/// Includes `msg_index` so multiple messages from the same JSONL line get unique hashes.
fn compute_message_hash(
    agent: &str,
    conversation_id: &str,
    raw_line: &str,
    msg_index: usize,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(agent.as_bytes());
    hasher.update(b":");
    hasher.update(conversation_id.as_bytes());
    hasher.update(b":");
    hasher.update(msg_index.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(raw_line.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_message_hash_is_stable() {
        let h1 = compute_message_hash("claude_code", "sess-1", "line content", 0);
        let h2 = compute_message_hash("claude_code", "sess-1", "line content", 0);
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_message_hash_differs_by_agent() {
        let h1 = compute_message_hash("claude_code", "sess-1", "line", 0);
        let h2 = compute_message_hash("kimi_code", "sess-1", "line", 0);
        assert_ne!(h1, h2);
    }

    #[test]
    fn compute_message_hash_differs_by_content() {
        let h1 = compute_message_hash("claude_code", "sess-1", "line A", 0);
        let h2 = compute_message_hash("claude_code", "sess-1", "line B", 0);
        assert_ne!(h1, h2);
    }

    #[test]
    fn compute_message_hash_differs_by_index() {
        let h1 = compute_message_hash("claude_code", "sess-1", "line", 0);
        let h2 = compute_message_hash("claude_code", "sess-1", "line", 1);
        assert_ne!(h1, h2);
    }

    // ---- Integration tests using codex_cli (direct transcript_path) ----

    const CODEX_JSONL_V1: &str = r#"{"timestamp":"2026-04-29T10:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"fix the login bug"}}
{"timestamp":"2026-04-29T10:00:05Z","type":"event_msg","payload":{"type":"agent_message","message":"I'll look at the code."}}
{"timestamp":"2026-04-29T10:00:10Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"cat src/auth.rs\"}","call_id":"c1"}}
"#;

    const CODEX_JSONL_V2: &str = r#"{"timestamp":"2026-04-29T10:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"fix the login bug"}}
{"timestamp":"2026-04-29T10:00:05Z","type":"event_msg","payload":{"type":"agent_message","message":"I'll look at the code."}}
{"timestamp":"2026-04-29T10:00:10Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"cat src/auth.rs\"}","call_id":"c1"}}
{"timestamp":"2026-04-29T10:00:20Z","type":"event_msg","payload":{"type":"agent_message","message":"Found it on line 42."}}
{"timestamp":"2026-04-29T10:00:25Z","type":"event_msg","payload":{"type":"user_message","message":"great, fix it"}}
"#;

    fn setup_codex_jsonl(dir: &std::path::Path, content: &str) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(&path, content).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn first_sync_creates_messages_and_state() {
        let dir = std::env::temp_dir().join(format!("cp-sync-first-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V1);
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-1";

        let result = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );

        assert_eq!(result.messages_synced, 3);
        assert_eq!(result.total_messages, 3);
        assert!(result.last_error.is_none());

        // Verify sync state
        let state = store.get_sync_state(conv_id).unwrap().unwrap();
        assert_eq!(state.line_offset, 3);
        assert_eq!(state.message_count, 3);
        assert!(state.last_error.is_none());
        assert!(state.last_synced_at.is_some());

        // Verify messages are readable
        let msgs = store.get_conversation_messages(conv_id, 100, 0).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "user");
        assert_eq!(msgs[1].get("role").unwrap().as_str().unwrap(), "assistant");
        assert_eq!(
            msgs[2].get("role").unwrap().as_str().unwrap(),
            "tool_summary"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn incremental_sync_only_reads_new_lines() {
        let dir = std::env::temp_dir().join(format!("cp-sync-incr-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V1);
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-2";

        // First sync: 3 lines
        let r1 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        assert_eq!(r1.messages_synced, 3);

        // Append more lines
        std::fs::write(&transcript_path, CODEX_JSONL_V2).unwrap();

        // Second sync: should only process new lines
        let r2 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        assert_eq!(r2.messages_synced, 2); // 2 new messages
        assert_eq!(r2.total_messages, 5);

        // Verify sync state updated
        let state = store.get_sync_state(conv_id).unwrap().unwrap();
        assert_eq!(state.line_offset, 5);
        assert_eq!(state.message_count, 5);

        // Verify all messages readable
        let msgs = store.get_conversation_messages(conv_id, 100, 0).unwrap();
        assert_eq!(msgs.len(), 5);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bridge_restart_resumes_from_line_offset() {
        let dir = std::env::temp_dir().join(format!("cp-sync-restart-{}", std::process::id()));
        let db_path = dir.join("test.db");
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V1);
        let conv_id = "test-conv-3";

        // First "session" — sync initial data
        {
            let store = AuditStore::open(&db_path).unwrap();
            let r = sync_conversation_messages(
                &store,
                "codex_cli",
                conv_id,
                conv_id,
                None,
                Some(&transcript_path),
            );
            assert_eq!(r.messages_synced, 3);
        }
        // Store dropped — simulates bridge shutdown

        // Append more data while bridge is down
        std::fs::write(&transcript_path, CODEX_JSONL_V2).unwrap();

        // Second "session" — bridge restart, reopens DB
        {
            let store = AuditStore::open(&db_path).unwrap();
            let r = sync_conversation_messages(
                &store,
                "codex_cli",
                conv_id,
                conv_id,
                None,
                Some(&transcript_path),
            );
            assert_eq!(r.messages_synced, 2); // only new lines
            assert_eq!(r.total_messages, 5);

            let state = store.get_sync_state(conv_id).unwrap().unwrap();
            assert_eq!(state.line_offset, 5);
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_truncation_clears_and_rebuilds() {
        let dir = std::env::temp_dir().join(format!("cp-sync-trunc-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V2);
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-4";

        // Sync full file (5 messages, 5 lines)
        let r1 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        assert_eq!(r1.messages_synced, 5);

        // Truncate file to smaller content
        std::fs::write(&transcript_path, CODEX_JSONL_V1).unwrap();

        // Sync should detect truncation and rebuild
        let r2 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        // After clear + rebuild: 3 messages from truncated file
        assert_eq!(r2.total_messages, 3);

        let state = store.get_sync_state(conv_id).unwrap().unwrap();
        assert_eq!(state.line_offset, 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn duplicate_sync_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("cp-sync-idemp-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V1);
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-5";

        // Sync twice without changing file
        let r1 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        let r2 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );

        assert_eq!(r1.messages_synced, 3);
        assert_eq!(r2.messages_synced, 0); // no new lines
        assert_eq!(r2.total_messages, 3); // unchanged

        // Verify no duplicates
        let msgs = store.get_conversation_messages(conv_id, 100, 0).unwrap();
        assert_eq!(msgs.len(), 3);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn bad_json_line_is_skipped_with_error() {
        let dir = std::env::temp_dir().join(format!("cp-sync-badjson-{}", std::process::id()));
        let content = r#"{"timestamp":"2026-04-29T10:00:00Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}
not valid json at all
{"timestamp":"2026-04-29T10:00:05Z","type":"event_msg","payload":{"type":"agent_message","message":"hi there"}}"#;
        let transcript_path = setup_codex_jsonl(&dir, content);
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-6";

        let r = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );

        // Bad JSON line is skipped; the other 2 lines produce 2 messages
        assert_eq!(r.messages_synced, 2);
        assert_eq!(r.total_messages, 2);
        // P2b fix: bad JSON must be recorded as last_error
        assert!(r.last_error.is_some(), "bad JSON must produce last_error");
        assert!(
            r.last_error.as_ref().unwrap().contains("bad JSON"),
            "error should mention bad JSON, got: {:?}",
            r.last_error
        );

        let msgs = store.get_conversation_messages(conv_id, 100, 0).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "user");
        assert_eq!(msgs[1].get("role").unwrap().as_str().unwrap(), "assistant");

        // Verify sync state records the error
        let state = store.get_sync_state(conv_id).unwrap().unwrap();
        assert!(state.last_error.is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcript_not_found_preserves_existing_cache() {
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-7";

        // First sync with real file
        let dir = std::env::temp_dir().join(format!("cp-sync-notfound-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V1);
        let r1 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        assert_eq!(r1.messages_synced, 3);

        // Delete the file
        std::fs::remove_dir_all(&dir).unwrap();

        // Sync again — file gone, but cache preserved
        let r2 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        assert_eq!(r2.messages_synced, 0);
        assert_eq!(r2.total_messages, 3); // cache preserved
        assert!(r2.last_error.is_some()); // error reported

        // Messages still readable from cache
        let msgs = store.get_conversation_messages(conv_id, 100, 0).unwrap();
        assert_eq!(msgs.len(), 3);
    }

    #[test]
    fn delta_cursor_returns_seq_after() {
        let dir = std::env::temp_dir().join(format!("cp-sync-delta-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V2);
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-8";

        sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );

        // Get messages after seq 2 (should return seq 3, 4, 5)
        let delta = store
            .get_conversation_messages_after_seq(conv_id, 2, 100)
            .unwrap();
        assert_eq!(delta.len(), 3);
        assert_eq!(delta[0].get("seq").unwrap().as_i64().unwrap(), 3);
        assert_eq!(delta[2].get("seq").unwrap().as_i64().unwrap(), 5);

        // Get messages after seq 5 (should be empty)
        let empty = store
            .get_conversation_messages_after_seq(conv_id, 5, 100)
            .unwrap();
        assert!(empty.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // P1a: A single Claude JSONL line with text + tool_use produces multiple messages.
    // Before the fix they'd collide on (conversation_id, raw_hash) and only the first survived.
    #[test]
    fn multi_message_line_all_inserted() {
        let dir = std::env::temp_dir().join(format!("cp-sync-multi-{}", std::process::id()));
        // Claude format: one assistant line with text block + tool_use block
        let content = r#"{"type":"user","message":{"role":"user","content":"fix it"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"call_1","name":"Read","input":{"file_path":"/src/main.rs"}}]}}
"#;
        std::fs::create_dir_all(&dir).unwrap();
        let conv_id = "multi-test-session";
        let jsonl_path = dir.join(format!("{conv_id}.jsonl"));
        std::fs::write(&jsonl_path, content).unwrap();

        let store = AuditStore::open_in_memory().unwrap();

        // Pass transcript_path directly to avoid $HOME-dependent project dir resolution.
        let r = sync_conversation_messages(
            &store,
            "claude_code",
            conv_id,
            conv_id,
            None,
            Some(jsonl_path.to_str().unwrap()),
        );

        // Line 1: user → 1 message. Line 2: assistant text + tool_use → 2 messages. Total = 3.
        assert_eq!(
            r.messages_synced, 3,
            "expected 3 messages (user + assistant text + tool_use)"
        );
        assert_eq!(r.total_messages, 3);
        assert!(r.last_error.is_none());

        let msgs = store.get_conversation_messages(conv_id, 100, 0).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].get("role").unwrap().as_str().unwrap(), "user");
        assert_eq!(msgs[1].get("role").unwrap().as_str().unwrap(), "assistant");
        assert_eq!(
            msgs[2].get("role").unwrap().as_str().unwrap(),
            "tool_summary"
        );
        assert_eq!(msgs[2].get("tool_name").unwrap().as_str().unwrap(), "Read");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // P2a: line_offset > actual file lines should trigger a rebuild, not silently skip.
    #[test]
    fn line_offset_exceeding_file_lines_triggers_rebuild() {
        let dir = std::env::temp_dir().join(format!("cp-sync-overflow-{}", std::process::id()));
        let transcript_path = setup_codex_jsonl(&dir, CODEX_JSONL_V1); // 3 lines
        let store = AuditStore::open_in_memory().unwrap();
        let conv_id = "test-conv-overflow";

        // First sync: 3 messages
        let r1 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        assert_eq!(r1.messages_synced, 3);

        // Tamper: set line_offset to 100 (way more than 3 lines in file)
        let tampered = SyncStateRow {
            conversation_id: conv_id.to_string(),
            transcript_path: Some(transcript_path.clone()),
            file_size_bytes: 1000, // pretend file is bigger
            file_mtime_ms: 0,
            line_offset: 100,
            line_count: 100,
            message_count: 3,
            last_synced_at: Some("2026-01-01T00:00:00Z".to_string()),
            last_error: None,
        };
        store.upsert_sync_state(&tampered).unwrap();

        // Re-sync — should detect mismatch, rebuild, and re-read all 3 lines
        let r2 = sync_conversation_messages(
            &store,
            "codex_cli",
            conv_id,
            conv_id,
            None,
            Some(&transcript_path),
        );
        // After rebuild, all 3 messages should be present (dedup via hash means 0 new inserts,
        // but the cache should still have 3 total)
        assert_eq!(
            r2.total_messages, 3,
            "cache should have 3 messages after rebuild"
        );

        let state = store.get_sync_state(conv_id).unwrap().unwrap();
        assert_eq!(
            state.line_offset, 3,
            "line_offset should be reset to actual line count"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
