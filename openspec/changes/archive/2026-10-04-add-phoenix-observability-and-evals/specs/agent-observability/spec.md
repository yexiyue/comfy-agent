# Spec Delta

## Purpose

为每次真实 Agent 执行提供可关联的模型与工具轨迹、准确的运行统计及可靠终态。观测应帮助定位多步失败和取消，同时保持现有 SSE 协议兼容，保护内容并避免影响聊天可用性。

## ADDED Requirements

### Requirement: Execution hierarchy and correlation
系统 SHALL 为每轮生成独立 `runId`，记录 Agent、步骤、模型和工具的父子关系；有聊天 `id` 时 SHALL 以其关联 session。有效 W3C 上下文 SHALL 延续，无效上下文 SHALL 被忽略。

#### Scenario: Concurrent sessions
- **WHEN** 不同会话同时发送聊天请求
- **THEN** 各执行拥有不同 run ID，步骤和工具不混入其他请求的轨迹

#### Scenario: Evaluation trace continuation
- **WHEN** 请求携带有效 `traceparent`
- **THEN** 后端执行关联其父上下文；未提供上下文时创建独立轨迹

### Requirement: Compatible client identifiers
启用观测时，成功或步数耗尽的 SSE finish metadata SHALL 增加 `runId`、`traceId`，保留现有 `outcome`、`steps` 和消息事件语义；关闭观测时 SHALL 仍提供 `runId`，不伪造 `traceId`。

#### Scenario: Existing chat client
- **WHEN** 现有 AI SDK 客户端消费一次正常聊天
- **THEN** 消息和工具 parts 与原协议一致，并可在最终 metadata 读取新增关联标识

### Requirement: Exactly one terminal outcome
每个已启动执行 SHALL 恰好记录一次 `finished`、`step-limit`、`model-error`、`cancelled`、`shutdown`、`queue-overflow` 或 `internal-error` 终态。恢复后的工具错误 MUST NOT 自动判定整个执行失败。

#### Scenario: Consumer disconnect
- **WHEN** 客户端在模型或工具等待期间关闭响应
- **THEN** 后端取消执行并记录 `cancelled`，已结束 span 不重复完成

#### Scenario: Recovered tool error
- **WHEN** 工具失败后模型继续并成功回答
- **THEN** 工具 span 保留错误，执行终态为 `finished`

#### Scenario: Abnormal termination
- **WHEN** 分别发生模型失败、步数耗尽、队列溢出或服务关闭
- **THEN** 记录对应独立终态，而非统一当成 HTTP 成功或正常回答

### Requirement: Accurate execution statistics
系统 SHALL 记录各模型调用的 usage、模型标识及提供商结束原因，并记录步骤数、工具次数和耗时。请求首文本延迟与模型首文本延迟 SHALL 区分；缺失 usage SHALL 保持未知，汇总 MUST NOT 重复计入父子统计。

#### Scenario: Partial usage and no text
- **WHEN** 提供商未返回 usage 或请求在出现文本前取消
- **THEN** 缺失 token 或首文本延迟保持未知，不写成零；缓存及 reasoning 明细不重复累加到 total

### Requirement: Controlled content collection
内容采集 SHALL 默认关闭。显式开启后 SHALL 按统一策略脱敏、限制 UTF-8 字节数并标记截断；凭据、认证头、原始提供商错误响应及二进制内容 MUST NOT 进入轨迹。

#### Scenario: Content disabled or bounded
- **WHEN** 请求包含用户文本、工具参数和结果
- **THEN** 默认仅记录必要结构和统计；启用内容模式后使用脱敏及截断策略，无法完整重放的内容有明确标记

### Requirement: Non-blocking best-effort export
观测 SHALL 使用有界缓存；导出不可用或队列满时 SHALL 通过本地诊断报告失败/丢弃而不改变聊天结果。服务关闭 SHALL 在有界时间内尝试刷新，MUST NOT 无限等待。

#### Scenario: Phoenix outage
- **WHEN** Phoenix 不可达而模型正常工作
- **THEN** 聊天正常完成，本地日志可见导出故障且缓冲不无限增长
