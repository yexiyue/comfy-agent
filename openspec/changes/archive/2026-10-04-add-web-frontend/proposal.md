# Proposal

## Why

Agent 核心（`crates/agent`）与 HTTP 服务端（`crates/server`）已经实现并正在完善，但没有任何用户界面：`apps/` 目录预留至今为空。`crates/server` 的 `POST /api/chat` 已按 AI SDK 5 UI Message Stream 协议对外提供 SSE 流（`x-vercel-ai-ui-message-stream: v1`），CORS 白名单已包含 Vite 默认端口 5173——缺少的只是消费端。现在补上 Web 前端，才能完整闭环地使用和验证 agent，也为后续 ComfyUI 工具的可视化提供落点。

## What Changes

- 在 `apps/web` 新建 pnpm 管理的 Vite + React 19 + TypeScript 前端工程。
- 引入 Tailwind CSS v4、shadcn/ui、AI Elements（shadcn 风格 registry，组件源码入仓）。
- 锁定 AI SDK v5（`ai@5` + `@ai-sdk/react@5`），通过 `DefaultChatTransport` 直连后端 `POST /api/chat`（地址来自 `VITE_API_BASE`，开发期为 `http://localhost:3001`）。
- 实现 v1 单会话聊天页：流式文本渲染、工具调用 part 渲染（首个工具 `add`）、`messageMetadata`（outcome/steps）展示、错误与停止生成支持。
- 明确不提供 regenerate（后端仅支持 `submit-message` trigger）。
- 仓库配套调整：`.gitignore` 增加 Node 产物，`.env.example` 增加 `VITE_API_BASE`，文档补充前端开发命令。

## Capabilities

### New Capabilities

- `web-chat-ui`: 浏览器端单会话聊天界面——提交用户消息、渲染流式回复与工具调用、展示消息元数据与错误，对接 `crates/server` 的 `/api/chat` AI SDK 协议端点。

### Modified Capabilities

（无——项目尚无既有 spec；本变更不修改后端任何行为。）

## Impact

- **新增**：`apps/web` 整个 Node 工程（pnpm、Vite、React、Tailwind、shadcn、AI Elements、AI SDK v5）。对 Cargo workspace 无影响（members 仅 `crates/*`）。
- **修改**：`.gitignore`、`.env.example`、`README.md` / `AGENTS.md`（前端命令约定）。
- **依赖**：消费 `crates/server` 现有 `/api/chat` 契约（请求体 UIMessage v5、SSE 流、CORS 含 5173），本变更不改后端代码；与正在进行的 server 实现工作并行，契约以当前 `protocol.rs` / `stream.rs` 为准。
- **环境**：本机已有 Node v24 与 pnpm 11，无需额外装机。
