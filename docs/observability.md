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

环境变量见 `.env.example`。关闭 `OTEL_ENABLED` 后聊天仍可运行并返回 runId；开启后 finish metadata 还包含 traceId。CORS 允许 `traceparent`、`tracestate`、`X-Agent-Run-Source`；仅 `eval` source 有效，其作用是本地分类，不是认证。

## 轨迹与指标

每轮聊天是 AGENT span，下面是 CHAIN 步骤，以及 LLM 模型和 TOOL 执行。后台 SSE 任务携带 tracing 上下文；断开连接会取消正在等待的模型或工具，并记录终态，已发生的外部副作用不撤销。

| 字段 | 含义 |
| --- | --- |
| `session.id` / `agent.run_id` | 客户端聊天 ID / 本次执行 ID |
| `agent.outcome` | finished、step-limit、model-error、cancelled、shutdown、queue-overflow、internal-error |
| `agent.steps` / `agent.tool_calls` | 已完成步骤 / 开始执行的工具次数 |
| `agent.request_ttft_ms` | 后端接受请求至首个可见 TextDelta |
| `agent.model_ttft_ms` | 当前模型调用至首个文本增量 |
| `llm.token_count.*` | 每次模型调用提供的 token usage |
| `agent.tokens.known_total` / `complete` | 已知 token 小计及是否完整 |

token 只在 LLM span 使用官方计数字段，父 span 的自定义小计不重复计入。缓存和 reasoning 是明细，不能再次加到总量。纯工具步骤、提前取消和提供商未返回 usage 时，缺失指标保持未知。`finished` 仅表示执行完成；质量成功由评分器判断。工具错误恢复后根执行仍可 finished。Coding Plan 订阅不能换算为按 token 账单，首版无美元成本估计。

导出缓存最多 2048 spans、批量约每秒提交，网络超时 2 秒；失败/丢弃通过本地诊断报告，不影响聊天。正常关闭先取消并等待活动任务，再尝试有限时间刷新（最多约 5 秒）。强制终止进程可能丢失最后一批。测试 runner 在 Windows 使用显式 `SERVER_SHUTDOWN_STDIN=true` 并写入 `shutdown`，让退出和 flush 可验证；平时使用 Ctrl+C，Unix 还支持 SIGTERM。

## 内容策略

`OBS_CAPTURE_CONTENT=false` 是默认值，轨迹无原始提示、历史、回答和工具参数/结果。合成数据调试需要内容时显式开启；结构化凭据字段、常见凭据模式和 binary/base64 被屏蔽，字符串按 UTF-8 字节上限截断并标记。原始提供商错误 body 不记录。自由文本脱敏无法覆盖所有秘密，真实用户数据建议保持默认模式；截断内容不作为可重放历史。

## 评测

Node >=22，评测复用真实 Rust `/api/chat` 与官方 AI SDK parser。22 个版本化 JSONL 用例覆盖文本、加法、溢出恢复与完整多轮历史。模拟模型在本机运行，不使用真实 key；模型故障、取消、shutdown、overflow 由技术测试验证。

```powershell
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

默认 3 trials、并发 2、每 trial 60 秒和最多 100 个聊天请求，可用 `--trials`、`--concurrency`、`--timeout-ms`、`--max-requests` 覆盖。预算计聊天请求，Agent 每请求还受 `AGENT_MAX_STEPS` 约束；这是请求数量控制，不是费用上限。每 trial 独立历史，多轮用例显式保持自己的历史。fixtures 的答案采用精确文本规则，真实模型可能因额外说明而评分失败，这就是当前评分定义。

不带 `--phoenix` 时只保存本地结果；带此参数导入本地数据集，执行 `runExperiment`，发布 CODE 评分并查询实际持久化 span，验证 parent ID。Phoenix 20.19 为实验返回专属 `Experiment-*` 项目，但同一 trace 的项目由首次到达的 span 决定：快速 mock 通常归入实验项目，慢模型的 Rust span 可能先写入 `comfy-agent-evals`。runner 查询两处并记录实际 traceProject；后到的父 span 与既有 trace 合并。两类评测都与交互项目 `comfy-agent-local` 分离。不使用云端 scorer 或 LLM judge。

已有完整实验可通过 `npm run inspect --prefix scripts/evals -- <报告目录>` 重新查询持久化轨迹并生成 `<报告目录>-verified`，保留原报告；这不会重新调用模型，缺少实验执行记录时明确失败。

产物保存在 gitignored `outputs/evals/<timestamp>/`：逐条 JSONL、JSON 和 Markdown 汇总。包括数据集 hash、Git SHA/dirty 状态、模型/提示词/工具 schema 指纹、评分器版本、预算及 trial 配置。失败与超时计入任务成功率，跳过单列；不适用评分不计分母。上传失败保留结果并返回非零状态。

`--baseline <summary.json>` 比较兼容数据集/评分器版本的两次报告，展示质量、错误、延迟、步骤、工具错误及 usage 缺失；真实模型首版只报告变化，不设置任意通过率门槛。mock 命令有确定性门禁。Phoenix 不可用时本地评测仍可独立运行，聊天导出 best effort。
