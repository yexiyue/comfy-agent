# Spec Delta

## MODIFIED Requirements

### Requirement: Execution hierarchy and correlation
系统 SHALL 为每轮生成独立 runId，为恢复尝试生成 attemptId，记录 Agent、步骤、模型和工具的父子关系并关联 session。有效 W3C 上下文 SHALL 随后台调度传播，无效上下文 SHALL 被忽略；恢复尝试 SHALL 可通过 runId 和关联信息追溯前序尝试。

#### Scenario: Concurrent sessions
- **WHEN** 不同会话同时发送聊天请求
- **THEN** 各执行拥有不同 run ID，步骤和工具不混入其他请求的轨迹

#### Scenario: Evaluation trace continuation
- **WHEN** 请求携带有效 traceparent
- **THEN** 后台初次执行关联其父上下文；未提供上下文时创建独立轨迹

#### Scenario: Resumed execution correlation
- **WHEN** 同一任务暂停或重启后继续
- **THEN** 新 attempt 仍关联原 run 和会话，并能定位前序尝试，转向后的新 run 标识其来源

### Requirement: Compatible client identifiers
启用观测时，成功或步数耗尽的 SSE finish metadata SHALL 提供 runId、attemptId、traceId，保留 outcome、steps 和消息事件语义；关闭观测时 SHALL 仍提供 runId、attemptId，不伪造 traceId。任务标识 SHALL 在结束前可获取，支持超时后的显式控制。

#### Scenario: Existing chat client
- **WHEN** 现有 AI SDK 客户端消费一次正常聊天
- **THEN** 消息和工具 parts 与原协议一致，并可在最终 metadata 读取关联标识

### Requirement: Exactly one terminal outcome
每个已启动 attempt SHALL 在可观测的结束路径记录一次明确结果，包括完成、步数耗尽、模型错误、暂停、取消、被接替、关闭中断或内部失败。逻辑 run SHALL 最多提交一次终态；暂停和关闭中断 MUST NOT 永久终结 run。恢复后的工具错误 MUST NOT 自动判定整个执行失败。进程骤停导致的缺失 span SHALL 通过持久化 attempt 状态识别，不伪造已导出的轨迹。

#### Scenario: Consumer disconnect
- **WHEN** 客户端在模型或工具等待期间关闭响应
- **THEN** 仅结束订阅，执行继续且不记录 cancelled，已结束 span 不重复完成

#### Scenario: Recovered tool error
- **WHEN** 工具失败后模型继续并成功回答
- **THEN** 工具 span 保留错误，执行终态为 finished

#### Scenario: Abnormal termination
- **WHEN** 分别发生模型失败、步数耗尽、订阅溢出或服务关闭
- **THEN** 模型失败和步数耗尽记录各自结果；订阅溢出仅记录连接诊断，关闭记录 attempt 中断且保留 run 恢复资格

### Requirement: Accurate execution statistics
系统 SHALL 记录各模型调用的 usage、模型标识及提供商结束原因，并记录步骤数、工具次数和耗时。订阅延迟与任务、模型首文本延迟 SHALL 区分。run 汇总 SHALL 覆盖各 attempt 的实际调用，重放 MUST NOT 增加统计；缺失 usage SHALL 保持未知，父子、缓存及 reasoning 明细 MUST NOT 重复计入总量。

#### Scenario: Partial usage and no text
- **WHEN** 提供商未返回 usage 或请求在出现文本前取消
- **THEN** 缺失 token 或首文本延迟保持未知，不写成零；缓存及 reasoning 明细不重复累加到 total

#### Scenario: Replay and retry statistics
- **WHEN** 同一任务恢复模型调用并被多个页面重放
- **THEN** 新模型调用计入实际成本，重放不增加调用数或 token 数，逻辑步骤预算不被重置
