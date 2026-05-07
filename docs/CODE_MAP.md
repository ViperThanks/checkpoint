# CODE_MAP.md — Agent Aspect Source Map

> 本文件是所有 Rust 源文件的完整索引，记录每个文件的职责、架构角色和关键不变量。
> 维护时先查此文件定位模块，再进源文件看方法注释。

## 项目架构概览

```
用户操作
  │
  ├─ AI Agent (Claude/Codex/Kimi/Gemini)
  │     └─ hook → hook-cli → Unix Socket → daemon → 规则引擎 → 审计写入
  │
  ├─ 本地 Dashboard (浏览器)
  │     └─ HTTP → Bridge (tiny_http) → routes → AuditStore (SQLite)
  │
  └─ 手机端 Dashboard (浏览器)
        └─ HTTP → Relay (axum) → WebSocket → Bridge → routes → AuditStore
```

---

## docs — 产品与工程计划

| 文件 | 职责 |
|------|------|
| `PLAN.md` | 产品宪法：愿景、架构、阶段路线和事件模型 |
| `plan-m43-conversation-review-ux.md` | M43 会话审阅体验计划：选区残留修复、thinking 折叠结构化、审批 review payload 可配置化 |
| `plan-m41-mac-shell.md` | M41 Mac Shell MVP 计划：SwiftUI + WKWebView 壳、Bridge 生命周期管理、诊断与打包路径 |
| `relay-vps-tokyo.md` | Relay VPS 实际部署记录：vps-tokyo 上的 systemd/nginx/更新流程 |

---

## crates/core — 共享核心库

| 文件 | 职责 |
|------|------|
| `lib.rs` | crate 入口：模块声明 + Mode/RuleSource 的 Display/FromStr |
| `error.rs` | 统一错误类型，覆盖 DB/配置/协议/任务全链路 |
| `constants.rs` | 集中常量：分页上限、截断长度、聚合窗口 |
| `utils.rs` | 通用工具：Unicode 字符截断 + Claude 项目目录解析 |
| `paths.rs` | File system paths: all standard locations under `~/.agent-aspect/` |
| `config.rs` | TOML 配置的加载、保存、默认值生成 |
| `decision.rs` | 决策类型：Action 枚举 + Decision 记录 |
| `event.rs` | 核心事件类型：UnifiedEvent / AgentId / Phase / Risk / Scope / ToolInput |
| `wire.rs` | 线路协议：hook 请求/响应/HookResponse 的数据结构 |
| `adapter.rs` | Provider 适配器 trait 和 Claude/Codex/Kimi/Gemini 四个实现 |
| `normalize.rs` | Provider hook payload 归一化为 UnifiedEvent |
| `provider_resolver.rs` | Provider CLI 二进制发现（配置 > PATH > fallback 目录） |
| `audit.rs` | SQLite 审计存储 facade + schema 迁移 |
| `store/mod.rs` | DAO 层模块入口，按领域拆分 |
| `store/conversations.rs` | 会话 DAO：CRUD、标题优先级、统计缓存、回填 |
| `store/decisions.rs` | 决策 DAO：插入、多维度过滤查询、pending asks |
| `store/devices.rs` | 设备 DAO：注册、查询、标签管理 |
| `store/events.rs` | 事件 DAO：插入（含会话索引）、批量查询、过期清理 |
| `store/feedback.rs` | 反馈 DAO：用户对裁决的 useful/noisy 反馈 |
| `store/jobs.rs` | 任务 DAO：全生命周期管理 + 进程监控 + stale 恢复 |
| `store/messages.rs` | 消息 DAO：增量缓存读写 + 同步状态管理 |
| `store/suggestions.rs` | 建议规则 DAO：学习引擎生成的自动允许建议 |
| `conversation.rs` | 会话管理：ID 生成（SHA-256）、元数据提取、标题推断 |
| `title_import.rs` | 标题导入：从 provider transcript 提取真实标题 |
| `transcript.rs` | Transcript 解析：三 provider 的 JSONL 全量/逐行读取 |
| `transcript_sync.rs` | 增量 transcript 同步：行偏移游标 + 断点续传 |
| `rule.rs` | 规则引擎：10 条内置规则按 Mode 分级评估 |
| `learn.rs` | 学习引擎：分析审计历史生成自动允许建议 |
| `process_guard.rs` | 单实例守护：PID 检测 + 精确进程名验证 + 优雅关闭旧进程 |

---

## crates/bridge — HTTP 服务层（Mac 端 Dashboard + REST API）

