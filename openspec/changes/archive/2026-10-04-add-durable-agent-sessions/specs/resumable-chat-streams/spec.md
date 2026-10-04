# Spec Delta

## Purpose

在后台执行与网页连接之间提供可重连的聊天流，保持官方 AI SDK UI Message Stream 消费方式。通过持久化进度与稳定消息标识恢复显示，让慢客户端、网页关闭和主动任务控制具有明确且互不混淆的行为。

## ADDED Requirements

### Requirement: Subscription lifetime is independent
关闭响应、前端停止读取或慢消费者溢出 SHALL 仅结束该订阅，MUST NOT 取消后台任务。进度存储与内存缓冲 SHALL 有界；持久化失败 SHALL 显式中断执行，MUST NOT 将未保存的结果冒充可恢复进度。

#### Scenario: Close the browser
- **WHEN** 用户关闭正在显示模型或工具进度的网页
- **THEN** 后台继续执行，重新打开后可读取完成结果

#### Scenario: Slow subscriber
- **WHEN** 某订阅消费速度不足导致其缓冲满
- **THEN** 该连接结束，其它订阅和后台任务继续，客户端能重新加载进度

### Requirement: Replay produces a valid UI message
订阅 SHALL 使用稳定的 assistant 消息标识，重放已保存的有效响应前缀再接收新事件。重放与实时交接 SHALL 无遗漏、无重复并遵守文本块、步骤和工具事件边界；被放弃的草稿 SHALL 不混入最终有效回答。

#### Scenario: Reconnect during a tool step
- **WHEN** 第二个页面在工具执行期间订阅当前任务
- **THEN** 官方解析器得到相同 assistant ID、已完成文本和调用 ID，随后接收工具结果一次

#### Scenario: Resume an interrupted text segment
- **WHEN** 暂停的模型片段重新执行并产生不同回答
- **THEN** 有效消息保留已完成前缀，替换未完成片段，不拼接两次尝试的草稿

### Requirement: Protocol compatible streaming
聊天流 SHALL 遵循 UI Message Stream 的响应头、SSE 格式、文本和工具事件、步骤闭合及结束标记，使用注释心跳。正常完成和步数耗尽 SHALL 保留既有结束语义；暂停、终止和转向 SHALL 有明确状态，不伪装为成功回答。

#### Scenario: Official parser consumption
- **WHEN** 官方 AI SDK 解析器消费正常或多步工具流
- **THEN** 最终 UIMessage 的文本、工具输入输出和 metadata 正确，步骤和文本块完整闭合

#### Scenario: Idle connection heartbeat
- **WHEN** 工具长时间等待且没有新内容
- **THEN** 连接发送注释心跳，不产生虚假消息 part

### Requirement: Frontend reflects durable task state
前端 SHALL 加载服务端会话和任务状态，提供暂停、继续、追加指令转向和明确终止。UI SHALL 区分连接状态与任务状态，不能将 SDK 的 stop 或连接关闭当作服务端终止；冲突后 SHALL 刷新权威状态。

#### Scenario: Open a paused conversation
- **WHEN** 用户刷新页面打开 paused 任务
- **THEN** 页面显示暂停状态与已保存历史，并允许原样继续或追加指令

#### Scenario: Two tabs issue controls
- **WHEN** 两个页面同时对同一任务提交互斥操作
- **THEN** 冲突页面刷新后显示实际任务状态，不继续展示过期的可操作按钮
