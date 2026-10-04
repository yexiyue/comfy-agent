# Verification record

验证日期：2026-10-04。环境：Windows / PowerShell、Rust 1.98.1（依赖最低要求 1.95）、Node 24.18.0、PostgreSQL 17.6、AI SDK `ai@7.0.127`。所有模型和工具故障测试使用本机替身，不调用付费模型。

## 可重现命令

先按 README 启动 PostgreSQL，创建独立 `comfy_agent_test` 库并设置 `TEST_DATABASE_URL`。Node fixtures 另建随机 `agent_fixture_*_test` 子库并仅清理自身子库；进程执行测试使用 `agent_exec_*_test` 子库。仓库事务测试只操作自身随机会话。不要把测试变量指向业务库。

```powershell
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p persistence -p server -- --ignored
npm ci --prefix scripts/ai-sdk-check
npm run check:mock --prefix scripts/ai-sdk-check
npm run check --prefix scripts/evals
npm test --prefix scripts/evals
npm run smoke:phoenix --prefix scripts/evals
npm run eval:mock --prefix scripts/evals -- --phoenix --trials 1 --output outputs/evals/durable-phoenix-gate
pnpm -C apps/web test
pnpm -C apps/web build
openspec validate add-durable-agent-sessions --strict
```

`cargo test --workspace` 不代替 ignored 数据库测试。慢消费者测试使用真实 31 秒等待。强杀测试启动并结束自身子进程；本机 Phoenix 和业务数据库容器不被清理。

## Specs 场景与证据映射

