# Tasks

## 1. 依赖兼容性与本地数据库

- [x] 1.1 验证并锁定 Toasty/PostgreSQL 与 Apalis backend 的发布版本及 MSRV，加入最小真实数据库集成测试，证明原子事务、条件更新、取消回滚、重复投递和失联任务恢复可用；在 design.md 记录确切版本及 API 依据。
- [x] 1.2 增加独立 PostgreSQL compose 配置、仅本地监听的持久卷与 `.env.example` 数据库/worker 配置；验证启动、健康检查和重启保留数据，并在 README 记录启动与停止命令，保持 Phoenix 配置和已有数据卷不变。

## 2. 持久化模型与语义事务仓库

- [x] 2.1 建立聚焦的 runtime/persistence 边界与单向 crate 依赖，继承 workspace dependencies/lints；验证 cargo check --workspace，通过模块文档解释端口与装配职责。
- [x] 2.2 定义会话、消息、run/attempt、工具执行、事件、command receipt/outbox 的模型及版本化检查点 codec；用往返测试验证多步骤、工具错误、调用 ID、提供商专用字段和草稿标记无损恢复。
- [x] 2.3 增加显式、非破坏性迁移与 schema 版本检查；验证空库迁移、重复执行、旧版本拒绝启动和迁移回滚，在 README 记录迁移、备份与回退步骤。
- [x] 2.4 实现接受提交及控制命令的事务仓库，包含规范化幂等摘要、版本检查和会话 active run 排他；真实 PostgreSQL 测试覆盖同键重试、异内容冲突、双提交竞争及事务失败后无部分消息/outbox。
- [x] 2.5 实现阶段提交、执行租约/generation 和事件序号约束；真实数据库测试证明状态、结果、检查点和事件原子提交，过期租约及旧代次写入被拒绝。

## 3. 共享阶段执行核心与工具策略

- [x] 3.1 将 Agent Loop 提取为单一阶段状态机，保留 run_agent 的参数、返回类型与事件语义作为内存驱动入口；现有 agent/tool 测试通过，并新增无工具、工具成功/错误和逐项阶段转换测试。
- [x] 3.2 实现持久化驱动器，在工具前提交完整模型决策、每个工具后提交结果再继续；模拟模型测试验证两个工具间恢复只执行剩余项，保留 ID、步骤边界和错误回填。
- [x] 3.3 实现逻辑步骤预算和实际调用计数跨 attempt 恢复，区分未完成模型草稿与有效上下文；测试多次暂停不重置预算、不拼接草稿，step-limit 报告正确步骤数。
- [x] 3.4 为工具添加保守默认的恢复声明与执行上下文，将加法标为安全重做；用本地模拟工具测试原幂等键复用、外部 ID 查询和未知副作用 needs-attention，更新工具接口文档及宏兼容测试。
- [x] 3.5 实现历史展示/模型投影，转向时将未执行工具调用显式闭合且保留已完成结果；验证多步顺序、工具错误、专用字段与无悬空调用，文档说明草稿和未执行结果含义。

## 4. 后台调度、控制与故障恢复

- [x] 4.1 实现 outbox dispatcher 与 Apalis worker、重复投递去重及租约续期；真实数据库测试注入发布前崩溃、发布后标记失败和双 worker 竞争，证明任务不丢失且只有一个有效执行。
- [x] 4.2 实现 pause/resume/cancel 状态迁移、持久化控制意图与本地取消通知；等待中的模拟模型/工具测试验证立即发出取消、pausing 停稳后 paused、原 run 新 attempt、取消终结及迟到结果拒绝。
- [x] 4.3 实现 paused run 的原子 steer，保存追加输入、supersede 旧 run 并调度新 run；测试无旧待办执行、已完成结果可复用、版本竞争和 needs-attention/非 paused 转向被拒绝。
- [x] 4.4 实现恢复扫描、控制通知丢失补偿、有界恢复次数及优雅关闭；进程级测试覆盖 SIG/服务关闭、强制退出、租约到期、pausing 时崩溃，证明暂停不自动续跑、业务模型错误不因队列重试再次调用。
- [x] 4.5 在运行教程中记录 run/attempt、检查点、租约与外部副作用策略；依据模拟恢复日志验证示例，明确 graceful shutdown 与进程骤停的不同结果。

## 5. 会话与任务 HTTP API

