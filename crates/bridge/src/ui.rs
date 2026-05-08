//! 嵌入式前端 — 编译时将 HTML/CSS/JS 打包进二进制。
//!
//! 架构角色：通过 include_str! 在编译时将 ui/ 目录下的所有前端资源
//! 拼接为一个完整的 HTML 页面常量，由 GET / 返回。
//! 无需外部文件服务器，单个二进制即可提供 Dashboard UI。
//!
//! JS 注入顺序（不可颠倒）：
//! 1. marked.min.js — markdown 解析器
//! 2. view_model.js — 纯函数（escHtml, jsStr, shortId 等）
//! 3. render.js — renderMd, copyCodeBlock
//! 4. api_client.js — HTTP 请求封装
//! 5. job_body.js — job body 构造原语
//! 6. runtime_health.js — 运行环境健康 UI
//! 7. activity_segment.js — 活动时间线折叠（buildSegments, renderSegmentCard, renderTurnBanner）
//! 8. approval_review.js — 审批 review payload 统一渲染
//! 9. bridge shell JS — app.js, components.js, tabs/*

/// 完整的 Dashboard HTML 页面，包含所有 CSS 和 JS。
/// 修改 ui/ 目录下的文件后需要重新编译才能生效。
pub const INDEX_HTML: &str = concat!(
    r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=1">
<title>Agent Aspect</title>
<style>"##,
    include_str!("../../shared_ui/design_tokens.css"),
    include_str!("ui/styles.css"),
    r##"</style>
</head>
<body>"##,
    include_str!("ui/index.html"),
    r##"<script>"##,
    // === 共享层（shared_ui）===
    include_str!("../../shared_ui/marked.min.js"),
    include_str!("../../shared_ui/view_model.js"),
    include_str!("../../shared_ui/render.js"),
    include_str!("../../shared_ui/api_client.js"),
    include_str!("../../shared_ui/job_body.js"),
    include_str!("../../shared_ui/runtime_health.js"),
    include_str!("../../shared_ui/activity_segment.js"),
    include_str!("../../shared_ui/approval_review.js"),
    // === Bridge Shell ===
    include_str!("ui/app.js"),
    include_str!("ui/components.js"),
    include_str!("ui/tabs/home.js"),
    include_str!("ui/tabs/conversations.js"),
    include_str!("ui/tabs/events.js"),
    include_str!("ui/tabs/run.js"),
    include_str!("ui/tabs/workflows.js"),
    include_str!("ui/tabs/hooks.js"),
    // === 版本信息 ===
    "\nconsole.log('[agent-aspect-bridge] UI bundle v",
    env!("CARGO_PKG_VERSION"),
    " shell=bridge loaded=marked,view_model,render,api_client,job_body,runtime_health,activity_segment,workflows');\n",
    "\nvar __BUILD_VERSION__ = '",
    env!("CARGO_PKG_VERSION"),
    "';\n",
    r##"</script>
</body>
</html>"##,
);
