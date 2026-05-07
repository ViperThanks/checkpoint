//! `agent-aspect audit` — 查询最近审计决策记录。
//!
//! 直接读 audit.db，不走 IPC。支持 `agent-aspect audit <N>` 指定条数，默认 20。
//! 输出时间戳截断到秒（前 19 字符），避免 ISO 精度过长影响可读性。

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::paths;

/// 显示最近的审计决策记录。
///
/// 从第二个位置参数读取 limit（默认 20），查询 `recent_decisions(limit)`
/// 并以表格形式输出 TIME / ACTION / RULE / TOOL / PATH 五列。
pub fn cmd_audit() {
    let db_path = paths::audit_db_path();

    if !db_path.exists() {
        eprintln!("audit.db not found at {}", db_path.display());
        return;
    }

    let store = match AuditStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open audit.db: {e}");
            return;
        }
    };

    // 第二个参数可选，指定查询条数
    let limit: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);

    match store.recent_decisions(limit) {
        Ok(rows) => {
            if rows.is_empty() {
                println!("no audit entries");
                return;
            }
            println!(
                "{:<20} {:<8} {:<6} {:<12} {}",
                "TIME", "ACTION", "RULE", "TOOL", "PATH"
            );
            println!("{}", "-".repeat(80));
            for r in &rows {
                // ISO 时间戳截断到秒级（"YYYY-MM-DDTHH:MM:SS"），去掉毫秒和时区
                let short_time = if r.timestamp.len() >= 19 {
                    &r.timestamp[..19]
                } else {
                    &r.timestamp
                };
                let path = r.file_path.as_deref().unwrap_or("-");
                println!(
                    "{:<20} {:<8} {:<6} {:<12} {}",
                    short_time,
                    r.action,
                    r.rule_id.as_deref().unwrap_or("-"),
                    r.tool_name,
                    path,
                );
            }
            println!();
            println!(
                "showing {} of {} decisions",
                rows.len(),
                store.decision_count().unwrap_or(0)
            );
        }
        Err(e) => eprintln!("query failed: {e}"),
    }
}
