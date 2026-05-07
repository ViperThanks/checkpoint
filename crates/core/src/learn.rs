//! 学习引擎 — 分析审计历史，生成自动允许建议。
//!
//! 扫描用户一致批准（ask → allow，无 deny）的工具使用模式，
//! 当同一模式出现 >= 3 次时生成建议。所有建议需用户显式接受后才生效。
//!
//! 核心不变量：
//! - 同一模式 (agent, tool_name, path_dir) 只生成一条建议（幂等）
//! - 只有 ask → allow 且从未 deny 的模式才会被建议
//! - 无新决策时不重复扫描

use crate::audit::{AuditStore, DecisionRow, SuggestionRow};
use std::collections::HashMap;

const MIN_SAMPLE_COUNT: usize = 3;

/// Pattern key for grouping similar decisions.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
struct PatternKey {
    agent: String,
    tool_name: String,
    project_path: Option<String>,
    path_dir: Option<String>,
}

/// Generate suggestions from audit history.
///
/// Logic:
/// 1. Query all decisions (up to 10000)
/// 2. Find event_ids where action was "ask" then "allow" (never "deny")
/// 3. Group by (agent, tool_name, path_directory)
/// 4. For groups with >= MIN_SAMPLE_COUNT, create a suggestion
/// 5. Skip patterns that already have a suggestion in the DB
pub fn generate_suggestions(
    store: &AuditStore,
) -> crate::error::AgentAspectResult<Vec<SuggestionRow>> {
    // Skip regeneration if no new decisions since last generation
    if let Ok(Some(last_gen)) = store.latest_suggestion_created_at() {
        if let Ok(0) = store.decision_count_since(&last_gen) {
            return Ok(vec![]);
        }
    }

    let decisions = store.query_decisions(10000, 0, None, None, None, None, None, false)?;

    // Build map: event_id -> Vec<&DecisionRow>
    let mut event_actions: HashMap<String, Vec<&DecisionRow>> = HashMap::new();
    for d in &decisions {
        event_actions.entry(d.event_id.clone()).or_default().push(d);
    }

    // Filter: events where "ask" was followed by "allow" but never "deny"
    let mut ask_allowed: Vec<&DecisionRow> = Vec::new();
    for (_, dlist) in &event_actions {
        let was_asked = dlist.iter().any(|d| d.action == "ask");
        let was_denied = dlist.iter().any(|d| d.action == "deny");
        let was_allowed = dlist.iter().any(|d| d.action == "allow");
        if was_asked && was_allowed && !was_denied {
            if let Some(ask_d) = dlist.iter().find(|d| d.action == "ask") {
                ask_allowed.push(ask_d);
            }
        }
    }

    // Group by pattern
    let mut groups: HashMap<PatternKey, Vec<&DecisionRow>> = HashMap::new();
    for d in &ask_allowed {
        let path_dir = d.file_path.as_ref().map(|fp| {
            let parent = std::path::Path::new(fp)
                .parent()
                .map(|p| p.to_string_lossy().to_string())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| "*".to_string());
            parent
        });
        let key = PatternKey {
            agent: d.agent.clone(),
            tool_name: d.tool_name.clone(),
            project_path: None, // DecisionRow doesn't have project_path
            path_dir,
        };
        groups.entry(key).or_default().push(*d);
    }

    let mut suggestions = Vec::new();
    let now = chrono::Utc::now().to_rfc3339();

    for (key, events) in &groups {
        if events.len() < MIN_SAMPLE_COUNT {
            continue;
        }

        let pattern_str = key.path_dir.as_deref().unwrap_or("*");

        // Skip if suggestion already exists for this pattern
        if store
            .suggestion_exists(&key.agent, &key.tool_name, pattern_str)
            .unwrap_or(false)
        {
            continue;
        }

        let sample_ids: Vec<String> = events.iter().take(10).map(|e| e.event_id.clone()).collect();
        let title = format!(
            "Auto-allow {} -> {} ({})",
            key.agent, key.tool_name, pattern_str
        );
        let reason = format!(
            "You approved {} {} calls{} — all allowed, none denied.",
            events.len(),
            key.tool_name,
            key.path_dir
                .as_ref()
                .map(|d| format!(" under {}", d))
                .unwrap_or_default()
        );

        suggestions.push(SuggestionRow {
            id: pattern_hash(&format!("{}|{}|{}", key.agent, key.tool_name, pattern_str)),
            title,
            reason,
            confidence: 1.0, // All were allowed, none denied
            agent: key.agent.clone(),
            tool_name: key.tool_name.clone(),
            project_path: key.project_path.clone(),
            pattern: pattern_str.to_string(),
            sample_event_ids: sample_ids,
            sample_count: events.len(),
            suggested_action: "allow".to_string(),
            status: "pending".to_string(),
            created_at: now.clone(),
            resolved_at: None,
        });
    }

    // Persist new suggestions
    for s in &suggestions {
        if let Err(e) = store.insert_suggestion(s) {
            eprintln!("learn: insert suggestion failed: {e}");
        }
    }

    Ok(suggestions)
}

