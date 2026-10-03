# comfy-agent

**Rust AI Agent 系列教程的配套项目**：从零开始，一步一步写出一个本地的 ComfyUI agent——
用自然语言对话，把你已有的 ComfyUI 工作流当工具，自动调研素材、增强提示词、生成图片；
带评测监控体系（知道 agent 设计得对不对）和记忆系统（记住你的偏好）；
前端用 Vercel AI SDK 快速搭建聊天界面。

## 教程文档

| 文档 | 说明 |
|---|---|
| [docs/tutorial/00-大纲.md](docs/tutorial/00-大纲.md) | 系列大纲：24 篇规划、6 个里程碑、全部技术决策 |
| [docs/tutorial/01-项目启动.md](docs/tutorial/01-项目启动.md) | 第 01 篇：项目骨架 |
| [docs/tutorial/05-流式工具调用.md](docs/tutorial/05-流式工具调用.md) | 第 05 篇：流式输出与工具调用 |
| [docs/tutorial/06-AgentLoop.md](docs/tutorial/06-AgentLoop.md) | 第 06 篇：连续工具决策、步数上限与退出状态 |
| [docs/genai-guide.md](docs/genai-guide.md) | genai 0.7.0-rc.1 使用指南（LLM 适配层参考手册） |

> 每篇文章自带完整参考实现（均单独编译验证过）。**建议自己动手从零写**——本仓库的
> `src/` 由你自己随教程演进，docs 只负责教学；对照参考实现核对是最后的手段。

## 当前进度

- [x] 01 · 项目启动（骨架 + .env + 日志）——已完成
- [x] 02 · 第一次调用（genai exec_chat + Coding Plan 端点覆盖）——已完成
- [x] 03 · CLI 聊天（流式 + 多轮）【里程碑 M1】——已完成
- [x] 04 · 工具调用往返（非流式）——已完成：天气工具 + answer_turn 单次工具往返
- [x] 05 · 流式 × 工具调用——已完成：流式响应捕获 + 多工具结果回填
- [x] 06 · Agent Loop【里程碑 M2】——已完成：连续工具决策 + 步数上限 + 工具错误回填
- [ ] …完整列表见 [大纲](docs/tutorial/00-大纲.md)

## 快速开始

```bash
cp .env.example .env   # 填入 BIGMODEL_API_KEY
cargo run
```

## 技术栈

genai 0.7（LLM 多厂商适配）· tokio · axum · SQLite · ComfyUI HTTP API ·
Next.js + Vercel AI SDK（前端）· Rust edition 2024

所有选型理由与已定决策见 [大纲 §2/§7](docs/tutorial/00-大纲.md)。
