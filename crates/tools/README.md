# tools

用 `#[agent_tool]` 把异步函数转换为实现 `AgentTool` 的工具，同时保留原函数。
`tool-macros` 负责生成代码，本 crate 提供 trait 和宏的统一入口。

## 无状态工具

```rust
use tools::{agent_tool, AgentTool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, JsonSchema)]
struct AddArgs { a: i64, b: i64 }

#[derive(Serialize)]
struct AddResult { sum: i64 }

/// 计算两个整数的和。
#[agent_tool]
async fn add(args: AddArgs) -> anyhow::Result<AddResult> {
    Ok(AddResult { sum: args.a.checked_add(args.b)
        .ok_or_else(|| anyhow::anyhow!("整数溢出"))? })
}

// 自动生成单元结构体 AddTool：名称 add，描述取文档注释。
// registry.register(AddTool)?;
// add(AddArgs { a: 1, b: 2 }).await?; 仍可直接调用原函数。
```

参数类型需要 `DeserializeOwned + JsonSchema`；成功返回值需要 `Serialize`，
可以返回结构体，也可以返回 `serde_json::Value`。
宏在执行时解析参数、调用函数、序列化结果，错误交给调用方处理。
需要拒绝未知字段时，在参数结构体上添加 `#[serde(deny_unknown_fields)]`。

名称和描述可分别覆盖：

```rust,ignore
#[agent_tool(name = "calculator", description = "执行加法计算。")]
async fn add(args: AddArgs) -> anyhow::Result<AddResult> { /* ... */ }
```

工具名称默认取函数名，结构体名称取函数名的 PascalCase 加 `Tool`，覆盖工具名称不会改变结构体名。
名称限制为最多 64 个 ASCII 字符，首字符为字母或下划线，其余允许字母、数字、下划线、连字符。
描述默认按行读取 `///` 文档注释；没有文档注释时必须显式提供非空描述。

## 有状态工具

第二个参数声明为 `&Context`，生成的工具就会持有 `Arc<Context>`：

```rust
use tools::agent_tool;
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

#[derive(Deserialize, JsonSchema)]
struct CountArgs { amount: usize }

struct CounterContext { count: AtomicUsize }

/// 增加计数器并返回新值。
#[agent_tool]
async fn count(args: CountArgs, ctx: &CounterContext) -> anyhow::Result<usize> {
    Ok(ctx.count.fetch_add(args.amount, Ordering::SeqCst) + args.amount)
}

let ctx = Arc::new(CounterContext { count: AtomicUsize::new(0) });
let tool = CountTool::new(ctx.clone());
// registry.register(tool)?;
```

`Context` 不进入给模型的参数 schema，也不需要实现 serde trait。
它必须满足 `Send + Sync`；放入当前项目注册表时还需要 `'static`。
可以包含 HTTP 客户端、服务地址、数据库连接池，以及通过原子类型或锁管理的可变状态。
共享同一个 `Arc` 的工具共享状态；为每次会话创建独立 context 可以隔离会话状态。
避免持有同步锁跨越 `.await`，异步操作需要锁时可用 `tokio::sync::Mutex`。

## 支持范围

- 支持普通的、非泛型的安全 `async fn`，以及一个 owned 参数和可选的共享 context 引用。
- 返回类型写作 `anyhow::Result<T>` 或 `Result<T, E>`；错误必须可通过 `?` 转为 `anyhow::Error`。
- 暂不支持同步函数、方法中的 `self`、`&mut Context`、显式生命周期和自定义 Result 类型别名。
- 生成结构体与原函数的可见性相同；公开函数的参数和返回类型也应公开。
- 生成的工具仍需显式注册；宏不会维护全局注册表。
- 如果多个函数转换后产生同名结构体，请放入不同模块。

测试示例见本 crate 的 `tests/agent_tool.rs`，注册表集成示例见 `crates/agent/tests/agent.rs`。
运行 `cargo test --workspace` 验证。

## 持久化执行与副作用恢复

宏生成的工具仍兼容原接口，默认 `RecoveryPolicy::Conservative`。对于无外部副作用的纯计算工具，可注册 `tools::SafeToRetry(AddTool)`，加法后端已这样配置。不要把付费请求、文件写入或外部任务提交随意标为安全。

需要恢复外部动作时，手工实现 `AgentTool::execute_with_context`：`ExecutionContext.operation_key` 是稳定的 run/call 幂等键，恢复不会改变它。`Idempotent` 策略要求外部系统实际接受这个键并去重；工具自身应将键传给外部 API。`Reconcilable` 工具应在得到任务 ID 后先 `context.record_external_id(id).await?`，再等待结果；恢复改调用 `reconcile` 查询原 ID。查询失败不会重提交，会进入 `needs-attention`。

默认保守策略在未确认的外部结果上停止自动恢复。暂停和取消丢弃本地 future，不代表已发生的外部副作用被撤销。详细运行例子见 [持久化会话指南](../../docs/durable-sessions.md)。
