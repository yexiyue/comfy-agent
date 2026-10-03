use std::io::Write;

use anyhow::Context;
use futures::StreamExt;
use genai::{
    Client,
    chat::{ChatMessage, ChatRequest, ChatStreamEvent, ToolResponse},
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
    let res = client.exec_chat(model, req, None).await?;

    let first_text = res.first_text().unwrap_or("<无文本回答>").to_owned();
    let tool_calls = res.into_tool_calls();

    if tool_calls.is_empty() {
        history = history.append_message(ChatMessage::assistant(first_text.clone()));
        return Ok((history, first_text));
    }

    let tc = tool_calls.first().unwrap();

    println!("工具调用: {tc:#?}");

    let args: GetWeather = serde_json::from_value(tc.fn_arguments.clone())
        .with_context(|| format!("解析工具调用参数失败: {tc:#?}"))?;

    let result = args
        .run()
        .with_context(|| format!("执行工具调用失败: {tc:#?}"))?;

    let tool_response = ToolResponse::new(tc.call_id.clone(), result.to_string());

    history = history
        .append_message(tool_calls)
        .append_message(tool_response);

    let res = client.exec_chat(model, history.clone(), None).await?;
    let answer = res.first_text().unwrap_or("<无文本回答>").to_owned();
    history = history.append_message(ChatMessage::assistant(answer.clone()));

    Ok((history, answer))
}
