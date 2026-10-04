# Spec Delta

## Purpose

让 Agent 执行独立于单次 HTTP 连接，并以可靠阶段检查点支持任务控制和进程恢复。明确暂停、继续、转向及外部副作用的不确定性，确保故障恢复不会重复已完成动作或允许旧执行覆盖新状态。

## ADDED Requirements

### Requirement: Durable acceptance and dispatch
任务被接受后 SHALL 有持久化输入、任务标识和可恢复的待调度记录。进程在接受、入队或领取期间失败后 SHALL 能继续调度；重复投递 MUST NOT 产生并发有效执行或重复终态。

#### Scenario: Crash before queue publication
- **WHEN** 接受请求后、发布队列任务前进程退出
- **THEN** 重启后能发现待调度任务并执行，原输入和任务 ID 不变

#### Scenario: Duplicate delivery
- **WHEN** 相同任务被队列多次投递
- **THEN** 仅一个有效执行拥有写入权限，重复投递不重复已提交动作

### Requirement: Phase checkpoints and bounded progress
系统 SHALL 在启动工具前保存完整模型决策，并在启动下一个动作前保存当前工具结果和检查点。恢复 SHALL 跳过已提交动作，重新执行安全且未完成的阶段；步数预算与已发生的调用统计 MUST NOT 因恢复而重置。耗尽预算 SHALL 正常终结并报告 step-limit。

#### Scenario: Restart between two tools
- **WHEN** 一个决策有两个工具，第一项结果已提交而第二项尚未执行时进程退出
- **THEN** 恢复沿用原调用 ID，只执行第二项并保留第一项结果

#### Scenario: Resume near step limit
- **WHEN** 已使用大部分步骤的任务多次暂停并恢复
- **THEN** 恢复不增加原任务步骤预算，达到限制后报告实际步骤数

### Requirement: Immediate pause and explicit cancellation
暂停和终止 SHALL 立即发出当前动作的取消请求，并撤销旧执行的后续写入权限。暂停 SHALL 最终进入 paused 或 needs-attention；终止 SHALL 进入 cancelled，不再恢复。系统 MUST NOT 将取消请求解释为外部副作用已撤销。

#### Scenario: Pause during model streaming
- **WHEN** 用户在模型返回文本期间请求暂停
- **THEN** 当前模型等待被中断，未提交片段保留为草稿，旧执行不能继续提交工具动作

#### Scenario: Cancel a paused task
- **WHEN** 用户明确终止暂停任务
- **THEN** 任务终结为 cancelled，迟到结果不能改变该终态

### Requirement: Continue and steer have distinct semantics
原样继续 SHALL 复用 run ID 并创建新 attempt，从检查点恢复。暂停后追加指令 SHALL 原子地将旧 run 标为 superseded，保存指令并创建同会话的新 run；新 run SHALL 重新规划并复用合法的已完成上下文，MUST NOT 执行旧待办决策。未暂停或存在未解决外部副作用的转向 SHALL 被拒绝。

#### Scenario: Resume without new input
- **WHEN** 用户对 paused 任务选择继续
- **THEN** run ID 不变、attempt ID 更新，安全的未完成动作重新执行

#### Scenario: Additional instruction replaces pending work
- **WHEN** 用户在暂停后提交“改用另一种方案”并继续
- **THEN** 旧任务成为 superseded，新任务包含追加指令和已完成结果，不执行旧任务剩余调用

### Requirement: Recover uncertain external operations conservatively
工具恢复 SHALL 区分可安全重做、可用幂等键恢复、可查询外部任务和不可确认的动作。外部操作结果不明确且无法安全恢复时 SHALL 进入 needs-attention，MUST NOT 盲目重试。缺少恢复声明的工具 SHALL 使用保守策略。

#### Scenario: Unknown side effect after restart
- **WHEN** 外部工具可能已提交动作，但本地未保存结果且无法查询或去重
- **THEN** 任务进入 needs-attention，报告安全的原因，不再次提交外部动作

#### Scenario: Reconcile an external job
- **WHEN** 已保存外部任务 ID 的工具在重启后恢复
- **THEN** 查询原任务并保存结果，不创建第二个外部任务

### Requirement: Recoverable shutdown and stale writer exclusion
服务关闭 SHALL 在有界时间内中断本地等待并保存可恢复状态；进程失联后 SHALL 在执行租约失效后恢复任务。每次取得执行权 SHALL 使用新的代次，所有状态和结果提交 SHALL 校验执行权，拒绝过期执行的写入。

#### Scenario: Old worker finishes after recovery
- **WHEN** 新执行已接管，旧执行随后返回工具结果
- **THEN** 旧结果不能覆盖检查点、消息或任务终态
