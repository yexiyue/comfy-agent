# Tasks

## 1. 本地 Phoenix 部署

- [x] 1.1 添加固定 Phoenix 20.19.0 镜像的 `compose.phoenix.yaml`，配置回环端口、named volume 和隐私环境变量；使用 `docker compose -f compose.phoenix.yaml config` 检查配置与镜像 digest。
- [x] 1.2 补充 README 本地启动、引擎检查、停止、备份和删除数据说明；引擎可用后验证 UI、容器重建保留数据，以及关闭外部资源后页面可用，记录实际结果。

## 2. 观测基础模块

- [x] 2.1 新建 `crates/telemetry` 并将兼容 OpenTelemetry、OTLP 和 tracing bridge 依赖纳入 workspace；通过 `cargo check --workspace`，确认核心库不自行加载 `.env` 或启动 exporter。
- [x] 2.2 实现配置校验、可关闭 exporter、有界 batch queue、有限导出超时和本地失败诊断；用本地 OTLP stub 验证正常导出、服务不可达及缓冲溢出不会阻塞调用。
- [x] 2.3 实现统一 OpenInference 属性与内容策略，默认关闭原始内容，开启时脱敏及 UTF-8 截断；测试敏感 JSON 字段、凭据模式、多字节截断、原始错误 body 和 binary/base64 不泄漏。
- [x] 2.4 实现 run 终态 guard 与幂等提交，保留安全错误类别；测试每种终态、重复完成及 Drop 兜底均只有一条终结记录。
- [x] 2.5 更新 `.env.example` 和观测模块文档，说明开关、端点、内容策略、队列/刷新边界及本地数据范围；使用示例配置验证启用与关闭两种启动方式。

## 3. 核心 Agent 插桩与统计

- [x] 3.1 在 `llm.rs` 捕获 usage/stop reason 并保留在内部响应，保持公共 run_agent/AgentOutcome/AgentEvent 兼容；mock 流测试验证完整、部分、缺失 usage 及中途失败。
- [x] 3.2 添加 Agent/step/LLM/tool span 层级及 run、step、call ID 属性，异步上下文使用正确的 instrumentation；in-memory exporter 测试验证多步及并发请求的父子关系与隔离。
- [x] 3.3 记录请求和模型首文本延迟、duration、完成步骤数及工具次数，按模型调用计 token 并标记汇总完整性；测试纯工具步骤、文本前取消、cache/reasoning 不重复计量。
- [x] 3.4 标记工具失败与恢复、模型错误及步数耗尽，统一调用内容策略；测试工具错误后成功的根终态和内容关闭时所有 span 均无原始消息。
- [x] 3.5 记录指标定义与核心库观测边界，更新 AGENTS.md 的贡献指引；确认文档区分任务成功、执行结束、未知 usage 与订阅费用。

## 4. Axum SSE 生命周期与关联

- [x] 4.1 服务入口初始化 exporter，AppState 携带策略和固定项目配置；测试非法配置报错、观测关闭时聊天可用，修改不得影响正在进行的前端变更。
- [x] 4.2 为验证通过的请求生成 run ID，提取 W3C parent context 并关联 chat session；补充 CORS trace headers 和受限 eval source，测试有效/无效 traceparent、非法 source 与不信任 UI metadata。
- [x] 4.3 将观测上下文传播到 SSE 后台任务，finish metadata 追加 runId/traceId；扩展官方 AI SDK 协议脚本验证最终 UIMessage、现有 outcome/steps 和工具 parts 兼容。
- [x] 4.4 将 disconnect、shutdown、overflow、模型错误和正常完成接入终态 guard；扩展现有模型/工具取消和队列测试，验证结束时间覆盖真正执行且各分支恰好一次记录。
- [x] 4.5 追踪并等待活动任务退出后有界刷新 provider；测试服务关闭、等待中的工具取消及 exporter 卡住时总退出时间有界。
- [x] 4.6 输出脱敏的统一运行配置清单或启动记录，包含实际 model/prompt/tool schema 指纹供 eval 读取；测试指纹与真实工具注册/提示配置一致且没有 API key。
- [x] 4.7 更新 README 的 SSE metadata、观测启动与 CORS 示例；按文档发起多轮请求并验证相同 session、不同 run ID 和 exporter 故障下聊天正常完成。

## 5. 评测 runner 与 fixtures

- [x] 5.1 创建 `scripts/evals` 独立 TypeScript/Node >=22 包，固定 AI SDK 与 Phoenix client 版本并提交 lockfile；验证锁定安装、类型检查和最小 mock SSE 解析成功。
- [x] 5.2 实现官方流解析器调用真实后端、多轮 UI 历史回填、独立 trials、并发/超时/请求预算；测试第二轮工具调用 ID 保留、超时取消、预算跳过和失败样本不遗漏。
- [x] 5.3 先完成一条 Phoenix dataset/runExperiment task 到 Rust SSE 的关联链，传播上下文并选择 eval 项目；真实 Phoenix 验证 task、后端 parent/trace ID 和评分能关联，作为后续批量实验的前置验收。
- [x] 5.4 添加 20–30 个稳定 ID 加法 fixtures 及 schema 校验，涵盖无工具、必须工具、整数边界、溢出恢复和多轮；校验重复 ID、缺失期望及 dataset hash，mock 执行结果符合定义。
- [x] 5.5 实现答案、工具选择/参数/输出与步骤评分器及解释原因；单元测试覆盖正确、错误、恢复、不适用及 finished 但任务失败，校验评分分母。
- [x] 5.6 保存代码/dirty 状态、数据集/评分器版本、实际模型/prompt/schema 指纹和 trial 配置，发布本地 Phoenix 实验结果；验证重复运行可追溯且每条结果对应正确用例/trial。
- [x] 5.7 输出 JSONL、JSON/Markdown 汇总及兼容 baseline 比较，产物目录 gitignore；测试超时/解析失败/上传失败仍保存报告，质量成功率含执行失败且不兼容版本拒绝直接比较。
- [x] 5.8 提供 mock 门禁和显式真实模型命令，记录默认 trials/预算、成本边界与评分含义；更新 README、AGENTS.md 及脚本说明，按文档运行 mock，无自动付费模型调用。

## 6. 跨模块验收

- [x] 6.1 运行 `cargo build --workspace`、`cargo test --workspace`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings` 及 Node 类型/评分/mock 协议检查；记录命令结果并保留已有工作区改动。
- [x] 6.2 Docker 引擎可用后执行真实本地 Phoenix smoke：普通聊天、工具成功/失败恢复、多步、步数耗尽、模型中断、并发隔离、取消和服务关闭；从 Phoenix 查询实际 span 属性、终态、usage 和持久化，不能用 mock exporter 代替此项。
- [x] 6.3 执行完整 mock 实验并确认 Phoenix 数据集、实验评分及 trace 关联，另运行显式真实模型多 trial 基线；提交可复查的汇总样例，标明未完成或环境阻塞的验收，禁止把未执行项勾为完成。
