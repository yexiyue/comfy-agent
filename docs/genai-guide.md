# genai 使用指南（0.7.0-rc.1）

> 本文基于 genai **0.7.0-rc.1**（2026-09-27 发布，crates.io 最新版）。所有代码示例均改写自官方
> [rust-genai](https://github.com/jeremychone/rust-genai) 仓库的 examples，API 以 0.7 为准。
> 0.6 的部分写法（如 `Client::default()`）在 0.7 已不可用，本文不使用。

## 1. genai 是什么，不是什么

genai 是 Jeremy Chone 维护的**多厂商 LLM 适配层**：

- **是**：一套统一的 Chat API，背后用**原生协议**直连 27+ 厂商（OpenAI、Anthropic、Gemini、
  智谱 Zai、DeepSeek、Moonshot/Kimi、Qwen、Groq、Ollama……），支持流式、工具调用、内置
  WebSearch 工具。你不写任何 HTTP 代码，不依赖任何厂商 SDK。
- **不是**：agent 框架。它**没有** agent 循环、记忆管理、RAG、向量库。工具的执行永远发生在
  你的代码里，genai 只负责"把工具 schema 发给模型、把模型的工具调用解析出来"。

这个边界对学习者是优点：agent 的核心逻辑（循环、分发、上下文管理）全部由你掌控，genai
只抹平最没营养的厂商差异。

## 2. 安装

```toml
[dependencies]
genai = "0.7.0-rc.1"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

TLS 后端默认 `rustls`（rustls + aws-lc-rs + 系统信任库）。特殊环境可切换：

```toml
# 用系统 TLS（OpenSSL / Windows SChannel）
genai = { version = "0.7.0-rc.1", default-features = false, features = ["native-tls"] }
```

`rustls-tls` 与 `native-tls` 互斥，同时启用会编译报错。需要自定义 CA / mTLS 时，自建
`reqwest::Client` 后用 `Client::builder().with_reqwest(client)` 注入。

## 3. 五分钟上手

genai 从环境变量读 API key，各厂商变量名见 §5。先设一个（例如 `OPENAI_API_KEY`），然后：

```rust
use genai::Client;
use genai::chat::{ChatMessage, ChatRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 0.7 起 Client::new() 返回 Result（不再有 Client::default()）
    let client = Client::new()?;

    let chat_req = ChatRequest::new(vec![
        ChatMessage::system("Answer in one sentence"),
        ChatMessage::user("Why is the sky red?"),
    ]);

    let chat_res = client.exec_chat("gpt-4o-mini", chat_req, None).await?;

    println!("{}", chat_res.first_text().unwrap_or("NO ANSWER"));
    Ok(())
}
```

换成其他厂商通常**只改模型名字符串**：

```rust
client.exec_chat("claude-haiku-4-5", ...).await?;   // Anthropic（ANTHROPIC_API_KEY）
client.exec_chat("gemini-2.0-flash", ...).await?;   // Google（GEMINI_API_KEY）
client.exec_chat("glm-4.6", ...).await?;            // 智谱（ZAI_API_KEY）
client.exec_chat("deepseek-chat", ...).await?;      // DeepSeek（DEEPSEEK_API_KEY）
```

## 4. 消息与请求构建

```rust
use genai::chat::{ChatMessage, ChatRequest};

// 方式一：一次性给全消息
let req = ChatRequest::new(vec![
    ChatMessage::system("You are a helpful assistant"),
    ChatMessage::user("你好"),
]);

// 方式二：链式构建（system 单独设置，多轮对话累积消息）
let mut req = ChatRequest::default()
    .with_system("Answer in one sentence")
    .append_message(ChatMessage::user("Why is the sky blue?"));
```

**多轮对话的标准模式**（自己维护历史，genai 无状态）：

```rust
let mut chat_req = ChatRequest::default().with_system("Answer in one sentence");

for question in ["Why is the sky blue?", "Why is it red sometimes?"] {
    chat_req = chat_req.append_message(ChatMessage::user(question));

    let stream = client.exec_chat_stream(MODEL, chat_req.clone(), None).await?;

    // print_chat_stream 边打字机打印边消费流，结束后返回完整的 assistant 文本
    let assistant_answer = print_chat_stream(stream, None).await?;

    // 关键：把回答追加回历史，下一轮才有上下文
    chat_req = chat_req.append_message(ChatMessage::assistant(assistant_answer));
}
```

`append_message` 接受任何实现了 `Into<ChatMessage>` 的东西——`ChatMessage`、
`Vec<ToolCall>`（模型发起的工具调用）、`ToolResponse`（你的执行结果），这三种正是
工具调用循环的全部角色（见 §8）。

## 5. 模型名与 Provider 路由

genai 用**模型名前缀**推断适配器，常见规则：

| 模型名 | 适配器 | 环境变量 |
|---|---|---|
| `gpt-*`、`o3-*`、`chatgpt-*` | OpenAI | `OPENAI_API_KEY` |
| `claude-*` | Anthropic | `ANTHROPIC_API_KEY` |
| `gemini-*` | Gemini | `GEMINI_API_KEY` |
| `glm-*` | 智谱 Zai | `ZAI_API_KEY` |
| `deepseek-*` | DeepSeek | `DEEPSEEK_API_KEY` |
| `moonshot-*` / `kimi*` | Moonshot/Kimi | `MOONSHOT_API_KEY` |
| `grok-*` | xAI | `XAI_API_KEY` |
| 其他（如 `llama3.2`） | 本地 Ollama | 无需 key |

**namespace 语法**可以强制指定适配器，绕过推断：`adapter::model_name`。

```rust
// groq 上跑 OpenAI 开源模型
client.exec_chat("groq::openai/gpt-oss-20b", ...).await?;
// 智谱编程模型端点
client.exec_chat("zai_coding::glm-4.6", ...).await?;
// 任意 OpenAI 兼容端点（vLLM、OneAPI、本地代理……）
// 环境变量：GENAI_1_ENDPOINT / GENAI_1_API_KEY
client.exec_chat("genai_1::my-model-7b", ...).await?;
```

模型名可带推理力度后缀（如 `gpt-5.4-mini-high`），发送前会被自动剥离。

内置规则不满足时，可自定义 `AdapterKindResolver`（见 `examples/c03-mapper.rs`）或用
`ServiceTargetResolver` 按请求动态决定 endpoint / auth / model（见 `examples/c06-target-resolver.rs`）——
后者适合"同一模型名，按租户路由不同 API key"的场景。

**静态改端点**（例：智谱 Coding Plan 走 `.../api/coding/paas/v4/` 而非标准
`.../api/paas/v4/`）也用 `ServiceTargetResolver`，只重写 endpoint、适配器与认证不变：

```rust
use genai::resolver::{Endpoint, ServiceTargetResolver};
use genai::{Client, ServiceTarget};

let resolver = ServiceTargetResolver::from_resolver_fn(
    |mut target: ServiceTarget| -> Result<_, genai::resolver::Error> {
        target.endpoint = Endpoint::from_static("https://open.bigmodel.cn/api/coding/paas/v4/");
        Ok(target)
    },
);
let client = Client::builder().with_service_target_resolver(resolver).build()?;
```

注意它会覆盖该 Client 的**所有**模型请求；多厂商并存时应按 `target.model.adapter_kind`
条件改写。

## 6. ChatOptions：采样参数与捕获开关

```rust
use genai::chat::ChatOptions;
use genai::{Client, ClientConfig};

// 请求级：exec_chat / exec_chat_stream 的第三个参数
let options = ChatOptions::default()
    .with_temperature(0.0)
    .with_top_p(0.99)
    .with_max_tokens(1000);

client.exec_chat(MODEL, chat_req, Some(&options)).await?;

// Client 级：作为兜底默认值（请求级未设置的字段会回落到这里）
let client_config = ClientConfig::default()
    .with_chat_options(ChatOptions::default().with_temperature(0.0));
let client = Client::builder().with_config(client_config).build()?;
```

两个重要的"捕获"开关（默认关闭）：

| 开关 | 作用 |
|---|---|
| `with_capture_tool_calls(true)` | 流式模式下，把分片到达的工具调用聚合成完整 `ToolCall`，放在 `End` 事件的 `captured_content` 里 |
| `with_capture_raw_body(true)` | 捕获厂商原始响应体（调试用），放在 `ChatResponse::captured_raw_body` |

其他低层扩展点：`with_extra_body(...)` 可向 OpenAI 兼容请求体注入任意字段（唯一的
provider 专属逃生舱）；`with_tool_choice(...)` 控制"自动/禁用/必选/指定"工具。

## 7. 流式响应：ChatStreamEvent

```rust
use futures::StreamExt;
use genai::chat::{ChatOptions, ChatStreamEvent};

let options = ChatOptions::default().with_capture_tool_calls(true);
let mut chat_stream = client
    .exec_chat_stream(MODEL, chat_req, Some(&options))
    .await?;

while let Some(result) = chat_stream.stream.next().await {
    match result? {
        ChatStreamEvent::Start => { /* 流开始 */ }
        ChatStreamEvent::Chunk(chunk) => {
            print!("{}", chunk.content);          // 文本增量
        }
        ChatStreamEvent::ToolCallChunk(chunk) => {
            // 工具调用增量片段；完整聚合要靠 capture_tool_calls + End 事件
            println!("  ToolCallChunk: {:?}", chunk.tool_call);
        }
        ChatStreamEvent::ReasoningChunk(chunk) => {
            println!("  reasoning: {}", chunk.content); // 推理模型的思考过程
        }
        ChatStreamEvent::ThoughtSignatureChunk(chunk) => {
            // Gemini 3 等模型的签名思考块，见 §8 的"回填"注意事项
        }
        ChatStreamEvent::Heartbeat => { /* Anthropic SSE 心跳，忽略即可 */ }
        ChatStreamEvent::End(end) => {
            // 拿聚合后的工具调用：
            if let Some(content) = end.captured_content {
                for part in content.into_parts() {
                    match part {
                        genai::chat::ContentPart::ToolCall(tc) => {
                            println!("tool: {} args: {}", tc.fn_name, tc.fn_arguments);
                        }
                        genai::chat::ContentPart::ThoughtSignature(t) => { /* 保留备用 */ }
                        _ => {}
                    }
                }
            }
        }
    }
}
```

只想打印的话不必手写循环：`genai::chat::printer::print_chat_stream(stream, None).await?`
消费整个流并返回完整回答文本。

## 8. 工具调用：完整往返（本指南核心）

工具调用是一个**两阶段往返**：模型决定调用什么 → 你执行 → 把结果回填 → 模型给最终回答。

### 第 1 步：定义工具（JSON Schema）

```rust
use genai::chat::Tool;
use serde_json::json;

let weather_tool = Tool::new("get_weather")
    .with_description("Get the current weather for a location")
    .with_schema(json!({
        "type": "object",
        "properties": {
            "city":    { "type": "string", "description": "The city name" },
            "country": { "type": "string" },
            "unit":    { "type": "string", "enum": ["C", "F"] }
        },
        "required": ["city", "country", "unit"]
    }));
```

### 第 2 步：发起带工具的请求

```rust
use genai::chat::{ChatMessage, ChatRequest};

let chat_req = ChatRequest::new(vec![ChatMessage::user("东京今天天气怎么样？")])
    .with_tools(vec![weather_tool]);            // 或 .append_tool(tool)
```

### 第 3 步：拿到工具调用

```rust
let chat_res = client.exec_chat(MODEL, chat_req.clone(), None).await?;
let tool_calls = chat_res.into_tool_calls();     // Vec<ToolCall>

for tc in &tool_calls {
    println!("Function: {}", tc.fn_name);        // "get_weather"
    println!("Arguments: {}", tc.fn_arguments);  // JSON 字符串，需自己解析
}
```

### 第 4 步：执行工具（你的代码，genai 不参与）

```rust
let tc = &tool_calls[0];
let args: serde_json::Value = serde_json::from_str(&tc.fn_arguments)?;
// ……这里调用你真正的实现：查天气、跑 ComfyUI 工作流、查数据库……
```

### 第 5 步：回填结果，再调一次

```rust
use genai::chat::ToolResponse;

let tool_response = ToolResponse::new(
    tc.call_id.clone(),                          // 必须对得上 call_id
    json!({ "temperature": 22.5, "condition": "Sunny" }).to_string(),
);

// 把"模型的工具调用"和"你的执行结果"都追加进历史
let chat_req = chat_req
    .append_message(tool_calls)      // Vec<ToolCall> → assistant 消息
    .append_message(tool_response);  // ToolResponse  → tool 角色消息

let chat_res = client.exec_chat(MODEL, chat_req, None).await?;
println!("{}", chat_res.first_text().unwrap_or("NO ANSWER"));
```

**把这个往返套上 while 循环、加上步数上限，就是 agent loop**——教程系列的主角。

### 流式 + 工具调用并存

流式模式下工具调用被拆成 `ToolCallChunk` 分片，直接用会拿到半截 JSON。正确做法是开
`with_capture_tool_calls(true)`，在 `End` 事件里取聚合结果（§7 已示例），然后走同样的
回填流程。

**thought signature 注意事项**：Gemini 3 等模型在工具轮次会附带签名的思考块，回填历史时
必须把它和 `ToolCall` 一起放回 assistant 消息，否则下一轮会被服务端拒绝。完整处理见
`examples/c21-tooluse-streaming.rs`。

## 9. 内置 WebSearch 工具

需要联网调研时，不用自己写搜索工具：

```rust
use genai::chat::{ChatRequest, Tool, ToolName, WebSearchConfig};

let web_search_tool = Tool::new(ToolName::WebSearch)          // 归一化写法，跨厂商
    .with_config(WebSearchConfig::default().with_max_uses(3));

let chat_req = ChatRequest::from_user("给我最近 3 天 Rust 的新闻")
    .append_tool(web_search_tool);

// 执行是厂商侧完成的（没有 ToolResponse 回填环节），直接拿最终回答
let res = client.exec_chat(MODEL, chat_req, None).await?;
```

搜索在厂商服务器上执行，这和 §8 的"你自己执行"是两种不同的工具模式，教程里两种都会用到。

## 10. 结构化输出的正确姿势

genai 没有直接的 `response_format: json_schema` API（刻意的——那是 OpenAI 专属概念）。
两种替代：

1. **工具调用做结构化提取（推荐，跨厂商通用）**：定义一个名为 `submit_xxx` 的工具，
   schema 就是你想要的结构，`tool_choice` 设为必选，模型"调用工具"的参数就是结构化结果。
   教程的提示词增强、偏好提取都会用这个模式。
2. **`with_extra_body` 逃生舱（仅 OpenAI 兼容厂商）**：直接注入厂商私有字段。能跑，但
   换厂商就失效，只用于过渡。

## 11. genai 事件 → Vercel AI SDK UI Message Stream 的映射

教程会把 genai 的流直接转成 AI SDK 协议喂给 `useChat`。核心映射关系：

| genai（ChatStreamEvent） | 你的 agent loop | AI SDK（data: {...}） |
|---|---|---|
| `Start` | 新一轮 LLM 调用开始 | `{"type":"start-step"}` |
| `Chunk` | 文本增量 | `{"type":"text-delta","id":"...","delta":"..."}` |
| `End(captured_content 有 ToolCall)` | 执行工具前 | `{"type":"tool-input-available",...}` |
| （工具执行完成） | 你的代码 | `{"type":"tool-output-available",...}` |
| `End` | 本轮结束 | `{"type":"finish-step"}` |
| 循环退出 | 整个回答完成 | `{"type":"finish"}` + `data: [DONE]` |

协议本身见 [AI SDK Stream Protocols](https://ai-sdk.dev/docs/ai-sdk-ui/stream-protocol)：
SSE、每行 `data: {json}`、响应头 `x-vercel-ai-ui-message-stream: v1`。

## 12. 常见坑

- **`Client::new()` 要 `?`**：0.7 返回 `Result`，从 0.6 迁移最容易漏。
- **`first_text()` 返回 `Option<&str>`**：是借用不是 `String`，需要拥有时 `.to_owned()`。
  写 `unwrap_or_else(|| "...".into())` 会因 `into()` 被推导为 `&str` 恒等转换而报类型错误。
- **流式拿不到工具调用**：忘了 `with_capture_tool_calls(true)`，只能看到分片。
- **fn_arguments 是字符串**：模型给的是 JSON 字符串，`serde_json::from_str` 自己解析。
- **`call_id` 必须原样回填**：`ToolResponse::new(call_id, ...)` 对不上号模型会困惑。
- **历史要自己管**：genai 完全无状态，多轮/工具轮次全靠你 `append_message` 累积。
- **Ollama 兜底**：无法识别的模型名会落到本地 Ollama，没跑 Ollama 时报连接错误——
  看到这种错先检查模型名前缀或用 namespace。

## 13. 参考资源

- 仓库与示例：<https://github.com/jeremychone/rust-genai>（`examples/` 下 33 个示例，
  工具调用看 `c20/c21/c22`，WebSearch 看 `c23`，认证看 `c02/c06`）
- API 文档：<https://docs.rs/genai/0.7.0-rc.1>
- 本仓库教程大纲：[tutorial/00-大纲.md](tutorial/00-大纲.md)
