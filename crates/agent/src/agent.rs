use genai::{
    Client,
    chat::{ChatMessage, ChatRequest, ToolResponse},
};

use crate::{llm::stream_response, tool_registry::ToolRegistry};
use anyhow::{Context, bail};
use telemetry::attribute;
use tracing::Instrument;

#[derive(Debug, PartialEq, Eq)]
pub enum AgentOutcome {
    Finished { answer: String, steps: usize },
    StepLimit { steps: usize },
}

/// Events emitted as a turn progresses. Rendering is the caller's responsibility.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    StepStarted {
        step: usize,
        max_steps: usize,
    },
    TextDelta(String),
    StepFinished {
        step: usize,
    },
    ToolStarted {
        call_id: String,
        name: String,
        arguments: serde_json::Value,
    },
    ToolFinished {
        call_id: String,
        value: serde_json::Value,
        is_error: bool,
    },
}

pub async fn run_agent(
    client: &Client,
    model: &str,
    history: &mut ChatRequest,
    registry: &ToolRegistry,
    max_steps: usize,
    mut on_event: impl FnMut(AgentEvent) + Send,
) -> anyhow::Result<AgentOutcome> {
    if max_steps == 0 {
        bail!("max_steps must be greater than 0");
    }

    let definitions = registry.definitions();
    let root = tracing::Span::current();
    let mut calls = 0i64;
    let mut known_tokens = 0i64;
    let mut complete_usage = true;

    for step in 1..=max_steps {
        let span = tracing::info_span!("step");
        attribute(&span, "openinference.span.kind", "CHAIN");
        attribute(&span, "agent.step", step as i64);
        let answer = async {
            on_event(AgentEvent::StepStarted { step, max_steps });
            let req = history.clone().with_tools(definitions.clone());
            let response = stream_response(client, model, req, &mut on_event)
                .await
                .with_context(|| format!("第 {step} 步模型响应失败"))?;
            if let Some(total) = response.usage.as_ref().and_then(|u| u.total_tokens) {
                known_tokens += i64::from(total);
            } else {
                complete_usage = false;
            }
            let _ = response.stop_reason;
            attribute(&root, "agent.tokens.known_total", known_tokens);
            attribute(&root, "agent.tokens.complete", complete_usage);
            let content = response.content;

            let tool_calls = content.tool_calls();

            if tool_calls.is_empty() {
                let answer = content.texts().join("");
                if answer.trim().is_empty() {
                    bail!("模型没有给出回答");
                }
                history.messages.push(ChatMessage::assistant(content));
                on_event(AgentEvent::StepFinished { step });

                return Ok::<_, anyhow::Error>(Some(answer));
            }

            let mut responses = Vec::new();

            for tc in tool_calls {
                calls += 1;
                attribute(&root, "agent.tool_calls", calls);
                let tool_span = tracing::info_span!("tool");
                attribute(&tool_span, "openinference.span.kind", "TOOL");
                attribute(&tool_span, "tool.name", tc.fn_name.clone());
                attribute(&tool_span, "agent.tool_call_id", tc.call_id.clone());
                telemetry::content_policy().record(&tool_span, "input", &tc.fn_arguments);
                on_event(AgentEvent::ToolStarted {
                    call_id: tc.call_id.clone(),
                    name: tc.fn_name.clone(),
                    arguments: tc.fn_arguments.clone(),
                });
                let (value, is_error) =
                    match registry.execute(tc).instrument(tool_span.clone()).await {
                        Ok(value) => (value, false),
                        Err(error) => (serde_json::json!({ "error": format!("{error:#}") }), true),
                    };
                if is_error {
                    telemetry::error(&tool_span, "tool-error");
                } else {
                    telemetry::content_policy().record(&tool_span, "output", &value);
                }
                on_event(AgentEvent::ToolFinished {
                    call_id: tc.call_id.clone(),
                    value: value.clone(),
                    is_error,
                });
                responses.push(ToolResponse::new(tc.call_id.clone(), value.to_string()));
            }

            history.messages.push(ChatMessage::assistant(content));
            for response in responses {
                history.messages.push(ChatMessage::from(response));
            }
            on_event(AgentEvent::StepFinished { step });
            Ok(None)
        }
        .instrument(span.clone())
        .await;
        if answer.is_err() {
            telemetry::error(&span, "model-error");
        }
        let answer = answer?;
        attribute(&root, "agent.steps", step as i64);
        if let Some(answer) = answer {
            return Ok(AgentOutcome::Finished {
                answer,
                steps: step,
            });
        }
    }

    Ok(AgentOutcome::StepLimit { steps: max_steps })
}
