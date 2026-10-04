//! Model adapter boundary, shared by production and deterministic tests.
use agent::{AgentEvent, ModelResponse};
use anyhow::Result;
use futures::future::BoxFuture;
use genai::chat::ChatRequest;

/// Displayable model output; provider continuation data stays in ModelResponse.
pub enum ModelDelta {
    Text(String),
    Reasoning(String),
}

pub trait ModelGateway: Send + Sync {
    fn response<'a>(
        &'a self,
        model: &'a str,
        request: ChatRequest,
        reasoning_effort: Option<&'a str>,
        on_delta: &'a mut (dyn FnMut(ModelDelta) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>>;
}

pub struct GenaiGateway(pub genai::Client);
impl ModelGateway for GenaiGateway {
    fn response<'a>(
        &'a self,
        model: &'a str,
        request: ChatRequest,
        reasoning_effort: Option<&'a str>,
        on_delta: &'a mut (dyn FnMut(ModelDelta) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>> {
        Box::pin(async move {
            agent::stream_response_with_effort(
                &self.0,
                model,
                request,
                reasoning_effort,
                &mut |event| match event {
                    AgentEvent::TextDelta(text) => on_delta(ModelDelta::Text(text)),
                    AgentEvent::ReasoningDelta(text) => on_delta(ModelDelta::Reasoning(text)),
                    _ => {}
                },
            )
            .await
        })
    }
}
