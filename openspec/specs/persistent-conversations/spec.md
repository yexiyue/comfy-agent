# persistent-conversations Specification

## Purpose

让会话历史成为服务端维护的可靠事实，支持关闭网页后的重新加载以及后台任务恢复。区分用户可见的执行记录和模型可用的完整上下文，避免客户端覆盖历史、重复提交或半完成工具交换破坏后续推理。

## Requirements

### Requirement: Authoritative conversation history
系统 SHALL 为会话提供稳定 ID、版本、消息历史与当前任务标识，并持久化用户消息、完整模型决策和工具结果。客户端 SHALL 提交新增消息而非覆盖完整历史；不存在的会话和不支持的旧请求格式 SHALL 返回明确错误。

#### Scenario: Reload a conversation
- **WHEN** 用户关闭网页后重新打开已有会话
- **THEN** 可加载已保存消息和任务状态，已接受的输入不依赖浏览器内存

#### Scenario: Reject history overwrite
- **WHEN** 客户端使用旧格式提交整段 messages 试图覆盖会话
- **THEN** 服务返回明确的格式错误，不改写历史、不启动模型

### Requirement: Idempotent and versioned submissions
提交新消息和控制任务 SHALL 使用幂等键及预期版本。相同键、相同内容的重试 SHALL 返回同一操作结果；键被不同内容复用或版本冲突 SHALL 返回冲突，不产生重复消息或任务。同一会话 SHALL 最多拥有一个未终结任务，包括暂停和需人工处理的任务。

#### Scenario: Concurrent submissions
- **WHEN** 两个页面用同一会话版本提交不同消息
- **THEN** 最多一个提交成功，另一个收到冲突并可刷新历史

#### Scenario: Retry an accepted request
- **WHEN** 提交已被接受但响应丢失，客户端使用同一幂等键重试
- **THEN** 返回原消息和任务标识，不再次执行

### Requirement: Lossless completed context
模型上下文 SHALL 保留已完成步骤的顺序、调用 ID、参数、成功或失败结果及提供商要求的响应信息。未完成的模型片段 SHALL 保留为草稿审计记录，MUST NOT 当作完整 assistant 消息送入下一次模型调用。旧待办决策失效时 SHALL 形成合法上下文，不留下悬空工具调用。

#### Scenario: Continue after completed tools
- **WHEN** 下一轮使用包含多步工具调用与工具错误的已保存会话
- **THEN** 模型收到顺序正确的 assistant/tool 消息、原调用 ID 和错误结果

#### Scenario: Interrupted model draft
- **WHEN** 模型在完整决策保存前被暂停
- **THEN** 草稿可追溯，恢复的模型上下文从最近已提交阶段构建

### Requirement: Validate before accepting work
新增消息和可选的初始历史 SHALL 验证角色、文本、步骤边界及完整工具交换。附件、不支持的内容、未完成的导入工具交换或重复 ID SHALL 返回 HTTP 400，MUST NOT 静默丢弃内容或创建执行任务。

#### Scenario: Invalid imported history
- **WHEN** 创建会话时导入缺少输出的工具调用
- **THEN** 请求失败，不保存部分会话、不调用模型
