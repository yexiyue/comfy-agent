use genai::{
    Client,
    chat::{ChatMessage, ChatRequest, ToolCall, ToolResponse},
};

use crate::{llm::stream_response, tools::GetWeather};
use anyhow::{Context, bail};

pub enum AgentOutcome {
    Finished { answer: String, steps: usize },
    StepLimit { steps: usize },
}

fn execute_tool(tc: &ToolCall) -> anyhow::Result<serde_json::Value> {
    match tc.fn_name.as_str() {
        "get_weather" => {
            let args: GetWeather = serde_json::from_value(tc.fn_arguments.clone())
                .context("天气工具参数不符合 schema")?;
            args.run()
        }
        name => bail!("未知工具：{name}"),
    }
}

pub async fn run_agent(
    client: &Client,
    model: &str,
    history: &mut ChatRequest,
    max_steps: usize,
) -> anyhow::Result<AgentOutcome> {
    if max_steps == 0 {
        bail!("max_steps must be greater than 0");
    }

    for step in 1..=max_steps {
        println!("\n[步骤 {step}/{max_steps}]");
        let req = history.clone().with_tools(vec![GetWeather::tool()]);
        let content = stream_response(client, model, req)
            .await
            .with_context(|| format!("第 {step} 步模型响应失败"))?;

        let tool_calls = content.tool_calls();

        if tool_calls.is_empty() {
            let answer = content.texts().join("");
            if answer.trim().is_empty() {
                bail!("模型没有给出回答");
            }
            history.messages.push(ChatMessage::assistant(content));

            return Ok(AgentOutcome::Finished {
                answer,
                steps: step,
            });
        }

        let mut responses = Vec::new();

        for tc in tool_calls {
            println!("  [工具] {}({})", tc.fn_name, tc.fn_arguments);
            let value = match execute_tool(tc) {
                Ok(value) => value,
                Err(error) => serde_json::json!({ "error": format!("{error:#}") }),
            };
            responses.push(ToolResponse::new(tc.call_id.clone(), value.to_string()));
        }

        history.messages.push(ChatMessage::assistant(content));
        for response in responses {
            history.messages.push(ChatMessage::from(response));
        }
    }

    Ok(AgentOutcome::StepLimit { steps: max_steps })
}