| Spec / 场景 | 验证位置及断言 |
| --- | --- |
| persistent-conversations: Reload a conversation / Reject history overwrite | server `authoritative_snapshots_idempotent_submission_replay_and_second_turn`；`invalid_commands_cors_body_limit_and_health_do_not_call_model`：数据库权威历史、禁止旧 messages 合约 |
| Concurrent submissions / Retry an accepted request | persistence repository `competing_submissions_and_claims_have_one_winner`、`accepts_once_rejects_conflicts_and_rolls_back_partial_work`：单个有效 run、幂等 receipt、冲突与事务回滚 |
| Continue after completed tools / Invalid imported history | server `strict_import_and_pagination_preserve_completed_multi_step_history`；history 单元测试；官方协议脚本第二轮检查原调用 ID 与 assistant/tool 历史 |
| Interrupted model draft | agent phases、runtime checkpoint、execution `repeated_model_pause_drops_drafts_preserves_budget_and_counts_real_calls`；官方 parser 检查最终答案只有 resumed |
| durable-agent-execution: Crash before queue publication / Duplicate delivery | compatibility 队列孤儿恢复；execution `apalis_outbox_runs_without_an_http_subscriber`：先保存后发布、发布后确认丢失、双 worker、有效执行一次 |
| Restart between two tools / Old worker finishes after recovery | execution `force_killed_process_between_tools_recovers_only_the_uncommitted_action`、`lost_worker_lease_recovers_remaining_tool_and_pausing_never_auto_resumes`；repository stale generation 写入拒绝；第一项只执行一次 |
| Resume near step limit / Resume without new input | execution 多次模型暂停：逻辑步骤不重置、实际请求计数累加、旧草稿失效；工具暂停保留决策、只重做当前项 |
| Pause during model streaming / Cancel a paused task | server 控制路由；repository paused 控制；进程协议矩阵和浏览器暂停、刷新、恢复 |
| Additional instruction replaces pending work | execution `steer_reuses_completed_results_closes_pending_calls_and_discards_old_todo`；浏览器暂停后追加指令、新 run 替代旧待办 |
| Unknown side effect after restart / Reconcile an external job | execution `conservative_side_effects_need_attention_and_external_ids_reconcile_without_resubmit`：未知结果停待核对、复用 operation key、保存 external ID 并查询原任务 |
| 恢复上限、丢失控制通知、pausing 崩溃 | execution recovery limit / lost notification / force-killed pausing 测试：不得自动重做或自动续跑 |
| resumable-chat-streams: Close the browser / Slow subscriber | server disconnect 和真实慢消费者测试；进程矩阵无订阅仍完成；重连不改变执行统计 |
| Reconnect during a tool step / Resume an interrupted text segment | 阶段工具等待检查点测试、持久化高水位重放、官方 parser 工具流与恢复后的完整有效前缀；frontend replay 同 ID 替换测试 |
| Official parser consumption / Idle connection heartbeat | ai-sdk-check、durable-check：文本/工具/多步/错误/暂停/取消/转向/step-limit、10 秒 SSE 注释心跳，无虚假 part |
| Open a paused conversation / Two tabs issue controls | 两个真实 Chromium 页面：URL 刷新保持暂停、恢复替换草稿；旧 revision 返回 409 并刷新权威历史，再提交成功；控制版本竞争由 repository/HTTP 测试验证 |
| 存储故障与容量边界 | server `storage_failure_aborts_only_the_subscription_and_stops_inflight_execution`：503 安全响应、stream error/abort、watchdog 丢弃模型等待；execution progress capacity 测试：提交失败前不调用模型/工具；FLOOD 回调队列溢出终结请求 |
| agent-observability: Concurrent sessions / Evaluation trace continuation | server memory exporter：并发 trace 隔离、W3C 父 trace 保留、按 run/attempt ID 筛选；真实 Phoenix 评测关联验证 |
| Resumed execution correlation / Existing chat client | lifecycle smoke 的同 run 多 attempt、shutdown/interrupted 恢复；官方 metadata run/attempt/trace 标识；新 run supersedes 来源 |
| Consumer disconnect / Recovered tool error / Abnormal termination | memory exporter、工具错误继续测试；Phoenix lifecycle 的 finished/paused/cancelled/shutdown；强杀缺失 span 由数据库 interrupted 事实解释，不伪造 export |
| Partial usage and no text / Replay and retry statistics | 多次暂停 usageComplete=false、实际调用增加；两次并发 replay 不改变统计；22 用例 known tokens 与缺失 usage 的汇总；订阅/执行/模型延迟分开 |
| agent-evaluation: Multi-turn tool case | eval runner 独立服务端会话、增量提交，22 fixtures 包含多轮原工具结果引用 |
| Timeout or exhausted budget | runner 测试：预算跳过、超时显式取消、独立 cleanup deadline、接受响应丢失时查 receipt 并取消，无孤儿任务 |
| Inspect a failed evaluation | Phoenix experiment 保存 trial/run/attempt/trace IDs；Phoenix 不可用测试仍落盘全部 planned trial 报告 |
| Recover a multi-tool run / Fence an interrupted attempt | 真实数据库执行矩阵、真实进程强杀双工具测试、generation/lease 原子检查 |

## 运行记录与限制

本地证据输出位于忽略的 `outputs/`：`durable-check.json`、`phoenix-smoke.json`、`evals/durable-phoenix-gate/summary.json`。Phoenix 22 用例门禁全部适用评分为 1，22 个 trial 可关联执行轨迹，132 个评分记录，574 个已知 token。这些确定性替身结果验证实现和评分链路，不代表真实模型质量。

浏览器验证还覆盖暂停后追加指令、原样继续、刷新后历史一致性；前端构建通过，仍有原有大 bundle 提示。服务仍面向本地开发，无认证。外部副作用不承诺撤销，未知操作需要人工核对。迁移显式执行；仅空业务数据支持回退，已有数据必须先备份。

本次还修复验收发现的两个生产问题：所有事务统一 conversation → run 锁顺序（包括 claim）；状态事件仅在变化时发送，避免重置 Axum 注释心跳。观测层交由 exporter 筛选 OpenInference span，避免层过滤破坏执行根与父 trace 关联。
