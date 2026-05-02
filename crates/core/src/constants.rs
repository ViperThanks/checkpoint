//! 集中常量 — 分页上限、截断长度、聚合窗口等。
//!
//! 消除散落在各处的魔数；所有数值改动只需改这一处。

// 标题 & 文本截断
pub const TITLE_MAX_LEN: usize = 80;
pub const TOOL_INPUT_PREVIEW_LEN: usize = 160;
pub const TOOL_INPUT_FULL_LEN: usize = 2000;
pub const TOOL_SUMMARY_PREVIEW_LEN: usize = 120;

// 分页默认值
pub const DEFAULT_PAGE_SIZE: usize = 20;
pub const MAX_PAGE_SIZE: usize = 100;
pub const DEFAULT_ACTIVITY_PAGE_SIZE: usize = 50;
pub const MAX_ACTIVITY_PAGE_SIZE: usize = 200;
pub const DEFAULT_MESSAGES_PAGE_SIZE: usize = 100;
pub const MAX_MESSAGES_PAGE_SIZE: usize = 500;
pub const COMPACT_QUERY_LIMIT: usize = 5000;

// 聚合窗口
pub const AGGREGATION_WINDOW_SECS: i64 = 30;

// Codex 导入
pub const CODEX_IMPORT_MIN_LIMIT: usize = 250;

// Runtime Identity
pub const RUNTIME_UNKNOWN: &str = "unknown";
pub const PERMISSION_MODE_BYPASS: &str = "bypassPermissions";
pub const PERMISSION_MODE_DEFAULT: &str = "default";

// Resume Cost Guard
// token_count = chars/4 estimate
pub const RESUME_TOKEN_WARNING: i64 = 100_000;
pub const RESUME_TOKEN_CRITICAL: i64 = 500_000;
pub const RESUME_FILE_SIZE_WARNING: i64 = 5 * 1024 * 1024;
pub const RESUME_FILE_SIZE_CRITICAL: i64 = 20 * 1024 * 1024;
pub const RESUME_MESSAGE_WARNING: i64 = 200;
pub const RESUME_MESSAGE_CRITICAL: i64 = 1_000;
