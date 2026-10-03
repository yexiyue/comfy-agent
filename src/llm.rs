use std::io::Write;

use anyhow::{Context, bail};
use futures::StreamExt;
use genai::{
    Client,
    chat::{ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, MessageContent, ToolResponse},
};

use crate::tools::GetWeather;

pub async fn chat_once(
    client: &Client,
    model: &str,
    system: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    let request = ChatRequest::from_user(prompt).with_system(system);

    let res = client
        .exec_chat(model, request, None)
        .await
        .with_context(|| format!("exec_chat 失败（model = {model}）"))?;

    tracing::debug!("res: {res:#?}");

    Ok(res.first_text().unwrap_or("<无文本回答>").to_owned())
}

pub async fn chat_stream(client: &Client, model: &str, req: ChatRequest) -> anyhow::Result<String> {
    let mut stream = client.exec_chat_stream(model, req, None).await?;

    let mut full = String::new();
    while let Some(event) = stream.stream.next().await {
        match event? {
            ChatStreamEvent::Start => {}
            ChatStreamEvent::Chunk(chunk) => {
                print!("{}", chunk.content);
                std::io::stdout().flush()?;
                full.push_str(&chunk.content);
            }
            _ => {}
        }
    }
    println!();
    Ok(full)
}

pub async fn answer_turn(
    client: &Client,
    model: &str,
    mut history: ChatRequest,
) -> anyhow::Result<(ChatRequest, String)> {
    let req = history.clone().with_tools(vec![GetWeather::tool()]);
    let content = stream_response(client, model, req).await?;

    let answer = content.texts().join("");
    let tool_calls = content.tool_calls();

    if tool_calls.is_empty() {
        history = history.append_message(ChatMessage::assistant(content));
        return Ok((history, answer));
    }

    let mut responses = Vec::new();

    for tc in tool_calls {
        println!("  [工具] {}({})", tc.fn_name, tc.fn_arguments);
        let result: anyhow::Result<serde_json::Value> = (|| {
            if tc.fn_name != "get_weather" {
                bail!("未知工具：{}", tc.fn_name);
            }
            let args: GetWeather = serde_json::from_value(tc.fn_arguments.clone())
                .context("模型给的天气参数不符合 schema")?;
            args.run()
        })();
        let value = match result {
            Ok(value) => value,
            Err(error) => serde_json::json!({ "error": format!("{error:#}") }),
        };
        responses.push(ToolResponse::new(tc.call_id.clone(), value.to_string()));
    }

    history = history.append_message(ChatMessage::assistant(content));

    for response in responses {
        history = history.append_message(response);
    }

    let req = history.clone().with_tools(Vec::<genai::chat::Tool>::new());
    let content = stream_response(client, model, req).await?;
    if !content.tool_calls().is_empty() {
        bail!("总结阶段仍返回了工具调用；本篇仅支持一次工具往返");
    }

    let answer = content.texts().join("");
    history = history.append_message(ChatMessage::assistant(content));
    Ok((history, answer))
}

async fn stream_response(
    client: &Client,
    model: &str,
    req: ChatRequest,
) -> anyhow::Result<MessageContent> {
    let options = ChatOptions::default()
        .with_capture_content(true)
        .with_capture_tool_calls(true);

    let mut response = client.exec_chat_stream(model, req, Some(&options)).await?;

    while let Some(event) = response.stream.next().await {
        match event? {
            ChatStreamEvent::Chunk(chunk) => {
                print!("{}", chunk.content);
                std::io::stdout().flush()?;
            }
            ChatStreamEvent::End(end) => {
                println!();
                return end
                    .captured_content
                    .ok_or_else(|| anyhow::anyhow!("没有捕获到内容"));
            }
            _ => {}
        }
    }
    bail!("流式响应结束时没有收到 End 事件");
}
