//! 手机端 Relay UI 的 HTML 生成。
//!
//! 职责：将 CSS + JS 模块内联注入 HTML 模板，返回完整页面。
//!
//! 架构角色：手机浏览器访问 Relay 根路径 "/" 时返回的 SPA 入口页面。
//!
//! 不变量：
//! - JS 注入顺序：shared_ui 层 → relay shell 层。
//!   顺序不可颠倒，shell 层依赖 shared_ui 层中定义的全局函数。
//! - shared_ui 模块：marked → view_model → render → api_client → job_body → job_status → runtime_health → activity_segment

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::Response;

const HTML_TEMPLATE: &str = include_str!("ui/index.html");
const DESIGN_TOKENS_CSS: &str = include_str!("../../shared_ui/design_tokens.css");
const CSS: &str = include_str!("ui/style.css");

// === 共享层（shared_ui）===
const MARKED_JS: &str = include_str!("../../shared_ui/marked.min.js");
const VIEW_MODEL_JS: &str = include_str!("../../shared_ui/view_model.js");
const RENDER_JS: &str = include_str!("../../shared_ui/render.js");
const API_CLIENT_JS: &str = include_str!("../../shared_ui/api_client.js");
const JOB_BODY_JS: &str = include_str!("../../shared_ui/job_body.js");
const JOB_STATUS_JS: &str = include_str!("../../shared_ui/job_status.js");
const RUNTIME_HEALTH_JS: &str = include_str!("../../shared_ui/runtime_health.js");
const ACTIVITY_SEGMENT_JS: &str = include_str!("../../shared_ui/activity_segment.js");
const APPROVAL_REVIEW_JS: &str = include_str!("../../shared_ui/approval_review.js");

// === Relay Shell ===
const APP_JS: &str = include_str!("ui/app.js");

/// 生成完整的 HTML 页面，将 CSS/JS 内联到模板占位符中。
///
/// CACHE_CONTROL 设为 no-cache，确保手机端始终获取最新版本。
pub async fn serve_ui() -> Response {
    // JS 拼接：shared 层 → shell 层
    let js = MARKED_JS.to_string()
        + "\n"
        + VIEW_MODEL_JS
        + "\n"
        + RENDER_JS
        + "\n"
        + API_CLIENT_JS
        + "\n"
        + JOB_BODY_JS
        + "\n"
        + JOB_STATUS_JS
        + "\n"
        + RUNTIME_HEALTH_JS
        + "\n"
        + ACTIVITY_SEGMENT_JS
        + "\n"
        + APPROVAL_REVIEW_JS
        + "\n"
        + APP_JS
        + "\nconsole.log('[agent-aspect-relay] UI bundle v"
        + env!("CARGO_PKG_VERSION")
        + " build="
        + env!("BUILD_TIME")
        + " shell=relay loaded=marked,view_model,render,api_client,job_body,job_status,runtime_health,activity_segment,approval_review');\n";
    let html = HTML_TEMPLATE
        .replace("/*__CSS__*/", &(DESIGN_TOKENS_CSS.to_string() + "\n" + CSS))
        .replace("/*__JS__*/", &js)
        .replace("__VERSION__", env!("CARGO_PKG_VERSION"))
        .replace("__BUILD_TIME__", env!("BUILD_TIME"));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from(html))
        .unwrap()
}
