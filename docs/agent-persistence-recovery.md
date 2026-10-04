# 从零理解 Agent 持久化与恢复：关掉网页之后，任务去了哪里？

你已经写好了一个 Agent Loop：请求模型，解析工具调用，执行工具，把结果加入历史，再请求模型。接下来，用户提出了几个很自然的要求：

> 我能关掉网页，过一会儿回来再看吗？执行到一半能暂停吗？暂停后能补充一句要求吗？后端重启了，能接着做吗？

这些要求会把程序从“一次 HTTP 请求”变成“有生命周期的后台任务”。数据库在这里保存的不只是聊天记录，还要保存任务走到哪一步、谁有权继续、哪些结果可以相信。

本文用 `comfy-agent` 的真实实现讲解这个过程。你只需要知道 HTTP、JSON，以及 `async/await` 的基本含义。先读概念和时序图，再看代码对应位置，不必一开始就读懂数据库事务。

**实现基线：2026-10-04，提交 `7b6304d`。** 后端是 Rust + Axum，业务持久化使用 Toasty，任务队列使用 Apalis，两者连接 PostgreSQL；前端使用 AI SDK、AI Elements 和 TanStack Query。外部资料按同日官方文档核对，示例的能力范围与版本会明确标注。

建议分三次阅读：

- 第 1–5 节：理解问题、数据和总体架构。
- 第 6–13 节：跟着任务经历提交、暂停、恢复和重连。
- 第 14–18 节：对照 Vercel 等方案，动手验证，再理解限制。