fn pattern_hash(input: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_suggestions_from_approved_events() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");

        // Insert 3 events with ask->allow pattern (same agent/tool/path_dir)
        for i in 0..3 {
            let eid = format!("evt-{}", i);
            store
                .insert_event(
                    &eid,
                    "before",
                    "tool.request",
                    "claude_code",
                    "Bash",
                    Some(&format!("/tmp/project/run{}.sh", i)),
                    &format!("2026-04-25T10:0{}:00Z", i),
                    "{}",
                )
                .unwrap();
            store
                .insert_decision(&eid, "ask", None, "", &format!("2026-04-25T10:0{}:01Z", i))
                .unwrap();
            store
                .insert_decision(
                    &eid,
                    "allow",
                    None,
                    "",
                    &format!("2026-04-25T10:0{}:02Z", i),
                )
                .unwrap();
        }

        // Insert 1 event with ask->deny (should not generate suggestion)
        store
            .insert_event(
                "evt-deny",
                "before",
                "tool.request",
                "claude_code",
                "Bash",
                Some("/tmp/project/bad.sh"),
                "2026-04-25T10:10:00Z",
                "{}",
            )
            .unwrap();
        store
            .insert_decision("evt-deny", "ask", None, "", "2026-04-25T10:10:01Z")
            .unwrap();
        store
            .insert_decision("evt-deny", "deny", None, "", "2026-04-25T10:10:02Z")
            .unwrap();

        let suggestions = generate_suggestions(&store).expect("generate suggestions");

        assert!(
            suggestions.len() >= 1,
            "should generate at least one suggestion"
        );

        let s = suggestions
            .iter()
            .find(|s| s.tool_name == "Bash" && s.agent == "claude_code")
            .expect("should find Bash suggestion");

        assert_eq!(s.status, "pending");
        assert_eq!(s.suggested_action, "allow");
        assert!(s.sample_count >= 3);
        assert_eq!(s.confidence, 1.0);
    }

    #[test]
    fn no_suggestion_when_fewer_than_min_samples() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");

        // Only 2 ask->allow events (below MIN_SAMPLE_COUNT of 3)
        for i in 0..2 {
            let eid = format!("evt-few-{}", i);
            store
                .insert_event(
                    &eid,
                    "before",
                    "tool.request",
                    "claude_code",
                    "Edit",
                    Some(&format!("/tmp/edit/file{}.rs", i)),
                    &format!("2026-04-25T11:0{}:00Z", i),
                    "{}",
                )
                .unwrap();
            store
                .insert_decision(&eid, "ask", None, "", &format!("2026-04-25T11:0{}:01Z", i))
                .unwrap();
            store
                .insert_decision(
                    &eid,
                    "allow",
                    None,
                    "",
                    &format!("2026-04-25T11:0{}:02Z", i),
                )
                .unwrap();
        }

        let suggestions = generate_suggestions(&store).expect("generate suggestions");
        assert!(
            suggestions.is_empty(),
            "should not generate suggestion with fewer than {} samples",
            MIN_SAMPLE_COUNT
        );
    }

    #[test]
    fn deduplication_prevents_duplicate_suggestions() {
        let store = AuditStore::open_in_memory().expect("open in-memory db");

        for i in 0..3 {
            let eid = format!("evt-dedup-{}", i);
            store
                .insert_event(
                    &eid,
                    "before",
                    "tool.request",
                    "claude_code",
                    "Write",
                    Some(&format!("/tmp/write/file{}.rs", i)),
                    &format!("2026-04-25T12:0{}:00Z", i),
                    "{}",
                )
                .unwrap();
            store
                .insert_decision(&eid, "ask", None, "", &format!("2026-04-25T12:0{}:01Z", i))
                .unwrap();
            store
                .insert_decision(
                    &eid,
                    "allow",
                    None,
                    "",
                    &format!("2026-04-25T12:0{}:02Z", i),
                )
                .unwrap();
        }

        // First call generates the suggestion
        let first = generate_suggestions(&store).expect("first generation");
        assert!(!first.is_empty());

        // Second call should not produce duplicates
        let second = generate_suggestions(&store).expect("second generation");
        assert!(
            second.is_empty(),
            "should not generate duplicate suggestions"
        );
    }
}