- [x] 5.1 增加会话创建、分页列表、权威快照及 run 查询接口，支持严格初始历史导入；路由测试覆盖 400/404、重复 ID、附件、悬空工具、同快照 revision 和无效输入不创建任务。
- [x] 5.2 将 POST /api/chat 改为新消息、版本和 requestId 合约，幂等重试返回同一 run；测试旧 messages 覆盖和 regenerate 被明确拒绝、2 MiB 限制/CORS 保留、数据库不可用返回 503，更新 curl 与 transport 示例。
- [x] 5.3 增加 pause/resume/cancel/steer 路由与安全错误响应；路由测试覆盖接受结果、幂等重试、409 状态竞争及跨会话 run 检查，在 API 文档列出控制状态和版本规则。
- [x] 5.4 在服务入口装配数据库、dispatcher、worker 和有界关闭，保持 /health 仅存活；集成测试验证不需要 HTTP 订阅也能完成任务，启动配置错误可诊断且不输出凭据。

## 6. 持久化 SSE 与官方协议验证

- [x] 6.1 实现稳定 assistant/块 ID、持久化有效事件投影与高水位重放/实时交接；测试多订阅、交接竞争、丢失唤醒补偿和草稿失效，无遗漏、重复或跨 run 内容混入。
- [x] 6.2 实现 GET /api/chat/{id}/stream 与独立订阅生命周期，保留协议头、心跳和工具事件；测试网页关闭后执行继续、慢消费者仅断开自身、存储故障/容量上限停止未检查点执行，文档说明全前缀重放范围。
- [x] 6.3 编码 start metadata 中的可控制 run 标识、终态 metadata、data-run-state 与官方 abort 事件；固定版本 AI SDK 解析测试覆盖普通回答、工具成功/错误、多步、暂停/恢复/终止、转向和步数耗尽，确保文本/步骤闭合。
- [x] 6.4 更新 scripts/ai-sdk-check 的请求合约和 replay 用例；运行 npm ci --prefix scripts/ai-sdk-check 与 npm run check:mock --prefix scripts/ai-sdk-check，验证最终 UIMessage 和第二轮服务端历史，更新脚本使用说明。

## 7. 前端会话加载与任务控制

- [x] 7.1 增加 URL 会话标识、会话列表/快照加载和服务端 revision 管理，调整 DefaultChatTransport 只提交新增消息；前端测试和 pnpm -C apps/web build 验证刷新恢复、多轮输入及明确格式错误。
- [x] 7.2 实现固定 run 的重连与有效前缀重放，加载时排除正在重放的 assistant、恢复时替换旧草稿；测试刷新中途回复、同 ID 去重、工具等待期间重连及完成后快照一致性。
- [x] 7.3 增加暂停、原样继续、追加指令转向、明确终止以及 pausing/needs-attention 展示；测试 UI 不将 SDK stop 当作任务取消、双页面冲突后刷新和互斥操作禁用，更新前端接入与状态说明。

## 8. Phoenix 关联与评测迁移

- [x] 8.1 将执行观测迁移到 run/attempt 生命周期、传播后台 trace context 并关联恢复尝试；模拟 exporter 测试覆盖断开不 cancelled、暂停/接替/关闭中断、并发隔离和终态一次性，更新观测教程。
- [x] 8.2 汇总跨 attempt 实际调用 usage 与逻辑步骤，区分订阅/执行延迟；测试重放不增加统计、恢复请求计成本、未知 usage 不变零及不重复计父子明细，保持内容采集与 Phoenix 故障策略。
- [x] 8.3 迁移 eval runner 到独立服务端会话与增量多轮输入，超时显式 cancel 并记录确认/cleanup failure；runner 测试验证并发、预算跳过、接受响应丢失时查回 run 和超时无孤儿任务，更新 fixtures/provenance/使用文档。
- [x] 8.4 为 Phoenix 实验保存 run、各 attempt/trace 与 trial 关联，加入持久化恢复模拟门禁；执行既有评分/本地报告测试和新恢复用例，验证评分含义、项目隔离及 Phoenix 不可用仍保存报告。

## 9. 整体验收

- [x] 9.1 执行独立测试数据库上的完整故障矩阵：关闭页面继续、模型暂停重做、多工具间重启、外部未知结果、转向与旧执行迟到；核对最终数据库事实、官方 UIMessage、调用次数和状态，不使用生产凭据或真实付费模型。
- [x] 9.2 执行 cargo build --workspace、cargo test --workspace、cargo fmt --all -- --check、cargo clippy --workspace --all-targets -- -D warnings、前端 build 与官方协议/评测模拟门禁，记录结果和适用的数据库环境。
- [x] 9.3 核对 README、AGENTS.md、.env.example、API/运行/观测教程和迁移说明与实际命令一致；检查 git diff 保留既有未提交教程改动，运行 openspec validate add-durable-agent-sessions --strict，逐条对应 specs 场景与验证证据后标记交付完成。