| 文件 | 职责 |
|------|------|
| `lib.rs` | crate 根模块，重新导出所有子模块 |
| `main.rs` | HTTP 服务入口：配置加载 → token 生成 → 端口绑定 → 路由分发 → relay 客户端 |
| `auth.rs` | Bearer token 生成/持久化、relay 注册和凭证管理 |
| `context.rs` | 共享应用上下文（AuditStore + ProviderResolver 的容器） |
| `routes.rs` | HTTP 路由处理器：全部 REST API（事件/会话/模式/决策/反馈/活动聚合） |
| `jobs.rs` | Job 编排（排队、执行、超时保护、SSE 日志流、崩溃恢复） |
| `provider.rs` | Provider CLI 命令构建（new/continue 由 conversation_id 判定） |
| `relay_client.rs` | Relay WebSocket 客户端（后台线程代理手机请求到本地 Bridge） |
| `sse.rs` | SSE 广播器（实时向浏览器推送事件） |
| `ui.rs` | 嵌入式前端（编译时将 HTML/CSS/JS 打包进二进制常量） |

### 前端文件

| 文件 | 职责 |
|------|------|
| `ui/tabs/home.js` | Home tab：状态概览、最近会话、最近任务 |
| `ui/tabs/events.js` | Events tab：审计事件列表 + 筛选 |
| `ui/tabs/conversations.js` | Conversations tab：会话列表、详情、继续/新建 |
| `ui/tabs/run.js` | Run tab：agent prompt 提交、job 状态跟踪 |

---

## crates/shared_ui — Bridge / Relay 前端共享层

| 文件 | 职责 |
|------|------|
| `design_tokens.css` | 两端唯一主题 token 来源：颜色、字体、间距、圆角、阴影、light/dark 变量 |
| `view_model.js` | 前端纯函数：转义、截断、时间格式化、agent 标签、toast、日志清理、主题切换 |
| `render.js` | Markdown 渲染和复制按钮：封装 marked + 代码块复制 |
| `api_client.js` | HTTP client 封装：token、JSON 解析、错误规范化 |
| `job_body.js` | job body 唯一构造源：new 永不含 conversation_id，continue 缺 id 必须抛错 |
| `runtime_health.js` | runtime drift / health 的 badge、banner、首页告警渲染 |
| `activity_segment.js` | 活动时间线 view model：连续工具事件合并、`Worked for Xm Ys` 摘要、可展开详情 |
| `approval_review.js` | 审批 review 统一渲染：后端生成 review payload → 前端 `renderApprovalReview` / `renderApprovalReviewCompact` 渲染，桥端/移动端复用 |
| `marked.min.js` | 两端共用的 vendored marked |
| `tests/*.js` | 共享层和两端一致性的 Node 回归测试 |

边界：`shared_ui` 只放业务原语、展示纯函数和设计 token；bridge/relay shell 只负责布局和端侧事件绑定，不得重新定义主题 token 或手写 job body。

---

## crates/relay — 远程代理层（VPS 端，手机 → Mac 中转）

| 文件 | 职责 |
|------|------|
| `lib.rs` | crate 入口：全局状态定义 + axum 服务器启动 |
| `main.rs` | Relay 进程入口 |
| `server.rs` | axum 路由定义 |
| `mobile_ui.rs` | 前端注入：按顺序拼接 job_body.js + app.js → HTML |
| `http.rs` | HTTP 代理层：body 透传不修改，路径白名单控制 |
| `ws.rs` | WebSocket 长连接：Mac Bridge ↔ Relay 双向通信 |
| `register.rs` | Bridge 注册/注销 API：签发 token 对 + 持久化 sid 名册 |
| `token.rs` | HMAC-SHA256 自验证令牌签发和验证 |
| `beat.rs` | 心跳处理器：代理心跳 + 全链路延迟收集 |
| `protocol.rs` | WS 帧协议：ProxyRequest/ProxyResponse/Register 等 |
| `session.rs` | 会话注册表：活跃 WS 连接 + pending request 路由 |

### 手机端前端

| 文件 | 职责 |
|------|------|
| `ui/app.js` | 手机端 shell：状态、渲染、事件绑定；业务原语来自 `crates/shared_ui/` |
| `ui/app_test.js` | 手机端 shell + shared_ui 生产函数测试 |
| `ui/index.html` | HTML 模板 |
| `ui/style.css` | 手机端布局样式；主题 token 来自 `crates/shared_ui/design_tokens.css` |

---

## crates/cli — CLI tool (agent-aspect binary)

