//! Bridge HTTP 服务入口 — 启动监听、注册路由、驱动 relay 客户端。
//!
//! 架构角色：整个 bridge 进程的唯一 main。职责包括：
//! 1. 加载配置、生成/读取 Bearer token
//! 2. 单例守护（kill 上一个实例后绑定端口）
//! 3. 初始化共享状态（AppContext、SseBroadcaster、JobRunner）
//! 4. 可选启动 relay WebSocket 客户端
//! 5. thread-per-request 并发处理 HTTP 请求（SSE 已有独立线程）
//! 6. 后台线程定期执行 auto_import_titles
//!
//! 核心不变量：
//! - 端口绑定后立即写入 port 文件和 state 文件，供 CLI 和下次启动发现
//! - SSE (/stream) 是唯一使用独立线程的端点，因为它是长连接
//! - 每个非 SSE 请求在独立线程中处理，DB 通过 Arc<Mutex<>> 串行化
//! - 后台 import 线程每 5 分钟执行一次，与请求线程共享 DB 连接

use agent_aspect_bridge::{
    auth, context::AppContext, jobs, relay_client, routes, scanner, sse, workflows,
};
use agent_aspect_core::config::Config;
use agent_aspect_core::paths;
use agent_aspect_core::provider_registry::ProviderRegistry;
use agent_aspect_core::provider_resolver::ProviderResolver;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 最大并发请求线程数。超出时返回 503。
const MAX_CONCURRENT_REQUESTS: usize = 100;

fn is_loopback(request: &tiny_http::Request) -> bool {
    use std::net::IpAddr;
    request
        .remote_addr()
        .map(|addr| match addr.ip() {
            IpAddr::V4(v4) => v4.is_loopback(),
            IpAddr::V6(v6) => v6.is_loopback(),
        })
        .unwrap_or(false)
}