阅读导航：[总体架构](#3-总体架构网页只是任务的观察者) · [可靠提交](#6-一条消息如何可靠地变成后台任务) · [检查点](#7-检查点如何把-agent-loop-变成可恢复执行) · [暂停与转向](#9-暂停终止和转向分别做什么) · [重启恢复](#12-重启恢复的完整时序) · [前后端重连](#13-前后端怎样一起完成重连) · [方案对比](#14-与-vercel-ai-sdkworkflow-和-temporal-对比) · [动手实验](#16-从零动手先启动再做免费恢复实验)。

## 1. “保存聊天”为什么还不够？

先从最简单的实现开始。用户发来一条消息，HTTP handler 直接运行 Agent，再把模型输出通过 SSE 返回。

SSE，Server-Sent Events，可以理解为一个持续打开的 HTTP 响应：后端不断发送小段数据，前端一边接收、一边显示。

```mermaid
sequenceDiagram
    autonumber
    participant U as 浏览器
    participant H as HTTP handler
    participant A as 内存 Agent Loop
    participant M as 模型或工具
    U->>H: POST 一条消息
    H->>A: 开始执行
    A->>M: 请求模型或调用工具
    M-->>A: 返回片段与结果
    A-->>H: 进度事件
    H-->>U: SSE 显示答案
    Note over H,A: 执行状态仅在进程内存中
```

这足以做一个演示。但遇到故障时，会留下不同的问题：

| 发生了什么 | 只保存最终聊天记录会怎样 | 真正需要恢复什么 |
| --- | --- | --- |
| 浏览器刷新 | 可能失去正在显示的回复 | 已产生的输出与任务状态 |
| 网页关闭 | 若执行依赖响应消费，生成可能停止 | 独立于连接的执行生命周期 |
| 进程退出 | 内存中的历史和当前阶段消失 | 最后一个已确认的执行位置 |
| 工具成功后进程退出 | 不知道工具到底执行过没有 | 调用身份、结果与外部操作身份 |
| 用户重复发送 | 可能启动两次任务 | 命令去重与原结果查询 |

这里的“持久化”就是把需要跨进程保留的信息写入存储。“恢复”则是在新连接或新进程中，利用这些信息继续工作。两者相互依赖，但不是同一件事。

### 四种容易混在一起的能力

```mermaid
flowchart TD
    P["持久化与恢复"] --> H["历史持久化：能看见过去的消息"]
    P --> S["流重连：能重新读取正在产生的输出"]
    P --> B["后台执行：没有网页订阅也继续工作"]
    P --> D["执行恢复：进程退出后从检查点继续"]
    H --> DB["消息存储"]
    S --> EV["可重放的事件存储"]
    B --> WK["独立 worker"]
    D --> CP["检查点、租约与恢复策略"]
```

例如，数据库里有昨天的聊天记录，不代表昨天中断的任务今天能继续。Redis 里有已经发送的流片段，也不代表它能重新运行丢失的工具循环。

理解这四层，后面的组件就不会显得像“为了复杂而复杂”。

## 2. 用一个两工具任务认识最重要的五个概念

假设用户说：

> 先算 3 + 5，再算 8 + 2，最后告诉我两个结果。

模型可能在第一步决定调用两个 `add` 工具；执行完之后，再用第二步模型请求整理答案。这个例子不依赖真实模型一定如何规划，下面把它作为一个固定的执行过程来分析。

### 2.1 Conversation：一段会话

Conversation 保存整段对话：用户消息、assistant 消息、工具展示，以及模型需要的历史。

一个会话可以有很多次生成任务，但当前实现限制同一会话同时只有一个未终结任务。暂停的任务和需要人工核对的任务也占用这个位置，因为它们仍有未处理的工作。

### 2.2 Run：一次任务

Run 是“一次用户指令对应的一次生成任务”。它有自己的 ID、状态、assistant 消息 ID、模型配置和检查点。

关掉网页不会创建新 run；暂停后原样继续，也沿用原 run。暂停后追加新指令转向，则创建新 run，旧 run 记录为 `superseded`。

### 2.3 Attempt：一次取得执行权的尝试

任务没有变，但执行它的进程可能变了。

第一次 worker 领取任务时产生 attempt A；进程退出后，另一个 worker 领取恢复任务，产生 attempt B。两次 attempt 都属于同一个 run。

拆出 attempt，就能分别回答“任务最后完成了吗”和“中间执行了几次、哪里中断、额外请求了多少次模型”。

### 2.4 Checkpoint：检查点

检查点是一份可以序列化的“执行进度单”：当前逻辑步数、完整模型历史、已经保存的模型决策、下一个工具位置，以及是否有动作正在等待结果。

它不保存 Rust 的线程、调用栈或 Future。程序恢复时，会重新创建 Future，根据进度单选择下一项动作。

### 2.5 Event：用于展示的进度事件

模型产生了一段文字、工具参数准备完成、工具结果已经返回，都可以变成事件。事件保存之后，SSE 订阅者才能读到。

事件负责“你在页面上看见什么”；检查点负责“程序下一步该做什么”。两者会在关键事务里一起更新，但职责不同。

```mermaid
flowchart LR
    C["Conversation：这段聊天"] --> R1["Run 1：两次加法"]
    C --> R2["Run 2：后续追问"]
    R1 --> A1["Attempt A：执行后中断"]
    R1 --> A2["Attempt B：恢复并完成"]
    R1 --> CP["Checkpoint：第一项结果已保存"]
    R1 --> E["Events：文字、工具与完成事件"]
    CP --> NEXT["决定从第二项工具继续"]
    E --> UI["重建 assistant 的显示内容"]
```

**一个 run 多个 attempt，是恢复；一个 conversation 多个 run，是多轮交互。** 先把这句话记住。

## 3. 总体架构：网页只是任务的观察者

这套实现把“下达命令”“执行任务”“观看输出”分开：

```mermaid
flowchart TB
    subgraph Browser["浏览器"]
        UI["AI Elements：展示消息与工具"]
        Chat["AI SDK：提交与解析 SSE"]
        Query["TanStack Query：快照、状态与控制"]
        UI --> Chat
        UI --> Query
    end
    subgraph Process["Rust server 进程"]
        API["Axum：命令与查询接口"]
        SSE["SSE：读取有效事件"]
        Dispatch["Outbox 发布与恢复扫描"]
        Queue["Apalis worker：领取任务"]
        Runtime["runtime：执行生命周期"]
        Phase["agent：唯一的阶段状态机"]
        Store["persistence：Toasty 业务事务"]
        API --> Store
        Dispatch --> Queue
        Queue --> Runtime
        Runtime --> Phase
        Runtime --> Store
        SSE --> Store
    end
    PG[("PostgreSQL：业务表与队列表")]
    External["模型与外部工具"]
    Chat --> API
    Chat --> SSE
    Query --> API
    SSE --> Chat
    Store --> PG
    Dispatch --> PG
    Queue --> PG
    Runtime --> External
```

图里 worker 和 HTTP 在同一个可运行服务中，不是两个已经独立部署的服务。但它们的异步任务生命周期相互独立：HTTP 响应结束，不会因此结束 worker 中的 Agent。

这种划分让以后拆成 API 进程和 worker 进程有基础；当前并没有单独的 worker 启动入口，也没有把跨进程控制通知优化完成。

### 每个 crate 为什么存在？

| 模块 | 负责什么 | 为什么单独划分 |
| --- | --- | --- |
| `crates/agent` | 模型交互与通用阶段状态机 | 内存执行和持久执行共用循环规则 |
| `crates/tools` | 工具接口、执行上下文和恢复策略 | 工具明确声明重做是否安全 |
| `crates/runtime` | run/attempt 生命周期与存储端口 | 执行业务不绑定 HTTP 或 Toasty |
| `crates/persistence` | 业务事务、队列、outbox 与恢复扫描 | 数据库约束与原子操作集中维护 |
| `crates/server` | 请求校验、公共 DTO 与 SSE | 网络协议和内部状态分开 |
| `crates/telemetry` | trace、span 与内容采集规则 | 观测失败不改变业务执行结果 |
| `apps/web` | 消息展示、查询和交互协调 | 界面从权威快照与事件恢复 |

这可以用“端口与适配器”的设计方式理解：runtime 依赖 `ConversationStore` 这类业务接口，PostgreSQL 实现接口。端口规定的是 `submit`、`claim`、`commit`、`recover` 等完整业务操作，不是把每条 SQL 再包装一次。

这样做的目的，是让“提交任务必须在一个事务里完成”成为接口语义，而不是由 HTTP handler 临时拼凑。

## 4. 数据库到底保存了什么？

当前业务 schema 有八张主要表，另有 schema 版本表和 Apalis 自己的队列表。以下图只展示业务关系；外键以迁移文件为准，图中的 attempt/event 关联也包含逻辑关系。

```mermaid
erDiagram
    CONVERSATION ||--o{ MESSAGE : contains
    CONVERSATION ||--o{ RUN : owns
    CONVERSATION ||--o{ COMMAND : scopes
    RUN ||--o{ ATTEMPT : executes
    RUN ||--o{ TOOL_EXECUTION : records
    RUN ||--o{ EVENT : emits
    RUN ||--o{ OUTBOX : schedules
    ATTEMPT ||--o{ EVENT : attributes
    CONVERSATION {
        string id PK
        int revision
        string active_run_id
        string body
    }
    RUN {
        string id PK
        string conversation_id FK
        string status
        int version
        int generation
        int dispatch
        datetime lease_until
        string body
    }
    COMMAND {
        string scope
        string request_id
        string digest
        string result
    }
```

对应的真实表名是：

| 表 | 核心用途 |
| --- | --- |
| `agent_conversations` | 会话、修订号和当前任务 |
| `agent_messages` | 消息 ID、位置和展示内容 |
| `agent_runs` | 状态、版本、执行代次、租约与检查点 |
| `agent_attempts` | 每次执行的结果、调用统计与 trace ID |
| `agent_tool_executions` | 工具参数、调用 ID、恢复策略和外部 ID |
| `agent_events` | 有序的 UI 事件，及草稿／有效标记 |
| `agent_commands` | 命令去重回执 |
| `agent_outbox` | 等待发布的任务安排 |

### 为什么既有列，又有 `body`？

当前实现把复杂 Rust 结构序列化成 JSON，保存在 TEXT 类型的 `body` 列里；同时把需要查询、索引和加锁的字段放在独立列中，例如 `status`、`generation` 和 `lease_until`。

这是当前的实现取舍：减少模型映射工作，保留数据库查询能力。代价是同一事实可能同时存在于列和序列化 body 中，需要统一写入函数保持一致；消息也存在于会话 body 和消息表中。它不是一个消除了所有冗余的最终数据模型。

当前使用 Toasty 的连接、事务和 SQL 接口完成这些操作，并不是每张表都已经改成 Toasty 声明式模型。恢复正确性主要来自事务、约束和条件更新；换一个 ORM 并不会自动获得这些保证。

### 三种“历史”不能混为一谈

- **UI 消息**：适合显示，包含文本、工具 part、消息 ID 和 metadata。
- **模型历史**：适合下一次模型请求，保存 assistant 决策、工具调用及匹配的工具结果，包括模型 SDK 的专用内容。
- **进度事件**：适合重放流，告诉前端文字和工具状态如何变化。

不能简单地把页面上的文字拼起来作为模型上下文。工具的 `call_id` 必须与结果匹配，多步边界也要保留。

## 5. 先分清四个版本号，再看并发

最容易让人困惑的字段，是 `revision`、`version`、`generation` 和 `dispatch`。它们分别回答四个问题：

| 字段 | 在问什么 | 防止什么问题 |
| --- | --- | --- |
| `conversation.revision` | 你看到的会话还新鲜吗？ | 旧页面基于过期历史提交 |
| `run.version` | 你控制的是当前任务状态吗？ | 多个页面重复暂停或继续 |
| `run.generation` | 你还是当前合法执行者吗？ | 旧 worker 迟到写入 |
| `run.dispatch` | 这是当前这一轮调度吗？ | 旧队列消息重新启动任务 |

version 可以在一次正常执行中多次增加。generation 主要在领取、控制和恢复等执行权变化时增加。dispatch 在重新排队时增加。它们不要求一直相等。

### 两个网页同时发消息，会怎样？

```mermaid
sequenceDiagram
    participant A as 页面 A
    participant B as 页面 B
    participant DB as 业务事务
    A->>DB: 读取 revision 7
    B->>DB: 读取 revision 7
    A->>DB: 提交新消息，expectedRevision 7
    DB-->>A: 接受，创建 run，revision 更新
    B->>DB: 提交新消息，expectedRevision 7
    DB-->>B: 409，版本过期或已有活动 run
    B->>DB: 重新读取权威快照
```

409 的意思是“你的依据过期了，请重新确认”，不是“把版本换成最新数字，然后无限重试”。另一页面可能已经改变了用户意图。

数据库还用部分唯一索引限制同一 conversation 只有一个未终结 run。代码检查表达业务规则，数据库约束负责兜底。

## 6. 一条消息如何可靠地变成后台任务？

这里有一个常见陷阱：先保存用户消息，再调用队列 API。假如消息保存成功后进程退出，队列任务还没创建，用户就会看到一条永远没有回复的消息。

反过来也不行：先入队，再保存。worker 可能先开始执行，随后保存失败，任务便失去了对应的会话事实。

### 6.1 Outbox：把“之后要入队”也一起保存

Outbox 可以理解为数据库中的待发送清单。提交用户消息时，不立即要求队列成功，而是在同一业务事务中保存：

1. 新 user 消息及会话历史。
2. 新 run 和初始检查点。
3. 会话的 `active_run_id`。
4. 命令回执。
5. 一条 outbox 记录。

它们一起提交，或者一起回滚。后台 publisher 再把 outbox 发给 Apalis。事务 outbox 是常见的双写解决办法，也需要消费者处理重复消息；这与 [AWS 对 transactional outbox 的说明](https://docs.aws.amazon.com/prescriptive-guidance/latest/cloud-design-patterns/transactional-outbox.html)一致。

```mermaid
sequenceDiagram
    autonumber
    participant U as 浏览器
    participant H as Axum
    participant DB as Toasty 业务事务
    participant P as Outbox publisher
    participant Q as Apalis 队列
    participant W as Worker
    U->>H: POST /api/chat：新消息、revision、requestId
    H->>DB: 锁住 conversation，检查回执与版本
    Note over DB: 保存消息、run、回执和 outbox
    DB-->>H: COMMIT 成功，返回 run
    H-->>U: 打开该 run 的 SSE 订阅
    P->>DB: 读取未发布 outbox
    DB-->>P: runId 与 dispatch
    P->>Q: 发布任务
    Q-->>P: 发布成功
    P->>DB: 标记 outbox 已发布
    Q->>W: 投递任务
    W->>DB: claim：核对 queued 与 dispatch
    DB-->>W: 新 attempt、generation 与租约
    Note over W: 开始按检查点执行
```

开流和 worker 领取可以并发发生，不必谁先谁后。重要的是它们都发生在业务提交之后。

### 6.2 为什么允许队列重复投递？

publisher 已经向 Apalis 发布成功，但还没标记 outbox 已发布就退出。恢复之后，它会再发布一次。

因此我们接受“至少一次投递”：任务消息可能重复，但不能因为去重过度而漏掉任务。worker 在数据库里领取时，要求 run 仍为 `queued`，且 `dispatch` 匹配。第一个领取者把状态变成 `running`，后来的重复消息便不能再次取得执行权。

**Apalis 负责送来工作；业务库决定这份工作现在还能不能执行。** 不把队列显示的成功、失败或重试次数直接当成 run 的业务状态。

### 6.3 HTTP 重试和队列重复是两回事

HTTP 命令靠 `requestId` 去重。数据库按会话 scope 与 request ID 保存输入摘要和 `runId`：

- 相同 ID、相同输入：查回原 run，不创建第二个。
- 相同 ID、不同输入：返回 409。
- 响应丢失：调用 `GET /api/conversations/{id}/commands/{requestId}` 查询回执。

队列则靠 `dispatch` 与任务状态去重。外部工具靠 `operation_key` 与外部系统合作去重。不要用一个概念包揽这三层。

当前前端不自动重试 mutation，并且每次准备提交会生成新 request ID。后端的回执能力已经存在；前端“保存待确认命令、断网后自动查回”的完整体验仍是后续工作。如果以后实现重试，应保留同一 ID 和原始输入，而不是生成新 ID。

## 7. 检查点如何把 Agent Loop 变成可恢复执行？

代码中的 `Checkpoint` 包含这些核心字段：

```rust
// 摘自 crates/agent/src/phase.rs，省略 derive。
pub struct Checkpoint {
    pub codec_version: u32,
    pub history: ChatRequest,
    pub step: usize,
    pub max_steps: usize,
    pub model_inflight: bool,
    pub decision: Option<MessageContent>,
    pub next_tool: usize,
    pub tool_inflight: bool,
    pub answer: Option<String>,
}
```

`inflight` 表示“这项动作已经开始，但还没有保存完整结果”。`next_tool` 是已保存模型决策中，下一个需要执行的工具位置。

`Checkpoint::next()` 只读取这些字段，决定返回哪一种动作：

```mermaid
flowchart TD
    N["读取 Checkpoint"] --> D{"有已保存的模型决策？"}
    D -->|有| T{"还有未完成工具？"}
    T -->|有| TOOL["Tool：执行当前工具"]
    T -->|没有| STEP["StepComplete：结束本步"]
    D -->|没有| A{"已有最终答案？"}
    A -->|有| FIN["Finished"]
    A -->|没有| I{"model_inflight 为真？"}
    I -->|是| SAME["Model：重做当前逻辑步"]
    I -->|否| B{"步数预算耗尽？"}
    B -->|是| LIMIT["StepLimit"]
    B -->|否| NEW["Model：进入下一逻辑步"]
```

内存入口 `run_agent` 和持久化 driver 使用同一个阶段状态机。不同的是：内存入口直接改变内存；持久化入口在边界处提交数据库。这样不会维护两份彼此慢慢偏离的 Agent Loop。

### 必须保存的边界

以两工具例子为例：

```mermaid
sequenceDiagram
    autonumber
    participant W as 持久化 driver
    participant DB as 业务库
    participant M as 模型
    participant T as 工具
    W->>DB: 保存 begin_model，step 1，model_inflight true
    W->>M: 请求第一步决策
    M-->>W: 两个调用 c1、c2
    W->>DB: 保存完整 decision 与工具输入事件
    W->>DB: 保存 c1 的工具身份与 tool_inflight true
    W->>T: 执行 c1：add 3 与 5
    T-->>W: sum 8
    W->>DB: 原子保存结果、history、next_tool 1 和输出事件
    W->>DB: 保存 c2 的工具身份与 tool_inflight true
    W->>T: 执行 c2：add 8 与 2
    Note over W,DB: 此处若中断，c1 结果已确认，c2 仍未确认
    T-->>W: sum 10
    W->>DB: 保存 c2 结果，next_tool 2
    W->>DB: 保存本步完成事件
    W->>M: 第二步：完整决策与两个结果
    M-->>W: 最终答案
    W->>DB: 保存回答，再提交 finished 与 finish 事件
```

前后边界都有用途：

- **执行前保存**：恢复者知道这项动作可能已经发出，不能假定它从未发生。
- **执行后保存**：恢复者知道这项结果已确认，可以跳过它。
- **下一动作前提交**：避免第二项已经启动，第一项结果却还只在内存。

因此，工具结果和 `next_tool` 必须一起提交。如果结果写了但指针没写，可能重复执行；指针写了但结果没写，则可能跳过实际需要的结果。

### “逻辑步数”和“模型尝试次数”不同

第一步模型流被暂停，恢复后会重新请求这一逻辑步。`begin_model()` 看到 `model_inflight=true`，不会再次增加 `step`。

于是可能出现 `steps=1`，而 `modelCalls=2`。这表示同一逻辑步请求了两次，并不矛盾，也不会因为恢复偷偷获得更多逻辑步预算。

当前调用计数在发起动作前持久化，因此在极短的“计数已保存、HTTP 尚未发出”窗口里，计数可能大于外部服务实际收到的次数。它是已开始的执行尝试计数，不是提供商计费凭证。重做同样可能消耗额外 token，不能拿 `steps` 当成本。

## 8. 模型输出为什么有“草稿”？

模型流正在输出“我将先计算……”时，完整模型消息还没有返回。用户希望立即看到它，但下一次模型请求不应该把这段半截内容当作确认过的 assistant 消息。

因此当前实现把流片段保存为 `draft=true` 的事件：可以显示，但还不是确认后的模型历史。

```mermaid
flowchart LR
    TOKEN["模型文字片段"] --> DRAFT["保存草稿事件"]
    DRAFT --> SSE["前端即时显示"]
    DRAFT --> FULL{"完整模型响应已保存？"}
    FULL -->|是| VALID["本步事件转为已确认，history 保存完整内容"]
    FULL -->|暂停后重做| DROP["旧草稿标为无效"]
    DROP --> AGAIN["新 attempt 重新请求模型"]
    AGAIN --> REPLACE["重放时替换旧 assistant 草稿"]
```

模型完整响应保存后，该 attempt、该 step 的有效事件会转为非草稿。暂停时可以仍显示旧片段；resume、自动重排等重做路径会使未确认草稿失效，再从头重放有效内容。

这意味着：恢复后的文字可能与暂停前不同。我们恢复的是可确认的执行进度，而不是让新模型请求接着旧 token 接口继续输出。

遇到模型失败，run 会终结为 `failed`，页面可能保留部分输出作为失败诊断。不要把这些显示片段误认为完整回答；模型上下文只采用已提交的完整消息。

## 9. 暂停、终止和转向，分别做什么？

这三种按钮不能都叫“停止”，否则实现会互相冲突。

先看核心状态之间的关系。图里 `stepLimit` 对应接口的 `step-limit`，`needsAttention` 对应 `needs-attention`；省略了各种状态上的取消与失败箭头，以便先看清主干。

```mermaid
stateDiagram-v2
    [*] --> queued: 保存任务
    queued --> running: 领取执行权
    queued --> paused: 排队时暂停
    running --> pausing: 请求暂停
    pausing --> paused: 本地等待已结束
    pausing --> needsAttention: 外部结果未知
    paused --> queued: 原样继续
    paused --> superseded: 追加指令转向
    running --> finished: 完成回答
    running --> stepLimit: 预算耗尽
    running --> failed: 执行失败
    running --> cancelled: 明确终止
    running --> queued: 失联后的安全恢复
    running --> needsAttention: 恢复不安全或超限
    needsAttention --> cancelled: 核对后明确终止
    finished --> [*]
    stepLimit --> [*]
    failed --> [*]
    cancelled --> [*]
    superseded --> [*]
```

`needs-attention` 没画成终点，因为它仍保留待处理工作；当前没有人工回填结果后继续的接口，不能把它简单看成“失败后自动再试”。

| 操作 | 用户的意思 | 当前实现 |
| --- | --- | --- |
| 关闭网页／SDK `stop()` | 我暂时不看了 | 关闭订阅，run 继续 |
| `pause` | 先停下来，之后可能继续 | 撤销当前执行权，丢弃本地等待，保存 paused 或待核对状态 |
| `resume` | 按原指令继续 | 原 run 重新排队，新 attempt 从检查点继续 |
| `cancel` | 这个任务不再继续 | run 终结，拒绝迟到写入，不保证外部动作撤销 |
| `steer` | 暂停后加入新要求 | 旧 run 被替代，新 run 用保留结果重新规划 |

### 9.1 为什么暂停需要 `pausing`？

点击按钮时，数据库状态可以先改变，但 worker 丢弃 Future 和保存 attempt 结果需要时间。因此运行中的任务先进入 `pausing`，完成本地收尾后才变成 `paused`。

```mermaid
sequenceDiagram
    autonumber
    participant U as 前端
    participant H as 控制接口
    participant DB as 业务库
    participant W as 当前 worker
    participant M as 模型或工具
    W->>M: 正在等待结果
    U->>H: pause：runId、expectedVersion、requestId
    H->>DB: 原子改变状态，generation 增加
    DB-->>H: pausing
    H->>W: 同进程 CancellationToken 触发
    H-->>U: 返回新的 run 状态
    Note over W: select 分支结束，丢弃 drive Future
    W->>DB: settle attempt 与任务状态
    alt 当前动作可以安全恢复
        DB-->>W: paused
    else 外部结果无法确认
        DB-->>W: needs-attention
    end
    U->>H: 重新查询快照与 run
    Note over M: 外部服务已开始的动作可能仍然继续
```

“立即暂停”在这里指尽快中断当前本地异步等待。它不等于强制杀死远端模型或外部任务，也没有硬实时延迟保证。

同进程通过 CancellationToken 快速通知；不同进程没有共享这个内存 token，需要数据库心跳检查发现 generation 或状态改变。默认心跳 5 秒，因此跨实例暂停的传播可能有延迟。即使通知丢失，旧 generation 的写入也会被数据库拒绝。

数据库提交或已经发生的外部副作用也不能被简单“撤销”。如果工具自己启动了不随 Future drop 结束的后台工作，它必须自行管理生命周期。

### 9.2 原样继续与追加指令为何分开？

原样继续只改变调度，不改变用户意图。任务 ID、模型工具调用 ID 和已完成结果都继续使用，逻辑预算也不重置。

追加指令改变了计划。例如“第二项改成 8 + 20”。旧模型决策中仍然留着 `add(8,2)`，不能一边声称接受新要求，一边偷偷执行旧计划。

steer 会先整理旧历史：保留确认结果，为未执行调用补上“未执行”的工具错误；无法确认的外部动作不能伪装成成功。再保存新 user 消息，创建关联 `supersedes` 的新 run，从新计划开始。新 run 有自己的逻辑预算，旧 run 的执行记录仍可追溯。

当前只允许 paused 状态转向。尚未确认的 Idempotent／Reconcilable 外部操作必须先恢复并核对；虽然它们可安全继续，但不代表可安全放弃。直接 steer 会得到 409。

## 10. 最难的问题：工具真的只执行一次吗？

考虑一个支付工具，或者提交 ComfyUI 任务的工具：

```mermaid
sequenceDiagram
    participant W as Worker
    participant E as 外部服务
    participant DB as 本地业务库
    W->>DB: 保存工具正在执行
    W->>E: 提交操作
    E->>E: 扣款或创建生成任务
    E-->>W: 返回成功或外部任务 ID
    Note over W,DB: 进程在保存本地结果之前退出
    Note over DB: 本地只知道动作已开始，不知道外部是否完成
```

本地 PostgreSQL 事务管不到外部服务。不存在一个普通数据库事务，可以原子覆盖“远端创建图片”和“本地保存任务结果”。

如果直接重做，可能执行两次；如果直接跳过，可能永远没有结果。这个窗口必须靠工具的业务策略处理。

### 四种恢复策略

| 策略 | 含义 | 例子与恢复方式 |
| --- | --- | --- |
| `SafeToRetry` | 重做没有不允许的副作用 | 纯整数加法，重新算即可 |
| `Idempotent` | 同一操作身份重复提交，外部结果保持一致 | 外部 API 真正支持幂等 key，恢复复用它 |
| `Reconcilable` | 能查询已启动的原操作 | 已保存外部 job ID，恢复查询并等待原 job |
| `Conservative` | 无法证明重复安全 | 默认策略，未知结果进入 `needs-attention` |

这四种都是“未确认动作的恢复”策略，不是遇到任意工具错误都无限重试。当前普通工具执行错误会保存为工具错误结果，再交给模型继续处理；reconcile 查询出错则停待核对，不能把“查不到”冒充“原操作失败”。

### 幂等 key 必须跨 attempt 保持稳定

当前构造方式是：

```text
operation_key = run.id + "/" + tool_call_id
```

如果用 attempt ID 做 key，恢复时 attempt 变了，外部服务就会认为它是一次新操作。

给工具标注 `Idempotent` 只是声明；工具还必须把 key 传给实际会去重的外部系统。`Reconcilable` 工具必须持久化 external ID 并实现查询原操作，不能恢复时又提交一次。Temporal 也要求活动设计考虑幂等，并明确指出“执行成功但尚未上报就退出”会导致重试；幂等约束要由被调用的服务落实。[Temporal Activity Definition](https://docs.temporal.io/activity-definition)

### 保存 external ID 仍然有一个窗口

提交外部 job 后，我们会通过 `ExecutionContext.record_external_id()` 保存它，之后才继续等待。但外部 job 创建成功与 ID 保存成功之间仍可能退出。

如果拿不到 ID，又没有能按稳定 key 查询或去重的能力，系统只能停在待核对状态。因此 ComfyUI 工具未来接入时，需要同时设计提交身份、ID 记录与结果查询；不能仅给工具换一个 enum 就声称彻底可恢复。

`cancelled` 只表示本地任务不再继续。它不证明外部已取消。当前默认注册的生产工具是安全的整数加法；上述支付和 ComfyUI 是说明恢复接口如何扩展的例子，尚未作为持久化工具集成。

## 11. 后端重启后，为什么旧 worker 不能继续乱写？

任务恢复需要回答两个问题：谁可能失联了？谁现在有权写结果？分别由租约与 generation 解决。

### 11.1 Lease：有期限的执行权

worker 领取 run 时，数据库设置 `lease_until`。默认租期 30 秒，worker 每 5 秒续租。

只要它持续活着并仍拥有执行权，租约就向后延长。进程退出后，心跳停止；恢复扫描发现租约到期，便可以处理任务。

使用数据库时间判断期限，避免让不同 worker 的本地时钟各自裁定。worker 已错过期限后不能再自行续上旧租约。

### 11.2 Generation：旧执行者的结果不能覆盖新事实

想象旧 worker A 网络卡住。租约过期，worker B 已接管。A 随后又收到了模型结果，如果它还能保存，B 的状态就可能被覆盖。

generation 是单调增加的执行代次。每次领取、控制撤销或恢复都会使旧代次失效。保存关键结果时，不只按 run ID 更新，还要检查：

```sql
-- 来自 guarded() 的核心条件，省略返回列。
SELECT body FROM agent_runs
WHERE id = $1
  AND generation = $2
  AND status = 'running'
  AND lease_until > NOW()
FOR UPDATE;
```

phase commit 还检查 run version、attempt ID 和 attempt generation。事件 append 与 external ID 记录也要检查执行权。

```mermaid
sequenceDiagram
    participant A as 旧 worker A
    participant DB as PostgreSQL
    participant R as 恢复扫描
    participant B as 新 worker B
    A->>DB: 领取 generation 1，租期内执行
    Note over A: 卡住或进程失联
    R->>DB: 发现 lease 到期，撤销旧代次并重排
    B->>DB: 领取当前 dispatch，得到新 generation
    A->>DB: 迟到结果，携带 generation 1
    DB-->>A: 拒绝，旧执行权已失效
    B->>DB: 当前代次提交结果
    DB-->>B: 接受
```

这个机制常叫 fencing，中文可理解为“给写入加一道执行权栅栏”。它保证合法写入者的身份，不保证旧 worker 在外部世界没有做动作；后者仍需要幂等或核对。

### 11.3 为什么统一先锁 conversation，再锁 run？

提交、控制、领取和结果保存可能同时发生。如果一个事务先拿 run 锁再拿 conversation 锁，另一个按相反顺序拿锁，就可能相互等待。

本项目把相关事务的锁顺序统一为 conversation → run，让状态与显示快照在一致的业务边界下更新。行锁只在短事务里持有，不把模型网络等待包在长事务中。固定锁顺序是避免死锁的重要实践；参见 [PostgreSQL Explicit Locking](https://www.postgresql.org/docs/current/explicit-locking.html)。

## 12. 重启恢复的完整时序

回到两工具例子：c1 结果已经提交，c2 正在执行时进程退出。

```mermaid
sequenceDiagram
    autonumber
    participant A as 旧进程 worker
    participant DB as PostgreSQL
    participant R as 新进程恢复扫描
    participant Q as Outbox 与 Apalis
    participant B as 新进程 worker
    A->>DB: c1 结果已确认，next_tool 1
    A->>DB: c2 开始，tool_inflight true
    Note over A: 进程强制退出，心跳停止
    Note over DB: 持久数据保留，等待租约到期
    R->>DB: 扫描过期 running 或 pausing
    R->>DB: 锁定并重新核对状态
    Note over DB: 旧 attempt 标记 interrupted
    alt c2 可安全重做或核对，且未超恢复上限
        R->>DB: 新 generation、dispatch，queued 与 outbox
        Q->>DB: 读取待发布调度
        Q->>B: 投递恢复任务
        B->>DB: claim，创建新 attempt
        B->>B: Checkpoint.next 返回 c2
        Note over B: 跳过已确认 c1
        B->>DB: c2 完成后提交结果
        B->>B: 下一步模型整理最终答案
    else 外部结果不安全或恢复次数耗尽
        R->>DB: needs-attention，停止自动执行
    end
```

实际恢复行为还受原状态影响：

- `running`：符合策略时重新排队，从检查点继续。
- `pausing`：恢复成 paused 或 needs-attention，不自动续跑，尊重用户暂停意图。
- `finished`、`failed`、`cancelled`、`superseded`、`step-limit`：属于终结状态，不因队列重复投递而重新运行。
- 配置中的模型或工具 schema 与 run 指纹不一致：停在 needs-attention，避免用新规则解释旧计划。

默认恢复扫描约每 500ms 运行一次，最多自动恢复 5 次。强制退出后恢复要等剩余租期，还要经过扫描、发布和领取，因此不是“进程一起来就立刻继续”。演示脚本会缩短租期以加快测试，生产默认值不应照搬测试等待时间。

正常 Ctrl+C 会有最多 10 秒的优雅关闭窗口：丢弃本地等待，安全任务重新排队，未知外部动作转为待核对。强制杀进程没有这个收尾机会，只能依靠租约与恢复扫描。

## 13. 前后端怎样一起完成重连？

后台恢复解决执行，网页恢复解决显示。这两个循环通过 run ID 和持久事件联系起来。

### 13.1 普通查询与 SSE 分工

```mermaid
flowchart LR
    Rust["Rust DTO 与 utoipa-axum 路由"] --> Spec["OpenAPI"]
    Spec --> Gen["openapi-ts 生成客户端"]
    Gen --> TQ["TanStack Query"]
    TQ --> Snap["会话快照、run 查询、控制命令"]
    SSE["Axum UI Message Stream v1"] --> SDK["AI SDK DefaultChatTransport"]
    SDK --> Parts["UIMessage parts"]
    Snap --> Hook["useDurableChat 协调选择与重放"]
    Parts --> Hook
    Hook --> Elements["AI Elements 展示"]
```

OpenAPI 解决普通 HTTP 合约，AI SDK 解决 UIMessage 流解析。给 SSE 在 OpenAPI 里声明 `text/event-stream`，不会让普通生成客户端自动理解消息、工具和步骤边界。

当前常用接口如下：

| 接口 | 职责 |
| --- | --- |
| `POST /api/conversations` | 创建会话或导入合法历史 |
| `GET /api/conversations/{id}` | 读取 UI 消息与当前 run ID |
| `POST /api/chat` | 原子提交一条新消息并附着 SSE |
| `GET /api/chat/{runId}/stream` | 重放该 run 的有效前缀，再读取新增事件 |
| `GET /api/runs/{runId}` | 查询任务状态、attempt 与统计 |
| `POST /api/runs/{runId}/{action}` | pause、resume、cancel 或 steer |
| `GET /api/conversations/{id}/commands/{requestId}` | 查询命令回执 |

会话响应没有内部模型 history；run 响应没有 checkpoint、租约或内部配置。公共 DTO 是给页面使用的视图，执行状态由后端控制。

### 13.2 刷新网页后的完整过程

```mermaid
sequenceDiagram
    autonumber
    participant U as 浏览器重新打开
    participant H as useDurableChat
    participant Q as Query 客户端
    participant S as Axum SSE
    participant DB as PostgreSQL
    participant W as 后台 worker
    U->>H: URL 中的 conversation ID
    H->>Q: GET 会话快照
    Q->>DB: 读取消息与 activeRunId
    DB-->>H: 权威 UI 快照
    H->>Q: GET 当前 run
    Q-->>H: queued、running 或 pausing
    H->>H: 移除该 assistantId 的旧显示
    H->>S: resumeStream，GET runId 的 stream
    S->>DB: 从 sequence 0 读取有效事件
    DB-->>S: 已确认前缀和当前有效草稿
    S-->>H: start、文字、工具等事件
    H-->>U: 官方 parser 重建 assistant
    W->>DB: 保存后续进度
    S->>DB: 按 sequence 读取新增事件
    S-->>H: 新片段与 run 状态
    H-->>U: 继续显示
```

当前选择的是**全前缀重放**：每次订阅从头读取有效事件，而不是让浏览器保存一个 token 游标。正常结束的任务可以直接使用快照显示，显式读取流也能重放已保存结果。

为什么先移除旧 assistant？假设页面已有“结果是”，重放又收到同样的前缀，如果直接追加就会得到“结果是结果是”。保持稳定的 assistant ID，然后替换重建，可以避免这类重复。

重连读取事件不会再调用模型，也不增加执行的 token 统计。执行重试与输出重放的区别，在这里变得非常具体。

### 13.3 数据库事件怎样变成 UIMessage？

典型协议次序如下。这是协议示意，实际 run 还会发送 metadata 和状态数据：

```text
start
start-step
tool-input-available
tool-output-available
finish-step
start-step
text-start
text-delta ...
text-end
finish-step
finish
[DONE]
```

工具失败发送 `tool-output-error`。暂停、代次变化或取消时，流适配器闭合已经打开的文本块与步骤，发送状态与 `abort`；只有正常完成／步数耗尽路径发送对应 `finish`。

响应采用 `x-vercel-ai-ui-message-stream: v1`。这个公开协议允许 Rust 等其他语言的后端与 AI SDK UI 配合；它规定传输形状，不负责数据库、任务排队或恢复。[AI SDK Stream Protocols](https://ai-sdk.dev/docs/ai-sdk-ui/stream-protocol)

### 13.4 generation 改变时，旧订阅也要结束

旧订阅可能已经显示了即将失效的草稿。检测到 generation 变化后，服务端结束这一订阅，让前端加载新快照并重建；不能在同一显示上混合两次模型尝试的文字。

订阅每批最多读 256 个事件，结合本进程 Notify 和 250ms 轮询。Notify 用于快速唤醒，轮询补偿丢失通知以及跨进程没有内存通知的情况。状态和事件批次在业务事务锁下读取，避免拼出彼此不一致的快照。

10 秒一次的 SSE 注释心跳只保持连接，不生成消息 part。慢消费者的事件 yield 恢复后若发现阻塞超过 30 秒，会脱离订阅；这是当前实现的慢消费判断，不是所有完全停止轮询连接的硬超时承诺。无论订阅如何结束，后台 run 都独立执行。

### 13.5 为什么用了 TanStack Query，还要选择代次？

假设选了会话 A，A 查询比较慢；紧接着切到 B，B 很快加载成功；最后 A 的旧响应才回来。如果直接把结果放进当前页面，会把 B 覆盖成 A。

TanStack Query 按会话和 run 身份隔离缓存，但本地 `setMessages()`、`resumeStream()` 等副作用仍要判断自己是否属于当前选择。

`SessionRequests` 每次选择都增加 epoch。旧结果即使忽略了 AbortSignal，也会因为 epoch 不匹配被丢弃。流的状态数据还检查 conversation ID、run ID 与 version。

当前 run 查询约每秒一次，终结后停止轮询；控制 mutation 期间停止状态轮询。SSE 用来传细粒度输出，普通查询用来校准状态，两条通道配合完成显示。

## 14. 与 Vercel AI SDK、Workflow 和 Temporal 对比

先纠正一个称呼：Vercel AI SDK 是 SDK，不是一种唯一的“Vercel 后端”。官方提供多层能力和不同示例，应用可以选择自建服务器、Vercel Functions，或持久工作流。

### 14.1 AI SDK 的消息持久化示例

官方说明区分 UIMessage 和模型消息，提供消息保存与校验的集成方式；处理断线时，可以在后端继续消费模型流，再保存完成结果。当前指南使用的具体转换 API 会随 SDK 版本变化，因此这里只比较职责，不把最新 TypeScript 例子直接当成本项目固定版本的可运行代码。[Chatbot Message Persistence](https://ai-sdk.dev/docs/ai-sdk-ui/chatbot-message-persistence)

我们的对应点是：保存 UI 视图；加载后用官方 `validateUIMessages` 校验；worker 独立消费模型输出。进一步增加了阶段检查点，保存粒度不只在最终回答完成时。

这层适合讲解保存聊天，但仅做到这层，不能证明进程退出后能继续工具执行。

### 14.2 Redis resumable-stream 方案

官方重连指南组合消息存储、活动 stream ID、Redis 和 `resumable-stream`，通过 POST 创建、GET 重连。可重连场景里的客户端 abort 只表示连接断开；真正停止需要单独的后端控制接口。[Chatbot Resume Streams](https://ai-sdk.dev/docs/ai-sdk-ui/chatbot-resume-streams)

```mermaid
flowchart LR
    UI["AI SDK UI"] --> POST["POST 创建生成"]
    POST --> Producer["输出生产者"]
    Producer --> Redis["Redis 保存并广播流"]
    POST --> Store["保存活动 stream ID"]
    UI --> GET["GET 找到原 stream"]
    Store --> GET
    Redis --> GET
    GET --> UI
```

我们用 PostgreSQL 事件表提供重放，并用 run ID 定位生产任务。实现不同，但前端连接与生产者分开的职责相同。Redis 方案可以解决读流的重连；要评估生产者崩溃后的恢复，仍需检查它背后的执行系统。

这里没有必要为了使用 AI SDK UI，强行把 PostgreSQL 事件复制一份到 Redis。更大的并发或重放负载出现后，再决定缓存、广播和游标优化。

### 14.3 `waitUntil`／`after` 为什么不等于任务恢复？

在 Vercel Functions 中，`waitUntil()` 可让异步工作在响应发送后继续，但它仍受函数本身的超时限制；官方对 Next.js 的集成还介绍了 `after()`。这不是保存执行位置的 API。[Vercel Functions API](https://vercel.com/docs/functions/functions-api-reference/vercel-functions-package)

因此不能从“断线后继续”推导出“后端重启后继续”。本项目的 Axum 是常驻进程，worker 不受单个响应生命周期约束；进程退出后的恢复则由数据库事实、租约和重新领取共同完成。

### 14.4 Vercel 现在也有持久 Agent 工作流

截至本次调研，官方已有 `@ai-sdk/workflow` 的 `WorkflowAgent`：在工作流中运行 Agent，将工具执行变成持久步骤，保存结果并支持重试。指南还介绍了流输出转换、重连 transport，以及重试时清除失败步骤的部分输出；其安装说明依赖 Workflow 5 的 beta 版本。[WorkflowAgent](https://ai-sdk.dev/docs/agents/workflow-agent)

所以不能笼统说“Vercel 只有 Redis，没有执行恢复”。我们的手写阶段 driver，与它解决的持久执行问题属于同一层；区别是运行时、语言和恢复机制。Workflow 通过步骤结果回放重建执行；我们用显式 Checkpoint 决定下一动作。[Workflow 的执行与步骤说明](https://github.com/vercel/workflow/blob/main/docs/content/docs/v5/how-it-works/understanding-directives.mdx)

对 Rust、自托管和已有 Agent Loop，这套显式实现便于理解和控制；对 TypeScript 项目，现成工作流可以减少自行维护的基础设施。不能仅比较代码行数，还要考虑部署、版本、工具副作用和维护成本。

### 14.5 Temporal 给我们的参考

Temporal 将编排与实际活动执行分开，保留历史并恢复；活动取消也需要协作，心跳可以传递取消信号。当前官方说明提醒开发者不要把活动取消理解成强制撤销外部动作。[Temporal Activity Execution](https://github.com/temporalio/documentation/blob/main/docs/encyclopedia/activities/activity-execution.mdx)

借鉴到本项目，就是把任务控制写进持久状态，用执行监测传播取消，用工具策略处理不确定结果。我们没有引入 Temporal，也没有实现它的全部工作流能力。

### 一张表看清职责

以下是根据上述官方说明与本项目源码做的能力归纳，不是产品性能测评。

| 层或方案 | 消息历史 | 断线后读流 | 进程退出后继续执行 | 谁负责工具副作用 |
| --- | --- | --- | --- | --- |
| 消息持久化集成 | 应用负责保存 | 需另行接入 | 仅保存消息不提供 | 应用 |
| Redis resumable-stream 示例 | 结合应用存储 | 流存储与重连接口 | 取决于输出生产者 | 应用 |
| Vercel 持久工作流 Agent | 结合应用会话 | 工作流流与 transport | 工作流持久步骤 | 工具与外部服务仍需配合 |
| Temporal 类工作流 | 应用定义会话 | 需应用接入 UI 协议 | 工作流／活动恢复 | 幂等、重试和业务补偿 |
| 本项目 PostgreSQL + Apalis | 服务端权威会话 | 有效事件全前缀重放 | 显式检查点与租约恢复 | 四类恢复策略，未知结果停待核对 |

我们采用的边界符合这些方案共同的思路：**连接可丢失，执行事实必须可恢复，外部副作用必须有自己的安全规则。** 这说明设计方向合理，不代表无需继续验证或已经达到成熟工作流平台的能力。

## 15. 哪些证据支持我们的实现？

“图看起来正确”不是测试结论。当前仓库用本地模拟模型和真实 PostgreSQL 验证关键窗口，而不是只检查最终答案。

| 需要证明的性质 | 对应验证 |
| --- | --- |
| 重复提交不产生第二个 run | 仓储回执、冲突与事务回滚测试 |
| 已确认工具不因恢复而重做 | 两工具之间强制杀进程恢复测试 |
| 暂停不会被重启自动解除 | pausing 进程强制退出测试 |
| 旧执行者不能继续写 | generation、lease 与过期提交测试 |
| 未知外部结果不盲目重提 | Conservative／Reconcilable 测试 |
| 重放不增加模型调用和 usage | 进程级重放前后统计对比 |
| 第二轮模型历史完整 | assistant/tool 历史和调用 ID 测试 |
| 页面 A 旧响应不覆盖 B | SessionRequests 与 React/query 集成测试 |
| SSE 被官方 SDK 接受 | 官方 UIMessage parser 与 DefaultChatTransport 门禁 |

本文基线对应的验证包括 31 项真实 PostgreSQL 测试、8 个进程级生命周期场景，以及独立的官方 parser 步数上限门禁；前端有 4 项会话规则测试和 3 项 React/query 集成测试。Phoenix 门禁另验证 attempt 生命周期关联。这里描述的是已验证的场景，不是所有故障组合的证明。

测试本身也可能有竞态。例如，模型调用计数已经持久化，不代表 mock HTTP 服务已经收到请求。强制退出测试会先等待 mock 明确接收请求，再杀进程，避免把“尚未开始的外部请求”误当成“请求中途崩溃”。

## 16. 从零动手：先启动，再做免费恢复实验

### 16.1 启动正常应用

准备 Docker、Rust 1.95+、Node 22.18+ 和 pnpm。已有 `.env` 时保留配置；新环境按 `.env.example` 创建 `.env`，设置模型与数据库连接。

```powershell
# 仓库根目录。PostgreSQL 数据卷会保留数据。
docker compose -f compose.postgres.yaml up -d --wait

# 显式创建或检查业务与 Apalis schema。
cargo run -p persistence --bin migrate

# 一个终端启动后端。
cargo run -p server
```

另一个终端启动前端：

```powershell
pnpm -C apps/web install
pnpm -C apps/web dev
```

默认前端是 `http://localhost:5173`，后端是 `127.0.0.1:3001`。`apps/web/.env` 的 `VITE_API_BASE` 与根 `.env` 分别由各自入口读取。

先发一条消息，记下 URL 中的 conversation ID。刷新网页，会话会从后端快照加载；任务仍活动时，前端再连接 run 的 SSE。真实模型的短任务可能很快结束，观察暂停和重启更适合使用下面的 mock 门禁。

迁移不会在 server 启动时自动运行。进程恢复与数据库升级是两件事：运行旧任务前，还要保证 schema 和代码对检查点的解释一致。

### 16.2 看懂一次提交的 JSON

创建会话后，前端发送的是一条新消息，不是整段历史：

```json
{
  "id": "<conversationId>",
  "expectedRevision": 0,
  "requestId": "<stable-command-id>",
  "message": {
    "id": "<user-message-id>",
    "role": "user",
    "parts": [{ "type": "text", "text": "计算 3 + 5" }]
  }
}
```

三个 ID 分别标识会话、命令和消息。`expectedRevision` 来自最新快照，后续轮次不能继续硬编码为零。

暂停命令示意：

```json
{
  "conversationId": "<conversationId>",
  "expectedVersion": 4,
  "requestId": "<stable-pause-command-id>"
}
```

发送到 `POST /api/runs/{runId}/pause`。数字 4 只是示意，必须换成刚读取的 version；即使先查过版本，提交时也仍可能冲突。

### 16.3 用 mock 验证，不依赖真实模型速度

测试需要一个专用 `_test` 父库。以下创建命令仅在父库尚不存在时执行一次；密码和端口应与本地配置一致。

```powershell
docker compose -f compose.postgres.yaml exec postgres createdb -U comfy_agent comfy_agent_test

$env:TEST_DATABASE_URL = 'postgresql://comfy_agent:comfy_agent_local@127.0.0.1:5432/comfy_agent_test'
cargo build --workspace
npm ci --prefix scripts/evals
npm ci --prefix scripts/ai-sdk-check

# 数据库事务、恢复、外部工具策略和 HTTP 测试。
cargo test -p persistence -p server -- --ignored --test-threads=1

# 本地 mock 服务 + 进程重启 + 全前缀重放。
node --experimental-strip-types scripts/durable-check/check.mjs

# 工具往返与第二轮历史：由固定版本官方 SDK 解析。
npm run check:mock --prefix scripts/ai-sdk-check

# 前端选择隔离与查询协调。
pnpm -C apps/web test
```

mock 脚本创建随机子库和独立服务进程，测试后只清理自己的资源。业务库和测试父库保留。不要把 `TEST_DATABASE_URL` 指向日常会话数据库。

进程级脚本会输出 `outputs/durable-check.json`。观察暂停恢复场景：同一 run 有两个 attempt、逻辑步数仍为 1、模型尝试次数为 2，旧草稿不会出现在最终重放结果中。`usageComplete=false` 表示中断尝试的 usage 未知，而不是没有花费。

有本地 Phoenix 时，还可以执行：

```powershell
docker compose -f compose.phoenix.yaml up -d
npm run smoke:phoenix --prefix scripts/evals
```

它会验证执行 trace 与任务身份的关联。强制退出可能丢失最后一批 span，恢复判断以业务库为准；trace 用来解释过程，不承担任务账本的职责。

## 17. 当前实现合理，但哪些地方还需要升级？

把边界写清楚，是为了知道下一项改动应该解决什么问题。

### 当前已经成立的边界

提交先持久化；任务独立于网页；检查点细到模型决策和单个工具；控制命令有版本约束；旧执行写入有代次和租约限制；未知外部结果停止自动执行。这些设计与前面的故障窗口逐一对应。

同时，它是面向本项目 Agent 的显式运行时，不是一个任意程序都能接入的通用工作流引擎。

### 后续值得做的改进

| 当前限制 | 影响 | 合理的下一步 |
| --- | --- | --- |
| 全前缀重放 | 长回复重复传输，断线越多越明显 | 设计游标、快照与代次失效协议，避免直接拼接 |
| 事件逐片段持久化，投影读取完整有效事件 | 长回复写入和投影成本较高 | 合并片段、增量投影和性能测试，同时保留提交边界 |
| 草稿失效只改变有效标记 | 旧事件仍占用 run 事件容量 | 定义保留、清理和容量统计规则 |
| 控制与进度通知主要是进程内 Notify | 多实例依赖轮询／心跳补偿 | 可加入 PostgreSQL 通知，保留轮询兜底 |
| 同一可执行文件装配 API 和 worker | 扩容策略耦合 | 有需求时增加独立启动角色 |
| 前端未保存待确认 command | 断网后缺少完整自动查回体验 | 保存稳定 request ID 和原输入，明确恢复流程 |
| 没有人工结果回填接口 | needs-attention 主要靠人工核对与明确终止 | 增加受约束的核对与修复操作 |
| 仅校验模型与工具 schema 指纹 | 工具实现改变但 schema 未变时无法自动发现 | 增加工具实现／工作流版本及兼容策略 |
| JSON body 与索引列、消息记录存在冗余 | 写入维护复杂，查询与存储成本增加 | 根据实际访问模式调整模型，不先做无依据的大改 |
| schema 迁移目前是固定 v1 | 不适合无限增长的版本升级链 | 引入清晰的顺序迁移与升级演练 |
| 当前无鉴权、租户隔离和删除策略 | 适合本地开发，不足以公开多人服务 | 在对外部署前设计权限、备份和数据生命周期 |

连接池也影响实际稳定性。当前 Toasty 业务池默认 16，Apalis 队列池默认 8；一个实例最多按两者之和规划连接，再为迁移和管理预留。请求等不到连接要有超时，不能依赖用户关掉网页来释放无限等待。

每 run 的进度事件默认限 16 MiB，模型回调队列限 128 项；超限会停止执行。容量限制是明确的失败边界，不意味着所有长任务都应该失败在这个大小。扩大容量前应先测清片段数量、重复草稿与投影成本。

这些是从当前实现推导出的维护方向，不代表每项都要马上引入新库。规模、工具副作用和运行时长决定哪些升级优先。

## 18. 带着问题回到源码

按下面的顺序阅读，比从所有模块的第一行开始更容易：

1. [阶段状态机](../crates/agent/src/phase.rs)：`next()` 如何选择下一动作，`begin_model()` 为何不增加恢复步骤预算。
2. [运行数据](../crates/runtime/src/model.rs)：conversation、run、attempt、工具执行与事件的关系。
3. [执行生命周期](../crates/runtime/src/execution.rs)：claim、CancellationToken、watchdog 和 settle 如何配合。
4. [模型阶段](../crates/runtime/src/execution/driver/model.rs)与[工具阶段](../crates/runtime/src/execution/driver/tool.rs)：哪些状态必须在外部动作前后提交。
5. [业务仓储](../crates/persistence/src/repository.rs)：submit、control、commit、recover 的事务边界和锁顺序。
6. [队列与发布](../crates/persistence/src/queue.rs)：为什么发布成功后才确认 outbox，以及重复消息为何无害。
7. [SSE 订阅](../crates/server/src/stream.rs)：重放、generation 变化、协议闭合和订阅生命周期。
8. [前端协调](../apps/web/src/hooks/use-durable-chat.ts)：查询快照、替换草稿、resumeStream 与控制请求。
9. [工具恢复接口](../crates/tools/src/lib.rs)：operation key、external ID 与四种恢复策略。

下一次设计一个工具时，可以先拿这三个问题检查自己的实现：

- 调用已经发出，但结果没有保存，我能知道它发生了什么吗？
- 同样的动作执行两次，业务上能接受吗？
- 用户暂停、取消或追加指令时，外部系统的动作应该怎样处理？

数据库和队列只能保留这些问题的证据。具体答案要由工具和业务一起提供。当每个阶段都有明确的开始记录、结果记录和不确定状态，Agent 才能在连接断开与进程退出之后，继续做可信的工作。

---

日常操作查 [持久化会话与后台任务运行指南](durable-sessions.md)；trace 与恢复成本分析可接着读 [从零理解 Agent 观测与评测](agent-observability-evaluation.md)。本文对比依据均在相关段落链接到官方资料；外部 SDK 的接口、状态和版本以阅读时的官方文档为准。
