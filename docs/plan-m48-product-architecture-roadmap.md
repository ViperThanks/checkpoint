# M48+ Product Architecture Roadmap

> 目标：把 Agent Aspect 从“本地 hook + bridge 工具”推进成一个本地优先、手机顺滑、编排可靠、provider 易扩展的软件化控制面。
> 本文是 M47 Completion Strategy Layer 之后的长期路线图，不是一次性大重构清单。

## Status

2026-05-10：

- M48 Mobile Control Plane Closure 第一版已完成：client token renew 使用 hash + generation CAS，旧 token 轮换后失效；mobile lease v2 以内存 lease 表达手机控制面活性；手机首页使用 `/mobile/summary` 聚合轻量摘要；hidden 状态停止重型轮询，visible / online / pageshow 恢复时补 beat、renew 和当前视图刷新；relay smoke 覆盖 renew、旧 token 拒绝、mobile summary 和 beat lease。
- M48.5 Runtime HA Core 第一版已完成：JobRunner heartbeat 是活进程权威；scanner 只收 heartbeat stale 的 orphan job；fresh heartbeat 时 scanner 不写 job 终态；transcript no delta 先进入 maybe_idle，hard deadline + stale heartbeat 才经 CompletionSink 写 `scanner_timeout`；新增失败注入单测覆盖 fresh/stale heartbeat 和 no-delta idle。
- 未纳入本轮：workflow recovery、retry/fallback policy、provider capability registry、Hook Strategy Config v2。

---

## 0. 北极星

Agent Aspect 的长期形态：

- Mac 本地是权威执行面：daemon / bridge / JobRunner / scanner 负责真实控制和状态收口。
- 手机是可选控制面：审批、观察、轻量发起任务；离开页面后只保留租约和续期，不拖累 Mac。
- Web UI 是产品界面：密度高、好看、响应快，错误状态可解释。
- Provider 是插件：新 CLI 接入只写 adapter、fixture、capability，不碰主干。
- Hook 是生命周期入口：before / after / stop / timeout 都走同一套事件与策略模型。
- Workflow 是高可用编排：任何 step 都有权威状态、heartbeat、deadline、retry、completion authority。

一句话：

> Hook 负责感知，Runner 负责执行，Scanner 负责兜底，Workflow 只消费稳定的 completion signal。

---

## 1. 设计约束

### 1.1 性能约束

- 手机端 hidden 状态不得持续拉 `/overview`、`/jobs`、`/messages`、`/hook-status` 这类重接口。
- 手机端后台只允许轻量 heartbeat / token renew，且要有频率上限。
- Bridge 首页首屏必须走摘要接口，不允许每次打开都触发全量 transcript scan。
- Relay 只做认证、租约、代理和轻量会话状态，不成为第二个业务数据库。
  - 允许持久化 `registered_tokens`、current client token hash / generation、expires_at。
  - 不允许持久化 jobs、workflow、conversation、audit 这类业务状态。
- 所有轮询必须能被 page lifecycle、job status、conversation status 停止。

### 1.2 状态权威约束

- 活进程权威：`JobRunner`。
- orphan / stale 权威：`scanner`。
- 强完成信号：`StopHook`、`ProcessExit`。
- 推断完成信号：`TranscriptIdle`、`ScannerTimeout`。
- 手机在线状态只表示“控制面存在”，不能影响 job 终态。
- `completed_reason` 只能写稳定枚举，细节进入 observer / logs。

### 1.3 扩展约束

- 新 provider 不允许在 `routes.rs`、`jobs.rs`、`relay` UI 里新增散落分支。
- Provider 差异进入 `AgentAdapter` / capability / fixture。
- Hook 配置只做 3 层：global / agent / event；strategy 是 event 配置里的枚举字段，不成为第四层继承轴。
- 策略模式只放真实变化点：provider、completion、hook strategy、workflow retry。

---

## 2. 外部方案借鉴

这些方案只取原则，不照搬复杂度。

### 2.1 Web Page Lifecycle

参考：

