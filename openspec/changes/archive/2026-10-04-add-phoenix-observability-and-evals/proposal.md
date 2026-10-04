# Proposal

## Why

现有 Rust Agent Loop 与 Axum SSE 已能完成多步工具调用，但缺少可关联的执行轨迹、token 统计与真实任务质量回归。接入本地 Phoenix 并让评测调用同一后端，可以在继续扩展工具与前端之前建立可靠的诊断和比较基础，数据保留在自己的环境。

## What Changes

- 提供固定版本的 Phoenix 本地 Docker Compose 部署，绑定回环地址、持久化数据并关闭遥测及自动外部资源加载。
- 用 OpenTelemetry 和 OpenInference 语义记录每轮 Agent、步骤、模型与工具 span，支持会话关联及 W3C trace context。
- 保留模型返回的 token usage、结束原因，记录首文本延迟、步骤数、工具错误及取消等终态；缺失统计保持未知。
- 为现有 SSE finish metadata 增加 `runId`、`traceId`，保持 `outcome`、`steps` 和现有聊天协议兼容。
- 内容采集默认关闭，提供显式开启、脱敏与截断控制；观测导出失败不影响聊天。
- 新增独立 TypeScript 评测 runner，通过官方 AI SDK 解析器调用 Rust SSE 后端，将版本化数据集、实验与确定性评分写入本地 Phoenix，并输出本地结果报告。
- 建立模拟测试的确定性门禁与真实模型的可选多次试验，区分技术执行结束和语义任务成功。

## Capabilities

### New Capabilities

- `phoenix-local-deployment`: 本地 Phoenix 服务的版本、网络、持久化、隐私配置与启动验证。
- `agent-observability`: Rust Agent 执行的 trace、关联信息、指标、内容策略和可靠终态记录。
- `agent-evaluation`: 基于真实后端的数据集评测、评分、实验关联与可复现报告。

### Modified Capabilities

无。仓库当前没有已发布的主规格；新增能力保持现有后端行为兼容。

## Impact

- 涉及 `crates/agent` 内部模型响应处理、`crates/server` 的初始化/请求上下文/SSE 生命周期，以及新增观测辅助模块或 crate。
- 增加 workspace OpenTelemetry 依赖、本地 Compose 文件、`scripts/evals` 独立 Node 包及评测 fixtures；更新 README、AGENTS.md、`.env.example`。
- 不修改进行中的 `add-web-frontend` 变更，不实现前端反馈 UI、聊天持久化、ComfyUI 工具、LLM judge 或通用基础设施监控。
- Docker 与 WSL 已安装，但系统功能需要重启后才能启动容器引擎；真实 Phoenix 验收以引擎可用为前提。
