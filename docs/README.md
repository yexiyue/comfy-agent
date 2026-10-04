# 项目文档

从这篇博客开始：

**[从零用 Rust 和 genai 写一个 Agent Loop](genai-agent-loop.md)**

独立的入门博客。从新建项目和第一次请求开始，逐步讲解工具定义、消息历史、工具往返和流式 Agent 循环，提供三个完整示例文件及 8 张示意图。

示例仅依赖 genai 和常用 Rust 库，不依赖本仓库的自定义宏或工程接口。所有示意图直接使用正文中的 Mermaid 代码块。

进一步了解本项目的工具属性宏：

**[一个异步函数如何变成 Agent 工具](agent-tool-macro.md)**

结合真实实现，讲解属性宏的输入、语法解析、元数据默认值、代码生成、状态注入、依赖路径与编译错误，配有 7 张 Mermaid 图。

Agent 跑起来之后，学习如何定位问题和验证质量：

**[从零理解 Agent 观测与评测：为什么需要，以及如何接入 Phoenix](agent-observability-evaluation.md)**

从日志、trace、span 和评分规则开始，解释技术完成与任务成功的区别，再通过本地 Phoenix、聊天请求、mock 实验和真实模型基线学习集成方式。结合 Rust 异步上下文、SSE 取消、token 统计与实际失败案例，配有 6 张 Mermaid 图及可执行的 PowerShell 示例。

日常操作查 [观测与评测指南](observability.md)，实际结果见 [评测基线样例](evaluation-baseline.md)。

持久化与任务生命周期：

**[持久化会话与后台任务运行指南](durable-sessions.md)**

学习 run/attempt、检查点、outbox、租约、暂停/转向和外部副作用恢复，包含增量 HTTP 合约、前端全前缀重放、迁移与故障测试命令。
