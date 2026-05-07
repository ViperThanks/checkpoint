//! `agent-aspect conversations` — 会话管理命令。
//!
//! 目前只有一个子命令 `import-titles`：从 provider 的 transcript 文件
//! 导入真实会话标题，替换 audit.db 中的 fallback 标题。

use agent_aspect_core::audit::AuditStore;
use agent_aspect_core::paths;
use agent_aspect_core::title_import;

/// conversations 子命令入口。
pub fn cmd_conversations(sub: Option<&str>) {
    match sub {
        Some("import-titles") => import_titles(),
        _ => {
            eprintln!("usage: agent-aspect conversations <import-titles>");
            std::process::exit(1);
        }
    }
}

/// 从 provider transcript 导入真实标题。
///
/// 流程：
/// 1. 查询 audit.db 中最多 500 条使用 fallback 标题的会话
/// 2. 对每条会话调用 `title_import::import_title_for` 提取真实标题
/// 3. 写回 audit.db，输出导入统计
fn import_titles() {
    let db_path = paths::audit_db_path();
    let store = match AuditStore::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("FAIL: cannot open audit db: {e}");
            std::process::exit(1);
        }
    };

    let conversations = match store.list_conversations_for_title_import(500) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: query conversations: {e}");
            std::process::exit(1);
        }
    };

    if conversations.is_empty() {
        println!("No conversations with fallback titles found.");
        return;
    }

    let total = conversations.len();
    let mut updated = 0;

    for conv in &conversations {
        let result = title_import::import_title_for(
            &conv.agent,
            &conv.conversation_id,
            conv.project_path.as_deref(),
            conv.transcript_path.as_deref(),
        );

        if let Some((title, source)) = result {
            if let Err(e) = store.update_conversation_title(&conv.id, &title, &source) {
                eprintln!("  error updating {}: {e}", conv.id);
                continue;
            }
            updated += 1;
            println!("  [{source}] {title}");
        }
    }

    println!();
    println!("Updated {updated} / {total} conversations.");
}
