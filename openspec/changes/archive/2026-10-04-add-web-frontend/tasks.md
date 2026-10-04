# Tasks

## 1. 工程脚手架与基座

- [x] 1.1 在仓库根用 pnpm 创建 Vite React-TS 工程 `apps/web`（React 19、TS strict），验证 `pnpm dev` 能起默认页面、`pnpm build` 通过
- [x] 1.2 接入 Tailwind CSS v4（`@tailwindcss/vite` 插件 + `src/index.css`），验证构建产物包含样式且 `pnpm build` 通过
- [x] 1.3 配置路径别名 `@/*` 并运行 `shadcn init` 生成 `components.json` 与 `src/lib/utils.ts`（`cn()`），验证 `components.json` 存在且构建通过
- [x] 1.4 通过 ai-elements registry 添加 `conversation`、`message`、`composer`、`tool`、`response` 组件（不引入 reload/regenerate 能力），验证组件文件入仓且 `pnpm build` 通过。实际组件名为 registry 演进后的 `conversation`/`message`/`tool`/`prompt-input`（原 composer），response 能力并入 message 体系；已裁剪掉 CLI 误装的全量其余组件
- [x] 1.5 安装并锁定 `ai@^5`、`@ai-sdk/react@^5`、`zod`，验证 `package.json` 版本与 `pnpm install` 成功。变更：按用户决策改用最新 v7 线（`ai@^7.0.127` + `@ai-sdk/react@^3.0.303` + `zod@^4.6.5`，当前 registry 生态按 v7 演进；对后端 v1 协议流的兼容性由 2.1 联调实证）
- [x] 1.6 仓库配套：`.gitignore` 增加 `node_modules`/`dist` 等 Node 产物，`.env.example` 增加 `VITE_API_BASE=http://localhost:3001` 及注释，验证 `git status` 不出现 node_modules 且示例文件可读
- [x] 1.7 更新 `README.md` 与 `AGENTS.md`：Node/pnpm 前置条件与 `pnpm dev`、`pnpm build` 命令约定，验证文档中的命令按原文可运行（修正为 `pnpm -C apps/web <cmd>` 形式，`--dir` 后置不被 pnpm 接受）

## 2. 聊天核心接线

- [x] 2.1 实现 `DefaultChatTransport`（`api` 取 `VITE_API_BASE ?? http://localhost:3001` 拼 `/api/chat`）并接入 `useChat`，验证启动后端（`cargo run -p server`）后浏览器提交消息能收到流式回复
- [x] 2.2 用 Conversation + Composer 组装聊天页：消息列表、流式文本增量渲染、自动滚动，验证多轮对话与流式逐字显示正常
- [x] 2.3 提交守卫：空消息不发送；流式进行中禁用发送并给出"生成中"可见状态，验证两个场景的 UI 行为与网络面板（无重复请求）
- [x] 2.4 停止生成：停止按钮调用 `stop()`，验证中途停止后已渲染内容保留、输入恢复、界面有已停止提示
- [x] 2.5 错误处理：`onError` 展示行内错误提示，验证停掉后端后发消息出现连接失败提示、历史保留、恢复后端后可重试
- [x] 2.6 后端就绪探测：挂载时 GET `${VITE_API_BASE}/health` 并展示状态徽标，验证后端停止时显示"后端不可达 + 期望地址"、启动后显示正常

## 3. 工具调用与元数据

- [x] 3.1 建立 `src/lib/tools.tsx` 声明式注册点：zod 声明 `add`（`{a, b}` 整数）的 inputSchema 并映射渲染组件，验证用"帮我算 2+3"类提示触发工具，区块依次呈现执行中/完成（求和结果）状态。变更：AI SDK v7 移除了 useChat 的 tools 选项，inputSchema 声明改为类型层（zod schema 经 z.infer 进入 UIMessage 的 UITools 泛型）
- [x] 3.2 工具错误态渲染：`tool-output-error` 展示错误文本且对话不中断，验证后端工具报错场景（如构造溢出输入"算 9999999999999999999 + 1"）下 UI 不崩、后续文本继续（实测 int64 最大值 + 1 → "add Error" 状态 + 模型继续解释溢出原因）
- [x] 3.3 扩展 `UIMessage` metadata 泛型为 `{outcome: 'finished' | 'step-limit', steps: number}`，在助手消息上渲染完成状态与 step 数，验证正常回复显示 finished + 步数，且 `AGENT_MAX_STEPS=1` 启动后端时 step-limit 有可区分标识（橙色"已达步数上限 · N 步" vs 常规"已完成 · N 步"）
- [x] 3.4 未声明工具的兜底渲染：默认样式展示工具名/输入/输出而不崩溃，验证临时移除 `add` 声明后触发工具页面仍正常渲染（实测默认 ToolInput/ToolOutput 渲染，无崩溃）

## 4. 集成验证

- [x] 4.1 全量回归：`cargo test --workspace`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings` 全绿（确认前端引入未影响 Rust 侧）。实测：fmt OK、clippy 零警告、12 个测试套件全部通过零失败
- [x] 4.2 按 spec `web-chat-ui` 各 Scenario 逐条做浏览器手工走查（提交/空消息/生成中守卫/流式/多步/工具三态/元数据/停止/两类错误/健康探测），结果记录在变更目录 `verification.md`
- [x] 4.3 `pnpm build` 产出 `dist/` 且无类型错误，验证生产构建可用（部署方式另行变更，不在本次范围）。实测：tsc -b + vite build 通过，dist/ 完整产出（shiki 语法高亮 chunk 体积警告，属已知可接受项）
