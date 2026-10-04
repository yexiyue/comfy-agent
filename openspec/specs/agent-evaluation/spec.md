# agent-evaluation Specification

## Purpose

通过版本化用例执行实际 Rust 聊天后端，并把确定性评分、执行轨迹和实验配置关联到本地 Phoenix。评测结果应支持重复运行和基线比较，区分模型任务质量与协议或运行故障。

## Requirements

### Requirement: Real backend evaluation
评测 SHALL 调用现有聊天 SSE 接口并使用官方 AI SDK 流解析器生成最终 UIMessage，MUST NOT 实现另一套 Agent Loop。每个 trial SHALL 使用独立历史，显式多轮用例除外。

#### Scenario: Multi-turn tool case
- **WHEN** 用例先调用加法工具，再提交完整 UI 历史继续对话
- **THEN** 第二轮执行使用真实后端历史转换，评分可检查调用 ID、参数、结果与最终回答

### Requirement: Versioned fixtures and provenance
用例 SHALL 有稳定 ID、输入和明确评分期望；实验 SHALL 保存数据集版本、代码版本及工作区状态、模型、提示词与工具 schema 标识、评分器版本、trial 和运行配置。

#### Scenario: Repeat an experiment
- **WHEN** 使用相同 fixtures 和配置重复评测
- **THEN** 每条结果可追溯到用例和 trial，并能识别代码、提示词或评分期望发生的变化

### Requirement: Deterministic quality scores
首版 SHALL 提供最终结果、工具选择、参数、输出及步骤约束的确定性评分；技术 `finished` MUST NOT 等同任务成功。不适用的评分 SHALL 标为不适用并从该项分母排除。

#### Scenario: Finished but wrong answer
- **WHEN** 后端正常结束但回答或工具参数不符合用例期望
- **THEN** 保留执行成功状态，但对应质量分数失败，并附可解释原因

### Requirement: Bounded and complete trials
runner SHALL 配置并发、每次请求超时及请求数量预算；所有已计划的 trial SHALL 有成功、错误、超时或预算跳过记录，MUST NOT 静默漏掉失败样本。

#### Scenario: Timeout or exhausted budget
- **WHEN** 一次执行超时或剩余用例超过预算
- **THEN** 超时请求被取消，相应用例有明确记录，报告同时给出质量分母、执行失败及跳过数量

### Requirement: Phoenix experiment association
runner SHALL 将数据集、实验任务结果及评分写入本地 Phoenix，传播 trace context 并保留后端 run/trace 标识。评测与交互聊天 SHALL 使用不同项目以免混淆统计。

#### Scenario: Inspect a failed evaluation
- **WHEN** 查看某次实验失败结果
- **THEN** 可定位对应的后端轨迹与评分原因，不将其他 trial 的 trace 误关联

### Requirement: Reproducible local reports and gates
runner SHALL 输出本地逐项结果与汇总，支持同一数据集/评分器版本的基线比较。模拟测试 SHALL 作为确定性门禁；真实模型评测 SHALL 显式启动、默认多次试验，首版不设置未经基线验证的通过率阈值。

#### Scenario: Reporting with unavailable Phoenix
- **WHEN** 实验上传失败
- **THEN** 本地已获得的结果仍被保存，命令明确报告发布失败，不宣称 Phoenix 实验验收通过

#### Scenario: Baseline comparison
- **WHEN** 比较兼容版本的两次真实模型实验
- **THEN** 报告质量分数、错误率、步骤和延迟变化，并展示 trial 数及 usage 缺失情况