- [MDN Page Visibility API](https://developer.mozilla.org/en-US/docs/Web/API/Page_Visibility_API)
- [Chrome Page Lifecycle API](https://developer.chrome.com/articles/page-lifecycle-api)
- [web.dev bfcache lifecycle events](https://web.dev/articles/bfcache?hl=en)

可借鉴点：

- `visibilitychange` 是移动端最可靠的“用户可能离开”信号。
- hidden 应被视为“本次交互可能结束”，要停止 UI 更新和非必要网络请求。
- frozen / discarded 不一定可观测，所以恢复时必须用 server 状态重新校准。

落地原则：

- visible：允许重型刷新。
- hidden：只 heartbeat / renew。
- pageshow / online / resume：补一次 beat、renew、当前页轻刷新。
- 不依赖 unload / beforeunload 做关键清理。

### 2.2 Kubernetes Lease

参考：

- [Kubernetes Leases](https://kubernetes.io/docs/concepts/architecture/leases/)
- [Kubernetes Node Heartbeats](https://kubernetes.io/docs/concepts/architecture/nodes/)

可借鉴点：

- Lease 是轻量协调原语，只表达 holder、renew time、duration。
- Heartbeat 不是业务状态，只是活性证据。
- 控制器根据 lease 过期推断 offline，而不是要求客户端显式 logout。

落地原则：

- mobile lease 只记录 `sid/device_id/last_seen_at/expires_at`。
- job runner heartbeat 记录进程活性。
- scanner 用 stale threshold 收 orphan，不抢 runner 对活进程的主控权。

### 2.3 Temporal Durable Workflow

参考：

- [Temporal Docs](https://docs.temporal.io/)
- [Temporal durable execution overview](https://temporal.io/home)

可借鉴点：

- Workflow 负责持久编排，Activity 负责可重试副作用。
- Timeout、retry、heartbeat 是一等概念，不是日志字符串。
- 崩溃恢复依赖事件历史和幂等状态转换。

落地原则：

- workflow step 是持久状态机，不直接依赖内存线程存在。
- provider run 是 activity：有 timeout、attempt、heartbeat、completion signal。
- bridge 重启后可以恢复 queued / running / observing / stale。

### 2.4 OAuth Token Rotation

参考：

- [RFC 9700 OAuth 2.0 Security Best Current Practice](https://www.rfc-editor.org/rfc/rfc9700)

可借鉴点：

- token rotation 后旧 token 应失效。
- refresh/renew 需要并发防护，避免旧 token 多次换新。
- refresh token 有过期、撤销、重放检测语义。

落地原则：

- mobile client token renew 必须 CAS：`stored.client_token == request.raw_token`。
- renew 后旧 client token 立即失效。
- client token 不能活得比 mac token 更久，除非同时设计 mac token renew。
- 这要求 Relay 保留每个 sid 的 current client token hash / generation；这是会话控制状态，不是业务数据库。

### 2.5 OpenTelemetry Semantic Conventions

参考：

- [OpenTelemetry Semantic Conventions](https://opentelemetry.io/docs/concepts/semantic-conventions/)

可借鉴点：

- 统一命名比到处自由发挥更重要。
- event/log/span 的字段要稳定，便于 UI 和工具消费。

落地原则：

- completion / workflow / hook / provider 事件使用稳定字段名。
- `completed_reason`、`authority`、`signal` 使用枚举。
- UI 展示文案从枚举映射，不把人类句子写入数据库主字段。

### 2.6 VS Code Contribution Points

参考：

- [VS Code Contribution Points](https://code.visualstudio.com/api/references/contribution-points)
- [VS Code Activation Events](https://code.visualstudio.com/api/references/activation-events)

可借鉴点：

- 插件声明能力，宿主按 contribution 渲染功能。
- 激活事件控制插件何时真正加载。

落地原则：

- provider adapter 声明 capability。
- UI 按 capability 渲染按钮和健康状态。
- 新 provider 只注册 contribution，不改核心流程。

---

## 3. 分阶段路线

### Phase A — Mobile Control Plane Smoothness

目标：手机像原生控制面一样顺滑，但后台几乎无性能开销。

任务：

- A0. Relay token 语义收口
  - `registered_tokens` 明确是 relay 会话控制状态。
  - client token renew 使用 CAS；旧 token 立即失效。
  - client token 续期上限不得超过 mac token exp。
  - 后续如果要让手机长期续期超过 mac token 生命周期，必须同时设计 mac token renew / bridge re-register。

- A1. Mobile lease v2
  - 多设备 lease 聚合：`sid -> devices[]`。
  - `/api/mac-status` 返回最近手机设备、租约剩余时间、控制面在线数量。
  - Relay 启动时不持久化 mobile lease，只从 heartbeat 重建。

- A2. Page lifecycle 收口
  - visible：允许 Home / Convos / Run 重型刷新。
  - hidden：停止 job log、conversation delta、overview、pending、hook-status。
  - online/pageshow/resume：补一次 beat + renew + 当前页刷新。

- A3. Mobile first screen budget
  - 首页首屏最多 3 个请求。
  - offline 时禁止继续拉 Mac 代理接口。
  - pending ask、last job、runtime health 合并为轻量摘要 API。

- A4. Mobile UX polish
  - 统一 skeleton / empty / error / slow network。
  - 待审批卡片支持“原因、影响、建议动作”三行信息。
  - job 卡片明确显示 signal / authority / deadline。

验收：

- iPhone Safari hidden 10 分钟内不持续打重接口。
- relay smoke 覆盖 renew、old token revoked、mobile lease、mac offline。
- mobile JS 测试覆盖 lifecycle policy。

### Phase B — Shared UI Product Layer

目标：Bridge 和 Relay 像同一个产品，视觉稳、信息密度高；先盘点和收敛已有 shared_ui，不做大重写。

任务：

- B0. shared_ui inventory
  - 列出 bridge/relay 仍重复的渲染函数、状态映射、错误态、badge。
  - 标记每个重复点：迁入 shared_ui、保留 shell-local、删除。
  - 不新建“组件系统”，先消除真实重复。

- B1. 现有 shared_ui 资产收敛
  - 复用 `design_tokens / view_model / api_client / runtime_health / activity_segment / approval_review`。
  - status row、badge、job card、conversation row、empty/error state 只有在两端真实重复时才上移。

- B2. 页面信息架构
  - Home：健康、待办、最近任务、最近会话。
  - Convos：按项目/agent/健康状态过滤。
  - Run：新建/继续/模板/运行中状态。
  - Hooks：agent × event 矩阵 + 简单策略。
  - Workflows：流程、step、retry、logs。

- B3. Visual QA
  - Playwright 或 browser smoke 截图检查桌面/手机关键页。
  - 检查空白、溢出、按钮文字截断、主要元素存在。

验收：

- Bridge / Relay 不再复制同类渲染函数。
- 每个迁移项都有 JS 单测或 screenshot smoke 覆盖。
- 手机 3 秒内能判断：Mac 是否在线、有无待审批、job 是否卡住。

### Phase C — Provider Adapter SDK

目标：新 CLI 接入像写插件。

核心接口：

```rust
pub trait AgentAdapter {
    fn id(&self) -> AgentId;
    fn capabilities(&self) -> ProviderCapabilities;
    fn supported_events(&self) -> Vec<LifecycleEvent>;
    fn build_command(&self, request: ProviderRunRequest) -> Result<CommandSpec>;
    fn transcript_locator(&self, identity: &ConversationIdentity) -> Option<PathBuf>;
    fn parse_transcript_delta(&self, cursor: TranscriptCursor) -> Result<TranscriptDelta>;
    fn completion_policy(&self) -> CompletionPolicy;
}
```

任务：

- C1. 清理剩余 `match agent`
  - transcript、title_import、provider command、resume/new、hook status 逐步迁移。

- C2. Capability registry
  - `supports_pretooluse`
  - `supports_posttooluse`
  - `supports_stop`
  - `supports_transcript`
  - `supports_resume`
  - `supports_native_timeout`

- C3. Provider fixture kit
  - payload fixture。
  - transcript fixture。
  - command build snapshot。
  - deny/allow round-trip 测试模板。

- C4. Provider author guide
  - “30 分钟接入一个 CLI”的文档。
  - 只允许 adapter 层写 provider 特判。

验收：

- 新增 provider 不改 `routes.rs` / relay UI / workflow runner。
- UI 通过 capability 自动展示可用功能。

### Phase D — Lifecycle Hook Config v2

目标：before / after / stop 可配置，但不复杂。

配置层级：

```text
global default
  -> agent override
    -> event override { enabled, decision_strategy, completion_strategy, timeout_strategy }
```

事件：

- `SessionStart`
- `UserPromptSubmit`
- `PreToolUse`
- `PostToolUse`
- `PermissionRequest`
- `Stop`
- `ScannerSynthetic`

策略：

- hook decision：`observe / allow / ask / deny`
- completion：`stop_hook / process_exit / transcript_idle / hard_deadline`
- timeout：`mark_observing / mark_failed / retry / fallback`

任务：

- D0. PermissionRequest / PostToolUse 真实闭环
  - normalize 成 UnifiedEvent。
  - audit 落库。
  - fixture 覆盖。
  - UI 最小展示：PermissionRequest 意图、PostToolUse exit code / duration / stdout preview。
  - D0 完成前不把这两个事件暴露成复杂策略配置。

- D1. Config schema 收敛
  - event-level toggle 继续保留。
  - 策略只允许有限枚举，不开放任意脚本。

- D2. Hook install/reconcile 与 config 统一
  - `hook_status.rs` 只读 config + provider spec。
  - bridge Hook tab 只展示 capability 支持的事件。

- D3. Completion strategy 接入 config
  - provider 默认策略。
  - agent override。
  - event override 内的 completion/timeout 枚举。
  - job request 只能提供一次性运行参数，不进入全局继承模型。

验收：

- 用户能按 agent/event 调整 hook 行为。
- stop/scanner/timeout 的 UI 展示来自同一套 completion model。

### Phase E — Workflow Orchestration HA

目标：编排不怕 bridge 重启、provider 假死、relay 断线、stop hook 缺失。

先拆出 Runtime HA Core，避免 UI 和配置层继续暴露底层假死问题：

- Runner heartbeat 权威化。
- Scanner stale guard。
- LM stream idle timeout。
- CompletionSink 终态统一。

Job 状态机验收表：

| From | Event | Guard | To | Writer |
|------|-------|-------|----|--------|
| queued | runner started | job claimed | running | JobRunner |
| running | stdout/stderr delta | process alive | running | JobRunner |
| running | stop hook | same job/conversation | observing/succeeded | CompletionSink |
| running | process exit 0 | pid matched | succeeded | CompletionSink |
| running | process exit non-zero | pid matched | failed | CompletionSink |
| running | hard deadline | pid alive | timeout | JobRunner |
| running/observing | scanner timeout | runner heartbeat stale | timeout | Scanner + CompletionSink |
| running/observing | cancel requested | any | cancelled | JobRunner/CompletionSink |
| observing | transcript delta | before hard deadline | running | Scanner |

Workflow step 必备字段：

- `attempt_id`
- `idempotency_key`
- `retry_budget`
- `started_at / heartbeat_at / hard_deadline_at`
- `input_context_bytes / output_context_bytes`
- `redaction_policy`

任务：

- E1. Job 状态机收严
  - 所有状态转换走 DAO / CompletionSink。
  - 禁止 handler 直接拼 completed_reason 字符串。

- E2. Runner heartbeat 权威化
  - `heartbeat_at` 表示活进程。
  - `started_at + hard_deadline` 表示不可突破的上限。
  - runner 负责 kill 活进程。

- E3. Scanner orphan 收口
  - scanner 只处理 runner heartbeat stale 的 job。
  - fresh heartbeat 时 scanner 只更新 observer，不终结 job。

- E4. LM stream idle timeout
  - N 秒无 delta：fail/retry/fallback。
  - 不是无限等 background_output。
  - 错误写入 job_logs + completion observer。

- E5. Workflow recovery
  - bridge 启动时恢复 queued/running/observing。
  - workflow step 支持 retry policy。
  - step result 可作为下游 context，但必须有大小和敏感信息限制。

验收：

- kill bridge 后重启，不留下永久 running。
- provider 卡死最终进入 timeout/retry/fallback。
- stop hook、process exit、scanner timeout 都能解释 authority。

### Phase F — Softwareization & Operations

目标：从工程项目变成可安装、可诊断、可维护的软件。

任务：

- F1. Doctor v2
  - daemon / bridge / relay / mobile / hook / scanner / provider / launchd。
  - 输出明确 OK/WARN/FAIL 和修复建议。

- F2. Diagnostic bundle
  - config 摘要。
  - 最近 jobs/workflows/completion_observers。
  - relay status。
  - hook status。
  - 日志尾部。
  - token 不输出原文。

- F3. Release smoke
  - core smoke。
  - bridge smoke。
  - relay smoke。
  - mobile JS。
  - shared UI。
  - provider fixtures。

- F4. Mac app shell
  - bridge supervisor。
  - logs/doctor/keep-awake。
  - mobile pairing 展示。

验收：

- 用户说“卡住了”，一条命令能导出足够证据。
- 新机器 5 分钟内完成 bridge + mobile pairing + sample job。

---

## 4. Milestone 切分

### M48 Mobile Control Plane Closure

- Relay token 语义收口。
- Mobile lease v2。
- Mobile summary API。
- Page lifecycle 测试。
- Relay smoke 扩展。
- Mobile UI polish 第一轮。

### M48.5 Runtime HA Core

- Runner heartbeat 权威化。
- Scanner stale guard。
- LM stream idle timeout。
- CompletionSink 终态统一。
- 失败注入：kill provider、stop hook missing、transcript no delta。

### M49 Shared UI Product Layer

- shared_ui inventory。
- 重复渲染函数收敛。
- Bridge/Relay 页面统一。
- Screenshot smoke。
- Design token 审计。

### M50 Provider Adapter SDK

- Provider capabilities。
- 清理核心 `match agent`。
- Provider fixture kit。
- 新 CLI 接入文档。

### M51 Hook Strategy Config v2

- Event strategy schema。
- Hook tab 策略矩阵。
- completion strategy override。
- Config migration。

### M52 Orchestration HA

- Workflow/job 状态机完整化。
- Workflow recovery。
- Retry/fallback policy。
- attempt_id / idempotency_key / retry_budget。
- context size / redaction policy。

### M53 Softwareization

- Doctor v2。
- Diagnostic bundle。
- Release smoke。
- Mac app shell 收口。

---

## 5. 风险清单

- 过度抽象：Provider SDK 不应在两个 provider 之前设计成大型插件系统。
- 配置爆炸：Hook config 只能有限枚举，不做脚本 DSL。
- 双权威：scanner 不能和 runner 同时终结活 job。
- Relay 变胖：Relay 只放租约和代理，不承载业务状态。
- UI 共享过度：shared_ui 只放纯函数和稳定组件，页面布局仍由 bridge/relay shell 控制。
- Token 语义分裂：client renew 不得超过 mac token 生命周期，除非同时实现 mac renew；Relay 必须持久化 current token generation/hash。
- Workflow 幂等性：恢复和 retry 前必须定义状态机、attempt_id、idempotency_key、context 上限和脱敏策略。

---

## 6. 当前优先级建议

最近三步：

1. M48：手机控制面收口。
   - 因为这直接减少性能浪费，也让 relay/mobile 成为可靠入口。

2. M48.5：Runtime HA Core。
   - 因为 opencode/subagent 假死已经是当前最痛点，必须先让 job 可失败、可重试、可解释。

3. M50 的一部分提前做：Provider capability registry。
   - 因为 UI、hook、workflow 都需要知道 provider 能力，早做能减少后续分支。

不建议马上做：

- 大规模 UI 重写。
- 任意脚本 hook DSL。
- Relay 数据库化。
- 完整 Temporal 级 workflow engine。

---

## 7. Review Checklist

每个 milestone 完成前必须问：

- 是否减少了一个长期分支，而不是新增一个特殊情况？
- 是否让状态权威更清楚？
- 是否降低手机后台和 bridge 首屏开销？
- 是否让新 provider 更容易接入？
- 是否有 smoke / unit / UI 测试证明？
- 用户看到错误时，是否能知道原因和下一步？