fn main() {
    // 1. 加载配置：环境变量优先于 config.toml
    let config = Config::load_or_create();
    let config_addr = config.bridge_addr.clone();
    let addr = agent_aspect_core::env_compat::env_var_or("AGENT_ASPECT_BRIDGE_ADDR", config_addr);

    // 2. 加载或生成 Bearer token（首次启动时原子创建文件）
    let token = auth::load_or_create_token();

    // 3. 单例守护：杀掉上一个实例再绑定端口，避免端口冲突
    let state_path = paths::bridge_state_path();
    if let Some(old_pid) =
        agent_aspect_core::process_guard::kill_existing(&state_path, "agent-aspect-bridge")
    {
        eprintln!("agent-aspect-bridge: replaced previous instance (pid {old_pid})");
    }

    // 4. 初始化共享状态
    let broadcaster = sse::SseBroadcaster::shared();
    let registry = ProviderRegistry::from_config(&config);
    let resolver = ProviderResolver::from_config(&config, &registry);
    let ctx = AppContext::new(&paths::audit_db_path(), resolver.clone(), registry.clone())
        .unwrap_or_else(|e| {
            eprintln!("agent-aspect-bridge: {e}");
            std::process::exit(1);
        });

    // 4.5 Bootstrap 默认用户（sys_user 为空时自动创建 admin）
    {
        let store = ctx.store.lock().unwrap();
        auth::bootstrap_owner_user(&store);
    }

    let agent_prompt_timeout_secs = config.agent_prompt_timeout_secs.max(600);
    if agent_prompt_timeout_secs != config.agent_prompt_timeout_secs {
        eprintln!(
            "agent-aspect-bridge: agent_prompt_timeout_secs={} too low, using minimum {}s",
            config.agent_prompt_timeout_secs, agent_prompt_timeout_secs
        );
    }

    let job_runner = Arc::new(jobs::JobRunner::new(
        paths::audit_db_path(), // job runner 打开独立 DB 连接用于内部操作
        config.job_timeout_secs,
        agent_prompt_timeout_secs,
        config.job_max_output_kb,
        broadcaster.clone(),
        resolver.clone(),
        registry.clone(),
        ctx.store.clone(), // shared store for CompletionSink
    ));

    let workflow_runner = Arc::new(workflows::WorkflowRunner::new(
        paths::audit_db_path(),
        broadcaster.clone(),
        job_runner.clone(),
    ));

    // 5. 启动后台 auto_import 线程：每 5 分钟执行一次标题导入
    //    使用独立 DB 连接，避免长时间持锁阻塞请求线程。
    {
        let db_path = paths::audit_db_path();
        std::thread::spawn(move || {
            // 启动时立即执行一次，之后每 5 分钟执行
            let run_bg = || {
                if let Ok(store) = agent_aspect_core::audit::AuditStore::open(&db_path) {
                    routes::auto_import_titles_bg(&store, 10);
                    routes::warm_uncached_stats_bg(&store, 50);
                    routes::invalidate_overview_cache();
                }
            };
            run_bg();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(300));
                run_bg();
            }
        });
    }

    // 5.5 启动 transcript scanner 后台线程：定时扫描活跃 observer 的 transcript 增量
    {
        let scanner_db_path = paths::audit_db_path();
        let scanner_sink = agent_aspect_bridge::completion::CompletionSink::new(
            ctx.store.clone(),
            broadcaster.clone(),
        );
        std::thread::spawn(move || {
            scanner::start_scanner_loop(scanner_db_path, 5, scanner_sink);
        });
    }

    // 6. 绑定端口
    let server = tiny_http::Server::http(&addr).unwrap_or_else(|e| {
        eprintln!("agent-aspect-bridge: bind {addr} failed: {e}");
        std::process::exit(1);
    });

    // 7. 写入端口文件和进程状态文件，供 CLI 发现本实例
    let port_path = paths::bridge_port_path();
    let actual_port = server.server_addr().to_ip().unwrap().port();
    if let Some(parent) = port_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&port_path, actual_port.to_string()).ok();

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let state = serde_json::json!({
        "pid": std::process::id(),
        "exe": exe,
        "addr": format!("127.0.0.1:{actual_port}"),
        "started_at": chrono::Local::now().to_rfc3339(),
    });
    std::fs::write(&state_path, state.to_string()).ok();

    eprintln!("agent-aspect-bridge: listening on {addr}");
    eprintln!(
        "agent-aspect-bridge: token at {}",
        paths::bridge_token_path().display()
    );

    // 8. 可选启动 relay 客户端（配置了 relay_url 时才连接）
    let relay_url_env = agent_aspect_core::env_compat::env_var("AGENT_ASPECT_RELAY_URL");
    let relay_url = relay_url_env
        .as_deref()
        .or(config.relay_url.as_deref())
        .map(String::from);

    if let Some(url) = relay_url {
        match auth::ensure_relay_tokens(&url) {
            Ok(relay_tokens) => {
                let actual_port_for_relay = actual_port;
                relay_client::spawn_relay_client(relay_client::RelayConfig {
                    relay_url: url,
                    mac_token: relay_tokens.mac_token,
                    client_token: relay_tokens.client_token,
                    bridge_token: token.clone(),
                    bridge_port: actual_port_for_relay,
                });
                eprintln!(
                    "agent-aspect-bridge: relay client token (for phone) at {}",
                    paths::relay_client_token_path().display()
                );
            }
            Err(e) => {
                eprintln!("agent-aspect-bridge: relay: {e}");
            }
        }
    }

    // 9. HTTP 请求主循环：thread-per-request + 并发上限
    let active_count = Arc::new(AtomicUsize::new(0));
    for mut request in server.incoming_requests() {
        let queue_start = std::time::Instant::now();
        let path = request.url().split('?').next().unwrap_or("/").to_string();
        let method = request.method().clone();
        let is_get = request.method() == &tiny_http::Method::Get;
        let is_post = request.method() == &tiny_http::Method::Post;
        let is_put = request.method() == &tiny_http::Method::Put;
        let is_delete = request.method() == &tiny_http::Method::Delete;

        // SSE 端点：已在独立线程中处理，直接 continue
        if is_get && path == "/stream" {
            if !sse::check_query_auth(request.url(), &token) {
                let response =
                    routes::json_response(403, &serde_json::json!({"error": "unauthorized"}));
                let _ = request.respond(response);
            } else {
                let rx = broadcaster.lock().unwrap().add_client();
                match rx {
                    Some(rx) => std::thread::spawn(move || {
                        sse::handle_sse_raw(request, rx);
                    }),
                    None => {
                        let _ = request.respond(routes::json_response(
                            429,
                            &serde_json::json!({"error": "too many SSE connections"}),
                        ));
                        continue;
                    }
                };
            }
            continue;
        }

        // 克隆共享状态，移入请求线程
        let ctx = ctx.clone();
        let broadcaster = broadcaster.clone();
        let job_runner = job_runner.clone();
        let workflow_runner = workflow_runner.clone();
        let token = token.clone();
        let sem = active_count.clone();

        if sem.fetch_add(1, Ordering::Relaxed) >= MAX_CONCURRENT_REQUESTS {
            sem.fetch_sub(1, Ordering::Relaxed);
            let _ = request.respond(routes::json_response(
                503,
                &serde_json::json!({"error": "server busy, try again"}),
            ));
            continue;
        }

        std::thread::spawn(move || {
            let timing = routes::RequestTiming::new(queue_start);

            // 无认证的公开端点
            let response = match (is_get, is_post, path.as_str()) {
                (true, _, "/") => routes::handle_index(),
                (true, _, "/health") => routes::handle_health(),
                (_, true, "/login") => {
                    if !is_loopback(&request) {
                        routes::json_response(
                            403,
                            &serde_json::json!({"error": "login only allowed from loopback"}),
                        )
                    } else {
                        routes::handle_post_login(&ctx, &mut request, &token)
                    }
                }
                (_, true, "/password/change") => {
                    if !is_loopback(&request) {
                        routes::json_response(
                            403,
                            &serde_json::json!({"error": "password change only allowed from loopback"}),
                        )
                    } else if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_post_password_change(&ctx, &mut request)
                    }
                }
                (true, _, "/beat") => routes::handle_beat(),
                (true, _, "/mode") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_mode()
                    }
                }
                (_, true, "/mode") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_post_mode(&mut request, &broadcaster)
                    }
                }
                (true, _, "/rules") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_rules()
                    }
                }
                (true, _, "/job-kinds") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_job_kinds()
                    }
                }

                (true, _, "/relay/status") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_relay_status()
                    }
                }
                (true, _, "/relay/pairing") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_relay_pairing()
                    }
                }
                (true, _, "/hook-status") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_hook_status()
                    }
                }
                (_, true, "/hook-config") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_post_hook_config(&mut request, &broadcaster)
                    }
                }
                (true, _, "/events") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_events(&ctx, &request)
                    }
                }
                (true, _, p) if p.starts_with("/events/") && !p.ends_with("/feedback") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let event_id = &p["/events/".len()..];
                        if event_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing event_id"}),
                            )
                        } else {
                            routes::handle_get_event(&ctx, event_id)
                        }
                    }
                }
                (_, true, "/decide") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_post_decide(&ctx, &mut request, &broadcaster)
                    }
                }
                (true, _, "/pending") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_pending(&ctx)
                    }
                }
                (true, _, "/mobile/summary") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_mobile_summary(&ctx)
                    }
                }
                (_, true, p) if p.starts_with("/events/") && p.ends_with("/feedback") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let event_id = &p["/events/".len()..p.len() - "/feedback".len()];
                        if event_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing event_id"}),
                            )
                        } else {
                            routes::handle_post_feedback(&ctx, event_id, &mut request)
                        }
                    }
                }

                (true, _, "/devices") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_devices(&ctx, &request)
                    }
                }
                (_, _, p) if is_put && p.starts_with("/devices/") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let device_id = &p["/devices/".len()..];
                        if device_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing device_id"}),
                            )
                        } else {
                            routes::handle_put_device_label(&ctx, device_id, &mut request)
                        }
                    }
                }

                // Learn mode
                (true, _, "/learn/suggestions") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_learn_suggestions(&ctx)
                    }
                }
                (_, true, p) if p.starts_with("/learn/suggestions/") && p.ends_with("/accept") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let id = &p["/learn/suggestions/".len()..p.len() - "/accept".len()];
                        if id.is_empty() {
                            routes::json_response(400, &serde_json::json!({"error": "missing id"}))
                        } else {
                            routes::handle_post_suggestion_action(&ctx, id, "accepted")
                        }
                    }
                }
                (_, true, p) if p.starts_with("/learn/suggestions/") && p.ends_with("/dismiss") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let id = &p["/learn/suggestions/".len()..p.len() - "/dismiss".len()];
                        if id.is_empty() {
                            routes::json_response(400, &serde_json::json!({"error": "missing id"}))
                        } else {
                            routes::handle_post_suggestion_action(&ctx, id, "rejected")
                        }
                    }
                }

                (true, _, "/learn/rules") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_learn_rules(&ctx)
                    }
                }

                // Conversation routes
                (true, _, "/overview") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_overview(&ctx, &request)
                    }
                }
                (true, _, "/conversations") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_conversations(&ctx, &request)
                    }
                }
                (true, _, p)
                    if p.starts_with("/conversations/") && p.ends_with("/runtime-check") =>
                {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let cid = &p["/conversations/".len()..p.len() - "/runtime-check".len()];
                        if cid.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing conversation id"}),
                            )
                        } else {
                            routes::handle_get_conversation_runtime_check(&ctx, cid)
                        }
                    }
                }

                (true, _, p) if p.starts_with("/conversations/") && p.ends_with("/events") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let cid = &p["/conversations/".len()..p.len() - "/events".len()];
                        if cid.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing conversation id"}),
                            )
                        } else {
                            routes::handle_get_conversation_events(&ctx, cid, &request)
                        }
                    }
                }

                (true, _, p) if p.starts_with("/conversations/") && p.ends_with("/activity") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let cid = &p["/conversations/".len()..p.len() - "/activity".len()];
                        if cid.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing conversation id"}),
                            )
                        } else {
                            routes::handle_get_conversation_activity(&ctx, cid, &request)
                        }
                    }
                }

                (true, _, p)
                    if p.starts_with("/conversations/") && p.ends_with("/messages/delta") =>
                {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let cid = &p["/conversations/".len()..p.len() - "/messages/delta".len()];
                        if cid.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing conversation id"}),
                            )
                        } else {
                            routes::handle_get_conversation_messages_delta(&ctx, cid, &request)
                        }
                    }
                }

                (true, _, p) if p.starts_with("/conversations/") && p.ends_with("/messages") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let cid = &p["/conversations/".len()..p.len() - "/messages".len()];
                        if cid.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing conversation id"}),
                            )
                        } else {
                            routes::handle_get_conversation_messages(&ctx, cid, &request)
                        }
                    }
                }

                (true, _, p) if p.starts_with("/conversations/") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let cid = &p["/conversations/".len()..];
                        if cid.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing conversation id"}),
                            )
                        } else {
                            routes::handle_get_conversation(&ctx, cid)
                        }
                    }
                }

                // Workflow routes
                (_, true, "/workflows") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        workflows::handle_post_workflows(&ctx, &mut request, &workflow_runner)
                    }
                }
                (true, _, "/workflows") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        workflows::handle_get_workflows(&ctx, &request)
                    }
                }
                (_, true, p) if p.starts_with("/workflows/") && p.ends_with("/run") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..p.len() - "/run".len()];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_post_workflow_run(wf_id, &workflow_runner)
                        }
                    }
                }
                (_, true, p) if p.starts_with("/workflows/") && p.ends_with("/next-step") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..p.len() - "/next-step".len()];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_post_workflow_next_step(wf_id, &ctx)
                        }
                    }
                }
                (_, true, p) if p.starts_with("/workflows/") && p.ends_with("/cancel") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..p.len() - "/cancel".len()];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_post_workflow_cancel(wf_id, &workflow_runner)
                        }
                    }
                }
                // PUT /workflows/:id/steps/reorder — 重排序步骤
                (_, _, p)
                    if is_put && p.starts_with("/workflows/") && p.ends_with("/steps/reorder") =>
                {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..p.len() - "/steps/reorder".len()];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_put_workflow_steps_reorder(&ctx, wf_id, &mut request)
                        }
                    }
                }
                // PUT /workflows/:id — 更新工作流
                (_, _, p) if is_put && p.starts_with("/workflows/") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_put_workflow(&ctx, wf_id, &mut request)
                        }
                    }
                }
                // DELETE /workflows/:id — 删除工作流
                (_, _, p) if is_delete && p.starts_with("/workflows/") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_delete_workflow(&ctx, wf_id)
                        }
                    }
                }
                // GET /workflows/:id/steps/:step_id/logs — 获取步骤的 job 日志
                (true, _, p)
                    if p.starts_with("/workflows/")
                        && p.contains("/steps/")
                        && p.ends_with("/logs") =>
                {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        // 解析 /workflows/{wf_id}/steps/{step_id}/logs
                        let path = &p["/workflows/".len()..];
                        let parts: Vec<&str> = path.splitn(3, "/steps/").collect();
                        if parts.len() == 2 {
                            let wf_id = parts[0];
                            let step_and_rest = parts[1];
                            let step_id = step_and_rest.trim_end_matches("/logs");
                            if wf_id.is_empty() || step_id.is_empty() {
                                routes::json_response(
                                    400,
                                    &serde_json::json!({"error": "missing ids"}),
                                )
                            } else {
                                workflows::handle_get_workflow_step_logs(
                                    &ctx, wf_id, step_id, &request,
                                )
                            }
                        } else {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "invalid path"}),
                            )
                        }
                    }
                }
                (true, _, p) if p.starts_with("/workflows/") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let wf_id = &p["/workflows/".len()..];
                        if wf_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing workflow id"}),
                            )
                        } else {
                            workflows::handle_get_workflow(&ctx, wf_id)
                        }
                    }
                }

                // Job routes
                (true, _, "/run/context") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        routes::handle_get_run_context(&ctx)
                    }
                }
                (_, true, "/jobs") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        jobs::handle_post_jobs(&mut request, &job_runner)
                    }
                }
                (true, _, "/jobs") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        jobs::handle_get_jobs(&request, &job_runner)
                    }
                }
                (true, _, p) if p.starts_with("/jobs/") && p.ends_with("/logs") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let job_id = &p["/jobs/".len()..p.len() - "/logs".len()];
                        if job_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing job_id"}),
                            )
                        } else {
                            jobs::handle_get_job_logs(job_id, &job_runner)
                        }
                    }
                }
                (_, true, p) if p.starts_with("/jobs/") && p.ends_with("/logs/delta") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let job_id = &p["/jobs/".len()..p.len() - "/logs/delta".len()];
                        if job_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing job_id"}),
                            )
                        } else {
                            jobs::handle_post_job_logs_delta(job_id, &mut request, &job_runner)
                        }
                    }
                }
                (_, true, p) if p.starts_with("/jobs/") && p.ends_with("/cancel") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let job_id = &p["/jobs/".len()..p.len() - "/cancel".len()];
                        if job_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing job_id"}),
                            )
                        } else {
                            jobs::handle_post_cancel(job_id, &job_runner)
                        }
                    }
                }
                (true, _, p) if p.starts_with("/jobs/") => {
                    if !auth::check_auth(&request, &token) {
                        routes::json_response(403, &serde_json::json!({"error": "unauthorized"}))
                    } else {
                        let job_id = &p["/jobs/".len()..];
                        if job_id.is_empty() {
                            routes::json_response(
                                400,
                                &serde_json::json!({"error": "missing job_id"}),
                            )
                        } else {
                            jobs::handle_get_job(job_id, &job_runner)
                        }
                    }
                }

                _ => routes::json_response(404, &serde_json::json!({"error": "not found"})),
            };

            if let Err(e) = request.respond(response) {
                eprintln!("agent-aspect-bridge: respond error: {e}");
            }

            timing.log(&method, &path);
            sem.fetch_sub(1, Ordering::Relaxed);
        });
    }
}
