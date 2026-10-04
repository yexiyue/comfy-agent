# Proposal

## Why

当前聊天历史由浏览器提交，Agent 的执行进度只存在于内存，关闭网页即取消执行。需要让会话与任务独立于连接，支持立即请求暂停、恢复、终止和追加指令转向，并在后端重启后从可靠检查点恢复。

## What Changes

- 使用 Toasty 与 PostgreSQL 持久化会话、UIMessage、run/attempt、阶段检查点、工具执行、进度事件和 outbox；使用 Apalis PostgreSQL 队列运行后台任务。
- 将 Agent 执行整理为共享的阶段状态机；保存模型完整决策及逐个工具结果，避免恢复时重复已完成动作。保留简单运行入口，持久化与评测复用同一执行核心。
- **BREAKING**：服务端成为历史权威来源；聊天提交改为新消息、会话版本与幂等键，不再接受整段历史覆盖。SSE 断开仅结束订阅，不取消任务；慢消费者可重连，不终止生产者。
- 支持立即请求暂停当前动作、原样继续和明确终止；暂停后继续复用 run 并创建新 attempt。追加指令创建同会话的新 run，旧 run 标记 superseded；已完成结果可复用，旧待办决策失效。
- 通过阶段检查点、队列故障恢复、执行租约与代次防写入支持进程重启恢复；无法确认外部副作用时进入 needs-attention，不盲目重试。
- 提供持久化事件重放与 AI SDK UI Message Stream 兼容接口；前端加载会话、展示任务状态并区分暂停、继续、转向和终止。
- 更新 Phoenix 的 run/attempt 关联与统计，以及评测的服务端历史、显式取消和恢复测试。

## Capabilities

### New Capabilities

- `persistent-conversations`: 会话、消息权威存储、版本与幂等提交，以及展示历史与模型上下文的分离。
- `durable-agent-execution`: 持久化阶段执行、任务控制、转向、崩溃恢复与工具副作用策略。
- `resumable-chat-streams`: 独立于执行的流订阅、持久化重放和客户端任务交互。

### Modified Capabilities

- `agent-observability`: 从连接生命周期改为 run/attempt 生命周期；明确暂停、接替、断开和恢复的观测与累计统计。
- `agent-evaluation`: 评测使用持久化会话和真实后台执行；超时显式取消，新增暂停与重启恢复验收。

## Impact

- 影响 `crates/agent`、`crates/server`、`crates/tools`、`crates/telemetry`、`apps/web`、`scripts/evals` 和官方协议检查脚本；新增聚焦的 runtime/persistence 模块或 crate，不引入通用 CRUD 框架。
- 新增数据库部署配置、迁移与 Toasty/Apalis 兼容依赖；保持 Phoenix 数据独立，复用现有本地模型与前端配置。
- 更新 README、AGENTS、`.env.example` 与教程；保留目前未提交的教程文档。
- 首版继续面向本地/自托管；不实现认证、多租户、Redis、完整事件溯源、任意编辑历史/分支 UI、工具审批、ComfyUI 工具或外部副作用撤销。预留分支关联，提供可重做与模拟外部任务恢复测试。
