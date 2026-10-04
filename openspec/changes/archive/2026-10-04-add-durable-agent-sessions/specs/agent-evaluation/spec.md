# Spec Delta

## MODIFIED Requirements

### Requirement: Real backend evaluation
评测 SHALL 通过真实后台聊天执行和官方 AI SDK 流解析器生成最终 UIMessage，MUST NOT 实现另一套 Agent Loop。每个 trial SHALL 使用独立持久化会话，显式多轮用例除外；后续轮次 SHALL 提交新增输入并使用服务端历史。

#### Scenario: Multi-turn tool case
- **WHEN** 用例先调用加法工具，再向同一会话提交新增用户消息
- **THEN** 第二轮使用真实后端保存的历史，评分可检查调用 ID、参数、结果与最终回答

### Requirement: Bounded and complete trials
runner SHALL 配置并发、请求超时及请求数量预算；所有已计划 trial SHALL 有成功、错误、超时或预算跳过记录。超时 SHALL 显式请求终止已接受任务并记录控制结果，MUST NOT 仅断开 SSE 或静默漏掉失败样本。

#### Scenario: Timeout or exhausted budget
- **WHEN** 一次执行超时或剩余用例超过预算
- **THEN** 超时任务收到显式取消请求，未能确认取消时报告清理失败；报告给出质量分母、执行失败及跳过数量

### Requirement: Phoenix experiment association
runner SHALL 将数据集、实验任务结果及评分写入本地 Phoenix，传播 trace context 并保留后端 run、attempt 和 trace 标识。恢复尝试 SHALL 关联同一 trial；评测与交互聊天 SHALL 使用不同项目以免混淆统计。

#### Scenario: Inspect a failed evaluation
- **WHEN** 查看某次实验失败结果
- **THEN** 可定位对应的后端尝试轨迹与评分原因，不将其他 trial 的 trace 误关联

## ADDED Requirements

### Requirement: Durable execution regression gates
确定性模拟验收 SHALL 覆盖断开连接继续、暂停后重做、转向、显式终止、崩溃恢复、重复投递和过期执行写入。验收 SHALL 检查最终历史、调用次数、步骤预算与官方解析结果；MUST NOT 依赖生产凭据或付费模型。

#### Scenario: Recover a multi-tool run
- **WHEN** 模拟模型返回多工具决策，保存首个结果后重启服务
- **THEN** 原调用 ID 和首项结果保留，已完成工具不重做，剩余执行和统计符合检查点

#### Scenario: Fence an interrupted attempt
- **WHEN** 暂停或接管后故意释放旧工具的迟到结果
- **THEN** 历史和任务状态不被旧执行改写，官方解析器得到合法的最终消息
