# M49 Shared UI Product Layer Inventory

> 目标：让 Bridge / Relay 共享真正稳定的产品原语，shell 只保留端侧布局和事件绑定。

## 已收敛

| 模块 | 职责 | 使用方 |
|------|------|--------|
| `crates/shared_ui/view_model.js` | 转义、截断、时间、agent 标签、主题、日志清理 | Bridge / Relay |
| `crates/shared_ui/render.js` | Markdown 渲染、代码块复制 | Bridge / Relay |
| `crates/shared_ui/api_client.js` | HTTP client、token、错误规范化 | Bridge / Relay |
| `crates/shared_ui/job_body.js` | job request body 唯一构造入口 | Bridge / Relay |
| `crates/shared_ui/runtime_health.js` | runtime drift badge / banner / alert | Bridge / Relay |
| `crates/shared_ui/activity_segment.js` | 工具活动折叠与时间线摘要 | Bridge / Relay |
| `crates/shared_ui/approval_review.js` | 审批 review payload 渲染 | Bridge / Relay |
| `crates/shared_ui/job_status.js` | job 状态 badge、completion reason/detail 文案 | Bridge / Relay |

## 本轮 M49 迁移

`job_status.js` 接管以下重复逻辑：

- job status → 中文 label
- job status → Relay badge class
- job status → Bridge badge type
- terminal status 判断
- `completed_reason` → 人类可读文案
- `completion.signal/authority/last_activity/deadline` → 详情摘要

迁移后，Bridge Run/Home 和 Relay Home/Run/History 不再各自维护状态映射。

## 暂不迁移

| 领域 | 原因 |
|------|------|
| 页面布局和 card 结构 | Bridge 是桌面/宽屏控制台，Relay 是手机 shell，布局语义不同 |
| 空状态文案 | Relay 需要区分 token/mac_offline/network_error，Bridge 更多是本地控制台空态 |
| Hook 矩阵布局 | M51 将基于 capability registry 重构，当前不提前抽象 |
| Workflow UI | 仍是 Bridge-only 功能，不应进入共享层 |

## 边界规则

- `shared_ui` 只放纯函数、业务展示原语和 design token。
- shell 文件只做 DOM 编排、表单绑定、tab 状态和端侧布局。
- 新增 Bridge/Relay 共同文案时，先判断是否属于共享业务语义；如果是，放入 `shared_ui` 并补 Node 测试。
