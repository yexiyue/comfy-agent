# comfy-agent

用于自然语言驱动 ComfyUI 工作流的 Rust Agent 工程。当前提供 Agent 核心库、工具运行接口、函数属性宏、兼容 Vercel AI SDK 的 Axum 聊天后端，以及 `apps/web` 下的聊天前端。

## 目录与职责

| 目录 | 职责 |
| --- | --- |
| `crates/agent` | Agent 循环、流式事件、工具注册表、模型客户端配置 |
| `crates/tools` | `AgentTool` trait 和 `#[agent_tool]` 宏入口 |
| `crates/tool-macros` | 函数属性过程宏实现 |
| `crates/runtime` | 业务状态、存储端口与持久阶段驱动器 |
| `crates/persistence` | Toasty 事务仓库、显式迁移与 Apalis outbox/worker |
| `crates/server` | Axum HTTP 服务、UIMessage 历史转换与 SSE 协议适配 |
| `apps/web` | 聊天前端：Vite + React 19 + Tailwind v4 + shadcn + AI Elements + AI SDK |
| `workflows/` | ComfyUI 工作流与 API 格式样例 |
| `scripts/` | ComfyUI 验证脚本 |
| `docs/` | Agent Loop 博客与配图 |

根目录是虚拟 Cargo workspace，Rust 源码与测试均位于 `crates/`。依赖版本、edition 和 lint 在根目录集中管理。

```mermaid
flowchart LR
    Frontend["apps/web：聊天前端"] --> Backend["server：Axum + UI Message Stream"]
    Backend --> Runtime["runtime：任务控制与持久驱动"]
    Runtime --> Persistence["persistence：Toasty + Apalis"]
    Runtime --> Agent["agent：共享阶段状态机"]
    Agent --> Tools["tools：工具接口"]
    Tools --> Macros["tool-macros：代码生成"]
```

## 开发与验证

