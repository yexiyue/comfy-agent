use std::io::Write;

use anyhow::Context;
use futures::StreamExt;
use genai::{
    Client,
    chat::{ChatRequest, ChatStreamEvent},
};

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