| File | Responsibility |
|------|------|
| `lib.rs` | CLI crate root module |
| `main.rs` | agent-aspect binary entry point, manual arg parsing |
| `commands/mod.rs` | 命令子模块注册与 cmd_* 入口函数导出 |
| `commands/status.rs` | 显示 daemon 运行状态、当前模式和审计计数 |
| `commands/rules.rs` | 以 Guard 模式实例化 RuleEngine 并打印默认规则 |
| `commands/audit.rs` | 查询 audit.db 中最近的审计决策记录 |
| `commands/mode.rs` | 查看或设置 config.toml 中的 daemon 运行模式 |
| `commands/doctor.rs` | 11 项安装健康检查（二进制/进程/socket/配置/hooks） |
| `commands/launchd.rs` | macOS launchd 服务管理（plist 生成 + launchctl） |
| `commands/bridge.rs` | Bridge HTTP 服务器生命周期、relay、keep-awake 管理 |
| `commands/init.rs` | 为 Claude Code / Codex CLI / Kimi Code 安装 hook 配置 |
| `commands/conversations.rs` | 从 provider transcript 导入会话标题 |
| `commands/daemon.rs` | agent-aspectd daemon start/stop/restart/status |
| `commands/helpers.rs` | 跨命令共享辅助：按 agent-aspect 新命名定位兄弟二进制和封装 launchctl 调用 |

---

## apps/macos/AgentAspect — macOS 桌面壳（M41）

SwiftPM macOS 13.0 app。SwiftUI + WKWebView 包裹现有 Bridge Web UI。

| 文件 | 职责 |
|------|------|
| `Package.swift` | SwiftPM 配置，macOS 13.0，无外部依赖 |
| `App/AgentAspectApp.swift` | SwiftUI @main 入口，创建 AppState |
| `App/AppDelegate.swift` | NSApplicationDelegate 预留（M41.4 launchd/Keychain） |
| `Models/AppRoute.swift` | 路由枚举：.loading / .web / .diagnostics |
| `Models/BridgeStatusModel.swift` | 解析 `agent-aspect bridge status` 输出为结构化 model |
| `Models/WebViewState.swift` | WebView 加载状态：.idle / .loading / .loaded / .failed |
| `Services/AgentAspectPaths.swift` | 集中路径解析（`~/.agent-aspect/` 优先，`~/.agent-aspect/` fallback） |
| `Services/BinaryLocator.swift` | 3 层 binary 搜索：bundle → env → PATH |
| `Services/BridgeSupervisor.swift` | Bridge 生命周期：status / start(async) / stop / health / readPort |
| `Services/CommandRunner.swift` | 非交互命令执行 + 10s timeout + pipe deadlock 防护 |
| `Stores/AppState.swift` | 全局 ObservableObject：route / diagnostics / webViewState |
| `Views/BridgeWebView.swift` | NSViewRepresentable WKWebView + 导航委托 + 状态报告 |
| `Views/ContentView.swift` | 根视图：WebView/Diagnostics 切换 + toolbar |
| `Views/DiagnosticsView.swift` | 诊断网格 + Start Bridge / Run Doctor |
| `Views/WebViewErrorView.swift` | WebView 错误叠加层 + Retry + Open in Browser |
| `scripts/build-app.sh` | swift build -c release + .app bundle 组装 |
| `scripts/build-rust-binaries.sh` | cargo build --release 全部 4 个 binary |
| `scripts/copy-rust-binaries.sh` | 复制 binary 到 Resources/Binaries/ |
| `scripts/smoke-mac-shell.sh` | 5 项基础验证 |

---

## crates/daemon — 守护进程（hook 请求处理器）

| 文件 | 职责 |
|------|------|
| `main.rs` | 守护进程主进程：Unix socket 接收 hook 请求 → 规则引擎判定 → 审计写入 |

---

## crates/hook-cli — Hook CLI（AI agent 的 hook 入口）

| 文件 | 职责 |
|------|------|
| `main.rs` | Hook CLI 入口：从 stdin 读 payload → IPC 委托 daemon 判定 → 输出 agent 响应 |

---

## 关键数据流

### 1. Hook 拦截流程
```
AI Agent (claude/codex/kimi)
  → hook 脚本调用 hook-cli
    → Unix Socket IPC 发送给 daemon
      → normalize.rs 归一化为 UnifiedEvent
        → rule.rs 规则引擎评估
          → decision 写入 audit.db
            → 返回 allow/deny 给 agent
```

### 2. 新建/继续会话流程
```
手机端/桌面端 UI
  → job_body.js 构造 body（buildNewJobBody 或 buildContinueJobBody）
    → POST /api/jobs（relay 透传或直连 bridge）
      → jobs.rs promote 字段到 input
        → provider.rs 构造 CLI 命令（有 conversation_id → resume）
          → 子进程执行 → 日志流 SSE → 前端实时显示
```

### 3. Relay 代理流程
```
手机浏览器
  → HTTP 请求到 Relay (axum)
    → token.rs 验证 client token
      → http.rs 路径白名单检查
        → ws.rs 通过 WebSocket 转发到 Mac Bridge
          → Bridge 本地执行请求并返回
            → Relay 回传响应给手机
```