```bash
cargo check --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

`cargo run -p server` 启动 HTTP 后端。根目录默认 Cargo member 仍为 `agent`，检查所有 crate 时请使用 `--workspace`。

## 聊天后端

### 业务数据库与兼容性测试

持久化实现使用独立 PostgreSQL，保留 Phoenix 的现有数据卷：

```powershell
docker compose -f compose.postgres.yaml up -d --wait
docker compose -f compose.postgres.yaml exec -T postgres psql -U comfy_agent -d postgres -c "CREATE DATABASE comfy_agent_test"
$env:TEST_DATABASE_URL = 'postgresql://comfy_agent:comfy_agent_local@127.0.0.1:5432/comfy_agent_test'
cargo test -p persistence --test compatibility -- --ignored --test-threads=1
docker compose -f compose.postgres.yaml stop
```

创建测试库只需执行一次；已有库不要删除或重建。测试要求库名以 `_test` 结尾，与 `DATABASE_URL` 业务库分开。`stop` 保留数据，`start --wait` 可重新启动。镜像固定版本与 digest，仅绑定 `127.0.0.1:5432`；本地默认密码见 `.env.example`。测试验证事务原子性、取消回滚、条件领取竞争以及队列孤儿恢复，默认 workspace 测试不自动连接数据库。

业务迁移是显式操作，读取根目录可选 `.env` 的 `DATABASE_URL`：

```powershell
cargo run -p persistence --bin migrate
docker compose -f compose.postgres.yaml exec -T postgres pg_dump -U comfy_agent comfy_agent > agent-backup.sql
```

升级前先停止应用 worker、备份，再迁移和启动。打开数据库不会自动建表；schema 版本不兼容时拒绝继续。`cargo run -p persistence --bin migrate -- --rollback-empty` 只允许回退没有业务记录的 schema，不删除 Apalis 数据；有数据时保留新表，停止 worker 后回退应用代码，旧版不能继续新任务。备份文件包含会话内容，请放在仓库外保存。

### 启动与协议

从根目录执行，按 `.env.example` 配置模型和 key（可复制为 `.env`），然后启动：

```bash
cargo run -p server
```

服务默认监听 `127.0.0.1:3001`。先启动 PostgreSQL、配置 `DATABASE_URL` 并执行上述显式迁移。`GET /health` 仅报告存活，数据库故障的业务请求返回 503。服务启动后台 Apalis worker，关闭网页不会停止任务。

接口概要：

| 接口 | 用途 |
| --- | --- |
| `POST /api/conversations` | 创建会话，可通过 `messages` 严格导入完整历史 |
| `GET /api/conversations?offset=0&limit=20` | 分页会话列表，limit 为 1–100 |
| `GET /api/conversations/{id}` | 权威历史、revision 与 activeRunId |
| `POST /api/chat` | 接受一条新 user 文本消息并订阅结果 |
| `GET /api/runs/{id}` | 状态、version、attempts 和实际调用统计 |
| `POST /api/runs/{id}/{pause,resume,cancel,steer}` | 明确控制后台任务 |
| `GET /api/chat/{runId}/stream` | 从头重放有效前缀，随后读取新增事件 |
| `GET /api/conversations/{id}/commands/{requestId}` | 响应丢失后查回已接受的 runId |

Bash 示例需要 jq；Windows 可保存 JSON 文件，用 `curl.exe --data-binary @request.json`：

```bash
conversation=$(curl -s http://127.0.0.1:3001/api/conversations -H 'Content-Type: application/json' -d '{}')
id=$(printf '%s' "$conversation" | jq -r .id)
curl -N http://127.0.0.1:3001/api/chat -H 'Content-Type: application/json' \
  -d "{\"id\":\"$id\",\"expectedRevision\":0,\"requestId\":\"demo-1\",\"message\":{\"id\":\"u1\",\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"使用 add 工具计算 3 + 5\"}]}}"
```

`messages` 全历史覆盖与 regenerate 被明确拒绝。之后每轮先读取最新会话 revision；重试保持同一 requestId 和相同输入，同键异内容返回 409。控制命令携带 `conversationId`、`expectedVersion`、`requestId`；steer 还需要新 `message` 和 `expectedRevision`。一个会话最多一个非终结任务，暂停和 needs-attention 仍占用它。

前端以服务端快照初始化 `useChat`，只提交新增消息：

```tsx
const transport = new DefaultChatTransport({
  api: `${base}/api/chat`,
  prepareSendMessagesRequest: ({ messages }) => ({
    body: { id: snapshot.id, expectedRevision: snapshot.revision,
      requestId: crypto.randomUUID(), message: messages.at(-1) },
  }),
  prepareReconnectToStreamRequest: () => ({ api: `${base}/api/chat/${run.id}/stream` }),
});
const chat = useChat({ transport });
// 重连前 setMessages(snapshot.messages.filter(m => m.id !== run.assistantId))，再 resumeStream。
```

重连需要替换旧 assistant 草稿，不能把前缀追加到旧消息。SDK `stop()` 仅停止订阅；暂停/终止必须调用控制接口。暂停立即中断当前等待；原样继续沿用 run、新建 attempt，追加指令则 supersede 旧 run 并重新规划。默认 `add` 工具可安全重做，其他工具默认保守处理。外部副作用不承诺回滚或 exactly-once。

SSE 使用 [UI Message Stream v1](https://ai-sdk.dev/docs/ai-sdk-ui/stream-protocol)，保留步骤/文本边界、工具事件与 `[DONE]`，10 秒注释心跳。开始 metadata 包含 runId；完成包含 outcome、steps、runId、attemptId，观测开启时增加 traceId。暂停、取消通过 `data-run-state` 与 `abort` 表达。内存事件队列 128 项、每 run 持久事件默认 16 MiB；保存失败停止执行。请求体 2 MiB、明确来源 CORS 和无鉴权本地开发范围保持不变。

固定 `ai@7.0.127` 的协议门禁和真实数据库测试均只使用免费本地模型：

```powershell
$env:TEST_DATABASE_URL = 'postgresql://comfy_agent:comfy_agent_local@127.0.0.1:5432/comfy_agent_test'
cargo test -p persistence -p server -- --ignored --test-threads=1
npm ci --prefix scripts/ai-sdk-check
npm run check:mock --prefix scripts/ai-sdk-check
```

协议与评测脚本创建并迁移随机子库，退出时仅清理各自子库，保留测试父库。完整生命周期与故障验证见 [持久化会话运行指南](docs/durable-sessions.md)。

## 聊天前端

本地 Phoenix 观测与评测启动见 [观测与评测指南](docs/observability.md)。使用 `docker compose -f compose.phoenix.yaml up -d` 启动 Phoenix，设置 `OTEL_ENABLED=true` 后运行后端即可查看轨迹；原始内容采集默认关闭。`scripts/evals` 通过同一 Rust SSE 后端进行确定性评分和本地实验比较。

第一次接触这些概念，可以先读 [从零理解 Agent 观测与评测](docs/agent-observability-evaluation.md)：从为什么需要开始，逐步理解 trace、指标与评分，再动手跑通 Phoenix 集成和基线实验。

`apps/web` 是 pnpm 管理的 Vite + React 19 + TypeScript 工程（Tailwind v4、shadcn/ui、AI Elements、AI SDK v7），消费上述 `/api/chat` 协议端点。前置条件：Node 20+ 与 pnpm 10+。

```bash
pnpm -C apps/web install   # 安装依赖
pnpm -C apps/web dev       # 开发服务器（默认 http://localhost:5173）
pnpm -C apps/web build     # 生产构建（tsc 类型检查 + vite build，产出 dist/）
```

后端地址通过 `apps/web/.env` 的 `VITE_API_BASE` 配置（参考 `apps/web/.env.example`），未设置时默认 `http://localhost:3001`；开发期直连依赖后端 CORS 白名单（默认已含 5173）。工具渲染声明集中在 `apps/web/src/lib/tools.tsx`，后端新增工具时在前端补一条 zod schema 与渲染映射即可。

## 核心接口

`agent::run_agent` 接收模型客户端、模型名称、会话历史、工具注册表、最大请求步数和事件回调。回调可收到步骤开始、文本增量、工具开始和工具完成事件，核心库不直接输出到终端。

- `AgentOutcome::Finished` 返回最终回答和实际步数。
- `AgentOutcome::StepLimit` 表示用尽模型请求步数；本步工具结果已经回填到历史中。
- 工具执行错误作为结构化结果回填给模型；模型请求或流式协议错误返回给调用方。
- 每个会话独立持有 `ChatRequest`；每次请求携带注册表中的工具定义。

注册工具使用 `ToolRegistry::register`，重复工具名会被拒绝。工具函数通过 `#[agent_tool]` 生成 schema、参数解析、异步执行和返回值序列化。第二个参数声明为 `&Context` 时，工具实例通过 `XxxTool::new(Arc<Context>)` 创建。

工具用法见 [crates/tools/README.md](crates/tools/README.md)。核心调用与本地模拟流式模型测试见 [crates/agent/tests/agent.rs](crates/agent/tests/agent.rs)。

## 模型配置

`agent::AgentConfig::from_env()` 读取进程环境变量，`build_client()` 创建 genai 客户端。环境变量参考 [.env.example](.env.example)，核心库不会自动读取 `.env` 文件；server 入口会加载可选 `.env`，已设置的进程变量优先。

| 变量 | 说明 |
| --- | --- |
| `MODEL` | 默认 `bigmodel::glm-4.6`，前缀决定 genai 适配器 |
| `BIGMODEL_API_KEY` | 智谱适配器读取的 API key；其他厂商使用对应的 key 变量 |
| `API_BASE_URL` | 显式覆盖 API 地址；未设置时 `bigmodel::` 模型使用 Coding Plan 地址，其他模型使用适配器默认地址；设为空时均使用适配器默认地址 |
| `AGENT_MAX_STEPS` | 每个 run 的逻辑步骤上限，默认 6；恢复重做不重置预算，实际请求次数可更多 |
| `SERVER_ADDR` | 后端监听地址，默认 `127.0.0.1:3001` |
| `CORS_ALLOWED_ORIGINS` | 允许跨域的来源，逗号分隔，默认 `http://localhost:3000,http://localhost:5173` |
| `RUST_LOG` | 日志过滤器，默认 `server=info,tower_http=info` |

API 地址只覆盖 endpoint，不会自动切换模型适配器或认证方式。请让模型前缀、API 地址与 key 配套。

## ComfyUI 与文档

`workflows/` 与 `scripts/comfy-smoke.ps1` 保留已有的 ComfyUI 出图验证流程，尚未接入当前 Agent 工具集合。

推荐阅读独立入门博客 [从零用 Rust 和 genai 写一个 Agent Loop](docs/genai-agent-loop.md)，从新建项目开始，通过完整示例和 8 张示意图理解工具调用、历史回填、流式响应和循环控制。

文档入口见 [docs/README.md](docs/README.md)，当前接口以 `crates/` 源码为准。

工具宏原理见 [一个异步函数如何变成 Agent 工具](docs/agent-tool-macro.md)，从使用代码和展开结果理解 `#[agent_tool]` 的实现。
