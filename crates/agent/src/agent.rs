use genai::{Client, chat::ChatRequest};

use crate::{Checkpoint, llm::stream_response, phase::Action, tool_registry::ToolRegistry};
use anyhow::Context;
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
    ReasoningDelta(String),
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

/// The in-memory driver uses the same phase machine as durable execution.
pub async fn run_agent(
    client: &Client,
    model: &str,
    history: &mut ChatRequest,
    registry: &ToolRegistry,
    max_steps: usize,
    mut on_event: impl FnMut(AgentEvent) + Send,
) -> anyhow::Result<AgentOutcome> {
    let mut checkpoint = Checkpoint::new(history.clone(), max_steps)?;
    let root = tracing::Span::current();
    let mut calls = 0i64;
    let mut known_tokens = 0i64;
    let mut complete_usage = true;
    let mut step_span = tracing::Span::none();
    loop {
        match checkpoint.next() {
            Action::Model { step } => {
                checkpoint.begin_model()?;
                step_span = tracing::info_span!("step");
                attribute(&step_span, "openinference.span.kind", "CHAIN");
                attribute(&step_span, "agent.step", step as i64);
                on_event(AgentEvent::StepStarted { step, max_steps });
                let request = checkpoint
                    .history
                    .clone()
                    .with_tools(registry.definitions());
                let response = stream_response(client, model, request, &mut on_event)
                    .instrument(step_span.clone())
                    .await;
                if response.is_err() {
                    telemetry::error(&step_span, "model-error");
                }
                let response = response.with_context(|| format!("第 {step} 步模型响应失败"))?;
                if let Some(total) = response.usage.as_ref().and_then(|usage| usage.total_tokens) {
                    known_tokens += i64::from(total);
                } else {
                    complete_usage = false;
                }
                attribute(&root, "agent.tokens.known_total", known_tokens);
                attribute(&root, "agent.tokens.complete", complete_usage);
                checkpoint.model_completed(response.content)?;
            }
            Action::Tool(_) => {
                let call = checkpoint.begin_tool()?;
                calls += 1;
                attribute(&root, "agent.tool_calls", calls);
                on_event(AgentEvent::ToolStarted {
                    call_id: call.call_id.clone(),
                    name: call.fn_name.clone(),
                    arguments: call.fn_arguments.clone(),
                });
                let (value, is_error) = execute_tool(registry, &call)
                    .instrument(step_span.clone())
                    .await;
                checkpoint.tool_completed(&call.call_id, value.clone())?;
                on_event(AgentEvent::ToolFinished {
                    call_id: call.call_id,
                    value,
                    is_error,
                });
            }
            Action::StepComplete { step } => {
                checkpoint.step_completed()?;
                *history = checkpoint.history.clone();
                on_event(AgentEvent::StepFinished { step });
                attribute(&root, "agent.steps", step as i64);
            }
            Action::Finished { answer, steps } => {
                return Ok(AgentOutcome::Finished { answer, steps });
            }
            Action::StepLimit { steps } => return Ok(AgentOutcome::StepLimit { steps }),
        }
    }
}

/// Execute and observe one tool action. The driver decides when to persist it.
pub async fn execute_tool(
    registry: &ToolRegistry,
    call: &genai::chat::ToolCall,
) -> (serde_json::Value, bool) {
    let span = tracing::info_span!("tool");
    attribute(&span, "openinference.span.kind", "TOOL");
    attribute(&span, "tool.name", call.fn_name.clone());
    attribute(&span, "agent.tool_call_id", call.call_id.clone());
    telemetry::content_policy().record(&span, "input", &call.fn_arguments);
    let (value, is_error) = match registry.execute(call).instrument(span.clone()).await {
        Ok(value) => (value, false),
        Err(error) => (serde_json::json!({"error":format!("{error:#}")}), true),
    };
    if is_error {
        telemetry::error(&span, "tool-error");
    } else {
        telemetry::content_policy().record(&span, "output", &value);
    }
    (value, is_error)
}
