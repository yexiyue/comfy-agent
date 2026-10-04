# 本地观测与评测

完整 mock 与真实模型的实际验收结果见 [评测基线样例](evaluation-baseline.md)。

## 启动与数据边界

Docker Desktop 使用 WSL 2；首次启用 Windows 功能后需要重启，先运行 `docker info` 确认 Server 可用。

```powershell
docker compose -f compose.phoenix.yaml config
docker compose -f compose.phoenix.yaml up -d
docker compose -f compose.phoenix.yaml logs --tail 30
```

Phoenix 固定 20.19.0 及镜像 digest，UI 为 http://localhost:6006，OTLP HTTP 为 `/v1/traces`。只绑定回环网卡，不开放 gRPC。默认关闭 Phoenix 遥测和自动加载外部资源；配置的远程模型仍收到模型请求。默认 SQLite 位于 `phoenix-data` named volume。

`docker compose -f compose.phoenix.yaml down` 停服务并保留数据；`down -v` 会删除卷。备份前停止服务，使用 `docker volume inspect comfy-agent_phoenix-data` 确认卷；可用 `docker run --rm --volumes-from <已停止的容器> -v <备份目录>:/backup alpine tar czf /backup/phoenix.tgz -C /mnt/data .` 备份（容器尚存在时使用 `stop`，不要先 `down`）。恢复时向空卷解压，保留原备份。镜像拉取目前已验证直连成功；若使用镜像代理，应保持 digest 校验。

```powershell
$env:OTEL_ENABLED='true'
$env:RUST_LOG='info'
New-Item outputs -ItemType Directory -Force | Out-Null
$env:AGENT_CONFIG_MANIFEST='outputs/agent-config.json'
cargo run -p server
```

环境变量见 `.env.example`。关闭 `OTEL_ENABLED` 后聊天仍可运行并返回 runId；开启后 finish metadata 还包含 attemptId 和 traceId。run 查询汇总全部 attempts。CORS 允许 `traceparent`、`tracestate`、`X-Agent-Run-Source`；仅 `eval` source 有效，其作用是本地分类，不是认证。

## 轨迹与指标

每个后台 attempt 是 `agent.run` AGENT 根，其下为 `agent.attempt`、CHAIN 步骤、LLM 模型和 TOOL 执行。traceparent 随持久化任务传播；断开 SSE 仅结束订阅，任务继续。暂停、明确终止和服务关闭会丢弃当前等待；已发生的外部副作用不撤销。恢复保持 runId，新增 attemptId；故障缺失 span 以数据库 attempts 为准。订阅与执行延迟分开统计，重放不会增加调用/token 数。启动前需要 PostgreSQL 和显式迁移，见 [持久化运行指南](durable-sessions.md)。

| 字段 | 含义 |
| --- | --- |
| `session.id` / `agent.run_id` | 服务端会话 ID / 逻辑任务 ID |
| `agent.outcome` | finished、step-limit、model-error、cancelled、shutdown、queue-overflow、internal-error |
| `agent.steps` / `agent.tool_calls` | 已完成步骤 / 开始执行的工具次数 |
| `agent.execution_ttft_ms` / `agent.subscription_ttft_ms` | 后台执行 / 当前订阅至首个回答文本增量；订阅可包含重放 |
| `agent.model_ttft_ms` | 当前模型调用至首个文本增量 |
| `agent.model_first_reasoning_ms` | 当前模型调用至首个非空推理增量 |
| `agent.execution_first_reasoning_ms` / `agent.subscription_first_reasoning_ms` | 后台执行 / 当前订阅至首个推理增量 |
| `agent.model` / `agent.reasoning_effort` | 当前任务选择的模型 / 显式推理强度；未设置强度时该字段缺失 |
| `llm.token_count.*` | 每次模型调用提供的 token usage |
| `agent.tokens.known_total` / `complete` | 已知 token 小计及是否完整 |

token 只在 LLM span 使用官方计数字段，父 span 的自定义小计不重复计入。缓存和 reasoning 是明细，不能再次加到总量。纯工具步骤、提前取消和提供商未返回 usage 时，缺失指标保持未知。`finished` 仅表示执行完成；质量成功由评分器判断。工具错误恢复后根执行仍可 finished。Coding Plan 订阅不能换算为按 token 账单，首版无美元成本估计。

推理首包与回答首包分别记录：长时间推理时，回答 TTFT 大不代表流一直没有数据。步骤 span 覆盖模型、工具与本步结果提交，终态判断不额外创建步骤。导出保留依赖 span 以维持父子关系，过滤依赖库低级别日志事件；业务事件及依赖的 WARN/ERROR 仍保留。

依赖 span 在进入导出队列前过滤，避免挤掉业务轨迹；业务导出缓存最多 2048 spans、批量约每秒提交，网络超时 2 秒；失败/丢弃通过本地诊断报告，不影响聊天。正常关闭先取消并等待活动任务，保留安全任务的恢复资格，再尝试有限时间刷新（最多约 5 秒）。强制终止进程可能丢失最后一批。测试 runner 在 Windows 使用显式 `SERVER_SHUTDOWN_STDIN=true` 并写入 `shutdown`，让退出和 flush 可验证；平时使用 Ctrl+C，Unix 还支持 SIGTERM。

