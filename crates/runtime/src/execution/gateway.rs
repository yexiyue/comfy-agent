//! Model adapter boundary, shared by production and deterministic tests.
use agent::{AgentEvent, ModelResponse};
use anyhow::Result;
use futures::future::BoxFuture;
use genai::chat::ChatRequest;
pub trait ModelGateway: Send + Sync {
    fn response<'a>(
        &'a self,
        model: &'a str,
        request: ChatRequest,
        on_text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>>;
}

pub struct GenaiGateway(pub genai::Client);
impl ModelGateway for GenaiGateway {
    fn response<'a>(
        &'a self,
        model: &'a str,
        request: ChatRequest,
        on_text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>> {
        Box::pin(async move {
            agent::stream_response(&self.0, model, request, &mut |event| {
                if let AgentEvent::TextDelta(text) = event {
                    on_text(text);
                }
            })
            .await
        })
    }
}
