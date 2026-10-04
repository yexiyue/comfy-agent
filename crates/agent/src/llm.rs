use crate::AgentEvent;
use anyhow::bail;
use futures::StreamExt;
use genai::{
    Client,
    chat::{ChatOptions, ChatRequest, ChatStreamEvent, MessageContent, StopReason, Usage},
};
use telemetry::attribute;
use tracing::Instrument;

pub(crate) struct ModelResponse {
    pub content: MessageContent,
    pub usage: Option<Usage>,
    pub stop_reason: Option<StopReason>,
}

pub(crate) async fn stream_response(
    client: &Client,
    model: &str,
    request: ChatRequest,
    on_event: &mut (impl FnMut(AgentEvent) + Send),
) -> anyhow::Result<ModelResponse> {
    let span = tracing::info_span!("model");
    attribute(&span, "openinference.span.kind", "LLM");
    attribute(&span, "llm.model_name", model.to_owned());
    telemetry::content_policy().record(&span, "input", &serde_json::to_value(&request)?);
    let started = std::time::Instant::now();
    let result = async {
        let options = ChatOptions::default()
            .with_capture_content(true)
            .with_capture_tool_calls(true)
            .with_capture_usage(true);
        let mut first_text = false;
        let mut response = client
            .exec_chat_stream(model, request, Some(&options))
            .await?;
        while let Some(event) = response.stream.next().await {
            match event? {
                ChatStreamEvent::Chunk(chunk) => {
                    if !first_text && !chunk.content.is_empty() {
                        first_text = true;
                        attribute(
                            &span,
                            "agent.model_ttft_ms",
                            started.elapsed().as_secs_f64() * 1000.0,
                        );
                    }
                    on_event(AgentEvent::TextDelta(chunk.content));
                }
                ChatStreamEvent::End(end) => {
                    if let Some(usage) = &end.captured_usage {
                        for (key, value) in [
                            ("llm.token_count.prompt", usage.prompt_tokens),
                            ("llm.token_count.completion", usage.completion_tokens),
                            ("llm.token_count.total", usage.total_tokens),
                            (
                                "llm.token_count.prompt_details.cache_read",
                                usage
                                    .prompt_tokens_details
                                    .as_ref()
                                    .and_then(|d| d.cached_tokens),
                            ),
                            (
                                "llm.token_count.prompt_details.cache_write",
                                usage
                                    .prompt_tokens_details
                                    .as_ref()
                                    .and_then(|d| d.cache_creation_tokens),
                            ),
                            (
                                "llm.token_count.completion_details.reasoning",
                                usage
                                    .completion_tokens_details
                                    .as_ref()
                                    .and_then(|d| d.reasoning_tokens),
                            ),
                        ] {
                            if let Some(value) = value {
                                attribute(&span, key, i64::from(value));
                            }
                        }
                    }
                    attribute(&span, "agent.usage.available", end.captured_usage.is_some());
                    if let Some(reason) = &end.captured_stop_reason {
                        attribute(&span, "agent.model_stop_reason", format!("{reason:?}"));
                    }
                    let content = end
                        .captured_content
                        .ok_or_else(|| anyhow::anyhow!("model stream did not capture content"))?;
                    telemetry::content_policy().record(
                        &span,
                        "output",
                        &serde_json::to_value(&content)?,
                    );
                    return Ok(ModelResponse {
                        content,
                        usage: end.captured_usage,
                        stop_reason: end.captured_stop_reason,
                    });
                }
                _ => {}
            }
        }
        bail!("model stream ended without an End event")
    }
    .instrument(span.clone())
    .await;
    if result.is_err() {
        telemetry::error(&span, "model-error");
    }
    result
}
