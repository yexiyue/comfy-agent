# Design

## Context

后端 `crates/server` 已按 AI SDK 5 协议提供 `POST /api/chat`：请求体为 UIMessage v5（`{id, messages, trigger, messageId}`，camelCase），响应为 SSE 流并带 `x-vercel-ai-ui-message-stream: v1` 头；CORS 白名单默认含 `http://localhost:5173` 并 expose 该协议头。前端是"照着插座造插头"，不写任何协议适配层。本机环境：Node v24、pnpm 11。

服务端约束（前端必须迁就）：仅支持 `trigger: "submit-message"`（无 regenerate）；历史中的工具 part 必须是已完成态；最后一条消息必须是 user；请求体上限 2MB；`finish` 事件携带 `messageMetadata: {outcome, steps}`。

## Goals / Non-Goals

**Goals:**

- 一个可直接开发、构建的 `apps/web` 前端工程，与 Cargo workspace 零干扰。
- 单会话聊天页完整消费现有 `/api/chat` 契约：流式文本、工具 part、元数据、停止、错误。
- 工具接入做成声明式扩展点，后续 ComfyUI 工具（含图片产出）只需加声明+渲染组件。

**Non-Goals:**

- 不做会话持久化、多会话切换、分支（Branch）——后端 stateless，等后端有能力再说。
- 不做 regenerate / 编辑重发——后端 trigger 限制。
- 不修改后端代码；生产部署方案（如 axum `ServeDir` 托管前端静态文件）留待后续变更。
- v1 不引入前端单测框架；以浏览器手工验证 + 构建通过为验收（后续可补 vitest）。

## Decisions

### D1: 工程形态——apps/web 独立 pnpm 工程

`pnpm create vite apps/web --template react-ts`（React 19 + TS strict）。不在仓库根建 Node workspace（单一 app 没有收益）；Cargo members 仅 `crates/*`，天然互不影响。备选：pnpm workspace + Turborepo——因只有一个 app 而否决，未来加第二个 app 时再升级。

### D2: AI Elements 经 shadcn registry 引入，连带 Tailwind v4 + shadcn 基座

AI Elements 是 shadcn 风格 registry（组件源码入仓，依赖 Radix/Tailwind）。依赖链：`@tailwindcss/vite` 插件（Tailwind v4）→ `shadcn init`（生成 `components.json`、`src/lib/utils.ts` 的 `cn()`）→ `npx ai-elements add conversation message composer actions tool`。v1 使用的 AI Elements 组件：`Conversation`、`Composer`、`Message`/`MessagePart`、`Tool`、`Response`；**不引入** `Actions` 的 reload/regenerate 能力（触发后端 400）。备选：自写聊天 UI——否决，AI Elements 与后端协议（UIMessage parts 模型）一一对应，重写即重造轮子。

### D3: AI SDK 采用当前最新 v7 线，DefaultChatTransport 直连

（apply 阶段修订，用户拍板）原设计锁 `ai@5`，但 2026-10 的 ai-elements registry 组件已按 `ai@^7` 演进，锁 v5 会与组件生态冲突。改为 `ai@^7` + `@ai-sdk/react@^3`（配对 react ^19.2.1）。后端下发的 `x-vercel-ai-ui-message-stream: v1` 流能否被 v7 客户端解析，以 2.1 任务的真实联调为实证标准；若不兼容则回退 `ai@5.0.271 + @ai-sdk/react@2.0.274` 并降级 ai-elements 组件。`useChat` 配 `DefaultChatTransport({ api: ${import.meta.env.VITE_API_BASE ?? 'http://localhost:3001'}/api/chat })`，开发期直连 + 走后端已就绪的 CORS（5173 已在白名单，协议头已 expose）。备选：Vite dev proxy `/api` → 3001——当前零收益而否决，生产若要同源再以后端 `ServeDir` 方案替换。SSE keep-alive 注释行（`: ping`）由 SDK 自动忽略。

### D4: 工具声明集中在单一注册文件

`src/lib/tools.tsx`：以 zod schema 声明 `useChat({ tools })` 所需的 inputSchema（首个为 `add: {a, b: i64}`），并映射到对应的 Tool 渲染组件。后端新增工具时：加一条 schema + 一个渲染分支即可，`protocol.rs` 已接受 `tool-<name>` part 回传。类型上以 `UIMessage<never, {outcome, steps}>` 扩展 metadata 泛型，读 `finish` 事件回填的 outcome/steps。

### D5: 后端就绪探测

挂载时对 `${VITE_API_BASE}/health`（GET，无自定义头）做一次探测并展示状态徽标；失败时给出"后端不可达 + 期望地址"提示，聊天输入保持可用（重试由用户发消息自然触发）。不做轮询，避免复杂化。

## Risks / Trade-offs

- [AI SDK 主版本漂移引入协议不兼容] → package.json 锁 `^5`；契约源头以 `crates/server/src/stream.rs` 为准，后端若升级协议需同步本前端。
- [shadcn / ai-elements CLI 在 Windows Git Bash 下的交互式提示或路径问题] → 优先使用非交互参数；失败时按 registry JSON 手工拷贝组件（源码入仓模式本身就允许手改）。
- [与 server 实现 agent 并行开发，契约演进（如新工具、新 metadata 字段）] → 前端只依赖已文档化的流事件集合；工具接入走 D4 扩展点，未声明的新工具 part 仍会以默认样式渲染，不会崩。
- [2MB 请求上限与超长对话] → v1 不处理分页/截断，超出属后端 400，按错误提示展示；留待会话持久化设计时一并考虑。
- [无前端自动化测试] → 以手工浏览器验证清单（对应 spec 各 Scenario）+ `pnpm build` 作为验收门槛；引入 vitest 属后续独立决策。

## Migration Plan

纯新增变更：回滚即删除 `apps/web` 与配置增项。无数据、无接口迁移。

## Open Questions

- ComfyUI 图片/视频产出工具的渲染形态（缩略图、灯箱、下载）——不影响本次架构，落在 D4 扩展点上另行设计。
- 生产部署是否收敛为 axum 单端口托管静态文件——待后端稳定后另起变更。
