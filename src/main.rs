use std::io::{BufRead, Write};

use anyhow::Result;
use comfy_agent::agent::{AgentOutcome, run_agent};
use genai::{
    Client,
    chat::{ChatMessage, ChatRequest},
    resolver::{Endpoint, ServiceTargetResolver},
};

#[tokio::main]
async fn main() -> Result<()> {
    // 从项目根目录加载 .env（已设置的环境变量优先，不覆盖）
    dotenvy::dotenv().ok();

    // 日志：默认用 RUST_LOG，没有则退回这个默认级别
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "comfy_agent=debug".into()),
        )
        .init();

    let model = std::env::var("MODEL").unwrap_or_else(|_| "glm-4.6".into());

    let resolver = ServiceTargetResolver::from_resolver_fn(|mut target: genai::ServiceTarget| {
        target.endpoint = Endpoint::from_static("https://open.bigmodel.cn/api/coding/paas/v4/");
        Ok(target)
    });

    let client = Client::builder()
        .with_service_target_resolver(resolver)
        .build()?;
    // let answer = chat_once(
    //     &client,
    //     &model,
    //     "你是一个简洁的助手，回答不超过两句话",
    //     "用一句话解释什么是 agent？",
    // )
    // .await?;
    // println!("answer: {answer}");

    let mut history = ChatRequest::default()
        .with_system("你是 comfy-agent，一个友好的助手。当前处于终端对话模式，回答保持简洁。");

    let stdin = std::io::stdin();

    loop {
        print!("\n你> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break;
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if input == "exit" || input == "quit" {
            break;
        }

        history = history.append_message(ChatMessage::user(input));

        print!("AI> ");
        match run_agent(&client, &model, &mut history, 6).await {
            Ok(AgentOutcome::Finished { steps, .. }) => {
                tracing::debug!(steps, "本回合完成");
            }
            Ok(AgentOutcome::StepLimit { steps }) => {
                println!("[停止] 已用完 {steps} 次模型请求，尚未获得最终回答。");
            }
            Err(error) => {
                eprintln!("[错误] {error:#}");
            }
        }
    }

    println!("\nBye!");
    Ok(())
}