## 内容策略

`OBS_CAPTURE_CONTENT=false` 是默认值，轨迹无原始提示、历史、回答和工具参数/结果。合成数据调试需要内容时显式开启；结构化凭据字段、常见凭据模式和 binary/base64 被屏蔽。超出字节预算时输出合法 JSON 包装 `{"truncated":true,"preview":"…"}`，预算过小时使用合法 JSON 占位值，并保留截断标记与原字节数。这只影响观测副本，模型历史与持久化 checkpoint 保持完整。原始提供商错误 body 不记录。自由文本脱敏无法覆盖所有秘密，真实用户数据建议保持默认模式；截断内容不作为可重放历史。

## 评测

Node >=22，评测复用真实 Rust `/api/chat` 与官方 AI SDK parser。22 个版本化 JSONL 用例覆盖文本、加法、溢出恢复与完整多轮历史。模拟模型在本机运行，不使用真实 key；模型故障、取消、shutdown、overflow 由技术测试验证。

```powershell
# 先按 README 启动 PostgreSQL 并创建专用测试库
$env:TEST_DATABASE_URL = 'postgresql://comfy_agent:comfy_agent_local@127.0.0.1:5432/comfy_agent_test'
cargo build -p server
npm ci --prefix scripts/evals
npm run check --prefix scripts/evals
npm test --prefix scripts/evals
npm run eval:mock --prefix scripts/evals
npm run eval:mock --prefix scripts/evals -- --phoenix
npm run smoke:phoenix --prefix scripts/evals
```

真实模型需先启动上述启用观测并生成 manifest 的后端：

```powershell
npm run eval --prefix scripts/evals -- --real --phoenix --manifest outputs/agent-config.json
```

也可加 `--start-server`，让 runner 在临时端口启动并正常关闭使用根目录 `.env` 的真实后端。runner 复制构建好的可执行文件，避免 Windows 运行期间锁住 Cargo 输出；仍需先 `cargo build -p server`。服务本身无默认 system prompt；manifest 的空提示 hash 表示这一事实，实验再根据 fixtures 中实际系统消息计算 systemPromptHash。

默认 3 trials、并发 2、每 trial 60 秒和最多 100 个聊天请求，可用 `--trials`、`--concurrency`、`--timeout-ms`、`--max-requests` 覆盖。预算计聊天请求，Agent 每请求还受 `AGENT_MAX_STEPS` 约束；这是请求数量控制，不是费用上限。每 trial 独立服务端会话，多轮仅提交新增消息，由后端保存并投影历史。fixtures 的答案采用精确文本规则，真实模型可能因额外说明而评分失败，这就是当前评分定义。

不带 `--phoenix` 时只保存本地结果；带此参数导入本地数据集，执行 `runExperiment`，发布 CODE 评分并查询实际持久化 span，验证 parent ID。Phoenix 20.19 为实验返回专属 `Experiment-*` 项目，但同一 trace 的项目由首次到达的 span 决定：快速 mock 通常归入实验项目，慢模型的 Rust span 可能先写入 `comfy-agent-evals`。runner 查询两处并记录实际 traceProject；后到的父 span 与既有 trace 合并。两类评测都与交互项目 `comfy-agent-local` 分离。不使用云端 scorer 或 LLM judge。

已有完整实验可通过 `npm run inspect --prefix scripts/evals -- <报告目录>` 重新查询持久化轨迹并生成 `<报告目录>-verified`，保留原报告；这不会重新调用模型，缺少实验执行记录时明确失败。

产物保存在 gitignored `outputs/evals/<timestamp>/`：逐条 JSONL、JSON 和 Markdown 汇总。包括数据集 hash、Git SHA/dirty 状态、模型/提示词/工具 schema 指纹、评分器版本、预算及 trial 配置。失败与超时计入任务成功率，跳过单列；不适用评分不计分母。上传失败保留结果并返回非零状态。

`--baseline <summary.json>` 比较兼容数据集/评分器版本的两次报告，展示质量、错误、延迟、步骤、工具错误及 usage 缺失；真实模型首版只报告变化，不设置任意通过率门槛。mock 命令有确定性门禁。Phoenix 不可用时本地评测仍可独立运行，聊天导出 best effort。

## 持久化评测与恢复门禁

设置专用 `TEST_DATABASE_URL` 后运行 mock 评测；每个 mock server 创建随机子库，trial 创建独立会话。多轮仅提交新增 user 消息；响应丢失可由 command receipt 查回 run，超时显式 cancel，报告 `cleanup.confirmed`/失败原因。结果包含 conversationId、runIds、attemptIds 和 traceIds，评分规则仍独立于技术完成。

```powershell
$env:TEST_DATABASE_URL = 'postgresql://comfy_agent:comfy_agent_local@127.0.0.1:5432/comfy_agent_test'
node --experimental-strip-types scripts/durable-check/check.mjs
```

模型暂停重做计入实际调用；旧草稿不进入最终回答。查询 `GET /api/runs/{id}` 的 `statistics.usageComplete` 区分已知小计与完整账单。订阅观测用 `agent.subscription_ttft_ms`，后台用 `agent.execution_ttft_ms`，模型用 `agent.model_ttft_ms`，不应直接混合比较。
