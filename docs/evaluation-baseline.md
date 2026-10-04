# Phoenix 评测验收样例

2026-10-04 在 Windows / Docker Desktop / Phoenix 20.19.0 本地执行。数据集为 `scripts/evals/cases.jsonl` 的 22 个合成用例，每例 3 trials，并发 2、单 trial 超时 60 秒、预算 100 个聊天请求、每请求最多 6 步。真实模型使用现有 `.env` 的 `bigmodel::glm-5.3-flash`；mock 不调用该提供商。

| 指标 | 完整 mock 实验 | 真实模型基线 |
| --- | --- | --- |
| 发布 / 计划执行 | 66 / 66 | 66 / 66 |
| 任务成功 | 66 / 66 | 57 / 66（86.36%） |
| 工具选择 | 66 / 66 | 66 / 66 |
| 工具参数、输出 | 各 54 / 54 | 各 54 / 54 |
| 执行错误 / 超时 / 跳过 | 0 / 0 / 0 | 0 / 0 / 0 |
| trial 延迟 p50 / p95 | 不作为性能基线 | 4,933 / 10,475 ms |
| 已知 token / usage 缺失调用 | 1,722 / 0（模拟值） | 33,917 / 0 |

真实模型的 9 个任务失败全部来自精确文本评分：`text-hello`、`text-你好` 增加了问候内容，`text-ready` 返回 `Ready` 而期望为 `ready`，各失败 3 次。它们的执行终态仍为 `finished`。工具错误率 3 / 54 来自预期的整数溢出测试，随后均恢复完成；不能把该指标直接视为故障率。token 不换算订阅账单金额。

两次实验均从 Phoenix 查询并验证全部 66 条执行的 Rust root span、run ID、trace ID 和已持久化的 SDK task 父 span。mock 实验为 #3；[真实实验 #5](http://localhost:6006/datasets/RGF0YXNldDox/compare?experimentId=RXhwZXJpbWVudDo1) 的轨迹实际位于 `comfy-agent-evals`。Phoenix 的实验项目列表可能为空，原因及重新查询命令见 [观测文档](observability.md)。

可复查信息：

- 数据集 SHA-256：`6fc9e99fac26ba3a45d4e61cc8fe8c33123f602d02efeb01193deaff41e3ca15`；评分器版本：`1`。
- 运行时 Git：`e5f59dde3757960fc78d0b362f2917f0647485e5`，工作区有未提交实现；不是该提交本身的性能承诺。
- 原始真实报告：`scripts/evals/outputs/real-baseline-clean/`；重新核验报告：同级 `real-baseline-clean-verified/`。这些生成产物被 Git 忽略，本文件仅保存验收摘要。
- 此次原始报告的 `systemPromptHash` 使用早期 manifest 策略标识算法；保留原值。当前 runner 已改为对实际 fixture 系统消息求 hash，后续比较应使用当前算法重新建立基线。

另有真实 Phoenix 技术 smoke，覆盖普通回答、工具成功与错误恢复、多步、步数耗尽、模型中断、并发隔离、队列溢出、消费者取消和服务关闭；验证默认不保存原始内容。首次真实评测曾因中途重启 Phoenix 导致发布不完整，该失败报告保留，未纳入上述成功基线。
